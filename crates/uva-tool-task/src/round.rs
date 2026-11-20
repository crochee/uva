//! One round of a child session — the module that owns it.
//!
//! A round is one peer run inside a child session: the input the peer
//! is handed, the token that ends it, and the text the parent is told
//! about. The `task` tool drives the first round; the control surface
//! drives the follow-up and the cold wake. The round lives here so
//! both callers drive a child the same way.
//!
//! # What the round owns
//!
//! - **the input** — the text the peer is handed, the child session's
//!   own thread, its session log, the steer channel it reads and the
//!   workspace it runs in;
//! - **the cancellation** — the token the peer watches, and the end
//!   reason the round reports when it fires;
//! - **the drain** — the peer's event stream, read to its end;
//! - **the outcome** — the end reason mapped once into the vocabulary
//!   the roster, the exit footer and the progress line share;
//! - **the report** — what the parent is told: the answer, the child
//!   session's handle line, and the wording of a round that ended
//!   without an answer;
//! - **the tail** — the roster settle, the `subagent_exit` footer and
//!   the artefact set, armed before the first `await` so a dropped
//!   dispatch future still closes the round;
//! - **the accounting** — the tokens the child consumed and the round
//!   it counted as, reported to the deployment's usage account.
//!
//! # What the round does not own
//!
//! The worktree's lifecycle and the gate's verdict. Whether a
//! delegation is isolated, and whether it has to pass a gate, are the
//! caller's decisions: the round asks the isolation it is handed where
//! to run and what it changed, and leaves the gate to the caller.
//!
//! # The depth rule
//!
//! The depth of a session — how deep the tree of child sessions goes
//! — belongs to this module too, in [`depth`]: the cap is one
//! judgment, the depths live in one table, and both callers ask the
//! module instead of counting layers themselves.

use std::{path::Path, sync::Arc};

use uva_agent::{Agent, AgentInput, RunInbox};
use uva_core::CancelToken;
use uva_events::SessionEndReason;
use uva_session::SessionLog;

use crate::{
    child::{drain_child_observed, outcome_of},
    control::{ControlSurface, RoundOutcome},
    progress::{L1Feed, L1Observer},
    tail::RoundTail,
};

pub(crate) mod depth;

/// The isolation a round runs inside.
///
/// The worktree's lifecycle belongs to the caller: creating it,
/// handing over its path and tearing it down are the caller's
/// business. The round asks the isolation for two things and nothing
/// else — where the child runs, and what the round changed.
///
/// The seam exists for the **timing of the patch**, not for
/// polymorphism: the diff can only be computed while the worktree is
/// still there, so the round asks for it before the caller's cleanup
/// takes the worktree down.
pub(crate) trait Isolation: Send + Sync {
    /// The directory the child runs in.
    fn path(&self) -> &Path;

    /// The diff the round produced, if it produced one.
    fn patch(&self) -> Option<String>;
}

/// One round's input: everything the round needs to run a peer.
pub(crate) struct RoundInput {
    /// The peer the round runs.
    pub(crate) peer: Arc<dyn Agent>,
    /// The text the peer is handed.
    pub(crate) prompt: String,
    /// The parent session the round belongs to.
    pub(crate) parent_id: String,
    /// The child session the round runs in.
    pub(crate) child_id: String,
    /// The name the child runs under.
    pub(crate) agent: String,
    /// The depth the child session runs at.
    pub(crate) depth: usize,
    /// The child's session log: the exit footer lands here.
    pub(crate) writer: Option<Arc<dyn SessionLog>>,
    /// The channel the child's steer messages arrive through.
    pub(crate) inbox: Arc<dyn RunInbox>,
    /// The token that ends the round.
    pub(crate) cancel: Arc<dyn CancelToken>,
    /// The isolation the round runs inside, if it is isolated.
    pub(crate) isolation: Option<Arc<dyn Isolation>>,
    /// The feed the round's progress line reaches, if the caller
    /// streams one.
    pub(crate) feed: Option<Arc<L1Feed>>,
}

/// What one round left behind.
pub(crate) struct RoundResult {
    /// The text the peer answered, if it answered.
    pub(crate) text: Option<String>,
    /// Why the round ended.
    pub(crate) end_reason: SessionEndReason,
    /// How the round ended, in the roster's vocabulary.
    pub(crate) outcome: RoundOutcome,
    /// The diff the round produced, if it ran inside an isolation.
    ///
    /// Carried, not dropped: a round's result is the round's whole
    /// story — the answer, why it ended, and what it changed. The
    /// artefact set borrows this diff; it never copies it.
    pub(crate) patch: Option<String>,
    /// The child session the round ran in.
    pub(crate) child_id: String,
    /// The name the child ran under.
    pub(crate) agent: String,
}

impl RoundResult {
    /// The answer the parent is told, if the round answered: the
    /// text the peer gave, the caller's own line, and the child
    /// session's handle line.
    ///
    /// `extra` is the caller's own line — the worktree report the
    /// delegation path adds — and lands between the answer and the
    /// handle line.
    pub(crate) fn answer(&self, extra: Option<&str>) -> Option<String> {
        let text = self.text.as_deref()?;
        let mut combined = text.to_string();
        if let Some(extra) = extra {
            combined.push_str(extra);
        }
        combined.push_str(&session_line(&self.child_id));
        Some(combined)
    }

    /// Why the round left no answer, in the words both callers
    /// report.
    ///
    /// A round that was stopped or that failed says how it ended; a
    /// round that ran to its end without saying anything says that.
    /// Both name the child session, so the model can open the
    /// transcript either way.
    pub(crate) fn failure(&self) -> String {
        if self.outcome != RoundOutcome::Completed {
            return format!(
                "sub-agent `{}` ended before completing: {}{}",
                self.agent,
                self.reason(),
                session_line(&self.child_id),
            );
        }
        format!(
            "sub-agent `{}` produced no final output{}",
            self.agent,
            session_line(&self.child_id),
        )
    }

    /// Why the round ended, in the words the model reads: the
    /// runtime's end-reason word, and — when the reason carries one —
    /// the detail it came with.
    ///
    /// The detail is the model's only clue about a failure: without
    /// it, "the peer gave up" and "the provider refused the key" read
    /// the same.
    fn reason(&self) -> String {
        match &self.end_reason {
            SessionEndReason::Error(detail) if !detail.trim().is_empty() => {
                format!("{} ({detail})", self.end_reason.as_str())
            }
            _ => self.end_reason.as_str().to_string(),
        }
    }
}

/// The machine-readable tail line every round's result carries,
/// naming the child session the trace lives in (L2 drill-down).
///
/// The line is the round's shape, not a caller's: both the
/// delegation and the wake report it, so the model reads the same
/// handle wherever the round was driven from.
pub(crate) fn session_line(child_session_id: &str) -> String {
    format!("\n\n[subagent session: {child_session_id}]")
}

impl std::fmt::Debug for RoundResult {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RoundResult")
            .field("child_id", &self.child_id)
            .field("end_reason", &self.end_reason)
            .field("outcome", &self.outcome)
            .field("answered", &self.text.is_some())
            .field("patched", &self.patch.is_some())
            .finish_non_exhaustive()
    }
}

/// The runner that owns one round of a child session.
///
/// The surface it runs over is the roster the round settles its row
/// in, the spawner its tail detaches onto and the usage account it
/// reports to.
pub(crate) struct ChildRound {
    control: Arc<ControlSurface>,
}

impl ChildRound {
    /// The round runner over `control`.
    pub(crate) fn new(control: Arc<ControlSurface>) -> Self {
        Self { control }
    }

    /// Run one round of a child session and report what it left
    /// behind.
    ///
    /// The round's tail is armed before the peer is drained: from here
    /// on, dropping the future that owns this round still closes it.
    pub(crate) async fn run(&self, input: RoundInput) -> RoundResult {
        let RoundInput {
            peer,
            prompt,
            parent_id,
            child_id,
            agent,
            depth,
            writer,
            inbox,
            cancel,
            isolation,
            feed,
        } = input;

        // The round's tail: the roster settle, the exit footer and the
        // artefact set. Armed here — before the first `await` a
        // cancellation can cut — so a dropped dispatch future still
        // closes the round.
        let mut tail = RoundTail::new(
            Arc::clone(&self.control),
            &parent_id,
            &child_id,
            &agent,
            depth,
            writer.clone(),
            self.control.spawner(),
        );

        // The input the peer is handed: the text, the child session's
        // own thread, its session log, the steer channel, and — when
        // the round is isolated — the worktree it runs in.
        let mut agent_input = AgentInput::text(prompt);
        agent_input.thread_id = Some(child_id.clone());
        agent_input.log_writer = writer;
        agent_input = agent_input.with_inbox(inbox);
        if let Some(isolation) = isolation.as_ref() {
            agent_input =
                agent_input.with_workspace(isolation.path().to_path_buf());
        }

        // The peer is drained to its end. Its activity reaches the
        // caller's progress feed and its consumption the deployment's
        // usage account.
        let usage = self.control.usage();
        let mut observer = feed.map(|feed| L1Observer::new(feed, &child_id));
        let (end_reason, text) = drain_child_observed(
            peer,
            agent_input,
            cancel,
            observer.as_mut(),
            usage.as_deref(),
            &child_id,
        )
        .await;
        let outcome = outcome_of(&end_reason);
        if let Some(observer) = observer.as_mut() {
            observer.finish(outcome);
        }

        // The round's artefact set lands in the child's own session
        // directory. An isolated round also leaves its diff.
        let patch = isolation.as_ref().and_then(|isolation| isolation.patch());

        // The child session closes before the gate runs: the gate is
        // the parent's verdict on the child, not part of the child's
        // run.
        tail.land(outcome, patch.as_deref()).await;

        RoundResult {
            text,
            end_reason,
            outcome,
            patch,
            child_id,
            agent,
        }
    }
}

#[cfg(test)]
mod tests;
