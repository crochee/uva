//! A writer-backed run reads its transcript from the session log
//! instead of `input.history` + `input.content`: setup derives it,
//! a conflicting `input.history` fails closed, and the step boundary
//! re-derives it when the log's tail produced a new transcript.

use std::sync::Arc;

use futures::StreamExt;
use tokio_util::sync::CancellationToken;
use uva_core::SystemClock;
use uva_provider::{Content, ContentPart, ImageContent, Role};
use uva_session::{
    LogWriter,
    assistant_message,
    in_memory::InMemorySessionSink,
    user_message,
};
use uva_tool::{Context, StreamOutput, Tool, ToolOutput, ToolRegistry};

use super::super::{support::*, *};

// Seed the user turn the host persisted before the run.
//
// The row goes through the typed constructor: an outside caller names
// the event, and the crate owns the row's shape.
async fn seed_user_turn(writer: &LogWriter, text: &str) {
    let row = user_message(
        serde_json::to_value(Message::user(text))
            .expect("a user message serialises"),
        None,
    );
    writer.append_event(&row).await.expect("seed the user turn");
}

// A recording provider that answers once, plus a writer whose log
// already holds the user turn the host persisted before the run — the
// shape every writer-backed deployment has.
async fn logged_turn(text: &str) -> (Arc<CapturingProvider>, Arc<LogWriter>) {
    let sink = Arc::new(InMemorySessionSink::new("log"));
    let writer = Arc::new(LogWriter::new(sink, Arc::new(SystemClock)));
    seed_user_turn(&writer, text).await;
    writer.flush().await.expect("flush the seeded row");
    (
        Arc::new(CapturingProvider::new(vec![done_chunk("ok")])),
        writer,
    )
}

async fn run_with_writer(
    provider: Arc<CapturingProvider>,
    writer: Arc<LogWriter>,
    input: AgentInput,
) -> Vec<AgentEvent> {
    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
        .with_log_writer(writer);
    let mut stream = agent.run(input, Arc::new(CancellationToken::new())).await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    events
}

// The user turns the model saw. The loop appends the runtime-context
// snapshot as a user-role message too; it is not a user turn.
fn user_turns(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message.role == Role::User)
        .filter_map(|message| message.content.extract_text())
        .filter(|text| !text.starts_with("Current runtime context"))
        .collect()
}

fn user_part(text: &str) -> Vec<ContentPart> {
    vec![ContentPart::Text(TextContent {
        text: text.to_string(),
        cache_control: None,
    })]
}

fn image_part() -> ContentPart {
    ContentPart::Image(ImageContent {
        data: "aGVsbG8=".to_string(),
        mime_type: "image/png".to_string(),
        detail: None,
    })
}

// Every image part across every message of one request.
fn image_count(messages: &[Message]) -> usize {
    messages
        .iter()
        .flat_map(|message| match &message.content {
            Content::Single(part) => vec![part],
            Content::Multi(parts) => parts.iter().collect(),
        })
        .filter(|part| matches!(part, ContentPart::Image(_)))
        .count()
}

/// A writer-backed run takes its transcript from the log. A host that
/// mirrors the log into `input.history` must not have it appended a
/// second time.
pub(super) async fn a_writer_backed_run_ignores_input_history() {
    let (provider, writer) = logged_turn("from-log").await;
    let input = AgentInput::multi_with_history(
        vec![Message::user("from-log")],
        user_part("next"),
    );
    let events = run_with_writer(Arc::clone(&provider), writer, input).await;

    let captured = provider.captured.lock().await;
    let first = captured.first().expect("the run must sample once");
    assert_eq!(
        user_turns(first),
        vec!["from-log".to_string()],
        "the log-derived history is the transcript; input.history must not be replayed: {:?}",
        events.iter().map(AgentEvent::kind).collect::<Vec<_>>()
    );
}

/// The log is the single source of truth: an `input.history` the log
/// does not support ends the run instead of being silently trusted.
pub(super) async fn conflicting_input_history_ends_the_run() {
    let (provider, writer) = logged_turn("from-log").await;
    let input = AgentInput::multi_with_history(
        vec![Message::user("from-input")],
        user_part("next"),
    );
    let events = run_with_writer(provider, writer, input).await;

    let ended_with_error = events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Error(message),
            }) if message.contains("history")
        )
    });
    assert!(
        ended_with_error,
        "a history the log does not support must fail closed; got {:?}",
        events.iter().map(AgentEvent::kind).collect::<Vec<_>>()
    );
}

/// The host already logged the user turn; the loop must not append
/// `input.content` on top of it.
pub(super) async fn a_writer_backed_run_does_not_duplicate_the_input() {
    let (provider, writer) = logged_turn("from-log").await;
    let input = AgentInput::text("from-input");
    run_with_writer(Arc::clone(&provider), writer, input).await;

    let captured = provider.captured.lock().await;
    let first = captured.first().expect("the run must sample once");
    assert_eq!(
        user_turns(first),
        vec!["from-log".to_string()],
        "the logged user turn must appear exactly once, with no input.content copy"
    );
}

/// A writer-backed run reads its own turns back from the log: the
/// step boundary derives the assistant turn and the tool result the
/// run just wrote, so the second request carries each exactly once.
pub(super) async fn a_writer_backed_run_reads_its_turns_back_from_the_log() {
    use serde_json::json;
    use uva_provider::ToolUse;

    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "call_1".to_string(),
            name: "echo".to_string(),
            input: json!({}),
        }]),
        done_chunk("done"),
    ]));
    let sink = Arc::new(InMemorySessionSink::new("log"));
    let writer = Arc::new(LogWriter::new(sink, Arc::new(SystemClock)));
    seed_user_turn(&writer, "go").await;
    writer.flush().await.expect("flush the seeded row");

    let (registry, _calls) = echo_registry();
    let agent = ReActAgent::new(
        Arc::clone(&provider) as Arc<dyn uva_provider::ModelProvider>,
        registry,
    )
    .with_log_writer(writer);
    let mut stream = agent
        .run(
            AgentInput::new(vec![
                ContentPart::Text(TextContent {
                    text: "go".to_string(),
                    cache_control: None,
                }),
                image_part(),
            ]),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while stream.next().await.is_some() {}

    let captured = provider.captured.lock().await;
    assert_eq!(
        captured.len(),
        2,
        "the scripted tool call must be followed by a second sample"
    );
    let second = &captured[1];
    assert_eq!(
        user_turns(second),
        vec!["go".to_string()],
        "the logged user turn appears once"
    );
    assert_eq!(
        image_count(&captured[0]),
        1,
        "the attachment rides the first request"
    );
    assert_eq!(
        image_count(second),
        1,
        "the rebuild after the step boundary keeps the attachment"
    );
    let count = |role: Role| {
        second.iter().filter(|message| message.role == role).count()
    };
    assert_eq!(count(Role::Assistant), 1, "assistant turn exactly once");
    assert_eq!(count(Role::Tool), 1, "tool result exactly once");
}

/// A host that passes the prior transcript is accepted even though
/// the log is one message longer: the current turn's user row is
/// already logged, so the contract is a prefix, not equality.
pub(super) async fn a_host_history_that_prefixes_the_log_is_accepted() {
    let sink = Arc::new(InMemorySessionSink::new("log"));
    let writer = Arc::new(LogWriter::new(sink, Arc::new(SystemClock)));
    let provider = Arc::new(CapturingProvider::new(vec![
        done_chunk("first answer"),
        done_chunk("second answer"),
    ]));
    seed_user_turn(&writer, "first").await;
    writer.flush().await.expect("flush the seeded row");

    // First run: completes the first turn and writes its answer.
    let first = run_with_writer(
        Arc::clone(&provider),
        Arc::clone(&writer),
        AgentInput::text("first"),
    )
    .await;
    assert!(
        first.iter().any(|event| matches!(
            event,
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Completed,
            })
        )),
        "the seeded turn must complete: {:?}",
        first.iter().map(AgentEvent::kind).collect::<Vec<_>>()
    );

    // The host now reads the transcript back and hands it to run 2,
    // while the log already holds run 2's user row.
    let prior =
        uva_session::derive_history(&writer.read().await.expect("read"))
            .expect("derive");
    seed_user_turn(&writer, "second").await;
    writer.flush().await.expect("flush the second row");
    let events = run_with_writer(
        Arc::clone(&provider),
        writer,
        AgentInput::history(prior, "second"),
    )
    .await;
    assert!(
        !events.iter().any(|event| matches!(
            event,
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Error(_),
            })
        )),
        "a prefixing history must not fail the run: {:?}",
        events.iter().map(AgentEvent::kind).collect::<Vec<_>>()
    );

    let captured = provider.captured.lock().await;
    let second = captured.last().expect("the second run must sample");
    assert_eq!(
        user_turns(second),
        vec!["first".to_string(), "second".to_string()],
        "the second request replays the log, including the new turn"
    );
}

/// The log holds a turn's text but never its bytes. A writer-backed
/// multimodal run must still send the image to the model exactly once
/// — and say, on the event stream, that the attachment is not durable.
pub(super) async fn a_writer_backed_run_sends_attachments_once_and_warns() {
    let (provider, writer) = logged_turn("look at this").await;
    let input = AgentInput::new(vec![
        ContentPart::Text(TextContent {
            text: "look at this".to_string(),
            cache_control: None,
        }),
        image_part(),
    ]);
    let events =
        run_with_writer(Arc::clone(&provider), Arc::clone(&writer), input)
            .await;

    let captured = provider.captured.lock().await;
    let first = captured.first().expect("the run must sample once");
    assert_eq!(
        user_turns(first),
        vec!["look at this".to_string()],
        "the logged text appears once, with no input.content copy"
    );
    assert_eq!(
        image_count(first),
        1,
        "the image must reach the model exactly once"
    );

    let warned = events.iter().any(|event| {
        matches!(
            event,
            AgentEvent::System(SystemEvent::Warning {
                kind: WarningKind::Attachments,
                message,
                ..
            }) if message.contains("not persisted in the session log")
        )
    });
    assert!(
        warned,
        "a non-durable attachment must be reported: {:?}",
        events.iter().map(AgentEvent::kind).collect::<Vec<_>>()
    );

    // The warning is not cosmetic: the bytes really are absent from
    // the durable log, so a resume cannot replay them.
    let rows = writer.read().await.expect("read the session log");
    assert!(
        !rows.iter().any(|row| row.to_string().contains("aGVsbG8=")),
        "attachment bytes must not be written to the log"
    );
}

/// A prune is request-local — the log still holds the full transcript,
/// so every step re-derives and re-prunes — but it must be reported
/// once per run, and every request must carry the same pruned head.
pub(super) async fn a_writer_backed_prune_warns_once_per_run() {
    use serde_json::json;
    use uva_provider::ToolUse;

    let sink = Arc::new(InMemorySessionSink::new("log"));
    let writer = Arc::new(LogWriter::new(sink, Arc::new(SystemClock)));
    for i in 0..6 {
        for row in [
            user_message(
                serde_json::to_value(Message::user(format!(
                    "{}{i}",
                    "q".repeat(399)
                )))
                .expect("a user message serialises"),
                None,
            ),
            assistant_message(
                serde_json::to_value(Message::assistant(format!(
                    "{}{i}",
                    "a".repeat(399)
                )))
                .expect("an assistant message serialises"),
            ),
        ] {
            writer.append_event(&row).await.expect("seed the log");
        }
    }
    seed_user_turn(&writer, "go").await;
    writer.flush().await.expect("flush the seeded rows");

    let provider = Arc::new(
        CapturingProvider::new(vec![
            tool_call_response(vec![ToolUse {
                id: "call_1".to_string(),
                name: "echo".to_string(),
                input: json!({}),
            }]),
            done_chunk("done"),
        ])
        .with_context_window(300),
    );
    let (registry, _calls) = echo_registry();
    let agent = ReActAgent::new(
        Arc::clone(&provider) as Arc<dyn uva_provider::ModelProvider>,
        registry,
    )
    .with_log_writer(writer);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut compaction_warnings = 0;
    while let Some(event) = stream.next().await {
        if matches!(
            event,
            AgentEvent::System(SystemEvent::Warning {
                kind: WarningKind::ContextCompaction,
                ..
            })
        ) {
            compaction_warnings += 1;
        }
    }
    assert_eq!(
        compaction_warnings, 1,
        "the prune is reported once per run, not once per LLM pass"
    );

    let captured = provider.captured.lock().await;
    assert_eq!(captured.len(), 2, "one tool iteration, then the answer");
    let evicted = format!("{}0", "q".repeat(399));
    let first_turns = user_turns(&captured[0]);
    assert!(
        !first_turns.contains(&evicted),
        "the prune must evict the oldest turn: {first_turns:?}"
    );
    for request in captured.iter() {
        let turns = user_turns(request);
        assert!(
            !turns.contains(&evicted),
            "the evicted head must stay evicted: {turns:?}"
        );
        assert!(
            turns.contains(&"go".to_string()),
            "the current turn stays: {turns:?}"
        );
    }
    assert_eq!(
        user_turns(&captured[1]),
        first_turns,
        "re-deriving and re-pruning must send the same history every pass"
    );
}

/// A streamed chunk is durable, not only live.
///
/// The dispatch writes the call's streamed chunks as `tool_output`
/// rows between the call's `tool_dispatch` row and its terminal
/// `tool_result` row, so a reload replays what the running client
/// saw. Chunks that arrive before a flush merge into one row — the
/// writer holds one pending row per call — which is why the two
/// chunks here are one row carrying both. The row stays log-only:
/// the model reads the settled result, not the preview.
pub(super) async fn streamed_chunks_land_as_tool_output_rows() {
    use serde_json::json;
    use uva_provider::ToolUse;

    struct Streamer;

    #[async_trait::async_trait]
    impl Tool for Streamer {
        fn name(&self) -> &str {
            "streamer"
        }

        fn description(&self) -> &str {
            "yields two chunks"
        }

        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }

        async fn call(
            &self,
            _input: serde_json::Value,
            _ctx: &Context,
        ) -> ToolOutput {
            ToolOutput::text("first second")
        }

        fn stream<'a>(
            &'a self,
            _input: serde_json::Value,
            _ctx: &'a Context,
        ) -> std::pin::Pin<
            Box<dyn futures::Stream<Item = StreamOutput> + Send + 'a>,
        > {
            Box::pin(futures::stream::iter(vec![
                StreamOutput::Progress(ToolOutput::text("first ")),
                StreamOutput::Progress(ToolOutput::text("second")),
                StreamOutput::Result(ToolOutput::text("first second")),
            ]))
        }
    }

    let sink = Arc::new(InMemorySessionSink::new("log"));
    let writer = Arc::new(LogWriter::new(sink, Arc::new(SystemClock)));
    seed_user_turn(&writer, "stream").await;
    writer.flush().await.expect("flush the seeded row");

    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "call_stream".to_string(),
            name: "streamer".to_string(),
            input: json!({}),
        }]),
        done_chunk("done"),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(Streamer)));
    let log = Arc::clone(&writer);
    let agent = ReActAgent::new(
        Arc::clone(&provider) as Arc<dyn uva_provider::ModelProvider>,
        registry,
    )
    .with_log_writer(log);
    let mut stream = agent
        .run(
            AgentInput::text("stream"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while stream.next().await.is_some() {}
    writer.flush().await.expect("flush the run's rows");

    let rows = writer.read().await.expect("read the session log");
    let types: Vec<&str> =
        rows.iter().filter_map(|row| row["type"].as_str()).collect();
    let chunks: Vec<&serde_json::Value> = rows
        .iter()
        .filter(|row| row["type"] == "tool_output")
        .collect();
    assert_eq!(
        chunks.len(),
        1,
        "chunks that arrive before a flush are one row: {types:?}"
    );
    assert_eq!(chunks[0]["data"]["output"], "first second");
    assert_eq!(chunks[0]["call_id"], "call_stream");
    assert_eq!(chunks[0]["tool"], "streamer");
    assert!(
        chunks[0]["data"]["original_bytes"].is_null(),
        "a row that fits its window records no cut: {:?}",
        chunks[0]["data"]
    );
    let dispatch = types
        .iter()
        .position(|kind| *kind == "tool_dispatch")
        .expect("the call's dispatch row");
    let first = types
        .iter()
        .position(|kind| *kind == "tool_output")
        .expect("the chunk's row");
    let result = types
        .iter()
        .position(|kind| *kind == "tool_result")
        .expect("the settled result row");
    assert!(
        dispatch < first && first < result,
        "the chunks sit between dispatch and result: {types:?}"
    );
}
