use std::time::Instant;

use elysper_core::AtomicCancelToken;
use elysper_provider::{ReasoningContent, TokenUsage};

use super::*;

fn done(text: &str, reason: &str) -> StreamChunk {
    StreamChunk::IsDone {
        result: Box::new(SamplingResult {
            text: text.to_string(),
            stop_reason: Some(reason.to_string()),
            ..SamplingResult::default()
        }),
    }
}

type CollectedChunks = Arc<Mutex<Vec<StreamChunk>>>;

fn collector() -> (CollectedChunks, Box<dyn FnMut(StreamChunk) + Send>) {
    let seen: CollectedChunks = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    (seen, Box::new(move |chunk| sink.lock().push(chunk)))
}

async fn stream_once(
    provider: &ReplayProvider,
) -> Result<CompletionResponse, Error> {
    let (_, on_delta) = collector();
    provider
        .complete_with_stream(CompletionRequest::default(), None, on_delta)
        .await
}

fn response_text(response: &CompletionResponse) -> String {
    match &response.content {
        Content::Single(ContentPart::Text(t)) => t.text.clone(),
        Content::Multi(parts) => {
            let mut text = String::new();
            for part in parts {
                if let ContentPart::Text(t) = part {
                    text.push_str(&t.text);
                }
            }
            text
        }
        _ => String::new(),
    }
}

#[tokio::test]
async fn scripts_replay_in_order_and_advance_per_call() {
    let provider = ReplayProvider::new(vec![
        vec![text_chunk("ignored"), done("hello", "end_turn")],
        vec![text_chunk("bye"), done("bye", "stop")],
    ]);

    let (seen_a, on_a) = collector();
    let first = provider
        .complete_with_stream(CompletionRequest::default(), None, on_a)
        .await
        .unwrap();
    let delivered_a = seen_a.lock().clone();
    assert_eq!(delivered_a.len(), 2);
    assert!(
        matches!(
            &delivered_a[0],
            StreamChunk::Content(ContentPart::Text(t)) if t.text == "ignored"
        ),
        "first call must deliver its own script first"
    );
    assert!(matches!(&delivered_a[1], StreamChunk::IsDone { .. }));
    // `IsDone` is authoritative over the streamed text.
    assert_eq!(response_text(&first), "hello");
    assert_eq!(first.stop_reason.as_deref(), Some("end_turn"));
    assert_eq!(first.id, "replay-1");
    assert_eq!(first.model, "replay-model");

    let (seen_b, on_b) = collector();
    let second = provider
        .complete_with_stream(CompletionRequest::default(), None, on_b)
        .await
        .unwrap();
    assert_eq!(seen_b.lock().len(), 2);
    assert_eq!(response_text(&second), "bye");
    assert_eq!(second.id, "replay-2");

    provider.assert_consumed().unwrap();
    assert_eq!(provider.remaining_chunks(1), 0);
    assert_eq!(provider.remaining_chunks(2), 0);
}

#[tokio::test]
async fn call_beyond_recorded_scripts_is_an_error() {
    let provider = ReplayProvider::new(vec![vec![done("only", "stop")]]);
    stream_once(&provider).await.unwrap();
    let err = stream_once(&provider).await.unwrap_err();
    assert!(
        err.to_string().contains("exhausted"),
        "error should name the underrun: {err}"
    );
    // The one recorded script was still fully consumed.
    provider.assert_consumed().unwrap();
}

#[tokio::test]
async fn assert_consumed_reports_never_bound_scripts() {
    let provider = ReplayProvider::new(vec![
        vec![done("first", "stop")],
        vec![done("second", "stop")],
    ]);
    stream_once(&provider).await.unwrap();
    let err = provider.assert_consumed().unwrap_err();
    assert!(matches!(err, ReplayError::NotConsumed(_)), "got: {err}");
    let message = err.to_string();
    assert!(message.contains("never bound"), "message: {message}");
    assert!(message.contains('1'), "message: {message}");
}

#[tokio::test]
async fn assert_consumed_reports_cancelled_undrained_call() {
    let provider = ReplayProvider::new(vec![vec![
        text_chunk("a"),
        text_chunk("b"),
        done("ab", "stop"),
    ]]);
    let token = AtomicCancelToken::shared();
    token.cancel();
    let (_, on_delta) = collector();
    let err = provider
        .complete_with_stream(
            CompletionRequest::default(),
            Some(token),
            on_delta,
        )
        .await
        .unwrap_err();
    assert!(matches!(err, Error::StreamAborted { .. }), "got: {err}");

    let consumed = provider.assert_consumed().unwrap_err();
    let message = consumed.to_string();
    assert!(message.contains("undelivered"), "message: {message}");
    assert!(message.contains('3'), "message: {message}");
}

#[tokio::test]
async fn failure_diagnostics_accumulate_across_calls() {
    let provider = ReplayProvider::new(vec![
        vec![text_chunk("x"), text_chunk("y")],
        vec![done("never used", "stop")],
    ]);
    let token = AtomicCancelToken::shared();
    token.cancel();
    let (_, on_delta) = collector();
    let _ = provider
        .complete_with_stream(
            CompletionRequest::default(),
            Some(token),
            on_delta,
        )
        .await;
    let message = provider.assert_consumed().unwrap_err().to_string();
    assert!(message.contains("never bound"), "message: {message}");
    assert!(message.contains("undelivered"), "message: {message}");
}

#[tokio::test]
async fn non_streaming_complete_aggregates_and_drains_script() {
    let provider = ReplayProvider::new(vec![vec![
        StreamChunk::ToolCallStart {
            id: "call-1".to_string(),
            name: "bash".to_string(),
            arguments: Value::String(String::new()),
        },
        StreamChunk::ToolCallDelta {
            id: "call-1".to_string(),
            arguments_delta: "{\"cmd\":".to_string(),
        },
        StreamChunk::ToolCallDelta {
            id: "call-1".to_string(),
            arguments_delta: "\"ls\"}".to_string(),
        },
        StreamChunk::ToolCallEnd {
            id: "call-1".to_string(),
        },
        StreamChunk::Stop("tool_use".to_string()),
    ]]);
    let response = provider
        .complete(CompletionRequest::default())
        .await
        .unwrap();
    assert_eq!(response.stop_reason.as_deref(), Some("tool_use"));
    match &response.content {
        Content::Single(ContentPart::ToolUse(call)) => {
            assert_eq!(call.name, "bash");
            assert_eq!(call.input, serde_json::json!({"cmd": "ls"}));
        }
        other => panic!("expected a single tool-use part, got {other:?}"),
    }
    provider.assert_consumed().unwrap();
}

#[tokio::test]
async fn folded_text_and_usage_surface_without_isdone() {
    let provider = ReplayProvider::new(vec![vec![
        StreamChunk::Content(ContentPart::Reasoning(ReasoningContent {
            text: "thinking".to_string(),
            signature: Some("sig".to_string()),
        })),
        text_chunk("part one "),
        text_chunk("part two"),
        StreamChunk::Usage(TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 4,
            total_tokens: 14,
            ..TokenUsage::default()
        }),
    ]]);
    let response = provider
        .complete(CompletionRequest::default())
        .await
        .unwrap();
    assert_eq!(response_text(&response), "part one part two");
    assert_eq!(response.usage.total_tokens, 14);
    assert_eq!(response.stop_reason, None);
}

#[tokio::test]
async fn fixture_derives_one_script_per_model_call() {
    let fixture = concat!(
        r#"{"session_id":"s1","created_at":"2026-09-12T00:00:00Z"}"#,
        "\n",
        r#"{"type":"user_message","seq":1,"ts":"t","data":{"text":"hi"}}"#,
        "\n",
        r#"{"type":"assistant_chunk","seq":2,"ts":"t","chunk_seq":0,"data":{"delta":"Hel"}}"#,
        "\n",
        r#"{"type":"assistant_chunk","seq":3,"ts":"t","chunk_seq":1,"data":{"delta":"lo"}}"#,
        "\n",
        r#"{"type":"assistant_chunk","seq":4,"ts":"t","chunk_seq":2,"finish_reason":"end_turn","data":{"delta":""}}"#,
        "\n",
        r#"{"type":"tool_result","seq":5,"ts":"t","data":{"text":"ok"}}"#,
        "\n",
        r#"{"type":"assistant_chunk","seq":6,"ts":"t","chunk_seq":0,"data":{"delta":"again"}}"#,
        "\n",
        r#"{"type":"assistant_chunk","seq":7,"ts":"t","chunk_seq":1,"finish_reason":"end_turn","data":{"delta":""}}"#,
        "\n",
    );
    let provider = ReplayProvider::from_events_jsonl(fixture).unwrap();

    let first = stream_once(&provider).await.unwrap();
    assert_eq!(response_text(&first), "Hello");
    assert_eq!(first.stop_reason.as_deref(), Some("end_turn"));

    let second = stream_once(&provider).await.unwrap();
    assert_eq!(response_text(&second), "again");

    // A third call is an underrun: two calls were recorded.
    assert!(stream_once(&provider).await.is_err());
    provider.assert_consumed().unwrap();
}

#[test]
fn fixture_rejects_unterminated_call() {
    let fixture = concat!(
        r#"{"type":"assistant_chunk","seq":1,"ts":"t","chunk_seq":0,"data":{"delta":"cut"}}"#,
        "\n",
    );
    let err = ReplayProvider::from_events_jsonl(fixture).unwrap_err();
    assert!(matches!(err, ReplayError::Fixture(_)), "got: {err}");
    assert!(err.to_string().contains("finish_reason"), "got: {err}");
}

#[test]
fn fixture_rejects_malformed_line() {
    let fixture = concat!(
        r#"{"type":"assistant_chunk","seq":1,"ts":"t","chunk_seq":0,"data":{"delta":"x"}}"#,
        "\n",
        "not json\n",
    );
    let err = ReplayProvider::from_events_jsonl(fixture).unwrap_err();
    assert!(err.to_string().contains("line 2"), "got: {err}");
}

#[test]
fn fixture_without_model_calls_yields_no_scripts() {
    let fixture = "\n{\"session_id\":\"s1\"}\n{\"type\":\"tool_result\",\"seq\":1,\"ts\":\"t\",\"data\":{}}\n";
    let provider = ReplayProvider::from_events_jsonl(fixture).unwrap();
    assert!(!provider.has_scripts());
    provider.assert_consumed().unwrap();
}

#[tokio::test]
async fn pacing_delays_each_delivered_chunk() {
    let provider = ReplayProvider::new(vec![vec![
        text_chunk("a"),
        text_chunk("b"),
        done("ab", "stop"),
    ]])
    .with_pacing(Duration::from_millis(20));

    let start = Instant::now();
    stream_once(&provider).await.unwrap();
    let elapsed = start.elapsed();
    assert!(
        elapsed >= Duration::from_millis(60),
        "three paced chunks must take at least 3 x 20ms, took {elapsed:?}"
    );
    provider.assert_consumed().unwrap();
}

/// `ReplayProvider::embed` falls through to the trait's default
/// implementation (empty vectors), since replay exists to
/// reproduce scripted `complete` calls — embeddings have no
/// replay script. This pins the contract so a future override
/// that hardcodes an error is a deliberate decision, not
/// accidental.
#[tokio::test]
async fn embed_returns_empty_vectors() {
    let provider = ReplayProvider::new(vec![]);
    let vectors = provider
        .embed(vec!["x".to_string(), "y".to_string()])
        .await
        .unwrap();
    assert_eq!(vectors.len(), 2);
    assert!(vectors.iter().all(|v| v.is_empty()));
}

#[tokio::test]
async fn provider_metadata_reflects_replay_identity() {
    let provider = ReplayProvider::new(vec![vec![done("x", "stop")]]);
    assert_eq!(provider.name(), "replay");
    assert!(provider.has_scripts());
    assert!(!ReplayProvider::new(vec![]).has_scripts());
    let config = provider.model_config();
    assert_eq!(config.name, "replay-model");
    assert!(config.supports_streaming);
    assert!(config.supports_tools);
}
#[test]
fn delivery_counter_tracks_each_chunk() {
    let provider =
        ReplayProvider::new(vec![vec![text_chunk("a"), text_chunk("b")]]);
    let (call_no, _) = provider.bind_next_script().unwrap();
    assert_eq!(call_no, 1);
    assert_eq!(provider.remaining_chunks(1), 2);
    provider.note_delivered(1);
    assert_eq!(provider.remaining_chunks(1), 1);
    provider.mark_all_delivered(1);
    assert_eq!(provider.remaining_chunks(1), 0);
    provider.assert_consumed().unwrap();
}

#[test]
fn call_ordinals_increment_across_binds() {
    let provider = ReplayProvider::new(vec![
        vec![done("a", "stop")],
        vec![done("b", "stop")],
    ]);
    let (first, _) = provider.bind_next_script().unwrap();
    let (second, _) = provider.bind_next_script().unwrap();
    assert_eq!(first, 1);
    assert_eq!(second, 2);
}
