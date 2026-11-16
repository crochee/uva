//! The tail every child round owes, and why it survives a dropped
//! future.
//!
//! A round ends with three facts: the roster entry settles (so
//! `agents_list` stops reporting a finished child as `running`), the
//! child's session closes with its `subagent_exit` footer, and the
//! child's artefact set lands in its session directory. `docs/log-format.md`
//! promises all three for every round — a cancelled one included.
//!
//! A plain `async fn` cannot promise that. When the parent run is
//! cancelled the harness stops draining the tool stream and **drops**
//! the dispatch future; everything after the current `await` in it —
//! the settle, the footer, the artefact set — never runs. The child is
//! left with a dangling `subagent_enter`, the roster row is stuck at
//! `running` with no `idle_since` (so the TTL never ages it), and the
//! round has no artefact set.
//!
//! [`RoundTail`] is the fix: the work lives in a value whose `Drop`
//! runs it. The normal path awaits [`RoundTail::land`] and leaves the
//! guard with nothing to do; the abandoned path settles the roster
//! synchronously inside `Drop` (a `Mutex` write, no `await` needed) and
//! detaches the writer half — queueing the footer, flushing, writing
//! the artefacts — onto the tool's [`Spawner`](uva_core::spawn::Spawner),
//! because `Drop` cannot await.
//!
//! An abandoned round reports `cancelled`: the future was discarded
//! before the child's end reason could be read, and the parent going
//! away is the only reason a dispatch future is dropped. It leaves no
//! `.patch` artefact — that needs the worktree this guard does not
//! own.

use std::sync::Arc;

use uva_core::spawn::SharedSpawner;
use uva_session::{SessionLog, subagent_exit};

use crate::control::{ControlSurface, RoundOutcome};

/// The footer status a round reports for `outcome`.
fn status_of(outcome: RoundOutcome) -> &'static str {
    match outcome {
        RoundOutcome::Completed => "completed",
        RoundOutcome::Cancelled => "cancelled",
        RoundOutcome::Failed => "error",
    }
}

/// One round's tail: armed when the child is filed, landed when the
/// round ends, run from `Drop` when the dispatch future is discarded.
pub(crate) struct RoundTail {
    pending: Option<Pending>,
}

/// The tail's owned half — everything the abandoned path needs, with
/// no borrow on the dispatch future's stack frame.
struct Pending {
    control: Arc<ControlSurface>,
    parent: String,
    child: String,
    agent: String,
    depth: usize,
    log: Option<Arc<dyn SessionLog>>,
    spawner: SharedSpawner,
}

impl RoundTail {
    /// Arm the tail for one child round.
    ///
    /// Call this as soon as the child is filed in the roster: from here
    /// on, dropping the future that owns this guard still closes the
    /// round.
    pub(crate) fn new(
        control: Arc<ControlSurface>,
        parent: &str,
        child: &str,
        agent: &str,
        depth: usize,
        log: Option<Arc<dyn SessionLog>>,
        spawner: SharedSpawner,
    ) -> Self {
        Self {
            pending: Some(Pending {
                control,
                parent: parent.to_owned(),
                child: child.to_owned(),
                agent: agent.to_owned(),
                depth,
                log,
                spawner,
            }),
        }
    }

    /// Land the round's tail: the roster settle first (the row the
    /// model reads), then the exit footer, then the artefact set.
    ///
    /// A second call is a no-op: the first one took the payload.
    pub(crate) async fn land(
        &mut self,
        outcome: RoundOutcome,
        patch: Option<String>,
    ) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        pending.settle(outcome);
        pending.record(outcome, patch).await;
    }
}

impl Drop for RoundTail {
    fn drop(&mut self) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        // The dispatch future was dropped, so the child's end reason
        // was never read: `cancelled` is what an abandoned round is —
        // the parent went away mid-flight.
        pending.settle(RoundOutcome::Cancelled);
        let spawner = Arc::clone(&pending.spawner);
        spawner.spawn(Box::pin(async move {
            pending.record(RoundOutcome::Cancelled, None).await;
        }));
    }
}

impl Pending {
    /// Mark the running round over, so the roster stops saying
    /// `running` and the TTL can age the row.
    fn settle(&self, outcome: RoundOutcome) {
        self.control
            .registry()
            .settle(&self.parent, &self.child, outcome);
    }

    /// The writer half: the exit footer and the artefact set.
    async fn record(self, outcome: RoundOutcome, patch: Option<String>) {
        let status = status_of(outcome);
        if let Some(log) = self.log.as_ref() {
            let footer = subagent_exit(
                &self.child,
                u32::try_from(self.depth).unwrap_or(u32::MAX),
                status,
            );
            if let Err(error) = log.append_event(&footer).await {
                tracing::warn!(
                    child_session_id = %self.child,
                    %error,
                    "task: the child's exit footer could not be queued"
                );
            }
            if let Err(error) = log.flush().await {
                tracing::warn!(
                    child_session_id = %self.child,
                    %error,
                    "task: the child's log did not flush"
                );
            }
        }
        self.control.write_artifacts(
            &self.parent,
            &self.child,
            &self.agent,
            self.depth,
            status,
            patch,
        );
    }
}
