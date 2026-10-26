//! Typed-event projection over a [`SessionSink`].
//!
//! The legacy `events_to_messages` projection in `session.rs`
//! handles the opaque `serde_json::Value` envelopes the agent
//! loop has emitted since v0.1 (the `{"type": "Message",
//! "data": <Message>}` envelope, plus the `{"type":
//! "UserInput"/"Model"}` shapes from the server controller).
//! This module sits on top of the typed-event layer introduced
//! in [`synthia-session::SessionEvent`]: it parses the durable
//! log into typed events, runs [`synthia_session::fold_surface`],
//! and projects the result back into `synthia_provider::Message`
//! for the conversation tier of [`Memory`].
//!
//! ## Behaviour
//!
//! - Each typed surface event becomes one [`Message`] in the
//!   order produced by `fold_surface`. `UserMessage` events map
//!   to `Role::User`, `AssistantMessage` to `Role::Assistant`,
//!   `ToolResult` to a synthetic assistant tool-result message
//!   using the `tool_use_id` field, and `Compaction` to a single
//!   assistant message carrying the summary text.
//! - Legacy envelopes (anything `SessionEvent::from_value` rejects)
//!   fall back to the existing `events_to_messages` projection
//!   so old JSONL logs keep replaying without a migration step.
//! - Replacement events are honored: when `fold_surface` produces
//!   the folded surface, edits by the `SummarizingContextManager`
//!   replace (not duplicate) earlier tool results on resume.
//!
//! [`Memory`]: crate::memory::Memory

use std::sync::Arc;

use serde_json::Value;
use synthia_provider::Message;
use synthia_session::{
    FoldedSurface,
    LogWriter,
    SessionEvent,
    SessionSink,
    surface_to_messages,
    try_fold_log_surface,
};
use thiserror::Error;

/// Errors the typed projection can surface.
#[derive(Debug, Error)]
pub enum TypedProjectionError {
    /// Sink I/O failure.
    #[error("sink error: {0}")]
    Sink(String),
    /// Fold failure (provenance violation or out-of-bounds
    /// replace range).
    #[error("fold error: {0}")]
    Fold(String),
}

/// Read a sink and project its events into `Vec<Message>` using
/// the typed-event layer.
///
/// Strategy:
///
/// 1. Read the full sink log.
/// 2. Parse every row into [`SessionEvent`] where the typed
///    shape matches (`SessionEvent::from_value`). Rows that
///    fail to parse (legacy `{"type": "Message"}` envelopes,
///    pre-R4 `UserInput`/`Model` shapes) fall through to
///    [`events_to_messages`](crate::memory::events_to_messages).
/// 3. Run [`fold_surface`](synthia_session::fold_surface) on the
///    typed events to get the ordered surface.
/// 4. Project each folded entry to a [`Message`].
/// 5. Concatenate the typed projection with the legacy
///    projection's output so a session mixing old and new
///    events replays correctly.
///
/// The `tail` parameter caps the projection to the last `n`
/// typed events (passed-through to the legacy path too). Pass
/// `None` for unbounded projection.
pub async fn typed_messages_from_sink(
    sink: Arc<dyn SessionSink>,
    tail: Option<usize>,
) -> Result<Vec<Message>, TypedProjectionError> {
    let events = sink
        .read()
        .await
        .map_err(|e| TypedProjectionError::Sink(e.to_string()))?;
    project(&events, tail)
}

/// Pure projection: `events` (the raw sink log) → `Vec<Message>`.
pub fn project(
    events: &[Value],
    tail: Option<usize>,
) -> Result<Vec<Message>, TypedProjectionError> {
    // One fold for the whole log — typed rows and the legacy
    // envelopes the controller wrote before the typed layer
    // existed — with every row stamped by its 1-based log ordinal
    // and compaction replacements applied. This is the same fold
    // `events_to_messages` uses; the only difference is that a
    // replacement whose provenance does not resolve is surfaced
    // here instead of being skipped.
    let folded = try_fold_log_surface(events)
        .map_err(|e| TypedProjectionError::Fold(format!("{e:?}")))?;
    Ok(project_folded_surface(&folded, tail))
}

/// Project a [`FoldedSurface`] into `Vec<Message>`.
///
/// Honours `tail`: the last `n` folded messages are returned in
/// order; earlier ones are dropped (with `tail == None`, the
/// whole surface is returned).
fn project_folded_surface(
    surface: &FoldedSurface,
    tail: Option<usize>,
) -> Vec<Message> {
    let start = match tail {
        Some(n) if surface.messages.len() > n => surface.messages.len() - n,
        _ => 0,
    };
    // Applied AFTER the tail slice, and shared with `events_to_messages`
    // (see `synthia_session::surface_to_messages`) so the two projections
    // cannot disagree about a turn's wire shape. A tail that lands
    // mid-turn therefore keeps whatever parts survived the slice rather
    // than reaching back past the caller's bound: `tail` exists to cap
    // the projection, so widening it to complete a turn would defeat its
    // purpose.
    surface_to_messages(&surface.messages[start..])
}

/// Repair an interrupted session by reading the session's log,
/// appending synthetic closer events for any open tail turn, and
/// writing them back through the session's writer.
///
/// The closers carry no ordinal of their own: the writer hands each
/// one out and stamps the row with it — the same rule every other
/// appender follows. Repair *adds* rows; it never truncates the log.
///
/// Returns the number of synthetic events appended. A zero
/// return means the log was already balanced.
pub async fn repair_session(
    writer: Arc<LogWriter>,
) -> Result<usize, TypedProjectionError> {
    let events = writer
        .read()
        .await
        .map_err(|e| TypedProjectionError::Sink(e.to_string()))?;
    let typed: Vec<SessionEvent> =
        events.iter().filter_map(SessionEvent::from_value).collect();
    let closers = synthia_session::interrupted_turn_closers(&typed);
    let count = closers.len();
    if count == 0 {
        return Ok(0);
    }
    writer
        .repair(closers)
        .await
        .map_err(|e| TypedProjectionError::Sink(e.to_string()))?;
    Ok(count)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

    use async_trait::async_trait;
    use serde_json::json;
    use synthia_core::SystemClock;
    use synthia_provider::Role;
    use synthia_session::{
        LogWriter,
        SessionEndReason,
        SessionError,
        SessionSnapshot,
        in_memory::InMemorySessionSink,
    };

    use super::*;

    fn sink() -> Arc<InMemorySessionSink> {
        Arc::new(InMemorySessionSink::new("s1"))
    }

    /// The session's one writer, over `sink`.
    fn writer_over(sink: &Arc<InMemorySessionSink>) -> Arc<LogWriter> {
        Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)))
    }

    /// Write one row through the writer and make it durable.
    async fn write(writer: &Arc<LogWriter>, row: Value) {
        writer.append(row).await.expect("append");
        writer.flush().await.expect("flush");
    }

    /// A sink that counts the writes it receives, so a test can see
    /// whether a batch of rows landed in one call or in several
    /// per-row appends.
    #[derive(Default)]
    struct CountingSink {
        rows: parking_lot::Mutex<Vec<Value>>,
        appends: AtomicUsize,
        batches: AtomicUsize,
        next_seq: AtomicU64,
    }

    impl CountingSink {
        fn new() -> Self {
            Self::default()
        }

        /// How many times a single row was appended.
        fn append_calls(&self) -> usize {
            self.appends.load(Ordering::SeqCst)
        }

        /// How many times a batch was appended.
        fn append_many_calls(&self) -> usize {
            self.batches.load(Ordering::SeqCst)
        }

        /// Forget the counts, keeping the rows.
        fn reset_counts(&self) {
            self.appends.store(0, Ordering::SeqCst);
            self.batches.store(0, Ordering::SeqCst);
        }
    }

    #[async_trait]
    impl SessionSink for CountingSink {
        fn id(&self) -> &str {
            "counting"
        }

        fn alloc_seq(&self) -> u64 {
            self.next_seq.fetch_add(1, Ordering::SeqCst) + 1
        }

        async fn append(&self, event: &Value) -> Result<(), SessionError> {
            self.appends.fetch_add(1, Ordering::SeqCst);
            self.rows.lock().push(event.clone());
            Ok(())
        }

        async fn append_many(
            &self,
            events: &[Value],
        ) -> Result<(), SessionError> {
            self.batches.fetch_add(1, Ordering::SeqCst);
            self.rows.lock().extend(events.iter().cloned());
            Ok(())
        }

        async fn read(&self) -> Result<Vec<Value>, SessionError> {
            Ok(self.rows.lock().clone())
        }

        async fn read_from(
            &self,
            from: u64,
        ) -> Result<Vec<Value>, SessionError> {
            let rows = self.rows.lock();
            let skip = usize::try_from(from).unwrap_or(usize::MAX);
            Ok(rows.iter().skip(skip).cloned().collect())
        }

        async fn snapshot(&self) -> Result<SessionSnapshot, SessionError> {
            let last = self.next_seq.load(Ordering::SeqCst);
            Ok(SessionSnapshot {
                session_id: "counting".to_string(),
                last_event_seq: last,
                last_stream_index: last,
                bytes_on_disk: 0,
            })
        }

        async fn close(
            &self,
            _reason: SessionEndReason,
        ) -> Result<(), SessionError> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn empty_sink_projects_to_empty_messages() {
        let msgs = typed_messages_from_sink(sink(), None).await.unwrap();
        assert!(msgs.is_empty());
    }

    #[tokio::test]
    async fn typed_user_message_projects_as_user_role() {
        let s = sink();
        let writer = writer_over(&s);
        let user = Message::user("hi");
        write(
            &writer,
            json!({
                "type": "user_message",
                "data": serde_json::to_value(&user).unwrap(),
                "surface_op": "append"
            }),
        )
        .await;
        let msgs = typed_messages_from_sink(s, None).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
    }

    #[tokio::test]
    async fn legacy_envelopes_fall_through_to_legacy_projection() {
        let s = sink();
        let writer = writer_over(&s);
        // `{"type": "UserInput", "data": {"text": "hi"}}` — the
        // pre-R4 envelope the server controller emitted.
        write(
            &writer,
            json!({
                "type": "UserInput",
                "data": {"text": "hi"}
            }),
        )
        .await;
        let msgs = typed_messages_from_sink(s, None).await.unwrap();
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].role, Role::User);
    }

    #[tokio::test]
    async fn mixed_typed_and_legacy_events_compose() {
        let s = sink();
        let writer = writer_over(&s);
        // Legacy first (the agent loop emitted this before R4).
        write(
            &writer,
            json!({
                "type": "UserInput",
                "data": {"text": "old"}
            }),
        )
        .await;
        // Typed second (the agent loop after R4). The data
        // payload is a real `Message` serde value.
        let assistant = Message::assistant("new");
        write(
            &writer,
            json!({
                "type": "assistant_message",
                "data": serde_json::to_value(&assistant).unwrap(),
                "surface_op": "append"
            }),
        )
        .await;
        let msgs = typed_messages_from_sink(s, None).await.unwrap();
        assert_eq!(msgs.len(), 2);
        // Order: legacy first, typed second (preserved by
        // indexed walk).
        assert_eq!(msgs[0].role, Role::User);
        assert_eq!(msgs[1].role, Role::Assistant);
    }

    #[tokio::test]
    async fn tail_cap_drops_oldest_typed_messages() {
        let s = sink();
        let writer = writer_over(&s);
        for i in 0..5 {
            let user = Message::user(format!("m{i}"));
            write(
                &writer,
                json!({
                    "type": "user_message",
                    "data": serde_json::to_value(&user).unwrap(),
                    "surface_op": "append"
                }),
            )
            .await;
        }
        let msgs = typed_messages_from_sink(s, Some(2)).await.unwrap();
        assert_eq!(msgs.len(), 2);
    }

    #[tokio::test]
    async fn repair_session_writes_closers_for_interrupted_log() {
        let s = sink();
        let writer = writer_over(&s);
        // Seed an interrupted log: turn/start + step/start +
        // assistant-with-tool-use + tool_call + nothing else.
        write(
            &writer,
            json!({
                "type": "turn",
                "data": {"kind": "start", "turn": 1}
            }),
        )
        .await;
        write(
            &writer,
            json!({
                "type": "step",
                "data": {"kind": "start", "turn": 1, "step": 1}
            }),
        )
        .await;
        write(
            &writer,
            json!({
                "type": "assistant_message",
                "data": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "calling"}],
                    "tool_calls": [{"id": "c1", "name": "read"}]
                },
                "surface_op": "append"
            }),
        )
        .await;
        write(
            &writer,
            json!({
                "type": "tool_call",
                "data": {"call_id": "c1", "name": "read", "arguments": {}}
            }),
        )
        .await;
        // The log's highest ordinal so far: the closers must carry
        // higher ones — the repair continues the log's numbering
        // instead of starting a second one.
        let max_seq = writer
            .read()
            .await
            .unwrap()
            .iter()
            .filter_map(|row| row["seq"].as_u64())
            .max()
            .expect("the seeded rows carry their ordinals");
        let count = repair_session(Arc::clone(&writer)).await.unwrap();
        assert!(
            count >= 2,
            "expected at least tool_result + step/end + turn/end, got {count}"
        );
        // Re-reading the log should now see the closers.
        let events = s.read().await.unwrap();
        assert!(
            events.iter().any(|v| v["type"] == "tool_result"
                && v["data"]["interrupted"] == true)
        );
        assert!(
            events
                .iter()
                .any(|v| v["type"] == "turn" && v["data"]["kind"] == "end")
        );
        // The closers carry the ordinals the writer handed out: they
        // continue the log's numbering instead of starting a second
        // one.
        for closer in events.iter().skip(4) {
            let seq = closer["seq"]
                .as_u64()
                .expect("a closer carries its ordinal");
            assert!(
                seq > max_seq,
                "the closers continue the numbering: {closer}"
            );
        }
    }

    /// The repair's closers are the session's writer's rows: they
    /// land in the writer's batch, not in one append per closer.
    #[tokio::test]
    async fn the_repair_writes_its_closers_in_one_batch() {
        let sink = Arc::new(CountingSink::new());
        let writer =
            Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
        // An interrupted log: turn/start + step/start +
        // assistant-with-tool-use + tool_call + nothing else.
        for row in [
            json!({"type": "turn", "data": {"kind": "start", "turn": 1}}),
            json!({
                "type": "step",
                "data": {"kind": "start", "turn": 1, "step": 1},
            }),
            json!({
                "type": "assistant_message",
                "data": {
                    "role": "assistant",
                    "content": [{"type": "text", "text": "calling"}],
                    "tool_calls": [{"id": "c1", "name": "read"}],
                },
                "surface_op": "append",
            }),
            json!({
                "type": "tool_call",
                "data": {"call_id": "c1", "name": "read", "arguments": {}},
            }),
        ] {
            writer.append(row).await.unwrap();
        }
        writer.flush().await.unwrap();
        sink.reset_counts();

        let count = repair_session(Arc::clone(&writer)).await.unwrap();
        assert!(count >= 2, "expected the closers, got {count}");
        assert_eq!(sink.append_calls(), 0, "no closer is appended on its own");
        assert_eq!(
            sink.append_many_calls(),
            1,
            "the closers land in the writer's one batch"
        );
    }

    #[tokio::test]
    async fn repair_session_no_op_for_balanced_log() {
        let s = sink();
        let writer = writer_over(&s);
        write(
            &writer,
            json!({
                "type": "turn",
                "data": {"kind": "end", "turn": 1}
            }),
        )
        .await;
        let count = repair_session(Arc::clone(&writer)).await.unwrap();
        assert_eq!(count, 0);
        assert_eq!(s.len(), 1);
    }

    #[tokio::test]
    async fn fold_failure_surfaces_as_typed_projection_error() {
        let s = sink();
        let writer = writer_over(&s);
        // Bad replace: source_event_seqs cites a future seq.
        write(
            &writer,
            json!({
                "type": "user_message",
                "data": {"role": "user", "content": [{"type": "text", "text": "u1"}]},
                "surface_op": "append"
            }),
        )
        .await;
        write(
            &writer,
            json!({
                "type": "assistant_message",
                "data": {"role": "assistant", "content": [{"type": "text", "text": "summary"}]},
                "surface_op": {
                    "start": 0,
                    "end": 1,
                    "source_event_seqs": [99]
                }
            }),
        )
        .await;
        let err = typed_messages_from_sink(s, None).await.unwrap_err();
        assert!(matches!(err, TypedProjectionError::Fold(_)));
    }
    /// The typed projection is PUBLIC (`typed_messages_from_sink`), so it
    /// must coalesce a turn's parallel tool calls exactly as the legacy
    /// projection does — otherwise a consumer that reads its history
    /// through this seam still sends an invalid request to OpenAI
    /// ("insufficient tool messages following tool_calls message").
    #[tokio::test]
    async fn typed_projection_merges_parallel_tool_calls() {
        let sink = sink();
        let writer = writer_over(&sink);
        for row in [
            json!({"type": "UserInput", "data": {"text": "run both"}}),
            json!({"type": "Model", "data": {
                "type": "tool_use", "id": "call_a", "name": "shell",
                "input": {"command": "echo ONE"},
            }}),
            json!({"type": "Model", "data": {
                "type": "tool_use", "id": "call_b", "name": "shell",
                "input": {"command": "echo TWO"},
            }}),
            json!({"type": "Model", "data": {
                "type": "tool_result", "tool_use_id": "call_a",
                "tool_name": "shell", "is_error": false,
                "content": [{"type": "text", "text": "ONE"}],
            }}),
        ] {
            write(&writer, row).await;
        }
        let events = sink.read().await.unwrap();
        let messages = project(&events, None).expect("projection");
        let assistant = messages
            .iter()
            .find(|m| m.role == Role::Assistant)
            .expect("an assistant turn");
        let ids: Vec<&str> = assistant
            .content
            .iter()
            .filter_map(|p| match p {
                synthia_provider::ContentPart::ToolUse(tu) => {
                    Some(tu.id.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(ids, vec!["call_a", "call_b"], "got {messages:?}");
    }
}
