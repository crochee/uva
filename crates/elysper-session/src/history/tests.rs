use serde_json::{Value, json};
use elysper_provider::{
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
    json!({"_meta": {"kind": "elysper_session_meta", "schema": 1}})
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
    let replayed = SessionEvent::from_value(&plugin).expect("the row replays");
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
    let rows = vec![json!({"type": "UserInput", "data": {"text": "hello"}})];
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
        Err(HistoryError::Fold(FoldError::BadProvenance { seq, bad_seq })) => {
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
    let decoded: Vec<_> = payloads.iter().filter_map(surface_message).collect();
    assert_eq!(decoded.len(), payloads.len());
    assert_eq!(decoded[2], Message::assistant("compact"));
    assert!(surface_message(&json!({"nonsense": true})).is_none());
}
