//! Steering wiring: guards, hooks, sanitizers, hints, output
//! transformers, and the context-manager + chunk persistence
//! contracts.

use std::sync::Arc;

use futures::StreamExt;
use serde_json::json;
use synthia_context::AgentState;
use synthia_provider::{ContentPart, traits::ModelProvider};
use synthia_steering::{HintMessage, HintPriority, InjectionPoint, Steering};
use synthia_test_support::FakeTool;
use synthia_tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use super::{support::*, *};
#[tokio::test]
async fn hint_injection_reaches_next_llm_request() {
    struct AlwaysHint;
    impl synthia_steering::Hint for AlwaysHint {
        fn name(&self) -> &str {
            "always"
        }

        fn should_trigger(&self, _state: &AgentState) -> bool {
            true
        }

        fn generate(&self, _state: &AgentState) -> HintMessage {
            HintMessage {
                content: "wrap up soon".to_string(),
                priority: HintPriority::Normal,
            }
        }

        fn injection_point(&self) -> InjectionPoint {
            InjectionPoint::BeforeNextLlmCall
        }
    }

    let tool_use = ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![tool_use.clone()]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(synthia_tool::ToolEntry::new(Arc::new(
        FakeTool::new("echo", "echoed"),
    )));
    let steering = Arc::new(Steering::noop().add_hint(Arc::new(AlwaysHint)));

    let agent =
        ReActAgent::new(provider.clone(), registry).with_steering(steering);
    let mut stream = agent
        .run(
            AgentInput::text("run echo"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while let Some(ev) = stream.next().await {
        let _ = ev;
    }

    let requests = provider.captured.lock().await;
    assert_eq!(requests.len(), 2, "two LLM passes expected");
    let second = &requests[1];
    let tail = second.last().expect("non-empty second request");
    assert!(
        matches!(&tail.content, Content::Single(ContentPart::Text(t))
                if t.text.starts_with("[reminder]")
                    && t.text.contains("wrap up soon")),
        "second request should carry the reminder at the tail, got {:?}",
        tail.content
    );
}

#[tokio::test]
async fn panicking_hint_and_tracker_degrade_on_every_call_site() {
    use synthia_context::AgentState;
    use synthia_provider::TokenUsage;
    use synthia_steering::{Hint, HintMessage, tracker::Tracker};

    struct BoomHint {
        asked: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Hint for BoomHint {
        fn name(&self) -> &str {
            "boom-hint"
        }

        fn should_trigger(&self, _state: &AgentState) -> bool {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        }

        fn generate(&self, _state: &AgentState) -> HintMessage {
            panic!("hint blew up");
        }

        fn injection_point(&self) -> InjectionPoint {
            InjectionPoint::SystemPrompt
        }
    }

    struct BoomToolHint {
        asked: Arc<std::sync::atomic::AtomicUsize>,
    }
    impl Hint for BoomToolHint {
        fn name(&self) -> &str {
            "boom-tool-hint"
        }

        fn should_trigger(&self, _state: &AgentState) -> bool {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            true
        }

        fn generate(&self, _state: &AgentState) -> HintMessage {
            panic!("tool hint blew up");
        }

        fn injection_point(&self) -> InjectionPoint {
            InjectionPoint::AppendToToolResult {
                tool_name: "echo".to_string(),
            }
        }
    }

    type Seen = Arc<parking_lot::Mutex<std::collections::BTreeSet<String>>>;
    struct BoomTracker {
        seen: Seen,
    }
    impl BoomTracker {
        fn note(&self, which: &str) -> ! {
            self.seen.lock().insert(which.to_string());
            panic!("tracker.{which}");
        }
    }
    impl Tracker for BoomTracker {
        fn on_iteration(&self, _state: &AgentState) {
            self.note("on_iteration");
        }

        fn on_tool_call(
            &self,
            _name: &str,
            _arguments: &serde_json::Value,
            _state: &AgentState,
        ) {
            self.note("on_tool_call");
        }

        fn on_llm_response(&self, _usage: &TokenUsage, _state: &AgentState) {
            self.note("on_llm_response");
        }

        fn recommended_concurrency(&self, _state: &AgentState) -> usize {
            self.note("recommended_concurrency");
        }
    }

    let provider: Arc<dyn ModelProvider> =
        Arc::new(ScriptedStreamProvider::new(vec![
            tool_call_response(vec![ToolUse {
                id: "call_1".to_string(),
                name: "echo".to_string(),
                input: json!({}),
            }]),
            empty_response(),
        ]));
    let (registry, _calls) = echo_registry();

    let seen: Seen = Arc::new(parking_lot::Mutex::new(Default::default()));
    let pre_sample_asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let tool_result_asked = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let steering = Arc::new(synthia_steering::Steering {
        hints: vec![
            Arc::new(BoomHint {
                asked: Arc::clone(&pre_sample_asked),
            }),
            Arc::new(BoomToolHint {
                asked: Arc::clone(&tool_result_asked),
            }),
        ],
        tracker: Arc::new(BoomTracker {
            seen: Arc::clone(&seen),
        }),
        ..synthia_steering::Steering::noop()
    });

    let agent = ReActAgent::new(provider, registry).with_steering(steering);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    let reason = events.iter().find_map(|e| match e {
        AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
            Some(reason.clone())
        }
        _ => None,
    });
    assert!(
        matches!(reason, Some(SessionEndReason::Completed)),
        "a panicking hint/tracker must not end the run, got {reason:?} \
         from {:?}",
        events.iter().map(|e| e.kind()).collect::<Vec<_>>()
    );
    let seen = seen.lock().clone();
    assert_eq!(
        seen.len(),
        4,
        "every Tracker method must have been reached and survived its \
         panic; saw {seen:?}"
    );

    // Both injection points were exercised, and the tool-result hint was
    // reached *exactly once* — by `append_tool_result_hints`, not by the
    // pre-sample path. Without the injection-point filter in
    // `inject_hints`, the tool-result hint would be evaluated there once
    // per iteration as well (this run has two), so the count would be 3.
    use std::sync::atomic::Ordering;
    assert!(
        pre_sample_asked.load(Ordering::SeqCst) > 0,
        "the pre-sample hint site must have been exercised"
    );
    assert_eq!(
        tool_result_asked.load(Ordering::SeqCst),
        1,
        "the tool-result hint must be evaluated only at commit time"
    );
}

/// A `SystemPrompt`-point hint fires once per session, not once per triggering
/// iteration: re-appending it each iteration grows the system message and breaks
/// the prompt-cache stability the crate keeps volatile facts out to protect.
#[tokio::test]
async fn every_system_prompt_hint_is_injected() {
    use synthia_context::AgentState;
    use synthia_steering::{
        Hint,
        HintMessage,
        HintPriority,
        hint::InjectionPoint,
    };

    struct Named(&'static str);
    impl Hint for Named {
        fn name(&self) -> &str {
            self.0
        }

        fn should_trigger(&self, _state: &AgentState) -> bool {
            true
        }

        fn generate(&self, _state: &AgentState) -> HintMessage {
            HintMessage {
                content: format!("MARKER-{}", self.0),
                priority: HintPriority::Normal,
            }
        }

        fn injection_point(&self) -> InjectionPoint {
            InjectionPoint::SystemPrompt
        }
    }

    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "call_1".to_string(),
            name: "echo".to_string(),
            input: json!({}),
        }]),
        empty_response(),
    ]));
    let (registry, _calls) = echo_registry();

    let steering = Arc::new(synthia_steering::Steering {
        hints: vec![Arc::new(Named("alpha")), Arc::new(Named("beta"))],
        ..synthia_steering::Steering::noop()
    });

    let agent =
        ReActAgent::new(provider.clone(), registry).with_steering(steering);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let captured = provider.captured.lock().await;
    let system_text = captured
        .last()
        .and_then(|req| req.first())
        .map(|m| m.content.extract_text().unwrap_or_default())
        .unwrap_or_default();
    for name in ["alpha", "beta"] {
        assert_eq!(
            system_text.matches(&format!("MARKER-{name}")).count(),
            1,
            "hint `{name}` must be injected exactly once; system message \
             was: {system_text}"
        );
    }
}
