//! The debug-level lifecycle logger, exposed as a [`HookMap`].

use futures::future::BoxFuture;

use crate::{HookDecision, HookEvent, HookMap};

/// Debug-level lifecycle logger — the default observability hook.
/// Build it with [`LoggingHook::hook_map`] and install the returned map on
/// [`Steering::hooks`](crate::Steering::hooks).
pub struct LoggingHook;

impl LoggingHook {
    /// One handler per lifecycle point the old trait covered, each doing the
    /// same `tracing::debug!` its method used to.
    #[must_use]
    pub fn hook_map() -> HookMap {
        HookMap::new()
            .on(
                "before_run",
                log(|e| {
                    let HookEvent::BeforeRun { agent_name, .. } = e else {
                        return HookDecision::Allow;
                    };
                    tracing::debug!(agent = %agent_name, "hook: agent start");
                    HookDecision::Allow
                }),
            )
            .on(
                "after_run",
                log(|e| {
                    let HookEvent::AfterRun { reason, .. } = e else {
                        return HookDecision::Allow;
                    };
                    tracing::debug!(reason = %reason, "hook: agent end");
                    HookDecision::Allow
                }),
            )
            .on(
                "before_provider_call",
                log(|e| {
                    let HookEvent::BeforeProviderCall {
                        provider, model, ..
                    } = e
                    else {
                        return HookDecision::Allow;
                    };
                    tracing::debug!(
                        provider = %provider,
                        model = %model,
                        "hook: provider start"
                    );
                    HookDecision::Allow
                }),
            )
            .on(
                "after_provider_call",
                log(|e| {
                    let HookEvent::AfterProviderCall {
                        provider,
                        model,
                        prompt_tokens,
                        completion_tokens,
                        ..
                    } = e
                    else {
                        return HookDecision::Allow;
                    };
                    tracing::debug!(
                        provider = %provider,
                        model = %model,
                        total_tokens = prompt_tokens + completion_tokens,
                        "hook: provider end"
                    );
                    HookDecision::Allow
                }),
            )
            .on(
                "before_tool",
                log(|e| {
                    let HookEvent::BeforeTool { tool_name, .. } = e else {
                        return HookDecision::Allow;
                    };
                    tracing::debug!(tool = %tool_name, "hook: tool start");
                    HookDecision::Allow
                }),
            )
            .on(
                "after_tool",
                log(|e| {
                    let HookEvent::AfterTool {
                        tool_name,
                        is_error,
                        ..
                    } = e
                    else {
                        return HookDecision::Allow;
                    };
                    tracing::debug!(
                        tool = %tool_name,
                        is_error,
                        "hook: tool executed"
                    );
                    HookDecision::Allow
                }),
            )
            .on(
                "on_error",
                log(|e| {
                    let HookEvent::OnError { reason, .. } = e else {
                        return HookDecision::Allow;
                    };
                    tracing::debug!(error = %reason, "hook: error");
                    HookDecision::Allow
                }),
            )
    }
}

/// Lift a sync `tracing` closure into the boxed-future handler shape.
fn log<F: Fn(&HookEvent) -> HookDecision + Send + Sync + 'static>(
    f: F,
) -> impl for<'a> Fn(&'a HookEvent) -> BoxFuture<'a, HookDecision>
+ Send
+ Sync
+ 'static {
    let f = std::sync::Arc::new(f);
    move |event: &HookEvent| {
        let f = std::sync::Arc::clone(&f);
        Box::pin(async move { f(event) })
    }
}

/// Convenience: `LoggingHook`'s map as a ready `Arc`-free value.
#[must_use]
pub fn logging_hook_map() -> HookMap {
    LoggingHook::hook_map()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn logging_hook_allows_every_event() {
        let hooks = LoggingHook::hook_map();
        for event in [
            HookEvent::BeforeRun {
                agent_name: "a".into(),
                run_idx: 0,
            },
            HookEvent::AfterRun {
                agent_name: "a".into(),
                run_idx: 0,
                reason: "completed".into(),
            },
            HookEvent::BeforeTool {
                tool_name: "echo".into(),
                call_id: "c1".into(),
                arguments: serde_json::Value::Null,
            },
            HookEvent::AfterTool {
                tool_name: "echo".into(),
                call_id: "c1".into(),
                is_error: false,
            },
            HookEvent::AfterStep {
                agent_name: "a".into(),
                run_idx: 0,
                iteration: 1,
            },
            HookEvent::BeforeProviderCall {
                provider: "p".into(),
                model: "m".into(),
                estimated_tokens: 10,
            },
            HookEvent::AfterProviderCall {
                provider: "p".into(),
                model: "m".into(),
                prompt_tokens: 1,
                completion_tokens: 2,
                stop_reason: None,
            },
            HookEvent::OnError {
                agent_name: "a".into(),
                reason: "x".into(),
            },
            HookEvent::OnCompaction {
                start: 0,
                end: 2,
                summary: "s".into(),
            },
        ] {
            assert_eq!(
                hooks.dispatch(&event).await,
                HookDecision::Allow,
                "the logger observes; it never decides"
            );
        }
        // A map that is never installed must still be constructible and
        // answer `is_empty` honestly.
        assert!(!hooks.is_empty());
        let _ = LoggingHook;
    }
}
