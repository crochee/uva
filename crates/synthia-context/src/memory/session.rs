//! Session-writer-backed implementation of the [`Memory`] trait.
//!
//! Bridges the layered-memory seam to the durable event log:
//!
//! - **Conversation tier** — projected from
//!   [`LogWriter::read`] via [`events_to_messages`]; `append`
//!   writes a lossless `"Message"` envelope back through the
//!   session's writer, which stamps the row's ordinal.
//! - **Working tier** — in-process map (volatile across process
//!   restarts; the log is reserved for the conversation log).
//! - **Long-term tier** — in-process vec (until a persistent
//!   backend exists).
//!
//! # Bounded rehydration
//!
//! Long sessions make the full-log projection cost grow without
//! bound. [`SessionMemory::with_max_events`] caps the projection
//! to the **tail** of the log — the oldest events are the ones
//! the live [`crate::ContextManager`] would prune first anyway,
//! so trading them away at rehydration time is the cheapest
//! place to bound the cost. The durable log itself stays
//! lossless.

use std::{collections::HashMap, sync::Arc};

use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::{Value, json};
use synthia_provider::Message;
use synthia_session::{LogWriter, fold_log_surface, surface_to_messages};

use super::{Memory, MemoryEntry, MemoryError};
/// Project sink events to conversation messages.
///
/// The log is folded by [`synthia_session::fold_log_surface`], which
/// is the single raw-log projection: it recognises the typed message
/// rows (`user_message` / `assistant_message` / `tool_result`), the
/// legacy envelopes this repo has written since v0.1 —
///
/// | `type` | `data` shape | Produced message |
/// |---|---|---|
/// | `"Message"` | full [`Message`] serde value | the message verbatim (lossless round-trip of [`SessionMemory::append`]) |
/// | `"UserInput"` | `{ "text": string }` | `Message { Role::User }` with a single text part |
/// | `"Model"` | a single [`ContentPart`](synthia_provider::ContentPart) serde value | one message per row: text → assistant, tool-use → assistant, tool-result → tool |
///
/// — stamps each with its 1-based log ordinal, and applies the
/// `SurfaceOp::Replace` a durable compaction checkpoint wrote, so a
/// resumed session sees the compacted surface instead of replaying
/// the span the compaction shadowed.
///
/// The folded payloads are projected by
/// [`synthia_session::surface_to_messages`], the one projection the
/// harness's [`synthia_session::derive_history`] also uses: it normalises a
/// compaction's record shape into the assistant turn the model saw,
/// and coalesces a legacy per-part assistant turn into the single
/// message every provider expects.
///
/// Unknown or malformed rows are dropped (warn-logged) by the fold,
/// and a replacement that cannot be validated is skipped rather than
/// aborting the projection — one bad row must not wedge a long-lived
/// session.
pub fn events_to_messages(events: &[Value]) -> Vec<Message> {
    surface_to_messages(&fold_log_surface(events).messages)
}

/// [`Memory`] view over one session's [`LogWriter`].
pub struct SessionMemory {
    /// The session's one writer: it stamps each row's ordinal and
    /// batches the flush, so the log has one numbering.
    writer: Arc<LogWriter>,
    /// Rehydration budget: project only the last `N` log events.
    max_events: Option<usize>,
    /// Working memory: `session_id` → (`key` → value).
    working: RwLock<HashMap<String, HashMap<String, Value>>>,
    /// Long-term memory entries (global).
    long_term: RwLock<Vec<MemoryEntry>>,
}

impl SessionMemory {
    /// Create a memory view over the session's `writer` with
    /// **unbounded** rehydration (projects the full event log).
    #[must_use]
    pub fn new(writer: Arc<LogWriter>) -> Self {
        Self {
            writer,
            max_events: None,
            working: RwLock::new(HashMap::new()),
            long_term: RwLock::new(Vec::new()),
        }
    }

    /// Cap rehydration to the last `max` log events.
    #[must_use]
    pub fn with_max_events(mut self, max: usize) -> Self {
        self.max_events = Some(max);
        self
    }

    fn tail<'a>(&self, events: &'a [Value]) -> &'a [Value] {
        match self.max_events {
            Some(max) if events.len() > max => &events[events.len() - max..],
            _ => events,
        }
    }
}

#[async_trait]
impl Memory for SessionMemory {
    async fn messages(
        &self,
        _session_id: &str,
    ) -> Result<Vec<Message>, MemoryError> {
        // The log is already session-scoped (one JSONL per
        // session id); the parameter stays for trait parity.
        let events = self.writer.read().await?;
        Ok(events_to_messages(self.tail(&events)))
    }

    async fn append(
        &self,
        _session_id: &str,
        message: Message,
    ) -> Result<(), MemoryError> {
        let envelope = json!({
            "type": "Message",
            "data": serde_json::to_value(&message)
                .map_err(|e| MemoryError::Decode(e.to_string()))?,
        });
        // The writer stamps the row with the ordinal it hands out
        // and with the wall clock; the flush makes the message
        // durable before the call returns — the promise the sink's
        // write-through `append` used to give.
        self.writer.append(envelope).await?;
        self.writer.flush().await?;
        Ok(())
    }

    async fn get_context(
        &self,
        session_id: &str,
        key: &str,
    ) -> Result<Option<Value>, MemoryError> {
        let working = self.working.read();
        Ok(working
            .get(session_id)
            .and_then(|ctx| ctx.get(key))
            .cloned())
    }

    async fn set_context(
        &self,
        session_id: &str,
        key: &str,
        value: Value,
    ) -> Result<(), MemoryError> {
        let mut working = self.working.write();
        working
            .entry(session_id.to_string())
            .or_default()
            .insert(key.to_string(), value);
        Ok(())
    }

    async fn recall(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryError> {
        let long_term = self.long_term.read();
        let results = long_term
            .iter()
            .filter(|entry| entry.content.contains(query))
            .take(limit)
            .cloned()
            .collect();
        Ok(results)
    }

    async fn store(&self, entry: MemoryEntry) -> Result<(), MemoryError> {
        let mut long_term = self.long_term.write();
        long_term.push(entry);
        Ok(())
    }

    async fn delete_session(
        &self,
        session_id: &str,
    ) -> Result<(), MemoryError> {
        // The durable conversation log is owned by the sink /
        // server registry and is NOT touched here. Only the
        // volatile tiers this adapter owns are cleared.
        self.working.write().remove(session_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use synthia_core::SystemClock;
    use synthia_provider::{Content, ContentPart, Role, TextContent};
    use synthia_session::{SessionSink, in_memory::InMemorySessionSink};

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

    #[tokio::test]
    async fn append_then_messages_round_trips_verbatim() {
        let mem = SessionMemory::new(writer_over(&sink()));
        let original = vec![
            Message::user("hello"),
            Message::assistant("hi"),
            Message {
                role: Role::Tool,
                content: Content::Multi(vec![
                    ContentPart::Text(TextContent {
                        text: "output".into(),
                        cache_control: None,
                    }),
                    ContentPart::Text(TextContent {
                        text: "more".into(),
                        cache_control: None,
                    }),
                ]),
                tool_call_id: Some("call_1".into()),
                name: Some("shell".into()),
                ..Default::default()
            },
        ];
        for message in &original {
            mem.append("s1", message.clone()).await.unwrap();
        }
        assert_eq!(mem.messages("s1").await.unwrap(), original);
    }

    #[tokio::test]
    async fn messages_empty_sink_is_empty() {
        let mem = SessionMemory::new(writer_over(&sink()));
        assert!(mem.messages("s1").await.unwrap().is_empty());
    }

    /// The row the memory tier writes must carry the stamp the
    /// session's writer puts on it: the ordinal it handed out and
    /// the wall clock. A row without them is numbered by something
    /// other than the log's one counter.
    #[tokio::test]
    async fn an_appended_message_carries_the_writers_stamp() {
        let sink = sink();
        let mem = SessionMemory::new(writer_over(&sink));
        mem.append("s1", Message::user("hello")).await.unwrap();
        let rows = sink.read().await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]["seq"].as_u64(),
            Some(1),
            "the writer stamps the ordinal: {rows:?}"
        );
        assert!(
            rows[0]["ts"].is_string(),
            "the writer stamps the wall clock: {rows:?}"
        );
    }

    #[tokio::test]
    async fn projection_reads_legacy_user_input_envelope() {
        let events = vec![json!({
            "type": "UserInput",
            "data": { "text": "legacy prompt" }
        })];
        let messages = events_to_messages(&events);
        assert_eq!(messages, vec![Message::user("legacy prompt")]);
    }

    #[tokio::test]
    async fn projection_reads_legacy_model_text_envelope() {
        let events = vec![json!({
            "type": "Model",
            "data": { "type": "text", "text": "legacy reply" }
        })];
        let messages = events_to_messages(&events);
        assert_eq!(messages, vec![Message::assistant("legacy reply")]);
    }

    #[tokio::test]
    async fn projection_reads_legacy_model_tool_result_envelope() {
        let events = vec![json!({
            "type": "Model",
            "data": {
                "type": "tool_result",
                "tool_use_id": "call_9",
                "content": [
                    { "type": "text", "text": "ok" }
                ],
            }
        })];
        let messages = events_to_messages(&events);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].role, Role::Tool);
    }

    #[tokio::test]
    async fn projection_drops_malformed_and_unknown_rows() {
        let events = vec![
            json!({"type": "UserInput", "data": {"no_text": true}}),
            json!({"type": "Model", "data": {"type": "not_a_part"}}),
            json!({"type": "Message", "data": {"role": "bogus"}}),
            json!({"type": "Feedback", "data": {}}),
            json!({"no_type": {}}),
        ];
        assert!(events_to_messages(&events).is_empty());
    }

    /// The production resume path (`SessionController` rebuilds every
    /// run's history with this function) must show the *compacted*
    /// surface: the rows a durable compaction checkpoint shadowed are
    /// gone, the summary is in their place, and the seqs are the log's
    /// own row ordinals.
    #[tokio::test]
    async fn resume_honours_a_durable_compaction_checkpoint() {
        let sink = sink();
        let writer = writer_over(&sink);
        let tool_row = |call_id: &str| {
            json!({
                "type": "Model",
                "data": {
                    "type": "tool_result",
                    "tool_use_id": call_id,
                    "tool_name": "read",
                    "content": [{
                        "type": "text",
                        "text": format!("raw output of {call_id}"),
                    }],
                }
            })
        };
        // Rows 1..4 are what the run persisted; row 5 is the record
        // `CompactionCheckpoint` wrote for the two tool results.
        // The writer stamps the ordinals, so the checkpoint's
        // citations name the rows they shadow.
        write(
            &writer,
            json!({"type": "UserInput", "data": {"text": "hello"}}),
        )
        .await;
        write(
            &writer,
            json!({
                "type": "Model",
                "data": {"type": "text", "text": "working"},
            }),
        )
        .await;
        write(&writer, tool_row("c1")).await;
        write(&writer, tool_row("c2")).await;
        write(
            &writer,
            json!({
                "type": "compaction",
                "surface_op": {"start": 2, "end": 4, "source_event_seqs": [3, 4]},
                "data": {"source_indices": [2, 3], "summary": "compacted tail"},
            }),
        )
        .await;

        let events = sink.read().await.unwrap();
        let messages = events_to_messages(&events);
        assert_eq!(messages.len(), 3, "got {messages:?}");
        assert_eq!(messages[2].role, Role::Assistant);
        assert_eq!(
            messages[2].content.extract_text().as_deref(),
            Some("compacted tail")
        );
        let flattened = format!("{messages:?}");
        assert!(
            !flattened.contains("raw output of c1")
                && !flattened.contains("raw output of c2"),
            "the pre-compaction span must not be replayed: {flattened}"
        );
    }

    /// A checkpoint whose provenance does not resolve must not break
    /// the resume: the span is replayed instead of being spliced at
    /// the wrong index.
    #[tokio::test]
    async fn resume_replays_the_span_when_the_checkpoint_cannot_resolve() {
        let events = vec![
            json!({"type": "UserInput", "data": {"text": "hello"}}),
            json!({"type": "Model", "data": {"type": "text", "text": "a"}}),
            json!({"type": "Model", "data": {"type": "text", "text": "b"}}),
            json!({
                "type": "compaction",
                "surface_op": {"start": 0, "end": 3, "source_event_seqs": [9]},
                "data": {"summary": "unresolvable"},
            }),
        ];
        let messages = events_to_messages(&events);
        assert_eq!(messages.len(), 3, "got {messages:?}");
        assert_eq!(messages[1], Message::assistant("a"));
        assert_eq!(messages[2], Message::assistant("b"));
    }

    /// A turn that asked for two tools in PARALLEL writes two `Model`
    /// rows, one per `tool_use` part. Projecting them as two assistant
    /// messages breaks the contract every provider enforces — a message
    /// carrying `tool_calls` must be followed immediately by its tool
    /// results, and here the first is followed by another assistant
    /// message. OpenAI rejects exactly that with "insufficient tool
    /// messages following tool_calls message".
    ///
    /// So the projection must yield ONE assistant message carrying both
    /// calls, in the order they were emitted.
    #[tokio::test]
    async fn parallel_tool_calls_merge_into_one_assistant_message() {
        let events = vec![
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
            json!({"type": "Model", "data": {
                "type": "tool_result", "tool_use_id": "call_b",
                "tool_name": "shell", "is_error": false,
                "content": [{"type": "text", "text": "TWO"}],
            }}),
        ];
        let messages = events_to_messages(&events);
        assert_eq!(messages.len(), 4, "got {messages:?}");
        assert_eq!(messages[0].role, Role::User);
        assert_eq!(
            messages[1].role,
            Role::Assistant,
            "the two tool_use rows are one assistant turn"
        );
        let ids: Vec<&str> = messages[1]
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolUse(tu) => Some(tu.id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            ids,
            vec!["call_a", "call_b"],
            "both calls must ride the same message, in order"
        );
        // And each is answered by its own tool message, so the
        // assistant message is immediately followed by results rather
        // than by another assistant message.
        assert_eq!(messages[2].role, Role::Tool);
        assert_eq!(messages[3].role, Role::Tool);
    }

    #[tokio::test]
    async fn max_events_bounds_projection_to_log_tail() {
        let mem = SessionMemory::new(writer_over(&sink())).with_max_events(3);
        for i in 0..5 {
            mem.append("s1", Message::user(format!("m{i}")))
                .await
                .unwrap();
        }
        let messages = mem.messages("s1").await.unwrap();
        assert_eq!(messages.len(), 3);
        assert_eq!(messages[0], Message::user("m2"));
        assert_eq!(messages[2], Message::user("m4"));
    }

    #[tokio::test]
    async fn max_events_larger_than_log_is_unbounded() {
        let mem = SessionMemory::new(writer_over(&sink())).with_max_events(50);
        mem.append("s1", Message::user("only")).await.unwrap();
        assert_eq!(mem.messages("s1").await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn working_and_long_term_tiers_behave_like_in_memory() {
        let mem = SessionMemory::new(writer_over(&sink()));
        mem.set_context("a", "k", serde_json::json!(7))
            .await
            .unwrap();
        assert_eq!(
            mem.get_context("a", "k").await.unwrap(),
            Some(serde_json::json!(7))
        );

        mem.store(MemoryEntry::now("e", "fact")).await.unwrap();
        assert_eq!(mem.recall("fact", 5).await.unwrap().len(), 1);

        // delete_session clears working memory but neither the
        // durable log nor long-term entries.
        mem.delete_session("a").await.unwrap();
        assert_eq!(mem.get_context("a", "k").await.unwrap(), None);
        assert_eq!(mem.messages("a").await.unwrap().len(), 0);
        assert_eq!(mem.recall("fact", 5).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn default_session_lifecycle_uses_defaults() {
        let mem = SessionMemory::new(writer_over(&sink()));
        let id = mem.create_session().await.unwrap();
        assert!(!id.is_empty());
        assert!(mem.list_sessions().await.unwrap().is_empty());
    }
}
