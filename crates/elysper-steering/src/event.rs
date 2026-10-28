//! The interception vocabulary: [`HookEvent`] (what happened) and
//! [`HookDecision`] (what a registration may answer).
//!
//! `HookEvent` is a closed enum so adding an event forces every
//! handler to update its match (no silent payload drift). The
//! registry itself is the harness's `Host`; this module only defines
//! the vocabulary both sides share.

use crate::request_params::RequestParams;

/// are deliberately a closed enum: adding a new event forces
/// every existing handler to update its match, which is the
/// design goal (no silent payload drift).
#[derive(Debug, Clone)]
pub enum HookEvent {
    /// `before_run` — fired once per `Agent::run` invocation,
    /// after the agent has resolved its descriptor but before
    /// the first iteration.
    BeforeRun {
        /// Agent descriptor name (e.g. `"default"`).
        agent_name: String,
        /// Run ordinal within the session.
        run_idx: u32,
    },
    /// `after_run` — fired once per `Agent::run` invocation,
    /// after the final `SessionEnded` event.
    AfterRun {
        agent_name: String,
        run_idx: u32,
        /// End reason string (e.g. `"completed"`,
        /// `"max_iterations"`, `"cancelled"`).
        reason: String,
    },
    /// `before_step` — fired before each model call, after the
    /// step's preparation and before the request is assembled. A
    /// handler that returns `HookDecision::Terminate` ends the run
    /// before the call goes out.
    BeforeStep {
        /// Agent descriptor name (e.g. `"default"`).
        agent_name: String,
        /// Run ordinal within the session.
        run_idx: u32,
        /// Zero-based index of the iteration that is about to sample.
        iteration: u32,
    },
    /// `before_request` — fired after the request is assembled and
    /// before it goes on the wire. The event carries the request's
    /// current parameters; a handler that returns
    /// `HookDecision::ReplaceParams` hands back the rewritten copy
    /// (model, temperature, max tokens, tool choice, tools — the
    /// message list and the system prompt are out of reach by type).
    BeforeRequest {
        /// The provider the request goes to.
        provider: String,
        /// The request's current parameters.
        params: RequestParams,
    },
    /// `before_tool` — fired immediately before a tool dispatch
    /// (after the guard pipeline has accepted the call). A
    /// handler that returns `HookDecision::Block { reason }`
    /// vetoes the call.
    BeforeTool {
        tool_name: String,
        call_id: String,
        /// Raw arguments JSON for the call.
        arguments: serde_json::Value,
    },
    /// `after_tool` — fired once a tool has returned. A handler
    /// that returns `HookDecision::Terminate` ends the agent
    /// run with the supplied reason.
    AfterTool {
        tool_name: String,
        call_id: String,
        /// `true` when the tool reported an error result.
        is_error: bool,
    },
    /// `after_step` — fired at the end of each loop iteration, after
    /// the iteration's tool results are committed but before the
    /// next iteration is sampled (pi `finishTurn` parity: a
    /// step-end termination decision point that is not tied to a
    /// specific tool call). `HookDecision::Block` is meaningless
    /// here — the step already ran — and is treated as `Allow`;
    /// `Terminate` ends the run with the supplied reason.
    AfterStep {
        agent_name: String,
        run_idx: u32,
        /// Zero-based index of the iteration that just finished.
        iteration: u32,
    },
    /// `before_provider_call` — fired right before the LLM
    /// dispatch. Handlers can mutate the request before it
    /// goes on the wire (e.g. for red-team prompt rewriting
    /// in tests).
    BeforeProviderCall {
        provider: String,
        model: String,
        /// Token estimate of the request body.
        estimated_tokens: usize,
    },
    /// `after_provider_call` — fired once the streaming
    /// response has been fully assembled.
    AfterProviderCall {
        provider: String,
        model: String,
        /// Total usage reported by the provider.
        prompt_tokens: usize,
        completion_tokens: usize,
        /// Provider stop reason.
        stop_reason: Option<String>,
    },
    /// `on_error` — fired when the agent run terminates via
    /// `SessionEndReason::Error`. The reason string is the
    /// `Display` form of the error.
    OnError { agent_name: String, reason: String },
    /// `on_compaction` — fired when the
    /// `SummarizingContextManager` fires its emitter (R5-8).
    /// The `start` / `end` indices are the pre-splice range.
    OnCompaction {
        start: usize,
        end: usize,
        /// Summary text the manager produced.
        summary: String,
    },
}

impl HookEvent {
    /// Whether `decision` means anything at this point.
    ///
    /// Each point accepts a subset of the vocabulary: a
    /// `before_tool` hook may block or ask, a `before_request` hook
    /// may rewrite the request, a `before_run` hook may only end the
    /// run, and a read-only point accepts nothing but `Allow`. A
    /// decision a point does not accept is a no-op — the dispatch
    /// layer records a diagnostic and lets the run continue, because
    /// a decision that could not be honoured must never be mistaken
    /// for one that was.
    #[must_use]
    pub fn accepts(&self, decision: &HookDecision) -> bool {
        match decision {
            HookDecision::Allow => true,
            HookDecision::Block { .. } | HookDecision::Ask { .. } => {
                matches!(self, HookEvent::BeforeTool { .. })
            }
            HookDecision::ReplaceParams(_) => {
                matches!(self, HookEvent::BeforeRequest { .. })
            }
            HookDecision::Terminate { .. } => matches!(
                self,
                HookEvent::BeforeRun { .. }
                    | HookEvent::BeforeStep { .. }
                    | HookEvent::BeforeTool { .. }
                    | HookEvent::AfterTool { .. }
                    | HookEvent::AfterStep { .. }
            ),
        }
    }
}

/// Handler decision. A handler can veto a call (`Block`), ask for an
/// approval (`Ask`), rewrite the request (`ReplaceParams`), end the
/// run (`Terminate`), or stay silent (`Allow`).
#[derive(Debug, Clone, Default, PartialEq)]
pub enum HookDecision {
    /// The call proceeds unchanged.
    #[default]
    Allow,
    /// The call is vetoed; the agent loop synthesises an
    /// `is_error=true` tool result carrying the reason.
    Block {
        /// Human-readable reason; surfaces in the wire-visible
        /// tool result so the model can react.
        reason: String,
    },
    /// The call waits for an explicit approval. `Ask` never
    /// short-circuits: the reasons are collected and reported
    /// together at the end of the gate phase, so one approval
    /// request can carry every reason the run collected.
    Ask {
        /// Human-readable reason the call needs an approval.
        reason: String,
    },
    /// The request's parameters are replaced by these. Only the
    /// `before_request` point accepts this decision.
    ReplaceParams(RequestParams),
    /// The agent run ends with the given reason. Distinct from
    /// `Block` (which only affects one tool call).
    Terminate {
        /// End reason string passed to `SessionEndReason`.
        reason: String,
    },
}

#[must_use]
pub fn event_name(event: &HookEvent) -> &'static str {
    match event {
        HookEvent::BeforeRun { .. } => "before_run",
        HookEvent::AfterRun { .. } => "after_run",
        HookEvent::BeforeStep { .. } => "before_step",
        HookEvent::BeforeRequest { .. } => "before_request",
        HookEvent::BeforeTool { .. } => "before_tool",
        HookEvent::AfterTool { .. } => "after_tool",
        HookEvent::AfterStep { .. } => "after_step",
        HookEvent::BeforeProviderCall { .. } => "before_provider_call",
        HookEvent::AfterProviderCall { .. } => "after_provider_call",
        HookEvent::OnError { .. } => "on_error",
        HookEvent::OnCompaction { .. } => "on_compaction",
    }
}
