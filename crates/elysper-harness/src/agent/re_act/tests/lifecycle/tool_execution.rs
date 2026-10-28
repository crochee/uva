use std::sync::Arc;

use serde_json::json;
use elysper_provider::{
    ContentPart,
    SamplingResult,
    StreamChunk,
    TokenUsage,
    ToolUse,
};
use elysper_test_support::FakeTool;
use elysper_tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use super::super::{support::*, *};

pub(super) async fn token_usage_emitted_per_iteration_can_be_aggregated() {
    // Verifies that `Usage` events are emitted **once per
    // LLM call** (per iteration), not just once for the
    // whole session, and that intermediate
    // `StreamChunk::Usage` deltas do **not** double-count
    // (Anthropic's `message_delta.usage` arrives before
    // `message.usage`; only the latter is authoritative).
    // The test runs a 2-iteration loop (tool call → final
    // empty response) and asserts exactly two distinct
    // Usage events fire with the expected values per
    // iteration, even though the provider scripts
    // intermediate Usage chunks too.
    let tool_use = ToolUse {
        id: "call_x".to_string(),
        name: "noop".to_string(),
        input: json!({}),
    };
    let final_text_response_with_usage = |input: usize, output: usize| {
        vec![
            // Intermediate delta—must NOT surface a
            // Usage event on its own.
            StreamChunk::Usage(TokenUsage {
                prompt_tokens: 1000, // decoy; would
                // pollute the sum if emitted
                completion_tokens: 1000,
                total_tokens: 2000,
                cached_prompt_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            }),
            StreamChunk::Content(ContentPart::Text(TextContent {
                text: "thinking".into(),
                cache_control: None,
            })),
            StreamChunk::IsDone {
                result: Box::new(SamplingResult {
                    text: String::new(),
                    tool_calls: vec![],
                    reasoning: String::new(),
                    reasoning_signature: None,
                    usage: TokenUsage {
                        prompt_tokens: input,
                        completion_tokens: output,
                        total_tokens: input + output,
                        cached_prompt_tokens: None,
                        cache_read_tokens: None,
                        cache_write_tokens: None,
                        reasoning_tokens: None,
                    },
                    ..Default::default()
                }),
            },
        ]
    };
    let tool_chunks = vec![
        tool_call_response_with_usage(
            vec![tool_use.clone()],
            TokenUsage {
                prompt_tokens: 1000, // decoy delta
                completion_tokens: 1000,
                total_tokens: 2000,
                cached_prompt_tokens: None,
                cache_read_tokens: None,
                cache_write_tokens: None,
                reasoning_tokens: None,
            },
        ),
        final_text_response_with_usage(120, 30),
    ];
    let provider = Arc::new(ScriptedStreamProvider::new(tool_chunks));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(elysper_tool::ToolEntry::new(Arc::new(
        FakeTool::new("noop", "done"),
    )));

    let events = run_and_collect(
        provider,
        registry,
        CancellationToken::new(),
        AgentInput::text("iterate"),
    )
    .await;

    let usage_events: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::System(SystemEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            }) => Some((*input_tokens, *output_tokens)),
            _ => None,
        })
        .collect();
    assert_eq!(
        usage_events.len(),
        2,
        "expected one Usage event per LLM iteration, got {usage_events:?}"
    );
    // Iteration 1: IsDone carries default usage
    // (tool_call_response used `TokenUsage::default()` in
    // its IsDone), so this iteration's Usage event is
    // (0, 0).
    assert_eq!(usage_events[0], (0, 0), "iteration 1");
    // Iteration 2: IsDone carries (120, 30)—proves the
    // intermediate decoy `(1000, 1000)` was suppressed.
    assert_eq!(usage_events[1], (120, 30), "iteration 2");

    // Sanity check: the aggregate is the IsDone totals,
    // not a doubled sum including the decoys.
    let (sum_in, sum_out) = usage_events
        .iter()
        .fold((0usize, 0usize), |(i, o), (x, y)| (i + x, o + y));
    assert_eq!((sum_in, sum_out), (120, 30));
}

pub(super) async fn run_executes_tool_and_emits_tool_result() {
    let tool_use = ToolUse {
        id: "call_1".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    };
    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![tool_use.clone()]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(elysper_tool::ToolEntry::new(Arc::new(
        FakeTool::new("echo", "echoed"),
    )));

    let events = run_and_collect(
        provider,
        registry,
        CancellationToken::new(),
        AgentInput::text("run tool"),
    )
    .await;

    let tool_uses: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolUse(tu)) => Some(tu.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(tool_uses.len(), 1);
    assert_eq!(tool_uses[0].id, "call_1");
    assert_eq!(tool_uses[0].name, "echo");

    let tool_results: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(tr)) => Some(tr.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(tool_results.len(), 1);
    assert_eq!(tool_results[0].tool_use_id, "call_1");
    assert_eq!(tool_results[0].tool_name.as_deref(), Some("echo"));
    assert!(!tool_results[0].is_error.unwrap_or(true));
    let text = match &tool_results[0].content[0] {
        ContentPart::Text(t) => &t.text,
        _ => panic!("expected text content"),
    };
    assert_eq!(text, "echoed");

    assert!(matches!(
        events.last(),
        Some(AgentEvent::System(SystemEvent::SessionEnded {
            reason: SessionEndReason::Completed,
        }))
    ));
}

/// A cancellation that lands between two Sequential calls must
/// answer the undispatched call with the cause-tagged error:
/// "cancelled before dispatch" is a *safe-to-retry* signal, as
/// opposed to the unknown-outcome skip a sibling error produces.
pub(super) async fn cancelled_mid_sequential_batch_tags_undispatched_calls() {
    use async_trait::async_trait;
    use elysper_tool::{Context, Tool, ToolOutput};

    struct CancelThenFinish(tokio_util::sync::CancellationToken);

    #[async_trait]
    impl Tool for CancelThenFinish {
        fn name(&self) -> &str {
            "canceller"
        }

        fn description(&self) -> &str {
            "cancels the run, then succeeds"
        }

        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }

        fn mode(&self) -> elysper_tool::traits::ExecutionMode {
            elysper_tool::traits::ExecutionMode::Sequential
        }

        async fn call(&self, _: serde_json::Value, _: &Context) -> ToolOutput {
            self.0.cancel();
            ToolOutput::text("done")
        }
    }

    struct NeverRuns;
    static NEVER_RAN: std::sync::atomic::AtomicUsize =
        std::sync::atomic::AtomicUsize::new(0);

    #[async_trait]
    impl Tool for NeverRuns {
        fn name(&self) -> &str {
            "never"
        }

        fn description(&self) -> &str {
            "must not run after the batch is cancelled"
        }

        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }

        fn mode(&self) -> elysper_tool::traits::ExecutionMode {
            elysper_tool::traits::ExecutionMode::Sequential
        }

        async fn call(&self, _: serde_json::Value, _: &Context) -> ToolOutput {
            NEVER_RAN.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            ToolOutput::text("ran")
        }
    }

    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![
            ToolUse {
                id: "c1".to_string(),
                name: "canceller".to_string(),
                input: json!({}),
            },
            ToolUse {
                id: "c2".to_string(),
                name: "never".to_string(),
                input: json!({}),
            },
        ]),
        empty_response(),
    ]));
    let cancel = CancellationToken::new();
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(elysper_tool::ToolEntry::new(Arc::new(
        CancelThenFinish(cancel.clone()),
    )));
    registry.register_entry(elysper_tool::ToolEntry::new(Arc::new(NeverRuns)));

    let events =
        run_and_collect(provider, registry, cancel, AgentInput::text("go"))
            .await;

    assert_eq!(
        NEVER_RAN.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a call after mid-batch cancellation must not dispatch"
    );
    let text_of = |tr: &elysper_provider::ToolResult| {
        tr.content
            .iter()
            .filter_map(|c| match c {
                ContentPart::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ")
    };
    let results: Vec<&elysper_provider::ToolResult> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(tr)) => Some(tr),
            _ => None,
        })
        .collect();
    let unanswered = results
        .iter()
        .find(|tr| tr.tool_use_id == "c2")
        .expect("the undispatched call must still be answered");
    let text = text_of(unanswered);
    assert!(
        text.contains("cancelled before dispatch")
            && text.contains("safe to retry"),
        "a cancelled-before-dispatch call is safe to retry; got {text:?}"
    );
    assert!(unanswered.is_error.unwrap_or(false));
}
