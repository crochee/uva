//! Pure types the loop flips between: the transcript, the per-run `RunState`, and the one-shot sample output.
use std::sync::Arc;

use synthia_context::AgentState;
use synthia_core::CancelToken;
use synthia_provider::Message;

use super::projection::ToolProjectionCache;
use crate::events::SessionEndReason;

/// The live LLM transcript: shared behind an `Arc` so the loop, snapshots and the durable log observe one history without copying it.
pub(crate) type Transcript = Arc<Vec<Message>>;

pub(crate) struct RuntimeSnapshotCache {
    pub today: chrono::NaiveDate,
    pub rendered: String,
}

pub(crate) struct SampleOutcome {
    pub(crate) result: Arc<synthia_provider::SamplingResult>,
    pub(crate) parts: Vec<synthia_provider::ContentPart>,
}

/// One iteration body did. Each variant carries the `step_end` label
/// the `synthia_session` wire records, so the event and the close
/// decision cannot drift apart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    Continue(&'static str),
    Completed,
    Failed(SessionEndReason),
}

impl Step {
    pub(super) fn action(&self) -> &'static str {
        match self {
            Self::Continue(label) => label,
            Self::Completed => "completed",
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
    /// Resume key (`<thread_id>:<agent_name>`).
    pub resume_key: Option<String>,
    pub last_snapshot: Option<String>,
    /// `descriptor.model_hint`, cloned once per run so the loop stops
    /// re-reading the shared descriptor each iteration.
    pub model_hint: Option<String>,
    pub runtime_snapshot: Option<RuntimeSnapshotCache>,
    pub projection: ToolProjectionCache,
    pub touched: synthia_context::CompactionDetails,
    /// Whether a schema-valid `structured_output` submission was
    /// committed (violations land as error results, not here).
    pub structured_output: bool,
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
            resume_key: None,
            last_snapshot: None,
            model_hint: None,
            runtime_snapshot: None,
            projection: ToolProjectionCache::new(),
            touched: synthia_context::CompactionDetails::default(),
            structured_output: false,
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

    #[must_use]
    pub(super) fn with_resume_key(mut self, key: Option<String>) -> Self {
        self.resume_key = key;
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
        RunState::failed(synthia_core::AtomicCancelToken::shared(), 0, reason)
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
