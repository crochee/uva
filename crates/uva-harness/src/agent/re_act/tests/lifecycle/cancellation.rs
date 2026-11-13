use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uva_provider::{ContentPart, SamplingResult, StreamChunk, ToolUse};
use uva_tool::{Context, Tool, ToolOutput, ToolRegistry};

use super::super::{support::*, *};

/// A token that already fired when `Agent::run` is called must end
/// the run at setup: `EventSink::begin` publishes `SessionStarted`
/// and, seeing the token, the terminal event itself. The loop never
/// reaches an iteration, so there is no request header, no
/// `SessionInterrupted`, and no provider call.
pub(super) async fn a_token_cancelled_before_the_run_ends_at_setup() {
    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "call_never".to_string(),
            name: "echo".to_string(),
            input: json!({}),
        }]),
        empty_response(),
    ]));
    let cancel = CancellationToken::new();
    cancel.cancel();

    let events = run_and_collect(
        provider,
        Arc::new(ToolRegistry::new()),
        cancel,
        AgentInput::text("hi"),
    )
    .await;

    let kinds: Vec<&str> = events.iter().map(|e| e.kind()).collect();
    assert_eq!(
        kinds,
        vec!["System", "System"],
        "a pre-cancelled run must end at setup, before any iteration; got {kinds:?}"
    );
    assert!(matches!(
        events[0],
        AgentEvent::System(SystemEvent::SessionStarted { .. })
    ));
    assert!(matches!(
        events[1],
        AgentEvent::System(SystemEvent::SessionEnded {
            reason: SessionEndReason::Cancelled,
        })
    ));
}

/// Cancelling *during* the run is the other shape: the loop is past
/// setup, so it reports the interruption and then the terminal
/// event. Distinct from the pre-cancelled case above, which never
/// starts an iteration.
pub(super) async fn a_token_cancelled_mid_run_reports_the_interruption() {
    let cancel = CancellationToken::new();
    let provider = Arc::new(CancellingStreamProvider::new(cancel.clone()));

    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()));
    let mut stream = agent.run(AgentInput::text("hi"), Arc::new(cancel)).await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    let kinds: Vec<&str> = events.iter().map(|e| e.kind()).collect();
    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::System(SystemEvent::SessionInterrupted { .. })
        )),
        "a mid-run cancel must report the interruption; got {kinds:?} of {events:?}"
    );
    let interrupted_at = events
        .iter()
        .position(|e| {
            matches!(
                e,
                AgentEvent::System(SystemEvent::SessionInterrupted { .. })
            )
        })
        .expect("checked above");
    let last = events.last().expect("the run always reports an end");
    assert!(
        interrupted_at + 1 < events.len(),
        "the interruption must not be the last event; got {events:?}"
    );
    assert!(
        matches!(
            last,
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Cancelled,
            })
        ),
        "the interruption must be followed by the terminal event; got {last:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                AgentEvent::System(SystemEvent::SessionEnded { .. })
            ))
            .count(),
        1,
        "a run publishes exactly one terminal event; got {kinds:?}"
    );
}

/// The loop's terminal event is the *last* event of a normal run
/// and carries `Completed` — the same shape a `ReasoningStrategy`
/// produces through `EventSink::finish`. Routing the loop through
/// the sink must not reorder or swallow it.
pub(super) async fn a_completed_run_ends_with_one_completed_terminal() {
    let provider = Arc::new(ScriptedStreamProvider::new(vec![vec![
        StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: "done".to_string(),
                ..Default::default()
            }),
        },
    ]]));

    let events = run_and_collect(
        provider,
        Arc::new(ToolRegistry::new()),
        CancellationToken::new(),
        AgentInput::text("hi"),
    )
    .await;

    let last = events.last().expect("a run always reports an end");
    assert!(
        matches!(
            last,
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Completed,
            })
        ),
        "the terminal event must be last and Completed; got {last:?}"
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(
                e,
                AgentEvent::System(SystemEvent::SessionEnded { .. })
            ))
            .count(),
        1,
        "a run publishes exactly one terminal event; got {events:?}"
    );
}

/// A tool that panics must not take the whole run's error reporting with
/// it: the caller still has to learn the session ended.
pub(super) async fn panicking_tool_still_reports_session_end() {
    struct BoomTool;

    #[async_trait]
    impl Tool for BoomTool {
        fn name(&self) -> &str {
            "boom"
        }

        fn description(&self) -> &str {
            "panics on call"
        }

        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }

        async fn call(
            &self,
            _input: serde_json::Value,
            _ctx: &Context,
        ) -> ToolOutput {
            panic!("tool blew up");
        }
    }

    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "call_boom".to_string(),
            name: "boom".to_string(),
            input: json!({}),
        }]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(BoomTool)));

    let events = run_and_collect(
        provider,
        registry,
        CancellationToken::new(),
        AgentInput::text("go"),
    )
    .await;

    let ended = events.iter().any(|e| {
        matches!(e, AgentEvent::System(SystemEvent::SessionEnded { .. }))
    });
    assert!(
        ended,
        "a panicking tool must not swallow the terminal event; got {:?}",
        events.iter().map(|e| e.kind()).collect::<Vec<_>>()
    );

    // And the panic is reported *as a tool error*, so the model can see
    // what happened and adapt rather than the call vanishing.
    let result = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(r)) => Some(r),
            _ => None,
        })
        .expect("the failed call must still produce a tool result");
    assert_eq!(result.tool_use_id, "call_boom");
    assert_eq!(
        result.is_error,
        Some(true),
        "a panic is an error result, not a silent success"
    );
    let body = serde_json::to_string(&result.content).unwrap();
    assert!(
        body.contains("blew up"),
        "the panic message must reach the model: {body}"
    );
}

/// The same guarantee for the *delegation* seam: a plugin that panics
/// must not take the run's error reporting with it. The delegation path
/// returns before the registry path is entered, so it needs its own
/// guard—a delegator that spawns subagents is exactly the kind of
/// third-party code that can panic.
pub(super) async fn panicking_delegator_still_reports_session_end() {
    use crate::agent::{DelegatingTool, DelegationCall};

    struct BoomInterceptor;

    impl DelegatingTool for BoomInterceptor {
        fn definitions(&self) -> Vec<uva_provider::ToolDefinition> {
            vec![uva_provider::ToolDefinition {
                name: "boom".to_string(),
                description: "panics".to_string(),
                input_schema: json!({"type": "object"}),
                cache_control: None,
                annotations: None,
            }]
        }

        fn execute(
            &self,
            _call: DelegationCall<'_>,
        ) -> futures::future::BoxFuture<'static, ToolOutput> {
            Box::pin(async { panic!("delegator blew up") })
        }
    }

    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "call_boom".to_string(),
            name: "boom".to_string(),
            input: json!({}),
        }]),
        empty_response(),
    ]));

    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
        .with_delegator(Arc::new(BoomInterceptor))
        .expect("first install must succeed");
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }

    assert!(
        events.iter().any(|e| matches!(
            e,
            AgentEvent::System(SystemEvent::SessionEnded { .. })
        )),
        "a panicking delegator must not swallow the terminal event; \
         got {:?}",
        events.iter().map(|e| e.kind()).collect::<Vec<_>>()
    );
    let result = events
        .iter()
        .find_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(r)) => Some(r),
            _ => None,
        })
        .expect("the failed call must still produce a tool result");
    assert_eq!(result.is_error, Some(true));
    let body = serde_json::to_string(&result.content).unwrap();
    assert!(
        body.contains("delegator blew up"),
        "the delegator's panic message must reach the model: {body}"
    );
}

/// The strategy backstop: a panic anywhere the inner guards cannot
/// reach—the loop's own bucketing, commit, hook fan-out, guards—
/// still owes the caller a reported end. `agent/impls.rs` wraps the whole
/// `strategy.run(..)` call for exactly this, since nothing below it can
/// catch a panic in the loop's own code.
pub(super) async fn panicking_strategy_still_reports_session_end() {
    use crate::agent::{AgentRuntime, EventSink, ReasoningStrategy};

    struct BoomStrategy;

    #[async_trait]
    impl ReasoningStrategy for BoomStrategy {
        fn name(&self) -> &str {
            "boom"
        }

        async fn run(
            &self,
            _runtime: AgentRuntime,
            _input: AgentInput,
            _sink: &EventSink,
        ) {
            panic!("strategy blew up");
        }
    }

    let agent = ReActAgent::new(
        Arc::new(uva_provider::traits_stub::ModelProviderStub::text_only(
            "unused",
        )),
        Arc::new(ToolRegistry::new()),
    )
    .with_strategy(Arc::new(BoomStrategy));

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
        matches!(reason, Some(SessionEndReason::Error(ref m)) if m.contains("strategy blew up")),
        "a panicking strategy must end the run reported, got {reason:?} \
         from {events:?}"
    );
}

/// A panicking `ModelProvider` must end the run reported too. It is the
/// one individually-guarded seam whose panic cannot be turned into a
/// value the loop merely records—there is no response to carry on
/// with, so it maps to `SessionEnded { reason: Error(..) }`.
pub(super) async fn panicking_provider_still_reports_session_end() {
    struct BoomProvider;

    #[async_trait]
    impl uva_provider::traits::ModelProvider for BoomProvider {
        async fn initialize(
            &mut self,
            _config: uva_provider::ProviderConfig,
        ) -> Result<(), uva_core::Error> {
            Ok(())
        }

        fn name(&self) -> &str {
            "boom"
        }

        fn model_config(&self) -> uva_provider::ModelConfig {
            uva_provider::ModelConfig {
                name: "boom".to_string(),
                provider: "boom".to_string(),
                context_window: 8_192,
                max_output_tokens: 1_024,
                supports_tools: true,
                supports_streaming: true,
                supports_reasoning: false,
            }
        }

        async fn complete(
            &self,
            _request: uva_provider::CompletionRequest,
        ) -> Result<uva_provider::CompletionResponse, uva_core::Error> {
            panic!("provider blew up");
        }

        async fn complete_with_stream(
            &self,
            _request: uva_provider::CompletionRequest,
            _cancel: Option<Arc<dyn uva_core::CancelToken>>,
            _on_delta: Box<dyn FnMut(StreamChunk) + Send>,
        ) -> Result<uva_provider::CompletionResponse, uva_core::Error> {
            panic!("provider blew up");
        }
    }

    let agent =
        ReActAgent::new(Arc::new(BoomProvider), Arc::new(ToolRegistry::new()));
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
        matches!(
            reason,
            Some(SessionEndReason::Error(ref m)) if m.contains("provider blew up")
        ),
        "a panicking provider must end the run reported, got {reason:?} \
         from {:?}",
        events.iter().map(|e| e.kind()).collect::<Vec<_>>()
    );
}
