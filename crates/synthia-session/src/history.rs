//! Model-history projection — replay the durable session log
//! into the provider history the next request sees.
//!
//! [`derive_history`] is the read end of the log: the agent loop
//! no longer owns the transcript, it asks the log what the model
//! has seen. Rows are projected by [`crate::try_fold_log_surface`],
//! the crate's single raw-log projection — it skips the sink's
//! metadata header ([`crate::jsonl::is_metadata_header_row`]),
//! recognises both the typed rows and the legacy envelopes the
//! repo still writes, numbers rows the way the rest of the system
//! does, and applies the `Replace` a durable compaction wrote.
//!
//! The projection is pure: the same rows always produce the same
//! history.
//!
//! It is also **pairing-aware**. The log records a turn's tool call
//! before the tool runs and its result after, so a message that
//! lands in between — a steer, appended while the batch is in
//! flight — would sit between an assistant's `tool_use` and the
//! result that answers it. Every provider rejects that transcript,
//! and the message would break the very run it steers. The
//! projection therefore holds a user message back until the pending
//! tool calls are answered, and releases it after their results.

use std::collections::HashSet;

use serde_json::Value;
use synthia_provider::{Content, ContentPart, Message, Role};

use crate::{log_surface::try_fold_log_surface, surface::FoldError};

/// Error returned by [`derive_history`].
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    /// The log does not fold into a model surface — a `Replace`
    /// cites a span or a seq the fold cannot prove.
    #[error("session log does not fold into a model surface: {0}")]
    Fold(#[from] FoldError),
    /// A folded surface payload is not a provider message.
    #[error("session log surface payload is not a message: {0}")]
    Decode(#[source] serde_json::Error),
}

/// Derive the model history from a raw session log.
///
/// Rows are folded by [`try_fold_log_surface`], which accepts the
/// typed rows and the legacy envelopes alike, skips the sink's
/// metadata header
/// ([`is_metadata_header_row`](crate::jsonl::is_metadata_header_row)),
/// numbers rows by the log's own ordinals and applies the
/// `Replace` a durable compaction wrote. Each folded surface
/// payload is then decoded into a [`Message`]: a message row is
/// the message it carries, and a compaction row — whose payload is
/// its record shape (`{"summary": …}`) — becomes the assistant
/// turn that replaced the span.
///
/// The fold is strict: a rewrite whose provenance cannot be proven
/// is an error rather than silently replaying the span it
/// shadowed. A resumed run must not continue on a history the log
/// does not support.
///
/// # Errors
///
/// Returns [`HistoryError::Fold`] when the log's surface rewrites
/// do not validate and [`HistoryError::Decode`] when a folded
/// payload is neither a message nor a compaction summary.
pub fn derive_history(rows: &[Value]) -> Result<Vec<Message>, HistoryError> {
    let surface = try_fold_log_surface(rows)?;
    let mut projection = SurfaceProjection::default();
    for payload in &surface.messages {
        let message = decode_message(payload)?;
        projection.push(message);
    }
    Ok(projection.finish())
}

/// Project folded surface payloads into provider messages.
///
/// The lenient counterpart of [`derive_history`]: a payload that is
/// neither a message nor a compaction summary is dropped with a
/// warning instead of failing the whole projection, because one bad
/// row must not wedge a long-lived session's transcript.
///
/// Both entry points share one payload decoder and one coalescing
/// rule, so the harness and the server's transcript projections
/// cannot disagree about a row's meaning or about a turn's wire
/// shape.
#[must_use]
pub fn surface_to_messages(payloads: &[Value]) -> Vec<Message> {
    let mut projection = SurfaceProjection::default();
    for payload in payloads {
        match decode_message(payload) {
            Ok(message) => projection.push(message),
            Err(error) => tracing::warn!(
                target: "synthia.session",
                %error,
                "surface projection: dropping a row that is not a message"
            ),
        }
    }
    projection.finish()
}

/// Decode one folded surface payload into the message it carries.
///
/// The index-preserving counterpart of [`surface_to_messages`]:
/// callers that map a position back to a log ordinal (a compaction's
/// `Replace` range) need one message per surface row, in fold order.
/// A payload that is neither a message nor a compaction summary is
/// `None` — the caller must then decline to map positions at all.
#[must_use]
pub fn surface_message(payload: &Value) -> Option<Message> {
    decode_message(payload).ok()
}

/// Normalise one folded surface payload into a [`Message`].
///
/// Message rows carry a `Message`-shaped payload; a compaction row
/// carries `{"summary": …}`, which the model saw as the assistant
/// turn replacing the batch.
fn decode_message(payload: &Value) -> Result<Message, HistoryError> {
    match serde_json::from_value::<Message>(payload.clone()) {
        Ok(message) => Ok(message),
        Err(error) => match payload.get("summary").and_then(Value::as_str) {
            Some(summary) => Ok(Message::assistant(summary)),
            None => Err(HistoryError::Decode(error)),
        },
    }
}

/// The shared projection of folded surface payloads into provider
/// messages.
///
/// One rule set, one place: the assistant-part coalescing, the
/// tool-pairing order, and the deferred user message all live here,
/// so [`derive_history`] and [`surface_to_messages`] cannot disagree
/// about a transcript's shape.
#[derive(Default)]
struct SurfaceProjection {
    /// The projected transcript, in the order the model sees it.
    messages: Vec<Message>,
    /// The tool-call ids an assistant message asked for that no tool
    /// result has answered yet.
    pending_tool_calls: HashSet<String>,
    /// The user messages that arrived while a tool call was pending.
    ///
    /// They wait for the tool results: a user message between an
    /// assistant's tool call and its result is a transcript every
    /// provider rejects.
    deferred_user: Vec<Message>,
}

impl SurfaceProjection {
    /// Append one projected message, applying the projection's
    /// ordering rules.
    fn push(&mut self, message: Message) {
        match message.role {
            Role::Assistant => self.push_assistant(message),
            Role::Tool => self.push_tool_result(message),
            Role::User => self.push_user(message),
            Role::System => self.messages.push(message),
        }
    }

    /// Append an assistant turn, coalescing its parts.
    ///
    /// A legacy `"Model"` log carries ONE part per row, so an
    /// assistant turn that asked for several tools in parallel is
    /// several `tool_use` rows. Projected as separate messages that
    /// breaks the contract every provider enforces: a message
    /// carrying `tool_calls` must be followed immediately by its
    /// tool results, and here the first one is followed by another
    /// assistant message instead — OpenAI rejects it with
    /// "insufficient tool messages following tool_calls message".
    /// Merging the parts keeps the turn one message, so the tool
    /// results that follow answer it directly.
    ///
    /// The merge is deliberately narrower than "adjacent assistant
    /// rows": it applies only while the message being extended holds
    /// a pending `ToolUse`. Two text-only assistant messages are
    /// genuinely separate turns (a compaction summary following the
    /// text it replaced), and merging those would rewrite the
    /// transcript's meaning rather than repair its wire shape.
    fn push_assistant(&mut self, message: Message) {
        let call_ids = tool_use_ids(&message);
        if let Some(last) = self.messages.last_mut()
            && last.role == Role::Assistant
            && last
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::ToolUse(_)))
        {
            merge_assistant_parts(last, message);
        } else {
            self.messages.push(message);
        }
        self.pending_tool_calls.extend(call_ids);
    }

    /// Append a tool result, answering the call it names.
    ///
    /// A tool result closes the pairing it belongs to, so a user
    /// message that waited for it is released here — after the
    /// result, which is where the provider expects it.
    fn push_tool_result(&mut self, message: Message) {
        for id in tool_result_ids(&message) {
            self.pending_tool_calls.remove(&id);
        }
        self.messages.push(message);
        if self.pending_tool_calls.is_empty() {
            self.release_deferred();
        }
    }

    /// Append a user message, deferring it while a tool call is
    /// pending.
    ///
    /// The deferral is what makes a steer safe. A steer lands in the
    /// log while the tool batch is in flight, so its ordinal falls
    /// between the assistant's `tool_use` row and the `tool_result`
    /// row that answers it. A user message in that gap is a
    /// transcript every provider rejects — and the steer would break
    /// the very run it steers. Waiting for the result puts the
    /// message back where it belongs.
    fn push_user(&mut self, message: Message) {
        if self.pending_tool_calls.is_empty() {
            self.messages.push(message);
        } else {
            self.deferred_user.push(message);
        }
    }

    /// Release the deferred user messages, in order.
    fn release_deferred(&mut self) {
        if self.deferred_user.is_empty() {
            return;
        }
        self.messages.append(&mut self.deferred_user);
    }

    /// The projected transcript.
    ///
    /// A user message that waited for a tool result that never came
    /// is released here: the projection never loses a message, and a
    /// dangling tool call is the repair path's problem
    /// (`crate::repair`), not the projection's.
    fn finish(mut self) -> Vec<Message> {
        self.release_deferred();
        self.messages
    }
}

/// The tool-call ids an assistant message asks for.
fn tool_use_ids(message: &Message) -> Vec<String> {
    message
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::ToolUse(call) => Some(call.id.clone()),
            _ => None,
        })
        .collect()
}

/// The tool-call ids a tool message answers.
///
/// The id travels either as the message's `tool_call_id` or as the
/// `tool_use_id` of its result part; a message may carry both, and
/// both name the same call.
fn tool_result_ids(message: &Message) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(id) = message.tool_call_id.as_ref() {
        ids.push(id.clone());
    }
    for part in message.content.iter() {
        if let ContentPart::ToolResult(result) = part {
            ids.push(result.tool_use_id.clone());
        }
    }
    ids
}

/// Append one assistant message's parts onto `into`.
///
/// The parts are carried in the wire-neutral `Content` the two
/// messages hold, so this stays a content merge rather than a
/// re-encode: a single-part `Content::Single` is expanded into a
/// `Multi` only when the other side has something to join it to.
fn merge_assistant_parts(into: &mut Message, from: Message) {
    let mut parts: Vec<ContentPart> = into.content.iter().cloned().collect();
    parts.extend(from.content.iter().cloned());
    into.content = Content::parts(parts);
    // A `tool_call_id` belongs to a tool message; two assistant
    // messages never carry one, but keep the first if either did.
    if into.tool_call_id.is_none() {
        into.tool_call_id = from.tool_call_id;
    }
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};
    use synthia_provider::{
        Content,
        ContentPart,
        Message,
        Role,
        TextContent,
        ToolUse,
    };

    use super::{HistoryError, derive_history, surface_message};
    use crate::{
        events::{SessionEvent, SurfaceOp},
        surface::FoldError,
    };

    fn message_json(message: &Message) -> Value {
        serde_json::to_value(message).expect("message serializes")
    }

    fn row(event: SessionEvent) -> Value {
        serde_json::to_value(event).expect("event serializes")
    }

    fn user_row(seq: u64, text: &str) -> Value {
        row(SessionEvent::UserMessage {
            seq,
            ts: "2026-10-25T00:00:00Z".into(),
            data: message_json(&Message::user(text)),
            source: None,
            surface_op: None,
        })
    }

    fn assistant_row(seq: u64, text: &str) -> Value {
        row(SessionEvent::AssistantMessage {
            seq,
            ts: "2026-10-25T00:00:01Z".into(),
            data: message_json(&Message::assistant(text)),
            surface_op: None,
        })
    }

    fn tool_row(seq: u64, call_id: &str, output: &str) -> Value {
        row(SessionEvent::ToolResult {
            seq,
            ts: "2026-10-25T00:00:02Z".into(),
            data: message_json(&Message::tool(output, call_id)),
            is_error: false,
            surface_op: None,
        })
    }

    fn assistant_tool_row(seq: u64, call_id: &str) -> Value {
        let parts = vec![ContentPart::ToolUse(ToolUse {
            id: call_id.into(),
            name: "read".into(),
            input: json!({"path": "a.rs"}),
        })];
        let message = Message::new(Role::Assistant, Content::parts(parts));
        row(SessionEvent::AssistantMessage {
            seq,
            ts: "2026-10-25T00:00:01Z".into(),
            data: message_json(&message),
            surface_op: None,
        })
    }

    fn compaction_row(
        seq: u64,
        summary: &str,
        start: usize,
        end: usize,
        source_event_seqs: &[u64],
    ) -> Value {
        row(SessionEvent::Compaction {
            seq,
            ts: "2026-10-25T00:00:03Z".into(),
            data: json!({ "summary": summary }),
            surface_op: SurfaceOp::Replace {
                start,
                end,
                source_event_seqs: source_event_seqs.to_vec(),
            },
        })
    }

    fn header_row() -> Value {
        json!({"_meta": {"kind": "synthia_session_meta", "schema": 1}})
    }

    fn text(message: &Message) -> Option<String> {
        message.content.extract_text()
    }

    #[test]
    fn derive_history_replays_user_and_assistant_in_order() {
        let rows = vec![user_row(1, "hello"), assistant_row(2, "hi")];
        let history = derive_history(&rows).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0].role, Role::User);
        assert_eq!(text(&history[0]).as_deref(), Some("hello"));
        assert_eq!(history[1].role, Role::Assistant);
        assert_eq!(text(&history[1]).as_deref(), Some("hi"));
    }

    #[test]
    fn derive_history_applies_a_compaction_replace() {
        // user(1), assistant(2), then compaction(3) replacing [0,2).
        let rows = vec![
            user_row(1, "a"),
            assistant_row(2, "b"),
            compaction_row(3, "summary", 0, 2, &[1, 2]),
        ];
        let history = derive_history(&rows).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, Role::Assistant);
        assert_eq!(text(&history[0]).as_deref(), Some("summary"));
    }

    #[test]
    fn derive_history_skips_the_metadata_header() {
        let rows = vec![header_row(), user_row(1, "hello")];
        let history = derive_history(&rows).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(text(&history[0]).as_deref(), Some("hello"));
    }

    #[test]
    fn a_plugin_row_replays_but_never_enters_the_history() {
        // The durable half of the plugin escape hatch: the row is a
        // log row like any other, and it is log-only — the model
        // history is exactly the surface-eligible rows around it.
        let plugin = row(SessionEvent::plugin(
            2,
            "2026-10-26T00:00:00Z",
            "acme.span",
            json!({"span": "s1", "attrs": {"attempt": 1}}),
        ));
        let rows =
            vec![user_row(1, "hello"), plugin.clone(), assistant_row(3, "hi")];

        // Replay: the row parses back into the same typed event with
        // its payload intact, so a reader that only understands the
        // typed layer still sees the plugin's fact.
        let replayed =
            SessionEvent::from_value(&plugin).expect("the row replays");
        let SessionEvent::Plugin {
            seq, kind, payload, ..
        } = &replayed
        else {
            panic!("a plugin row, got {replayed:?}");
        };
        assert_eq!((*seq, kind.as_str()), (2, "acme.span"));
        assert_eq!(payload, &json!({"span": "s1", "attrs": {"attempt": 1}}));

        // Not in the surface: the *raw-row* fold (the production
        // path, whose `classify_row` decides what a row is) steps
        // over it, and the seqs of the rows either side are
        // unchanged.
        let surface = crate::log_surface::fold_log_surface(&rows);
        assert_eq!(surface.surface_seqs, vec![1, 3]);

        // Not in the derived history: two messages, neither of them
        // the plugin's payload.
        let history = derive_history(&rows).unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(text(&history[0]).as_deref(), Some("hello"));
        assert_eq!(text(&history[1]).as_deref(), Some("hi"));
    }

    #[test]
    fn derive_history_keeps_tool_result_identity() {
        let rows = vec![tool_row(1, "call-1", "ok")];
        let history = derive_history(&rows).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, Role::Tool);
        assert_eq!(history[0].tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(text(&history[0]).as_deref(), Some("ok"));
    }

    #[test]
    fn a_user_message_that_lands_mid_tool_call_follows_the_result() {
        // The log records the assistant's tool call before the tool
        // runs and its result after, so a message appended while the
        // batch is in flight (a steer) sits between the two. The
        // projection holds it back: a user message between a tool
        // call and its result is a transcript every provider
        // rejects.
        let rows = vec![
            user_row(1, "start"),
            assistant_tool_row(2, "call-1"),
            user_row(3, "steer: focus on the tests"),
            tool_row(4, "call-1", "ok"),
        ];
        let history = derive_history(&rows).unwrap();
        assert_eq!(history.len(), 4, "got {history:?}");
        assert_eq!(history[0].role, Role::User);
        assert_eq!(text(&history[0]).as_deref(), Some("start"));
        assert_eq!(history[1].role, Role::Assistant);
        assert_eq!(history[2].role, Role::Tool);
        assert_eq!(history[2].tool_call_id.as_deref(), Some("call-1"));
        assert_eq!(history[3].role, Role::User);
        assert_eq!(
            text(&history[3]).as_deref(),
            Some("steer: focus on the tests")
        );
    }

    #[test]
    fn a_user_message_after_the_result_keeps_its_place() {
        let rows = vec![
            user_row(1, "start"),
            assistant_tool_row(2, "call-1"),
            tool_row(3, "call-1", "ok"),
            user_row(4, "next"),
        ];
        let history = derive_history(&rows).unwrap();
        assert_eq!(history.len(), 4, "got {history:?}");
        assert_eq!(history[2].role, Role::Tool);
        assert_eq!(history[3].role, Role::User);
        assert_eq!(text(&history[3]).as_deref(), Some("next"));
    }

    #[test]
    fn a_user_message_is_never_lost_to_an_unanswered_tool_call() {
        // A cancelled run leaves the tool call unanswered: the
        // repair path closes it on the next run. The projection must
        // not swallow the message in the meantime.
        let rows = vec![
            user_row(1, "start"),
            assistant_tool_row(2, "call-1"),
            user_row(3, "steer: focus on the tests"),
        ];
        let history = derive_history(&rows).unwrap();
        assert_eq!(history.len(), 3, "got {history:?}");
        assert_eq!(history[1].role, Role::Assistant);
        assert_eq!(history[2].role, Role::User);
        assert_eq!(
            text(&history[2]).as_deref(),
            Some("steer: focus on the tests")
        );
    }

    #[test]
    fn derive_history_preserves_a_multi_part_assistant_turn() {
        let parts = vec![
            ContentPart::Text(TextContent {
                text: "working".into(),
                cache_control: None,
            }),
            ContentPart::ToolUse(ToolUse {
                id: "call-1".into(),
                name: "read".into(),
                input: json!({"path": "a.rs"}),
            }),
        ];
        let message = Message::new(Role::Assistant, Content::Multi(parts));
        let rows = vec![row(SessionEvent::AssistantMessage {
            seq: 1,
            ts: "2026-10-25T00:00:00Z".into(),
            data: message_json(&message),
            surface_op: None,
        })];
        let history = derive_history(&rows).unwrap();
        assert_eq!(history, vec![message]);
    }

    #[test]
    fn derive_history_accepts_a_legacy_message_envelope() {
        // The server's memory tier still writes this envelope.
        let rows = vec![json!({
            "type": "Message",
            "data": message_json(&Message::user("legacy")),
        })];
        let history = derive_history(&rows).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(text(&history[0]).as_deref(), Some("legacy"));
    }

    #[test]
    fn derive_history_accepts_a_legacy_user_input_envelope() {
        // The production prompt row: `{"type":"UserInput", …}`.
        let rows =
            vec![json!({"type": "UserInput", "data": {"text": "hello"}})];
        let history = derive_history(&rows).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].role, Role::User);
        assert_eq!(text(&history[0]).as_deref(), Some("hello"));
    }

    #[test]
    fn derive_history_coalesces_a_legacy_parallel_tool_turn() {
        // A legacy log carries ONE part per `Model` row, so a turn
        // that asked for two tools in parallel is two `tool_use`
        // rows. Projected as two assistant messages the first is
        // followed by another assistant message instead of its
        // results, which every provider rejects.
        let rows = vec![
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
        let history = derive_history(&rows).unwrap();
        assert_eq!(history.len(), 4, "got {history:?}");
        assert_eq!(history[0].role, Role::User);
        assert_eq!(
            history[1].role,
            Role::Assistant,
            "the two tool_use rows are one assistant turn"
        );
        let call_ids: Vec<&str> = history[1]
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::ToolUse(tool_use) => Some(tool_use.id.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            call_ids,
            vec!["call_a", "call_b"],
            "both calls must ride the same message, in order"
        );
        assert_eq!(history[2].role, Role::Tool);
        assert_eq!(history[3].role, Role::Tool);
        let result_ids: Vec<&str> = history[2..]
            .iter()
            .filter_map(|message| {
                message.content.iter().find_map(|part| match part {
                    ContentPart::ToolResult(result) => {
                        Some(result.tool_use_id.as_str())
                    }
                    _ => None,
                })
            })
            .collect();
        assert_eq!(
            result_ids,
            vec!["call_a", "call_b"],
            "each result answers the merged turn, in order"
        );
    }

    #[test]
    fn derive_history_reports_a_provenance_violation() {
        let rows = vec![
            user_row(1, "a"),
            assistant_row(2, "b"),
            compaction_row(3, "summary", 0, 2, &[9]),
        ];
        match derive_history(&rows) {
            Err(HistoryError::Fold(FoldError::BadProvenance {
                seq,
                bad_seq,
            })) => {
                assert_eq!(seq, 3);
                assert_eq!(bad_seq, 9);
            }
            other => panic!("expected a fold error, got {other:?}"),
        }
    }

    #[test]
    fn derive_history_reports_an_undecodable_surface_payload() {
        // A compaction whose summary is not text folds, but its
        // surface payload is no provider message.
        let rows = vec![
            user_row(1, "a"),
            row(SessionEvent::Compaction {
                seq: 2,
                ts: "2026-10-25T00:00:03Z".into(),
                data: json!({"summary": 42}),
                surface_op: SurfaceOp::Replace {
                    start: 0,
                    end: 1,
                    source_event_seqs: vec![1],
                },
            }),
        ];
        match derive_history(&rows) {
            Err(HistoryError::Decode(_)) => {}
            other => panic!("expected a decode error, got {other:?}"),
        }
    }

    #[test]
    fn derive_history_is_deterministic() {
        let rows = vec![
            header_row(),
            user_row(1, "hello"),
            assistant_row(2, "hi"),
            tool_row(3, "call-1", "ok"),
            compaction_row(4, "summary", 0, 2, &[1, 2]),
        ];
        assert_eq!(
            derive_history(&rows).unwrap(),
            derive_history(&rows).unwrap()
        );
    }

    #[test]
    fn derive_history_of_an_empty_log_is_empty() {
        assert!(derive_history(&[]).unwrap().is_empty());
    }

    #[test]
    fn surface_message_keeps_one_message_per_row() {
        // A compaction's `Replace` addresses fold positions, so its
        // decoder must never drop, merge or reorder a row.
        let payloads = [
            message_json(&Message::user("go")),
            message_json(&Message::assistant("hi")),
            json!({"summary": "compact"}),
        ];
        let decoded: Vec<_> =
            payloads.iter().filter_map(surface_message).collect();
        assert_eq!(decoded.len(), payloads.len());
        assert_eq!(decoded[2], Message::assistant("compact"));
        assert!(surface_message(&json!({"nonsense": true})).is_none());
    }
}
