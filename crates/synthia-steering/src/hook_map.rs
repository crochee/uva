//! [`HookMap`] — the loop's one interception vocabulary.
//!
//! A typed, name-keyed `name → Vec<Handler>` registry: handlers
//! register for one event name (`before_tool`, `after_run`, …) and
//! receive that event's typed payload. Registering per name keeps
//! each handler specialised to the payload it cares about instead of
//! pattern-matching a sum-type; `HookEvent` stays a closed enum so
//! adding an event forces every handler to update (no silent payload
//! drift).
//!
//! Handlers are `async` and borrow the event for the life of the
//! future (the payloads are owned, so no per-handler clone). Plain
//! handlers fire before state-aware ones; the first non-`Allow`
//! decision short-circuits.

use std::{collections::HashMap, sync::Arc};

use futures::{FutureExt, future::BoxFuture};
use synthia_context::AgentState;

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

/// One handler registered for one [`HookEvent`] variant. The
/// handler is `Send + Sync` so the registry can be shared across
/// tasks. The event payload is fully owned, so the handler borrows
/// it for the life of the future — no per-handler clone.
pub type HookHandler = Arc<
    dyn for<'a> Fn(&'a HookEvent) -> BoxFuture<'a, HookDecision>
        + Send
        + Sync
        + 'static,
>;

/// State-aware variant of [`HookHandler`]. Receives the live
/// [`AgentState`] (read-only) alongside the event so handlers
/// can make decisions off the runtime state, not just the
/// event payload.
///
/// Adopted from pi `packages/agent/src/harness/hooks.ts`
/// (handlers receive harness state). Use this when the
/// decision depends on counters (iteration index, tool-call
/// count, context utilisation, …); use the plain
/// [`HookHandler`] when the decision depends only on the
/// event payload.
pub type HookHandlerWithState = Arc<
    dyn for<'a> Fn(&'a HookEvent, &'a AgentState) -> BoxFuture<'a, HookDecision>
        + Send
        + Sync
        + 'static,
>;
/// Typed, name-keyed hook registry. Construct one with
/// [`HookMap::new`], populate it via [`HookMap::on`], and
/// dispatch through it from your own code: the agent loop reads
/// [`Steering`](crate::Steering)'s positional hooks, so a map
/// is for callers that want name-keyed, state-aware dispatch.
///
/// The map is internally `HashMap<event_name, Vec<HookHandler>>`
/// keyed by the [`event_name`] of each [`HookEvent`]. A handler
/// registered for `BeforeTool` fires only for `BeforeTool`
/// events, never for `AfterTool` — payload type safety is
/// enforced by the variant, not by a string match.
///
/// The state-aware registry ([`HookMap::on_with_state`]) is
/// layered alongside the plain registry; both fire during a
/// single [`HookMap::dispatch`] / [`HookMap::dispatch_with_state`]
/// pass, in registration order. Plain handlers fire first so
/// pure-payload vetoes win before any state-dependent veto
/// runs.
#[derive(Default, Clone)]
pub struct HookMap {
    inner: HashMap<&'static str, Vec<HookHandler>>,
    inner_state: HashMap<&'static str, Vec<HookHandlerWithState>>,
}

impl HookMap {
    /// Build an empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register one handler for the [`HookEvent`] variant named
    /// by `name`. The handler is appended to the existing list
    /// (registration order = invocation order). Returns `self`
    /// for fluent chaining.
    ///
    /// # Panics
    ///
    /// The function does not panic; it is a pure builder.
    pub fn on<F>(mut self, name: &'static str, handler: F) -> Self
    where
        F: for<'a> Fn(&'a HookEvent) -> BoxFuture<'a, HookDecision>
            + Send
            + Sync
            + 'static,
    {
        self.inner.entry(name).or_default().push(Arc::new(handler));
        self
    }

    /// Register one **state-aware** handler. Receives the live
    /// [`AgentState`] alongside the event. Use when the
    /// decision depends on counters (iteration index, tool-call
    /// count, context utilisation, …).
    pub fn on_with_state<F>(mut self, name: &'static str, handler: F) -> Self
    where
        F: for<'a> Fn(
                &'a HookEvent,
                &'a AgentState,
            ) -> BoxFuture<'a, HookDecision>
            + Send
            + Sync
            + 'static,
    {
        self.inner_state
            .entry(name)
            .or_default()
            .push(Arc::new(handler));
        self
    }

    /// True when no handlers are registered (counts both
    /// plain and state-aware handlers).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.inner.values().all(Vec::is_empty)
            && self.inner_state.values().all(Vec::is_empty)
    }

    /// Number of distinct event names with at least one handler
    /// (counts both plain and state-aware handlers).
    #[must_use]
    pub fn len(&self) -> usize {
        let plain = self
            .inner
            .values()
            .filter(|handlers| !handlers.is_empty())
            .count();
        let state = self
            .inner_state
            .values()
            .filter(|handlers| !handlers.is_empty())
            .count();
        plain + state
    }

    /// Fire one event. Every handler registered for the
    /// event's [`event_name`] runs in registration order. A
    /// `Block` / `Terminate` / `ReplaceParams` decision
    /// short-circuits the rest and is returned; an `Ask` does
    /// not — the reasons are collected and returned as one `Ask`
    /// decision once every handler has run. If every handler
    /// returns `Allow` the function returns `Allow`.
    ///
    /// Handler panics are caught and treated as `Allow` (the
    /// registry never poisons the agent run on a user code
    /// bug; the `tracing` layer logs the panic).
    pub async fn dispatch(&self, event: &HookEvent) -> HookDecision {
        let name = event_name(event);
        let Some(handlers) = self.inner.get(name) else {
            return HookDecision::Allow;
        };
        let mut pass = Pass::default();
        for handler in handlers {
            // `catch_unwind` cannot span an `.await`, so the
            // future itself is caught instead of the closure.
            let result = std::panic::AssertUnwindSafe(handler(event))
                .catch_unwind()
                .await;
            match result {
                Ok(decision) => pass.absorb(decision),
                Err(_) => {
                    // Handler panicked — swallow + log so the
                    // agent run continues.
                    tracing::warn!(
                        event = name,
                        "HookMap handler panicked; treating as Allow"
                    );
                    continue;
                }
            }
            if pass.decided() {
                break;
            }
        }
        pass.outcome()
    }

    /// Fire one event with the live [`AgentState`]. Plain
    /// handlers fire first; state-aware handlers fire next.
    /// A `Block` / `Terminate` / `ReplaceParams` decision
    /// short-circuits the rest; an `Ask` is collected.
    ///
    /// Handler panics are caught and treated as `Allow` (the
    /// registry never poisons the agent run on a user code
    /// bug; the `tracing` layer logs the panic).
    pub async fn dispatch_with_state(
        &self,
        event: &HookEvent,
        state: &AgentState,
    ) -> HookDecision {
        let mut pass = Pass::default();
        // 1. Plain handlers first (pure-payload vetoes win).
        pass.absorb(self.dispatch(event).await);
        if pass.decided() {
            return pass.outcome();
        }
        // 2. State-aware handlers next.
        let name = event_name(event);
        let Some(handlers) = self.inner_state.get(name) else {
            return pass.outcome();
        };
        for handler in handlers {
            let result = std::panic::AssertUnwindSafe(handler(event, state))
                .catch_unwind()
                .await;
            match result {
                Ok(decision) => pass.absorb(decision),
                Err(_) => {
                    tracing::warn!(
                        event = name,
                        "HookMap state-aware handler panicked; treating as Allow"
                    );
                    continue;
                }
            }
            if pass.decided() {
                break;
            }
        }
        pass.outcome()
    }
}

/// One dispatch pass: the decision that ended it, plus the approval
/// reasons the handlers collected on the way.
///
/// `Ask` is the one decision that does not end a pass: the call
/// waits for an approval, and every reason the run collected belongs
/// in the same request. A `Block` / `Terminate` / `ReplaceParams`
/// decision ends the pass immediately and wins over the asks.
#[derive(Default)]
struct Pass {
    decision: Option<HookDecision>,
    asks: Vec<String>,
}

impl Pass {
    /// Fold one handler's decision into the pass.
    fn absorb(&mut self, decision: HookDecision) {
        match decision {
            HookDecision::Allow => {}
            HookDecision::Ask { reason } => self.asks.push(reason),
            other => {
                if self.decision.is_none() {
                    self.decision = Some(other);
                }
            }
        }
    }

    /// Whether a decision ended the pass.
    fn decided(&self) -> bool {
        self.decision.is_some()
    }

    /// The pass's outcome: the decision that ended it, else the
    /// collected asks, else `Allow`.
    fn outcome(self) -> HookDecision {
        match self.decision {
            Some(decision) => decision,
            None if self.asks.is_empty() => HookDecision::Allow,
            None => HookDecision::Ask {
                reason: self.asks.join("; "),
            },
        }
    }
}

/// Wire tag for a [`HookEvent`] variant. Mirrors the variant
/// name in `lower_snake_case` so the registry key matches the
/// event name a lib consumer registers.
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[tokio::test]
    async fn empty_map_dispatches_allow() {
        let map = HookMap::new();
        let decision = map
            .dispatch(&HookEvent::BeforeRun {
                agent_name: "x".to_string(),
                run_idx: 0,
            })
            .await;
        assert_eq!(decision, HookDecision::Allow);
        assert!(map.is_empty());
        assert_eq!(map.len(), 0);
    }

    #[tokio::test]
    async fn before_tool_handler_vetoes_dispatch() {
        let map = HookMap::new().on("before_tool", |event| {
            Box::pin(async move {
                match event {
                    HookEvent::BeforeTool { tool_name, .. }
                        if tool_name == "shell" =>
                    {
                        HookDecision::Block {
                            reason: "shell vetoed by test".to_string(),
                        }
                    }
                    _ => HookDecision::Allow,
                }
            })
        });
        let decision = map
            .dispatch(&HookEvent::BeforeTool {
                tool_name: "shell".to_string(),
                call_id: "c1".to_string(),
                arguments: json!({}),
            })
            .await;
        assert_eq!(
            decision,
            HookDecision::Block {
                reason: "shell vetoed by test".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn handler_runs_only_for_matching_event_name() {
        // A handler registered for `before_tool` must NOT fire
        // on `after_tool`. Pin the variant isolation.
        let map = HookMap::new().on("before_tool", |_| {
            Box::pin(async move {
                HookDecision::Block {
                    reason: "should not fire here".to_string(),
                }
            })
        });
        let decision = map
            .dispatch(&HookEvent::AfterTool {
                tool_name: "shell".to_string(),
                call_id: "c1".to_string(),
                is_error: false,
            })
            .await;
        assert_eq!(decision, HookDecision::Allow);
    }

    #[tokio::test]
    async fn after_step_name_and_dispatch() {
        assert_eq!(
            event_name(&HookEvent::AfterStep {
                agent_name: "a".to_string(),
                run_idx: 0,
                iteration: 3,
            }),
            "after_step"
        );
        let map = HookMap::new().on("after_step", |event| {
            Box::pin(async move {
                match event {
                    HookEvent::AfterStep { iteration, .. } => {
                        HookDecision::Terminate {
                            reason: format!("stop after {iteration}"),
                        }
                    }
                    _ => HookDecision::Allow,
                }
            })
        });
        let decision = map
            .dispatch(&HookEvent::AfterStep {
                agent_name: "a".to_string(),
                run_idx: 0,
                iteration: 3,
            })
            .await;
        assert_eq!(
            decision,
            HookDecision::Terminate {
                reason: "stop after 3".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn first_non_allow_decision_wins() {
        // Two handlers; the first returns Allow, the second
        // returns Terminate. The result is Terminate — first
        // non-allow short-circuits.
        let map = HookMap::new()
            .on("after_tool", |_| {
                Box::pin(async move { HookDecision::Allow })
            })
            .on("after_tool", |_| {
                Box::pin(async move {
                    HookDecision::Terminate {
                        reason: "second wins".to_string(),
                    }
                })
            });
        let decision = map
            .dispatch(&HookEvent::AfterTool {
                tool_name: "t".to_string(),
                call_id: "c".to_string(),
                is_error: false,
            })
            .await;
        assert_eq!(
            decision,
            HookDecision::Terminate {
                reason: "second wins".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn handler_panic_is_treated_as_allow() {
        let map = HookMap::new().on("before_run", |_| {
            Box::pin(async move {
                panic!("handler bug");
            })
        });
        let decision = map
            .dispatch(&HookEvent::BeforeRun {
                agent_name: "x".to_string(),
                run_idx: 0,
            })
            .await;
        assert_eq!(decision, HookDecision::Allow);
    }

    #[tokio::test]
    async fn multiple_handlers_for_distinct_events_coexist() {
        let map = HookMap::new()
            .on("before_tool", |event| {
                Box::pin(async move {
                    match event {
                        HookEvent::BeforeTool { tool_name, .. }
                            if tool_name == "read" =>
                        {
                            HookDecision::Block {
                                reason: "no reads".to_string(),
                            }
                        }
                        _ => HookDecision::Allow,
                    }
                })
            })
            .on("after_tool", |event| {
                Box::pin(async move {
                    match event {
                        HookEvent::AfterTool { is_error, .. } if *is_error => {
                            HookDecision::Terminate {
                                reason: "tool error".to_string(),
                            }
                        }
                        _ => HookDecision::Allow,
                    }
                })
            });
        assert_eq!(map.len(), 2);
        assert!(!map.is_empty());

        let read = map
            .dispatch(&HookEvent::BeforeTool {
                tool_name: "read".to_string(),
                call_id: "c".to_string(),
                arguments: json!({}),
            })
            .await;
        assert_eq!(
            read,
            HookDecision::Block {
                reason: "no reads".to_string(),
            }
        );

        let errored = map
            .dispatch(&HookEvent::AfterTool {
                tool_name: "t".to_string(),
                call_id: "c".to_string(),
                is_error: true,
            })
            .await;
        assert_eq!(
            errored,
            HookDecision::Terminate {
                reason: "tool error".to_string(),
            }
        );

        // Unrelated event name — no handler registered.
        let unrelated = map
            .dispatch(&HookEvent::OnError {
                agent_name: "x".to_string(),
                reason: "y".to_string(),
            })
            .await;
        assert_eq!(unrelated, HookDecision::Allow);
    }

    #[tokio::test]
    async fn event_name_matches_variant() {
        assert_eq!(
            event_name(&HookEvent::BeforeRun {
                agent_name: "a".to_string(),
                run_idx: 0,
            }),
            "before_run"
        );
        assert_eq!(
            event_name(&HookEvent::AfterRun {
                agent_name: "a".to_string(),
                run_idx: 0,
                reason: "x".to_string(),
            }),
            "after_run"
        );
        assert_eq!(
            event_name(&HookEvent::BeforeTool {
                tool_name: "t".to_string(),
                call_id: "c".to_string(),
                arguments: json!({}),
            }),
            "before_tool"
        );
        assert_eq!(
            event_name(&HookEvent::AfterTool {
                tool_name: "t".to_string(),
                call_id: "c".to_string(),
                is_error: false,
            }),
            "after_tool"
        );
        assert_eq!(
            event_name(&HookEvent::OnError {
                agent_name: "a".to_string(),
                reason: "x".to_string(),
            }),
            "on_error"
        );
        assert_eq!(
            event_name(&HookEvent::OnCompaction {
                start: 0,
                end: 1,
                summary: "x".to_string(),
            }),
            "on_compaction"
        );
    }

    #[tokio::test]
    async fn on_registration_preserves_order() {
        // Three handlers in registration order; the third
        // returns Terminate. The dispatch must reach the third
        // (no early short-circuit on Allow).
        let map = HookMap::new()
            .on("after_run", |_| {
                Box::pin(async move { HookDecision::Allow })
            })
            .on("after_run", |_| {
                Box::pin(async move { HookDecision::Allow })
            })
            .on("after_run", |_| {
                Box::pin(async move {
                    HookDecision::Terminate {
                        reason: "third".to_string(),
                    }
                })
            });
        let decision = map
            .dispatch(&HookEvent::AfterRun {
                agent_name: "x".to_string(),
                run_idx: 0,
                reason: "y".to_string(),
            })
            .await;
        assert_eq!(
            decision,
            HookDecision::Terminate {
                reason: "third".to_string(),
            }
        );
    }

    #[tokio::test]
    async fn state_aware_handler_receives_agent_state() {
        let map =
            HookMap::new().on_with_state("before_run", |_event, state| {
                Box::pin(async move {
                    if state.iteration_count >= 3 {
                        HookDecision::Block {
                            reason: "too many iterations".to_string(),
                        }
                    } else {
                        HookDecision::Allow
                    }
                })
            });
        let mut state = AgentState::with_window(100);
        state.iteration_count = 0;
        assert_eq!(
            map.dispatch_with_state(
                &HookEvent::BeforeRun {
                    agent_name: "x".to_string(),
                    run_idx: 0
                },
                &state
            )
            .await,
            HookDecision::Allow
        );
        state.iteration_count = 3;
        let decision = map
            .dispatch_with_state(
                &HookEvent::BeforeRun {
                    agent_name: "x".to_string(),
                    run_idx: 0,
                },
                &state,
            )
            .await;
        assert!(matches!(decision, HookDecision::Block { .. }));
    }

    #[tokio::test]
    async fn state_aware_handler_panic_is_swallowed() {
        let map = HookMap::new().on_with_state("before_run", |_, _| {
            Box::pin(async move {
                panic!("intentional panic for test");
            })
        });
        let state = AgentState::with_window(100);
        // Panicking handler must not poison dispatch — the
        let decision = map
            .dispatch_with_state(
                &HookEvent::BeforeRun {
                    agent_name: "x".to_string(),
                    run_idx: 0,
                },
                &state,
            )
            .await;
        assert_eq!(decision, HookDecision::Allow);
    }

    #[tokio::test]
    async fn plain_veto_wins_before_state_aware_check() {
        let map = HookMap::new()
            .on("before_run", |_| {
                Box::pin(async move {
                    HookDecision::Terminate {
                        reason: "plain".to_string(),
                    }
                })
            })
            .on_with_state("before_run", |_, _| {
                Box::pin(async move { HookDecision::Allow })
            });
        let state = AgentState::with_window(100);
        let decision = map
            .dispatch_with_state(
                &HookEvent::BeforeRun {
                    agent_name: "x".to_string(),
                    run_idx: 0,
                },
                &state,
            )
            .await;
        // Plain handler vetoes before state-aware handlers even
        // run.
        assert!(matches!(decision, HookDecision::Terminate { .. }));
    }

    #[tokio::test]
    async fn on_with_state_does_not_affect_plain_dispatch() {
        let map = HookMap::new().on_with_state("before_run", |_, _| {
            Box::pin(async move {
                HookDecision::Terminate {
                    reason: "state-aware".to_string(),
                }
            })
        });
        // Plain dispatch ignores state-aware handlers entirely.
        let decision = map
            .dispatch(&HookEvent::BeforeRun {
                agent_name: "x".to_string(),
                run_idx: 0,
            })
            .await;
        assert_eq!(decision, HookDecision::Allow);
    }

    #[tokio::test]
    async fn an_ask_does_not_short_circuit_the_handlers_after_it() {
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = Arc::clone(&seen);
        let map = HookMap::new()
            .on("before_tool", |_| {
                Box::pin(async move {
                    HookDecision::Ask {
                        reason: "needs approval".to_string(),
                    }
                })
            })
            .on("before_tool", move |_| {
                let counter = Arc::clone(&counter);
                Box::pin(async move {
                    counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    HookDecision::Allow
                })
            });
        let decision = map
            .dispatch(&HookEvent::BeforeTool {
                tool_name: "shell".to_string(),
                call_id: "c1".to_string(),
                arguments: json!({}),
            })
            .await;
        // The ask is collected, and the handler behind it still ran.
        assert_eq!(
            decision,
            HookDecision::Ask {
                reason: "needs approval".to_string()
            }
        );
        assert_eq!(seen.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn a_block_wins_over_an_ask_collected_before_it() {
        let map = HookMap::new()
            .on("before_tool", |_| {
                Box::pin(async move {
                    HookDecision::Ask {
                        reason: "needs approval".to_string(),
                    }
                })
            })
            .on("before_tool", |_| {
                Box::pin(async move {
                    HookDecision::Block {
                        reason: "denied".to_string(),
                    }
                })
            });
        let decision = map
            .dispatch(&HookEvent::BeforeTool {
                tool_name: "shell".to_string(),
                call_id: "c1".to_string(),
                arguments: json!({}),
            })
            .await;
        // The block wins: the run does not wait for an approval it
        // will never get.
        assert_eq!(
            decision,
            HookDecision::Block {
                reason: "denied".to_string()
            }
        );
    }

    #[tokio::test]
    async fn a_terminate_wins_over_an_ask_collected_before_it() {
        let map = HookMap::new()
            .on("before_tool", |_| {
                Box::pin(async move {
                    HookDecision::Ask {
                        reason: "needs approval".to_string(),
                    }
                })
            })
            .on("before_tool", |_| {
                Box::pin(async move {
                    HookDecision::Terminate {
                        reason: "policy ended the run".to_string(),
                    }
                })
            });
        let decision = map
            .dispatch(&HookEvent::BeforeTool {
                tool_name: "shell".to_string(),
                call_id: "c1".to_string(),
                arguments: json!({}),
            })
            .await;
        assert!(matches!(decision, HookDecision::Terminate { .. }));
    }

    #[test]
    fn each_point_accepts_only_its_own_decisions() {
        let before_tool = HookEvent::BeforeTool {
            tool_name: "shell".to_string(),
            call_id: "c1".to_string(),
            arguments: json!({}),
        };
        let before_run = HookEvent::BeforeRun {
            agent_name: "x".to_string(),
            run_idx: 0,
        };
        let before_request = HookEvent::BeforeRequest {
            provider: "p".to_string(),
            params: crate::RequestParams::from_request(
                &synthia_provider::CompletionRequest::default(),
            ),
        };
        let after_provider_call = HookEvent::AfterProviderCall {
            provider: "p".to_string(),
            model: "m".to_string(),
            prompt_tokens: 0,
            completion_tokens: 0,
            stop_reason: None,
        };
        let block = HookDecision::Block {
            reason: "no".to_string(),
        };
        let ask = HookDecision::Ask {
            reason: "maybe".to_string(),
        };
        let terminate = HookDecision::Terminate {
            reason: "stop".to_string(),
        };
        // `before_tool` is the point that may block or ask.
        assert!(before_tool.accepts(&block));
        assert!(before_tool.accepts(&ask));
        assert!(before_tool.accepts(&terminate));
        // `before_run` may only end the run.
        assert!(before_run.accepts(&terminate));
        assert!(!before_run.accepts(&block));
        assert!(!before_run.accepts(&ask));
        // `before_request` may only rewrite the request.
        assert!(before_request.accepts(&HookDecision::ReplaceParams(
            crate::RequestParams::from_request(
                &synthia_provider::CompletionRequest::default(),
            )
        )));
        assert!(!before_request.accepts(&block));
        // A read-only point accepts nothing but `Allow`.
        assert!(after_provider_call.accepts(&HookDecision::Allow));
        assert!(!after_provider_call.accepts(&terminate));
        assert!(!after_provider_call.accepts(&block));
    }

    #[test]
    fn the_new_points_have_wire_names() {
        assert_eq!(
            event_name(&HookEvent::BeforeStep {
                agent_name: "x".to_string(),
                run_idx: 0,
                iteration: 2,
            }),
            "before_step"
        );
        assert_eq!(
            event_name(&HookEvent::BeforeRequest {
                provider: "p".to_string(),
                params: crate::RequestParams::from_request(
                    &synthia_provider::CompletionRequest::default(),
                ),
            }),
            "before_request"
        );
    }
}
