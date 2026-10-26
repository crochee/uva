//! The wire operations a [`SessionController`](super::SessionController)
//! accepts, and the lifecycle state it reports.

use synthia::harness::{ResumeDecision, ResumeToken};

/// Operations that can be submitted to a
/// [`SessionController`](super::SessionController).
#[derive(Debug, Clone)]
pub enum SessionOp {
    /// Plain-text prompt. Flows through the in-memory
    /// [`InputQueue`](crate::session::input_queue::InputQueue) so a
    /// multi-turn chat can accumulate prompts while a run is in
    /// flight and drain them on the next `maybe_start_run`.
    Prompt {
        content: String,
        priority: u8,
    },
    /// Multimodal prompt. Carries one or more
    /// [`synthia::provider::ContentPart`] values (text + image /
    /// audio / file). Bypasses the text-only `InputQueue` —
    /// multimodality is meaningless without the original bytes,
    /// so we can't losslessly round-trip through the JSON-string
    /// `Value` channel the queue exposes. `agent_name`, when
    /// `Some`, overrides the configured default for this run
    /// Set `Some(name)` to override the configured default.
    PromptMulti {
        parts: Vec<synthia::provider::ContentPart>,
        agent_name: Option<String>,
        priority: u8,
    },
    /// Rerun a user turn from parts recovered off the durable log.
    /// Used by the `POST /api/v1/chat/sessions/:id/regenerate`
    /// endpoint that powers the chat UI's "Regenerate" affordance.
    ///
    /// The payload is whatever the log held: the prompt **text**.
    /// Attachment bytes are not persisted (see the persistence block
    /// in the run task), so a regenerated image turn replays its text
    /// without the image. That is a property of the caller, not of
    /// this op — a caller holding the bytes may pass them.
    ///
    /// `agent_name` follows the same override semantics as
    /// `PromptMulti`. `priority` mirrors `PromptMulti`'s.
    Rerun {
        parts: Vec<synthia::provider::ContentPart>,
        agent_name: Option<String>,
        priority: u8,
    },
    /// Append a feedback record to the session sink. The wire
    /// payload is `{thumbs_up: bool, message_id: String}` —
    /// persisted as a JSONL row tagged `kind: "feedback"` so a
    /// future analytics endpoint can aggregate it.
    Feedback {
        message_id: String,
        thumbs_up: bool,
    },
    Steer {
        content: String,
        priority: u8,
    },
    Cancel {
        reason: Option<String>,
    },
    /// Continue a suspended run. The token binds the resume to the
    /// log tail and request envelope it was issued for; the decisions
    /// answer every open interrupt. The route validates both before
    /// submitting, and the harness validates them again.
    Resume {
        token: ResumeToken,
        decisions: Vec<ResumeDecision>,
        allow_drift: bool,
    },
    /// Start the next run if the controller already holds work.
    ///
    /// Appends nothing and parks nothing: the turn it starts is one the
    /// log already carries — a prompt a previous owner parked and this
    /// node re-queued (see
    /// [`synthia::session::repair::parked_prompts`]). The controller
    /// loop only starts a run in response to an operation, so recovery
    /// needs one that names no input of its own.
    StartPending,
    Shutdown,
}

/// Where a [`SessionController::steer`](super::SessionController::steer)
/// call landed the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SteerDelivery {
    /// Appended to the live run's log as a `user_message` row with
    /// `source: "steer"`; the run's next step boundary injects it.
    Steer,
    /// No run was live: the message became an ordinary queued turn
    /// and carries no pre-appended row of its own.
    Queue,
}

/// Lifecycle state of a session controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionState {
    Idle,
    Running,
    /// A suspended turn is waiting for approval decisions. Derived
    /// from the log (open `approval_asked` rows in the last suspended
    /// segment), never from a memory flag: a process that restarts
    /// reports the same state as the one that suspended.
    AwaitingApproval,
    Cancelled,
}
