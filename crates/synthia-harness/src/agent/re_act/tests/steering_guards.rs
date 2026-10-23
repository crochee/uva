//! Steering wiring: guards, hooks, sanitizers, hints, output
//! transformers, and the context-manager + chunk persistence
//! contracts.

use std::sync::Arc;

use serde_json::{Value, json};
use synthia_context::AgentState;
use synthia_provider::{ContentPart, TokenUsage};
use synthia_steering::{GuardResult, Steering};
use synthia_test_support::FakeTool;
use synthia_tool::ToolRegistry;

use super::{support::*, *};
#[tokio::test]
async fn guard_denial_surfaces_as_error_result_and_warning() {
    let tool_use = ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    };
    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![tool_use.clone()]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(synthia_tool::ToolEntry::new(Arc::new(
        FakeTool::new("echo", "echoed"),
    )));
    let steering =
        Arc::new(Steering::noop().add_guard(Arc::new(DenyEchoGuard)));

    let events = run_and_collect_with_steering(
        provider,
        registry,
        steering,
        AgentInput::text("run echo"),
    )
    .await;

    let tool_results: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(tr)) => Some(tr.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(tool_results.len(), 1);
    assert!(tool_results[0].is_error.unwrap_or(false));
    let text = match &tool_results[0].content[0] {
        ContentPart::Text(t) => t.text.clone(),
        _ => panic!("expected text content"),
    };
    assert!(
        text.contains("[blocked by guard `deny_echo` (high)]")
            && text.contains("echo is disabled"),
        "denial text should carry guard name + reason, got: {text}"
    );

    let guard_warnings: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::System(SystemEvent::Warning {
                    kind: WarningKind::Guard,
                    ..
                })
            )
        })
        .collect();
    assert_eq!(
        guard_warnings.len(),
        1,
        "expected exactly one Guard warning event"
    );

    // The session still ends Completed — denials are
    // errors-as-results, never run aborts.
    assert!(matches!(
        events.last(),
        Some(AgentEvent::System(SystemEvent::SessionEnded {
            reason: SessionEndReason::Completed,
        }))
    ));
}

/// Pins the denial wire string and the veto-epilogue order: the tracker must see
/// the denied call before `on_error` fans out, or nothing in-tree would notice.
#[tokio::test]
async fn hook_block_surfaces_as_error_result() {
    #[derive(Default)]
    struct CountingTracker {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl synthia_steering::tracker::Tracker for CountingTracker {
        fn on_iteration(&self, _state: &AgentState) {}

        fn on_tool_call(
            &self,
            _name: &str,
            _arguments: &Value,
            _state: &AgentState,
        ) {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }

        fn on_llm_response(&self, _usage: &TokenUsage, _state: &AgentState) {}

        fn recommended_concurrency(&self, _state: &AgentState) -> usize {
            1
        }
    }

    fn observing_block_hook(
        tracker_calls: Arc<std::sync::atomic::AtomicUsize>,
        recorded: Arc<std::sync::Mutex<Option<usize>>>,
    ) -> synthia_steering::HookMap {
        synthia_steering::HookMap::new()
            .on("before_tool", move |event: &synthia_steering::HookEvent| {
                Box::pin(async move {
                    match event {
                        synthia_steering::HookEvent::BeforeTool {
                            tool_name,
                            ..
                        } if tool_name == "echo" => {
                            synthia_steering::HookDecision::Block {
                                reason: "echo requires user approval"
                                    .to_string(),
                            }
                        }
                        _ => synthia_steering::HookDecision::Allow,
                    }
                })
            })
            .on("after_tool", move |_event: &synthia_steering::HookEvent| {
                let tracker_calls = Arc::clone(&tracker_calls);
                let recorded = Arc::clone(&recorded);
                Box::pin(async move {
                    let seen =
                        tracker_calls.load(std::sync::atomic::Ordering::SeqCst);
                    *recorded.lock().expect("lock") = Some(seen);
                    synthia_steering::HookDecision::Allow
                })
            })
    }

    let tracker_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let recorded = Arc::new(std::sync::Mutex::new(None));

    let tool_use = ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    };
    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![tool_use.clone()]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(synthia_tool::ToolEntry::new(Arc::new(
        FakeTool::new("echo", "echoed"),
    )));
    let steering = Arc::new(
        Steering::noop()
            .add_hooks(observing_block_hook(
                Arc::clone(&tracker_calls),
                Arc::clone(&recorded),
            ))
            .with_tracker(Arc::new(CountingTracker {
                calls: Arc::clone(&tracker_calls),
            })),
    );

    let events = run_and_collect_with_steering(
        provider,
        registry,
        steering,
        AgentInput::text("run echo"),
    )
    .await;

    let text = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(tr)) => {
                Some(tr.content.clone())
            }
            _ => None,
        })
        .next()
        .expect("tool result event");
    assert!(text.iter().any(|p| {
        matches!(p, ContentPart::Text(t)
                if t.text.contains("[blocked by policy]")
                    && t.text.contains("user approval"))
    }));

    let seen = recorded
        .lock()
        .expect("lock")
        .expect("the block must have been observed by after_tool");
    assert!(
        seen >= 1,
        "the tracker must record the denied call before `after_tool` \
         observes it (saw {seen} recorded calls)"
    );
}

#[tokio::test]
async fn sanitized_action_is_adopted_for_dispatch() {
    struct RewriteToAlt;
    impl synthia_steering::Guard for RewriteToAlt {
        fn name(&self) -> &str {
            "rewrite_to_alt"
        }

        fn check(
            &self,
            action: &synthia_steering::Action,
            _state: &AgentState,
        ) -> GuardResult {
            match action {
                synthia_steering::Action::ToolCall { name, arguments }
                    if name == "echo" =>
                {
                    GuardResult::Sanitize {
                        action: synthia_steering::Action::ToolCall {
                            name: "alt".to_string(),
                            arguments: arguments.clone(),
                        },
                        warning: "routed to alt".to_string(),
                    }
                }
                _ => GuardResult::Allow,
            }
        }
    }

    let tool_use = ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({"x": 1}),
    };
    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![tool_use.clone()]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(synthia_tool::ToolEntry::new(Arc::new(
        FakeTool::new("echo", "echoed"),
    )));
    registry.register_entry(synthia_tool::ToolEntry::new(Arc::new(
        FakeTool::new("alt", "alt-ran"),
    )));
    let steering = Arc::new(Steering::noop().add_guard(Arc::new(RewriteToAlt)));

    let events = run_and_collect_with_steering(
        provider,
        registry,
        steering,
        AgentInput::text("run echo"),
    )
    .await;

    let result_text = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(tr)) => {
                Some(tr.content.clone())
            }
            _ => None,
        })
        .next()
        .expect("tool result event");
    assert!(
        result_text.iter().any(|p| {
            matches!(p, ContentPart::Text(t) if t.text == "alt-ran")
        })
    );
}
