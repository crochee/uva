//! Steering wiring: guards, hooks, sanitizers, hints, output
//! transformers, and the context-manager + chunk persistence
//! contracts.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use synthia_context::{AgentState, CompactionDetails, ContextManager};
use synthia_provider::{
    ContentPart,
    Message,
    SamplingResult,
    StreamChunk,
    TextContent,
    TokenUsage,
    traits::ModelProvider,
};
use synthia_test_support::FakeTool;
use synthia_tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use super::{support::*, *};
#[tokio::test]
async fn tool_file_activity_reaches_the_context_manager() {
    use std::sync::Mutex as StdMutex;

    struct Spy {
        seen: Arc<StdMutex<Vec<CompactionDetails>>>,
    }

    #[async_trait]
    impl ContextManager for Spy {
        async fn prepare(
            &self,
            _messages: &mut Vec<Message>,
            _state: &mut AgentState,
        ) {
        }

        fn set_compaction_details(&self, details: CompactionDetails) {
            self.seen.lock().unwrap().push(details);
        }
    }

    // One tool-only turn (read a.rs + write b.rs), then an empty
    // response so the loop ends.
    let tool_uses = vec![
        ToolUse {
            id: "t1".to_string(),
            name: "read".to_string(),
            input: json!({ "path": "a.rs" }),
        },
        ToolUse {
            id: "t2".to_string(),
            name: "write".to_string(),
            input: json!({ "path": "b.rs", "content": "hi" }),
        },
    ];
    let provider: Arc<dyn ModelProvider> =
        Arc::new(ScriptedStreamProvider::new(vec![
            tool_call_response(tool_uses),
            empty_response(),
        ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(synthia_tool::ToolEntry::new(Arc::new(
        FakeTool::new("read", "read ok"),
    )));
    registry.register_entry(synthia_tool::ToolEntry::new(Arc::new(
        FakeTool::new("write", "wrote ok"),
    )));

    let seen: Arc<StdMutex<Vec<CompactionDetails>>> =
        Arc::new(StdMutex::new(Vec::new()));
    let spy = Arc::new(Spy {
        seen: Arc::clone(&seen),
    });

    let agent = ReActAgent::with_descriptor(
        provider,
        registry,
        PathBuf::new(),
        crate::default_descriptor("SYS".to_string()),
        Arc::new(PromptContext::default()),
    )
    .with_context_manager(spy);

    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let captured = {
        let guard = seen.lock().unwrap();
        guard.clone()
    };
    let merged =
        captured
            .iter()
            .fold(CompactionDetails::default(), |mut acc, d| {
                acc.read_files.extend(d.read_files.iter().cloned());
                acc.modified_files.extend(d.modified_files.iter().cloned());
                acc
            });
    assert!(
        merged.read_files.contains(&PathBuf::from("a.rs")),
        "the `read` target must reach the manager; got {merged:?}"
    );
    assert!(
        merged.modified_files.contains(&PathBuf::from("b.rs")),
        "the `write` target must reach the manager; got {merged:?}"
    );
}

#[tokio::test]
async fn assistant_chunks_are_persisted_in_order_and_reassemble() {
    use synthia_session::{SessionEvent, assemble_chunks};

    let deltas = ["a", "b", "c", "d", "e"];
    let mut chunks: Vec<StreamChunk> = deltas
        .iter()
        .map(|d| {
            StreamChunk::Content(ContentPart::Text(TextContent {
                text: (*d).to_string(),
                cache_control: None,
            }))
        })
        .collect();
    chunks.push(StreamChunk::IsDone {
        result: Box::new(SamplingResult {
            text: String::new(),
            tool_calls: vec![],
            reasoning: String::new(),
            reasoning_signature: None,
            usage: TokenUsage::default(),
            stop_reason: Some("end_turn".to_string()),
        }),
    });

    let provider: Arc<dyn ModelProvider> =
        Arc::new(ScriptedStreamProvider::new(vec![chunks]));
    let (sink, mut rx) = synthia_session::TypedEventSink::channel(64);
    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
        .with_typed_event_sink(sink);

    let mut stream = agent
        .run(AgentInput::text("hi"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let mut events: Vec<SessionEvent> = Vec::new();
    while let Ok(Some(record)) = rx.try_recv() {
        events.push(record.event);
    }

    let chunk_events: Vec<&SessionEvent> = events
        .iter()
        .filter(|e| matches!(e, SessionEvent::AssistantChunk { .. }))
        .collect();
    assert_eq!(
        chunk_events.len(),
        deltas.len() + 1,
        "one assistant_chunk per streamed chunk (5 text + 1 terminal); got {chunk_events:?}"
    );
    match chunk_events.last().expect("terminal chunk") {
        SessionEvent::AssistantChunk {
            finish_reason,
            data,
            ..
        } => {
            assert_eq!(
                finish_reason.as_deref(),
                Some("end_turn"),
                "the terminal chunk must carry the provider stop reason"
            );
            assert_eq!(
                data.get("delta").and_then(|d| d.as_str()),
                Some(""),
                "the terminal chunk carries no delta"
            );
        }
        other => panic!("expected assistant_chunk, got {other:?}"),
    }
    assert_eq!(
        assemble_chunks(&events),
        "abcde",
        "the fold must rebuild exactly the streamed text"
    );
}

/// A `before_run` `Terminate` ends the run before the first sample — the
/// turn-stopping capability the previous vocabulary lacked.
#[tokio::test]
async fn terminate_before_run_ends_the_run_without_sampling() {
    use synthia_steering::{HookDecision, HookEvent, HookMap};

    let hooks = HookMap::new().on("before_run", move |_event: &HookEvent| {
        Box::pin(async move {
            HookDecision::Terminate {
                reason: "plan mode".to_string(),
            }
        })
    });
    let steering = Arc::new(synthia_steering::Steering {
        hooks,
        ..synthia_steering::Steering::noop()
    });

    let provider =
        Arc::new(CapturingProvider::new(vec![done_chunk("never reached")]));
    let events = run_and_collect_with_steering(
        provider.clone(),
        Arc::new(ToolRegistry::new()),
        steering,
        AgentInput::text("hi"),
    )
    .await;

    let ended: Vec<&SessionEndReason> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
                Some(reason)
            }
            _ => None,
        })
        .collect();
    assert_eq!(ended.len(), 1, "exactly one SessionEnded");
    assert!(
        matches!(ended[0], SessionEndReason::Error(m) if m == "plan mode"),
        "the terminate reason must reach the terminal event: {ended:?}"
    );
    assert_eq!(
        provider
            .call_count
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a before_run terminate must not sample"
    );
}

#[tokio::test]
async fn terminate_after_step_ends_the_run_after_commit() {
    use std::sync::Mutex as StdMutex;

    use synthia_steering::{HookDecision, HookEvent, HookMap};

    let seen: Arc<StdMutex<Vec<u32>>> = Arc::new(StdMutex::new(Vec::new()));
    let hook_seen = Arc::clone(&seen);
    let hooks = HookMap::new().on("after_step", move |event: &HookEvent| {
        let seen = Arc::clone(&hook_seen);
        let event = event.clone();
        Box::pin(async move {
            if let HookEvent::AfterStep { iteration, .. } = event {
                seen.lock().unwrap().push(iteration);
            }
            HookDecision::Terminate {
                reason: "policy-stop".to_string(),
            }
        })
    });
    let steering = Arc::new(synthia_steering::Steering {
        hooks,
        ..synthia_steering::Steering::noop()
    });

    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "c1".to_string(),
            name: "echo".to_string(),
            input: json!({ "text": "hi" }),
        }]),
        done_chunk("never reached"),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(synthia_tool::ToolEntry::new(Arc::new(
        FakeTool::new("echo", "echo ok"),
    )));
    let events = run_and_collect_with_steering(
        provider.clone(),
        registry,
        steering,
        AgentInput::text("hi"),
    )
    .await;

    let ended: Vec<&SessionEndReason> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
                Some(reason)
            }
            _ => None,
        })
        .collect();
    assert_eq!(ended.len(), 1, "exactly one SessionEnded: {events:?}");
    assert!(
        matches!(ended[0], SessionEndReason::Error(m) if m == "policy-stop"),
        "the after_step terminate reason must reach the terminal event"
    );
    assert_eq!(
        provider
            .call_count
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "terminate after step 0 must not sample again"
    );
    assert_eq!(
        *seen.lock().unwrap(),
        vec![0],
        "the hook must see iteration 0's payload once"
    );
}

#[tokio::test]
async fn after_step_no_hooks_run_completes_unchanged() {
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "c1".to_string(),
            name: "echo".to_string(),
            input: json!({ "text": "hi" }),
        }]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(synthia_tool::ToolEntry::new(Arc::new(
        FakeTool::new("echo", "echo ok"),
    )));
    let events = run_and_collect_with_steering(
        provider.clone(),
        registry,
        Arc::new(synthia_steering::Steering::noop()),
        AgentInput::text("hi"),
    )
    .await;

    let ended: Vec<&SessionEndReason> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
                Some(reason)
            }
            _ => None,
        })
        .collect();
    assert_eq!(ended.len(), 1);
    assert!(matches!(ended[0], SessionEndReason::Completed));
    assert_eq!(
        provider
            .call_count
            .load(std::sync::atomic::Ordering::SeqCst),
        2,
        "no hooks: the loop samples the tool turn and the final turn"
    );
}

/// Pins the `[blocked by policy]` denial wire string and that the model sees
/// the body on its next request.
#[tokio::test]
async fn block_at_before_tool_vetoes_and_reaches_the_next_request() {
    use synthia_steering::{HookDecision, HookEvent, HookMap};

    let hooks = HookMap::new().on("before_tool", move |event: &HookEvent| {
        Box::pin(async move {
            match event {
                HookEvent::BeforeTool { tool_name, .. }
                    if tool_name == "echo" =>
                {
                    HookDecision::Block {
                        reason: "echo is not allowed here".to_string(),
                    }
                }
                _ => HookDecision::Allow,
            }
        })
    });
    let steering = Arc::new(synthia_steering::Steering {
        hooks,
        ..synthia_steering::Steering::noop()
    });

    let tool_use = ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![tool_use]),
        done_chunk("blocked noted"),
    ]));
    let (registry, _executions) = echo_registry();

    let events = run_and_collect_with_steering(
        provider.clone(),
        registry,
        steering,
        AgentInput::text("run echo"),
    )
    .await;

    let committed = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(tr)) => Some(tr),
            _ => None,
        })
        .next()
        .expect("the vetoed call must still commit a tool result");
    assert_eq!(committed.is_error, Some(true));
    assert!(
        committed
            .content
            .iter()
            .any(|p| matches!(p, ContentPart::Text(t)
                if t.text.starts_with("[blocked by policy]"))),
        "the denial must carry the policy prefix: {:?}",
        committed.content
    );

    let captured = provider.captured.lock().await;
    let second = captured
        .get(1)
        .expect("the model must see the denial on the next request");
    let body = second
        .iter()
        .filter_map(|m| match &m.content {
            synthia_provider::Content::Single(ContentPart::Text(t)) => {
                Some(t.text.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        body.contains("[blocked by policy]"),
        "the next request must carry the denial body: {body}"
    );
}

/// The success path must carry the real call id — `after_tool` used
/// to hard-code an empty one, defeating observation correlation.
#[tokio::test]
async fn after_tool_success_carries_the_real_call_id() {
    use parking_lot::Mutex;
    use synthia_steering::{HookDecision, HookEvent, HookMap};

    let seen: Arc<Mutex<Vec<(String, String, bool)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);
    let hooks = HookMap::new().on("after_tool", move |event: &HookEvent| {
        let recorder = Arc::clone(&recorder);
        Box::pin(async move {
            if let HookEvent::AfterTool {
                tool_name,
                call_id,
                is_error,
            } = event
            {
                recorder.lock().push((
                    tool_name.clone(),
                    call_id.clone(),
                    *is_error,
                ));
            }
            HookDecision::Allow
        })
    });
    let steering = Arc::new(synthia_steering::Steering {
        hooks,
        ..synthia_steering::Steering::noop()
    });

    let tool_use = ToolUse {
        id: "call_echo_1".to_string(),
        name: "echo".to_string(),
        input: json!({"text": "hi"}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![tool_use]),
        done_chunk("done"),
    ]));
    let (registry, executions) = echo_registry();

    let _events = run_and_collect_with_steering(
        provider,
        registry,
        steering,
        AgentInput::text("run echo"),
    )
    .await;

    assert_eq!(*executions.lock(), 1, "the call must succeed");
    let captured = seen.lock();
    assert_eq!(
        captured.len(),
        1,
        "after_tool must fire exactly once for the batch"
    );
    assert_eq!(
        captured[0],
        ("echo".to_string(), "call_echo_1".to_string(), false),
        "the success-path AfterTool must carry the real call id"
    );
}
