//! Pure types the loop flips between: the transcript, the per-run `RunState`, and the one-shot sample output.
use std::{collections::HashSet, sync::Arc};

use elysper_context::AgentState;
use elysper_core::CancelToken;
use elysper_provider::Message;

use super::projection::ToolProjectionCache;
use crate::{agent::approval::ApprovalPolicy, events::SessionEndReason};

/// The live LLM transcript: shared behind an `Arc` so the loop, snapshots and the durable log observe one history without copying it.
pub(crate) type Transcript = Arc<Vec<Message>>;

pub(crate) struct RuntimeSnapshotCache {
    pub today: chrono::NaiveDate,
    pub rendered: String,
}

pub(crate) struct SampleOutcome {
    pub(crate) result: Arc<elysper_provider::SamplingResult>,
    pub(crate) parts: Vec<elysper_provider::ContentPart>,
    /// The ordinal of the `assistant_message` row this pass wrote, or
    /// `0` when the pass produced nothing (and wrote no row).
    pub(crate) assistant_seq: u64,
}

/// One iteration body did. Each variant carries the `step_end` label
/// the `elysper_session` wire records, so the event and the close
/// decision cannot drift apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    Continue(&'static str),
    Completed,
    // The batch held high-risk calls; the run ends here and a
    // resume continues it.
    Suspended,
    Failed(SessionEndReason),
}

impl Step {
    pub(super) fn action(&self) -> &'static str {
        match self {
            Self::Continue(label) => label,
            Self::Completed => "completed",
            Self::Suspended => "suspended",
            Self::Failed(SessionEndReason::Cancelled) => "cancelled",
            Self::Failed(_) => "failed",
        }
    }
}

/// Per-session state: the live transcript, the per-run scratch + meta
/// bookkeeping, and the runtime-snapshot cache. The loop flips between
/// phases by reading and writing this single struct. `cancel` and
/// `subagent_depth` are fixed for the run; `task` children run at
/// `subagent_depth + 1`.
pub(crate) struct RunState {
    pub(crate) cancel: Arc<dyn CancelToken>,
    pub(crate) subagent_depth: usize,
    pub messages: Transcript,
    pub agent_state: AgentState,
    pub end_reason: SessionEndReason,
    /// `SystemPrompt`-point hints already injected — at most once per
    /// session each, keyed by name so one hint cannot silence others.
    pub system_hinted: std::collections::HashSet<String>,
    pub last_snapshot: Option<String>,
    /// `descriptor.model_hint`, cloned once per run so the loop stops
    /// re-reading the shared descriptor each iteration.
    pub model_hint: Option<String>,
    pub runtime_snapshot: Option<RuntimeSnapshotCache>,
    /// The ordinal of each durable surface message in `messages`,
    /// index-parallel. A message with no row of its own (an injected
    /// inbox turn the host already wrote, whose ordinal this loop does
    /// not know) carries `0` and is never cited as compaction
    /// provenance.
    pub surface_seqs: Vec<u64>,
    /// The rows [`drive_setup`](super::loop) read, kept so the resume
    /// check compares the token against the same fold the transcript
    /// came from instead of re-reading the log.
    pub log_rows: Vec<serde_json::Value>,
    /// The current turn's non-text parts: the log persists a turn's
    /// text but never its bytes, so the request carries them.
    pub attachments: Vec<elysper_provider::ContentPart>,
    /// Tool calls the assistant turn made that have no result yet.
    /// A user message that arrives while this is non-empty waits in
    /// [`Self::deferred_user`]: a request may not interleave a user
    /// turn between a `tool_use` and its `tool_result`.
    pub pending_tool_calls: HashSet<String>,
    /// User messages an inbox delivered while a tool batch was open.
    /// Released in order once the batch commits.
    pub deferred_user: Vec<Message>,
    pub projection: ToolProjectionCache,
    /// Whether this run already reported a context-manager prune.
    /// A writer-backed run re-derives and re-prunes at every step, but
    /// the prune is request-local and the log still holds the full
    /// transcript, so one warning per run describes the whole run.
    pub context_pruned: bool,
    /// Whether a schema-valid `structured_output` submission was
    /// committed (violations land as error results, not here).
    pub structured_output: bool,
    /// The reason a policy ended the run after a tool returned. The
    /// run stops at the step boundary — the tool's own result is
    /// committed first, so the transcript stays whole.
    pub terminate: Option<String>,
    /// The calls a gate held for an approval. The suspension that
    /// asks for them arrives with the approval policy.
    pub pending_approvals: Vec<crate::agent::approval::ApprovalRequest>,
    /// The approval policy this run actually uses: the agent's
    /// configured policy plus the session-scope grants folded from
    /// the log. Held per run so a grant written by a resume is
    /// visible to the calls that follow it.
    pub approval: ApprovalPolicy,
    /// The request-envelope fingerprint this run was built under;
    /// a suspension's resume token carries it.
    pub request_snapshot_hash: Option<String>,
    /// The seq of the last `Compaction` row this run wrote. A second
    /// pass that could only shadow that row would rewrite the summary
    /// without shadowing anything new.
    pub(crate) last_compaction: Option<u64>,
    /// The run's system prompt as the run assembled it at setup —
    /// the **pre-injection** prompt. A `SystemPrompt` steering hint
    /// rewrites the system message inside `messages`, and that
    /// message is the authoritative one once a hint has fired: it
    /// carries the hint's addition, which this field does not. Read
    /// this field for the prompt the run started from, never as the
    /// prompt a request carries.
    pub(crate) system_prompt: String,
}

impl RunState {
    /// State for a run that ended before its first iteration.
    pub(super) fn failed(
        cancel: Arc<dyn CancelToken>,
        subagent_depth: usize,
        reason: SessionEndReason,
    ) -> Self {
        Self::seed(
            cancel,
            subagent_depth,
            Arc::new(Vec::new()),
            AgentState::with_window(0),
        )
        .with_end_reason(reason)
    }

    /// Canonical constructor: every loop build path goes through this fn. The three optional run facts have identical
    /// types, so they are named builders rather than positional parameters a caller could silently swap.
    pub(super) fn seed(
        cancel: Arc<dyn CancelToken>,
        subagent_depth: usize,
        messages: Transcript,
        agent_state: AgentState,
    ) -> Self {
        Self {
            cancel,
            subagent_depth,
            messages,
            agent_state,
            end_reason: SessionEndReason::Completed,
            system_hinted: std::collections::HashSet::new(),
            last_snapshot: None,
            model_hint: None,
            runtime_snapshot: None,
            surface_seqs: Vec::new(),
            log_rows: Vec::new(),
            attachments: Vec::new(),
            pending_tool_calls: HashSet::new(),
            deferred_user: Vec::new(),
            projection: ToolProjectionCache::new(),
            context_pruned: false,
            structured_output: false,
            terminate: None,
            pending_approvals: Vec::new(),
            approval: ApprovalPolicy::default(),
            request_snapshot_hash: None,
            last_compaction: None,
            system_prompt: String::new(),
        }
    }

    /// Override the terminal `end_reason`.
    pub(super) fn with_end_reason(mut self, reason: SessionEndReason) -> Self {
        self.end_reason = reason;
        self
    }

    #[must_use]
    pub(super) fn with_model_hint(mut self, hint: Option<String>) -> Self {
        self.model_hint = hint;
        self
    }

    /// Attach the prompt the run assembled at setup.
    #[must_use]
    pub(super) fn with_system_prompt(mut self, prompt: String) -> Self {
        self.system_prompt = prompt;
        self
    }

    /// Attach the current turn's non-text parts.
    #[must_use]
    pub(super) fn with_attachments(
        mut self,
        attachments: Vec<elysper_provider::ContentPart>,
    ) -> Self {
        self.attachments = attachments;
        self
    }

    /// Attach the provenance of the initial transcript.
    #[must_use]
    pub(super) fn with_surface_seqs(mut self, seqs: Vec<u64>) -> Self {
        self.surface_seqs = seqs;
        self
    }

    /// Attach the rows this run's setup read.
    #[must_use]
    pub(super) fn with_log_rows(
        mut self,
        rows: Vec<serde_json::Value>,
    ) -> Self {
        self.log_rows = rows;
        self
    }

    /// Seed the called-set from the post-resume transcript.
    pub(crate) fn seed_called_tools(&mut self, messages: &[Message]) {
        self.projection.seed(messages);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;

    fn zero_failed(reason: SessionEndReason) -> RunState {
        RunState::failed(elysper_core::AtomicCancelToken::shared(), 0, reason)
    }

    #[test]
    fn run_state_default_is_empty() {
        let s = zero_failed(SessionEndReason::Error("x".into()));
        assert!(s.system_hinted.is_empty());
        assert!(s.projection.called.is_empty());
        // Drop the cancel handle so the test does not leak an Arc.
        let _ = Arc::clone(&s.cancel);
    }

    #[test]
    fn system_hinted_inserts_persist() {
        let mut s = zero_failed(SessionEndReason::Completed);
        s.system_hinted.insert("policy".to_string());
        s.system_hinted.insert("tone".to_string());
        assert!(s.system_hinted.contains("policy"));
        assert!(s.system_hinted.contains("tone"));
        assert_eq!(s.system_hinted.len(), 2);
    }
}
