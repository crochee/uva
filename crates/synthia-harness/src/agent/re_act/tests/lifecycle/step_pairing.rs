use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use synthia_core::SystemClock;
use synthia_provider::{
    ContentPart,
    SamplingResult,
    StreamChunk,
    TokenUsage,
    ToolUse,
};
use synthia_session::{LogWriter, SessionSink, in_memory::InMemorySessionSink};
use synthia_tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use super::super::{support::*, *};

pub(super) async fn every_step_start_has_a_matching_end() {
    use synthia_core::CancelToken;
    use synthia_provider::traits::ModelProvider;
    use synthia_session::SessionEvent;

    // The typed rows the run wrote, read back from the log.
    async fn typed_rows(sink: &InMemorySessionSink) -> Vec<SessionEvent> {
        let rows = sink.read().await.expect("read");
        rows.iter()
            .filter_map(|row| {
                serde_json::from_value::<SessionEvent>(row.clone()).ok()
            })
            .collect()
    }

    fn step_pairs(events: &[SessionEvent]) -> Vec<(u64, &'static str)> {
        events
            .iter()
            .filter_map(|e| match e {
                SessionEvent::Step { data, .. } => {
                    let idx = data.get("step")?.as_u64()?;
                    let kind = match data.get("kind")?.as_str()? {
                        "start" => "start",
                        "end" => "end",
                        other => panic!("unknown step kind {other}"),
                    };
                    Some((idx, kind))
                }
                _ => None,
            })
            .collect()
    }

    fn assert_paired(name: &str, events: &[SessionEvent], expect_steps: bool) {
        let pairs = step_pairs(events);
        let starts = pairs.iter().filter(|(_, k)| *k == "start").count();
        let ends = pairs.iter().filter(|(_, k)| *k == "end").count();
        if expect_steps {
            assert!(starts > 0, "{name}: no step_start at all: {pairs:?}");
        } else {
            assert_eq!(
                starts, 0,
                "{name}: the run must end before any step boundary opens: \
                 {pairs:?}"
            );
        }
        assert_eq!(
            starts, ends,
            "{name}: unpaired step boundary in the typed log: {pairs:?}"
        );
        // The loop no longer emits `Iteration` events; their
        // presence would mean the old double-boundary bug came
        // back.
        let iteration_events = events
            .iter()
            .filter(|e| matches!(e, SessionEvent::Iteration { .. }))
            .count();
        assert_eq!(
            iteration_events, 0,
            "{name}: SessionEvent::Iteration must not appear in the \
             typed log anymore"
        );
    }

    // --- Scenario 1: a normal completion. ---
    {
        let provider: Arc<dyn ModelProvider> =
            Arc::new(ScriptedStreamProvider::new(vec![empty_response()]));
        let sink = Arc::new(InMemorySessionSink::new("s"));
        let writer =
            Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
        let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
            .with_log_writer(writer);
        let mut stream = agent
            .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
            .await;
        while stream.next().await.is_some() {}
        let events = typed_rows(&sink).await;
        assert_paired("normal completion", &events, true);
    }

    // --- Scenario 2: cancelled before the LLM call. ---
    // A token that already fired ends the run at setup, so the
    // typed log opens no step boundary at all — there is nothing
    // to pair, and nothing half-open.
    {
        let provider: Arc<dyn ModelProvider> =
            Arc::new(ScriptedStreamProvider::new(Vec::new()));
        let sink = Arc::new(InMemorySessionSink::new("s"));
        let writer =
            Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
        let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
            .with_log_writer(writer);
        let cancel = CancellationToken::new();
        cancel.cancel();
        let mut stream =
            agent.run(AgentInput::text("go"), Arc::new(cancel)).await;
        while stream.next().await.is_some() {}
        let events = typed_rows(&sink).await;
        assert_paired("cancelled before LLM", &events, false);
    }

    // --- Scenario 3: the sample failed (phase-3 early return). ---
    {
        struct FailingProvider;
        #[async_trait]
        impl ModelProvider for FailingProvider {
            async fn initialize(
                &mut self,
                _config: synthia_provider::ProviderConfig,
            ) -> Result<(), synthia_core::Error> {
                Ok(())
            }

            fn name(&self) -> &str {
                "failing"
            }

            fn model_config(&self) -> synthia_provider::ModelConfig {
                synthia_provider::ModelConfig {
                    name: "failing".to_string(),
                    provider: "failing".to_string(),
                    context_window: 8_192,
                    max_output_tokens: 1_024,
                    supports_tools: true,
                    supports_streaming: true,
                    supports_reasoning: false,
                }
            }

            async fn complete(
                &self,
                _request: synthia_provider::CompletionRequest,
            ) -> Result<synthia_provider::CompletionResponse, synthia_core::Error>
            {
                panic!("provider blew up");
            }

            async fn complete_with_stream(
                &self,
                _request: synthia_provider::CompletionRequest,
                _cancel: Option<Arc<dyn CancelToken>>,
                _on_delta: Box<dyn FnMut(StreamChunk) + Send>,
            ) -> Result<synthia_provider::CompletionResponse, synthia_core::Error>
            {
                panic!("provider blew up");
            }
        }

        let sink = Arc::new(InMemorySessionSink::new("s"));
        let writer =
            Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
        let agent = ReActAgent::new(
            Arc::new(FailingProvider),
            Arc::new(ToolRegistry::new()),
        )
        .with_log_writer(writer);
        let mut stream = agent
            .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
            .await;
        while stream.next().await.is_some() {}
        let events = typed_rows(&sink).await;
        assert_paired("sample failed", &events, true);
    }

    // --- Scenario 4: the cancel-before-tool path. ---
    {
        use synthia_steering::HookEvent;

        // Cancels mid-stream so the tool batch never runs.
        fn cancel_after_provider(
            cancel: CancellationToken,
        ) -> synthia_steering::HookMap {
            synthia_steering::HookMap::new().on(
                "after_provider_call",
                move |_: &HookEvent| {
                    let cancel = cancel.clone();
                    Box::pin(async move {
                        cancel.cancel();
                        synthia_steering::HookDecision::Allow
                    })
                },
            )
        }

        let cancel = CancellationToken::new();
        let provider: Arc<dyn ModelProvider> =
            Arc::new(ScriptedStreamProvider::new(vec![tool_call_response(
                vec![ToolUse {
                    id: "call_1".to_string(),
                    name: "echo".to_string(),
                    input: json!({}),
                }],
            )]));
        let (registry, _calls) = echo_registry();
        let steering = Arc::new(synthia_steering::Steering {
            hooks: cancel_after_provider(cancel.clone()),
            ..synthia_steering::Steering::noop()
        });
        let sink = Arc::new(InMemorySessionSink::new("s"));
        let writer =
            Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
        let agent = ReActAgent::new(provider, registry)
            .with_steering(steering)
            .with_log_writer(writer);

        let mut stream =
            agent.run(AgentInput::text("go"), Arc::new(cancel)).await;
        while stream.next().await.is_some() {}
        let events = typed_rows(&sink).await;
        assert_paired("cancelled before tool", &events, true);
    }
}

/// A signed reasoning block reaches the **next** request, in the
/// provider's own order.
///
/// This is the half of the reasoning contract the harness was missing.
/// `synthia-provider` states it four times as the agent layer's job
/// (`anthropic/types.rs`: the `signature` is "Required to preserve
/// reasoning continuity across turns"; `types/stream_chunk.rs`: it is
/// "propagated so the agent can preserve cross-turn reasoning
/// continuity"), and its Anthropic adapter already maps
/// `ContentPart::Reasoning` → `ThinkingBlock { thinking, signature }`
/// on the way out. What was missing was the middle: the loop dropped
/// the part, so the adapter never saw one in a real run.
///
/// A probe before the fix reported
/// `request 2 has reasoning part = false, signature preserved = false`.
pub(super) async fn reasoning_reaches_the_next_request() {
    use synthia_provider::ReasoningContent;

    // Turn 1 streams reasoning *before* its tool call, so a second
    // sampling pass follows with that assistant turn in history.
    //
    // The chunk's signature is `None` and the signature arrives on
    // `IsDone`, which is the only shape the Anthropic processor can
    // emit: `signature_delta` lands after the `thinking_delta`s that
    // carried the text, so a streamed reasoning chunk cannot know it
    // yet. Handing the chunk a filled-in signature here would test a
    // shape that never occurs and would pass while the real path lost
    // the value.
    // Two reasoning deltas, as the processor emits for one thinking
    // block (`content_block_start` text + each `thinking_delta`). The
    // history must end up with **one** block, not one per delta:
    // Anthropic requires a single thinking block per prior assistant
    // turn, so a delta-per-part history would be malformed.
    let turn1 = vec![
        StreamChunk::Content(ContentPart::Reasoning(ReasoningContent {
            text: "I should ".to_string(),
            signature: None,
        })),
        StreamChunk::Content(ContentPart::Reasoning(ReasoningContent {
            text: "call echo".to_string(),
            signature: None,
        })),
        StreamChunk::ToolCallStart {
            id: "c1".to_string(),
            name: "echo".to_string(),
            arguments: json!({}),
        },
        StreamChunk::ToolCallEnd {
            id: "c1".to_string(),
        },
        StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: String::new(),
                tool_calls: vec![ToolUse {
                    id: "c1".to_string(),
                    name: "echo".to_string(),
                    input: json!({}),
                }],
                reasoning: "I should call echo".to_string(),
                reasoning_signature: Some("sig_123".to_string()),
                usage: TokenUsage::default(),
                stop_reason: Some("tool_use".to_string()),
            }),
        },
    ];
    let provider =
        Arc::new(CapturingProvider::new(vec![turn1, empty_response()]));
    let (registry, _calls) = echo_registry();
    let agent =
        ReActAgent::new(provider.clone(), registry).with_max_iterations(2);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let captured = provider.captured.lock().await;
    assert!(
        captured.len() >= 2,
        "need a second request to inspect; got {}",
        captured.len()
    );
    // The assistant turn from pass 1 rides in pass 2's history.
    let all_parts: Vec<&ContentPart> =
        captured[1].iter().flat_map(|m| &m.content).collect();
    let reasoning: Vec<&synthia_provider::ReasoningContent> = all_parts
        .iter()
        .filter_map(|p| match p {
            ContentPart::Reasoning(r) => Some(r),
            _ => None,
        })
        .collect();
    assert_eq!(
        reasoning.len(),
        1,
        "the two deltas must commit as one thinking block; parts: {all_parts:?}"
    );
    assert_eq!(
        reasoning[0].text, "I should call echo",
        "and the block must carry the whole accumulated text"
    );
    assert_eq!(
        reasoning[0].signature.as_deref(),
        Some("sig_123"),
        "the signature must survive into the next request even though the \
         streamed chunks could not carry it; parts: {all_parts:?}"
    );
}
