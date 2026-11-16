//! What a caller hands to one [`Agent::run`](crate::Agent::run): multimodal content, the prior transcript, an optional prompt-manifest override, the run's own log writer and its steer channel.
use std::sync::Arc;

use async_trait::async_trait;
use futures::channel::mpsc;
use uva_approval::{ResumeDecision, ResumeStatus, ResumeToken};
use uva_provider::{Content, ContentPart, Message, Role, TextContent};
use uva_session::SessionLog;

use crate::prompt::PromptContext;

/// The run's steer channel: messages the host made durable while the
/// run was in flight.
///
/// The host writes the row first and then hands the message here, so a
/// crash between the two loses the in-memory copy and keeps the
/// durable fact — the next run's fold sees it. [`drain`](Self::drain)
/// must not block: the loop calls it at the top of every iteration and
/// a run with no pending steer pays one lock and no await.
#[async_trait]
pub trait RunInbox: Send + Sync {
    /// Take every message that arrived since the last drain.
    async fn drain(&self) -> Vec<Message>;
}

/// The inbox a single run owns: one unbounded channel whose receiver
/// the loop drains.
pub struct ChannelInbox {
    rx: parking_lot::Mutex<mpsc::UnboundedReceiver<Message>>,
}

impl ChannelInbox {
    /// Build the run's inbox and the sender the host keeps.
    #[must_use]
    pub fn channel() -> (Arc<Self>, mpsc::UnboundedSender<Message>) {
        let (tx, rx) = mpsc::unbounded();
        (
            Arc::new(Self {
                rx: parking_lot::Mutex::new(rx),
            }),
            tx,
        )
    }
}

#[async_trait]
impl RunInbox for ChannelInbox {
    async fn drain(&self) -> Vec<Message> {
        let mut rx = self.rx.lock();
        let mut drained = Vec::new();
        while let Ok(message) = rx.try_recv() {
            drained.push(message);
        }
        drained
    }
}

impl std::fmt::Debug for ChannelInbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ChannelInbox").finish_non_exhaustive()
    }
}

/// Opens the sessions delegated children run in.
///
/// A host that runs sub-agents installs one on the delegating tool
/// (`task`), which hands it down every time it opens a child. Opening
/// returns the child session's one writer: the child's loop then
/// writes its model facts — one `assistant_message` row per turn,
/// one `tool_result` row per result — into the child's own log,
/// exactly as a top-level run does. Opening also lands the
/// session's opening rows (enter header, dispatch prompt) before
/// the child's first row can be stamped, so the child's log reads
/// in order. Without a source the child's events stream live but
/// persist nothing.
pub trait SubagentWriters: Send + Sync {
    /// Open the child session and return its log.
    ///
    /// `child_session_id` is the id the delegating tool minted before
    /// dispatch; `parent_session_id` names the session the child
    /// descends from; `depth` is the child's nesting level (a direct
    /// child of the root session is `1`); `agent_name` is the peer's
    /// registry name and `prompt` the dispatch prompt — the two facts
    /// the child's opening rows carry.
    fn open_child(
        &self,
        child_session_id: &str,
        parent_session_id: &str,
        depth: usize,
        agent_name: &str,
        prompt: Option<&str>,
    ) -> Arc<dyn SessionLog>;
}

/// Where a delegated child's consumption is reported.
///
/// A child round runs on its own harness, and its events never enter
/// the parent session's stream: the parent's controller therefore sees
/// none of the child's `Usage` events and none of its terminal events.
/// Without this seam a deployment's token and turn counters would miss
/// every delegated round — the child's work is real consumption, but it
/// would vanish from the deployment-level account.
///
/// The `task` tool and the control surface report each round here while
/// they drain the child's event stream; the host owns the counters and
/// decides what a number means.
pub trait SubagentUsage: Send + Sync {
    /// Add one child step's token usage.
    ///
    /// `child_session_id` is the id the delegating tool minted; the
    /// numbers are the step's `Usage` event as the child's provider
    /// reported it.
    fn tokens(&self, child_session_id: &str, input: usize, output: usize);

    /// Count one finished child round as a turn.
    ///
    /// Every round counts, whatever its end reason: a cancelled or
    /// failed round consumed work too, and counting only the completed
    /// ones would under-report what the deployment actually did.
    fn round_ended(&self, child_session_id: &str);
}

/// Input to the agent run loop. Can contain text, images, or a mix of content parts.
///
/// Construct with [`AgentInput::new`] (text or multimodal payload) or
/// [`AgentInput::multi_with_history`] (continues a prior conversation).
/// Chain optional fields through [`AgentInput::prompt_context`] and
/// [`AgentInput::thread_id`] builder-style setters.
#[derive(Clone)]
pub struct AgentInput {
    /// This turn's payload. Without a log writer it becomes the turn's
    /// user message. With one, the host has already logged the user
    /// turn, so the loop derives the text from the log and sends only
    /// the non-text parts (`Image`, `Audio`, `Resource`) — in a user
    /// message of their own after the derived history — warning that a
    /// resume will not replay them.
    ///
    /// Task 9: the server MUST persist attachment references in the
    /// user message row so a resumed session can reproduce the turn.
    pub content: Vec<ContentPart>,
    /// The prior transcript, for a host that runs without a log
    /// writer. When a writer is configured the log is the single
    /// source of truth: `history` must be empty or a prefix of the
    /// log's derived history (the log may already hold this turn's
    /// user row), and any other value ends the run with
    /// [`SessionEndReason::Error`](uva_events::SessionEndReason).
    /// The prefix rule is deliberately looser than "empty or equal":
    /// the host supplies the prior transcript and the current turn
    /// separately, while the current turn's row is already logged.
    pub history: Vec<Message>,
    /// Optional per-dispatch prompt manifest (skills + peer
    /// agents). When `Some`,
    /// [`crate::Agent::run`] uses this at run start
    /// instead of the agent's own `prompt_context`. `None` uses the
    /// agent's manifest. Either way the host's registered prompt
    /// sections are appended, in registration order. Behind an `Arc`
    /// so a fan-out shares one manifest without cloning it.
    pub prompt_context: Option<Arc<PromptContext>>,
    /// This run's log writer, when the caller knows the session
    /// the run belongs to — the delegating tool sets it to the
    /// child session's writer so the child's loop writes its own
    /// rows. Overrides the agent's configured writer for this
    /// run; `None` (the default) uses the agent's.
    pub log_writer: Option<Arc<dyn SessionLog>>,
    /// This run's correlation id. It labels the tracing span and the
    /// sub-agent rows; it does **not** drive resume. A writer-backed
    /// run resumes from the log, and a run with no writer has nothing
    /// to resume from.
    pub thread_id: Option<String>,
    /// The run's steer channel. The host writes each message's
    /// `user_message` row first and hands the message here; the loop
    /// drains it at the top of every iteration. `None` (the default)
    /// means the run has no mid-flight input.
    pub inbox: Option<Arc<dyn RunInbox>>,
    /// The workspace this run's tools are bound to. `None` (the
    /// default) uses the agent's configured workspace. A delegating
    /// tool sets it to the worktree it created for the child run.
    pub workspace: Option<std::path::PathBuf>,
    /// The suspended run this input resumes. When `Some`, the loop
    /// validates the token against the log, dispatches the calls the
    /// suspension held, and continues; a writer is required.
    pub resume: Option<ResumeToken>,
    /// One decision per open interrupt. An empty list is refused:
    /// it does not cover the open set. Use
    /// [`AgentInput::resume_approving_all`] when the host really does
    /// mean "approve every held call"; a non-empty list must cover
    /// every open interrupt exactly once (checked by
    /// [`uva_approval::check_coverage`]).
    pub resume_decisions: Vec<ResumeDecision>,
    /// Accept a request-envelope change on resume instead of ending
    /// as [`SessionEndReason::ConfigDrift`](uva_events::SessionEndReason::ConfigDrift).
    pub allow_drift: bool,
}

impl std::fmt::Debug for AgentInput {
    // The writer handle prints as a presence flag: `LogWriter` is not
    // `Debug`, and its identity is all a debug reader needs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AgentInput")
            .field("content", &self.content)
            .field("history", &self.history)
            .field("prompt_context", &self.prompt_context)
            .field("log_writer", &self.log_writer.is_some())
            .field("thread_id", &self.thread_id)
            .field("inbox", &self.inbox.is_some())
            .field("workspace", &self.workspace)
            .field("resume", &self.resume)
            .field("resume_decisions", &self.resume_decisions)
            .field("allow_drift", &self.allow_drift)
            .finish()
    }
}

impl AgentInput {
    /// Create an input carrying a fresh user message.
    pub fn new(content: impl Into<Vec<ContentPart>>) -> Self {
        Self {
            content: content.into(),
            history: Vec::new(),
            prompt_context: None,
            log_writer: None,
            thread_id: None,
            inbox: None,
            workspace: None,
            resume: None,
            resume_decisions: Vec::new(),
            allow_drift: false,
        }
    }

    /// Resume the suspended run `token` names, approving every open
    /// interrupt.
    ///
    /// This is explicit coverage, not a wildcard: the builder writes
    /// one `Resolved` decision per call id the token holds, so the
    /// loop's coverage check sees an answer for every interrupt.
    /// Prefer [`Self::resume_decisions`] when the operator's per-call
    /// answer matters.
    #[must_use]
    pub fn resume_approving_all(mut self, token: ResumeToken) -> Self {
        self.resume_decisions = token
            .pending
            .iter()
            .map(|call_id| ResumeDecision {
                id: call_id.clone(),
                status: ResumeStatus::Resolved,
                payload: None,
            })
            .collect();
        self.resume = Some(token);
        self
    }

    /// Resume `token` with an explicit decision per open interrupt.
    ///
    /// The decisions must cover every interrupt the token names: a
    /// partial answer is refused rather than letting an undecided call
    /// inherit another call's status. A `Resolved` call executes;
    /// `Cancelled` / `Rejected` writes a model-visible `is_error`
    /// result instead.
    #[must_use]
    pub fn resume_decisions(
        mut self,
        token: ResumeToken,
        decisions: Vec<ResumeDecision>,
    ) -> Self {
        self.resume = Some(token);
        self.resume_decisions = decisions;
        self
    }

    /// Accept a changed request envelope on resume instead of ending
    /// as `ConfigDrift`. Only for a caller that knows the change is
    /// compatible; the default refuses.
    #[must_use]
    pub fn allow_drift(mut self) -> Self {
        self.allow_drift = true;
        self
    }

    /// Attach a per-dispatch prompt manifest. The next call to
    /// [`crate::Agent::run`] will see this manifest in
    /// `prepare()` and use it to assemble the system prompt.
    #[must_use]
    pub fn prompt_context(mut self, ctx: Arc<PromptContext>) -> Self {
        self.prompt_context = Some(ctx);
        self
    }

    /// Attach this run's correlation id (event meta and sub-agent
    /// rows). Resume is driven by the log, not by this value.
    #[must_use]
    pub fn thread_id(mut self, thread_id: impl Into<String>) -> Self {
        self.thread_id = Some(thread_id.into());
        self
    }

    /// Attach the run's steer channel.
    #[must_use]
    pub fn with_inbox(mut self, inbox: Arc<dyn RunInbox>) -> Self {
        self.inbox = Some(inbox);
        self
    }

    /// Bind this run's tools to `workspace` instead of the agent's
    /// configured root.
    #[must_use]
    pub fn with_workspace(mut self, workspace: std::path::PathBuf) -> Self {
        self.workspace = Some(workspace);
        self
    }

    /// Convenience: a plain-text input. Equivalent to
    /// `AgentInput::new(vec![text_part(text.into())])`.
    pub fn text(text: impl Into<String>) -> Self {
        Self::new(vec![ContentPart::Text(TextContent {
            text: text.into(),
            cache_control: None,
        })])
    }

    /// Resume a session: replay the given prior conversation
    /// messages and end with the given fresh parts.
    pub fn multi_with_history(
        history: Vec<Message>,
        parts: Vec<ContentPart>,
    ) -> Self {
        Self {
            content: parts,
            history,
            prompt_context: None,
            log_writer: None,
            thread_id: None,
            inbox: None,
            workspace: None,
            resume: None,
            resume_decisions: Vec::new(),
            allow_drift: false,
        }
    }

    /// Convenience: a single-text input that resumes a session.
    pub fn history(history: Vec<Message>, prompt: impl Into<String>) -> Self {
        Self::multi_with_history(
            history,
            vec![ContentPart::Text(TextContent {
                text: prompt.into(),
                cache_control: None,
            })],
        )
    }

    /// Convert this input into a [`Message`] with [`Role::User`].
    ///
    /// The loop's `prepare` step calls this for a caller-owned turn, and
    /// the single-shot strategy helpers do the same. The conversion is a
    /// plain shape change — one part becomes a single-content message,
    /// several become a multi-content one — with no log access.
    #[must_use]
    pub fn to_message(&self) -> Message {
        let content = if self.content.len() == 1 {
            Content::Single(self.content[0].clone())
        } else {
            Content::Multi(self.content.clone())
        };
        Message {
            role: Role::User,
            content,
            tool_call_id: None,
            name: None,
            ..Default::default()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_text_input() {
        let input = AgentInput::text("hello");
        assert_eq!(input.content.len(), 1);
        match &input.content[0] {
            ContentPart::Text(tc) => assert_eq!(tc.text, "hello"),
            _ => panic!("expected text content"),
        }
    }

    #[test]
    fn test_multi_input() {
        let parts = vec![
            ContentPart::Text(TextContent {
                text: "hello".to_string(),
                cache_control: None,
            }),
            ContentPart::Image(uva_provider::ImageContent {
                data: "img".to_string(),
                mime_type: "image/png".to_string(),
                detail: None,
            }),
        ];
        let input = AgentInput::new(parts);
        assert_eq!(input.content.len(), 2);
    }

    #[test]
    fn test_to_message_single() {
        let input = AgentInput::text("hello");
        let msg = input.to_message();
        assert_eq!(msg.role, Role::User);
        assert!(matches!(msg.content, Content::Single(_)));
    }

    #[test]
    fn test_to_message_multi() {
        let input = AgentInput::new(vec![
            ContentPart::Text(TextContent {
                text: "describe".to_string(),
                cache_control: None,
            }),
            ContentPart::Image(uva_provider::ImageContent {
                data: "img".to_string(),
                mime_type: "image/png".to_string(),
                detail: None,
            }),
        ]);
        let msg = input.to_message();
        assert!(matches!(msg.content, Content::Multi(_)));
    }

    #[test]
    fn builder_chain_attaches_optional_fields() {
        let pc = Arc::new(PromptContext::default());
        let input = AgentInput::text("hi")
            .prompt_context(Arc::clone(&pc))
            .thread_id("conv-1");
        assert!(input.prompt_context.is_some());
        assert_eq!(input.thread_id.as_deref(), Some("conv-1"));
    }
}
