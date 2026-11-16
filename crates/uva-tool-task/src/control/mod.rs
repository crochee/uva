//! The control surface: the three model tools a session drives its own
//! sub-agents with, and the lifecycle the handle table owns.
//!
//! The `task` tool spawns a child; the control surface is what the
//! model does with the children it already has:
//!
//! - `agents_list` — the roster of this session's sub-agents: id,
//!   name, state, and what each is doing now.
//! - `agents_message` — a message, dispatched by the target's state:
//!   a running child is steered, an idle child is asked for another
//!   round, and a child whose handles are gone is woken from a cold
//!   start.
//! - `agents_interrupt` — the current round of a running child is
//!   stopped. The child stays usable: an interrupt is not a kill.
//!
//! # The handle table
//!
//! Everything the surface needs about a live child lives in one
//! table ([`ControlRegistry`]): the sender half of its steer channel,
//! its own cancellation token, its session writer and the peer it
//! runs. The `task` tool fills the table when it dispatches a child
//! and settles it when the round ends; the three tools read it.
//!
//! # The roster's source
//!
//! The roster is read from the handle table, never rebuilt from a scan
//! at read time. The sessions are the **durable** record, so the table
//! reconciles itself against them: a child this process does not hold
//! — one a previous process dispatched — is adopted as a `parked` row
//! ([`ControlRegistry::adopt`]). `parked` means exactly "the handles
//! are gone", which is what a restart leaves behind, and a message to
//! such a row takes the cold-wake branch. The reading itself lives in
//! [`scan`]; the host owns the access through [`SessionIndex`].
//!
//! # The memory lifecycle
//!
//! In-memory children age. [`ControlSurface::tick`] parks every idle
//! child past [`PARK_TTL`], releasing its handles and keeping its row;
//! a message then wakes it from its session log, and the wake takes
//! over the row with the new round's handles. The session that owns
//! the table is never parked. A host that wants parking to happen
//! while the process is idle installs its runtime's seams with
//! [`ControlSurface::set_park_loop`]; the surface starts one loop on
//! the first tick and stops it when the surface is dropped.

mod artifacts;
pub(crate) mod park;
pub mod registry;
pub mod scan;
pub mod state;
pub mod tools;

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use parking_lot::RwLock;
pub use registry::{
    ChildHandle,
    ChildSummary,
    ColdWake,
    ControlError,
    ControlRegistry,
    FollowUpTarget,
    MessageTarget,
    SteerTarget,
};
pub use scan::SessionIndex;
pub use state::{
    ChildState,
    ChildStatus,
    MessageOutcome,
    PARK_TTL,
    RoundOutcome,
    TransitionError,
};
pub use tools::{
    AGENTS_INTERRUPT_TOOL_NAME,
    AGENTS_LIST_TOOL_NAME,
    AGENTS_MESSAGE_TOOL_NAME,
    AgentsInterruptTool,
    AgentsListTool,
    AgentsMessageTool,
};
use uva_agent::{AgentRegistry, SubagentUsage};
use uva_core::{SharedClock, sleeper::Sleeper, spawn::SharedSpawner};

/// How often the parking loop advances the lifecycle.
///
/// The TTL is five minutes; a one-minute tick parks a child within a
/// minute of crossing it.
pub const PARK_TICK: Duration = Duration::from_secs(60);

/// The runtime seams a host installs to let the surface park on its
/// own.
#[derive(Clone)]
struct ParkSeams {
    spawner: SharedSpawner,
    sleeper: Arc<dyn Sleeper>,
    interval: Duration,
}

/// The control surface one session's model drives.
///
/// It holds the handle table, the peer catalog the cold wake resolves
/// an agent name through, and the host's session index (the roster's
/// durable source and the artefact set's home).
pub struct ControlSurface {
    registry: Arc<ControlRegistry>,
    peers: Arc<AgentRegistry>,
    index: RwLock<Option<Arc<dyn SessionIndex>>>,
    state: RwLock<Option<Arc<crate::tool::TaskState>>>,
    park: RwLock<Option<ParkSeams>>,
    /// Where an abandoned round's writer half is detached. The default
    /// is the runtime-free [`InlineSpawner`](uva_core::spawn::InlineSpawner);
    /// a host on a runtime installs its own.
    spawner: RwLock<SharedSpawner>,
    /// Where a child round's token usage and its turn are reported.
    ///
    /// A child runs on its own harness, so its events never reach the
    /// parent session's controller; without this the deployment's
    /// counters would miss every delegated round.
    usage: RwLock<Option<Arc<dyn SubagentUsage>>>,
    parking: AtomicBool,
}

impl ControlSurface {
    /// A surface over `peers`, with an empty handle table and no
    /// session index.
    #[must_use]
    pub fn new(peers: Arc<AgentRegistry>) -> Self {
        Self {
            registry: Arc::new(ControlRegistry::with_clock(
                SharedClock::system(),
            )),
            peers,
            index: RwLock::new(None),
            state: RwLock::new(None),
            park: RwLock::new(None),
            spawner: RwLock::new(Arc::new(uva_core::spawn::InlineSpawner)),
            usage: RwLock::new(None),
            parking: AtomicBool::new(false),
        }
    }

    /// Measure the parking TTL on `clock`.
    ///
    /// A builder seam: install it before the first child is filed.
    #[must_use]
    pub fn with_clock(self, clock: SharedClock) -> Self {
        self.registry.set_clock(clock);
        self
    }

    /// Share the tool's depth table with this surface.
    ///
    /// The tool records a child's depth when it dispatches it; a round
    /// the surface wakes (or follows up) runs at the depth the handle
    /// table remembers, so it records it back into the same table — a
    /// nested `task` call from that round then sees the depth it runs
    /// at instead of starting over at zero.
    #[must_use]
    pub(crate) fn with_task_state(
        self,
        state: Arc<crate::tool::TaskState>,
    ) -> Self {
        *self.state.write() = Some(state);
        self
    }

    /// The tool's depth table, when the tool wired one in.
    pub(crate) fn task_state(&self) -> Option<Arc<crate::tool::TaskState>> {
        self.state.read().clone()
    }

    /// Install the host's session index.
    #[must_use]
    pub fn with_index(self, index: Arc<dyn SessionIndex>) -> Self {
        *self.index.write() = Some(index);
        self
    }

    /// Install the host's session index after the surface was built.
    ///
    /// The index is a host service: the tool brick is assembled
    /// before the sessions service is published, so the host fills it
    /// in once the mount finished.
    pub fn set_index(&self, index: Arc<dyn SessionIndex>) {
        *self.index.write() = Some(index);
    }

    /// The host's session index, when one is installed.
    #[must_use]
    pub fn index(&self) -> Option<Arc<dyn SessionIndex>> {
        self.index.read().clone()
    }

    /// The handle table.
    #[must_use]
    pub fn registry(&self) -> &Arc<ControlRegistry> {
        &self.registry
    }

    /// The peer catalog a cold wake resolves an agent name through.
    #[must_use]
    pub fn peers(&self) -> &Arc<AgentRegistry> {
        &self.peers
    }

    /// Install the spawner an abandoned round's tail is detached onto.
    ///
    /// The default is the runtime-free
    /// [`InlineSpawner`](uva_core::spawn::InlineSpawner) (one OS thread
    /// per round that outlives its dispatch); a host on a runtime
    /// installs its own, like every other spawn seam.
    pub fn set_spawner(&self, spawner: SharedSpawner) {
        *self.spawner.write() = spawner;
    }

    /// The spawner detached round tails run on.
    #[must_use]
    pub(crate) fn spawner(&self) -> SharedSpawner {
        Arc::clone(&self.spawner.read())
    }

    /// Install the deployment's sub-agent usage account.
    ///
    /// The control surface reports each follow-up and wake round here;
    /// a host that keeps deployment-wide counters implements
    /// [`SubagentUsage`] over them.
    pub fn set_usage(&self, usage: Arc<dyn SubagentUsage>) {
        *self.usage.write() = Some(usage);
    }

    /// The deployment's sub-agent usage account, when one is installed.
    #[must_use]
    pub(crate) fn usage(&self) -> Option<Arc<dyn SubagentUsage>> {
        self.usage.read().clone()
    }

    /// Let the surface park idle children on its own.
    ///
    /// The host installs the same runtime seams its runs already use
    /// (`Spawner` + `Sleeper`); the loop starts on the first
    /// [`tick`](Self::tick) and holds the surface weakly, so it ends
    /// with it.
    pub fn set_park_loop(
        &self,
        spawner: SharedSpawner,
        sleeper: Arc<dyn Sleeper>,
    ) {
        *self.park.write() = Some(ParkSeams {
            spawner,
            sleeper,
            interval: PARK_TICK,
        });
    }

    /// Advance the lifecycle: park every idle child past the TTL.
    ///
    /// Starts the parking loop when the host installed one, so a
    /// surface with seams keeps aging children between tool calls. A
    /// parked child's depth entry is dropped with its handles: the
    /// wake re-reads the depth from the child's log.
    /// Returns how many children parked.
    pub fn tick(self: &Arc<Self>) -> usize {
        self.ensure_parking();
        let parked = self.registry.tick();
        if let Some(state) = self.task_state() {
            for (_, child) in &parked {
                state.forget_depth(child);
            }
        }
        parked.len()
    }

    /// Start the parking loop exactly once.
    fn ensure_parking(self: &Arc<Self>) {
        if self.parking.load(Ordering::SeqCst) {
            return;
        }
        let Some(seams) = self.park.read().clone() else {
            return;
        };
        if self.parking.swap(true, Ordering::SeqCst) {
            return;
        }
        park::spawn_loop(self, seams.spawner, seams.sleeper, seams.interval);
    }

    /// Reconcile the roster with the durable sessions.
    ///
    /// Every child of `parent` the sessions record and this process
    /// does not hold is adopted as a `parked` row. The live row always
    /// wins: adoption never overwrites a child with handles.
    pub async fn refresh_roster(&self, parent: &str) {
        let Some(index) = self.index() else {
            return;
        };
        let held: Vec<String> = self
            .registry
            .list(parent)
            .into_iter()
            .map(|row| row.id)
            .collect();
        for record in scan::discover(index.as_ref(), parent).await {
            if held.iter().any(|id| id == &record.id) {
                continue;
            }
            self.registry.adopt(parent, &record.id, &record.agent_name);
        }
    }

    /// Project the round that just ended into the child's artefact
    /// set (`artifacts/<id>.{md,jsonl,patch}` in its session
    /// directory).
    ///
    /// Best-effort by design: the round already happened, and its
    /// summary failing to land is a log line, not a failed result. A
    /// host that keeps sessions off the filesystem returns `None` from
    /// [`SessionIndex::session_dir`], and no artefacts are written.
    pub(crate) fn write_artifacts(
        &self,
        parent: &str,
        child: &str,
        agent: &str,
        depth: usize,
        status: &str,
        patch: Option<String>,
    ) {
        let Some(index) = self.index() else {
            return;
        };
        let Some(session_dir) = index.session_dir(child) else {
            return;
        };
        let record = artifacts::RoundRecord {
            id: child,
            agent,
            parent,
            depth,
            status,
            patch,
        };
        match artifacts::write(&session_dir, &record) {
            Ok(paths) => tracing::debug!(
                child_session_id = child,
                markdown = %paths.markdown.display(),
                transcript = %paths.transcript.display(),
                "control: the child's artefact set was written"
            ),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                tracing::debug!(
                    child_session_id = child,
                    "control: the child has no durable session, so no \
                     artefact set was written"
                );
            }
            Err(error) => tracing::warn!(
                child_session_id = child,
                %error,
                "control: the child's artefact set could not be written"
            ),
        }
    }

    /// The `agents_list` tool.
    #[must_use]
    pub fn list_tool(self: &Arc<Self>) -> Arc<AgentsListTool> {
        Arc::new(AgentsListTool::new(Arc::clone(self)))
    }

    /// The `agents_message` tool.
    #[must_use]
    pub fn message_tool(self: &Arc<Self>) -> Arc<AgentsMessageTool> {
        Arc::new(AgentsMessageTool::new(Arc::clone(self)))
    }

    /// The `agents_interrupt` tool.
    #[must_use]
    pub fn interrupt_tool(self: &Arc<Self>) -> Arc<AgentsInterruptTool> {
        Arc::new(AgentsInterruptTool::new(Arc::clone(self)))
    }
}

impl std::fmt::Debug for ControlSurface {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlSurface")
            .field("peers", &self.peers.names())
            .field("index", &self.index.read().is_some())
            .field("parking", &self.parking.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
