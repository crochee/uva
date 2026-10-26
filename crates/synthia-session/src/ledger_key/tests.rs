//! The key scheme's tests: every shadowable row carries an identity,
//! and no two rows on one surface share one.

use serde_json::{Value, json};
use synthia_provider::{Content, ContentPart, Message, Role, ToolUse};

use crate::{ledger_key::key_of, log_surface::SurfaceLedger};

/// An assistant turn that asks for two calls at once.
fn assistant_turn(calls: &[&str]) -> Message {
    let parts = calls
        .iter()
        .map(|id| {
            ContentPart::ToolUse(ToolUse {
                id: (*id).to_string(),
                name: "read".to_string(),
                input: json!({}),
            })
        })
        .collect();
    Message::new(Role::Assistant, Content::parts(parts))
}

/// The tool row that answers `call_id`, in the shape the controller
/// persists: a typed row carrying the tool message.
fn tool_row(call_id: &str) -> Value {
    json!({
        "type": "tool_result",
        "data": Message::tool(Content::text("ok"), call_id),
    })
}

/// The assistant row that makes the call, in the same shape.
fn assistant_row(call_id: &str) -> Value {
    json!({
        "type": "assistant_message",
        "data": assistant_turn(&[call_id]),
    })
}

/// A tool message is named by the call it answers.
#[test]
fn a_tool_message_is_named_by_the_call_it_answers() {
    let message = Message::tool(Content::text("ok"), "call-1");

    assert_eq!(key_of(&message).as_deref(), Some("tool:call-1"));
}

/// A turn may ask for several calls at once. The first one names it:
/// a call id is unique, so one id is enough to find the row that
/// made it.
#[test]
fn an_assistant_turn_is_named_by_its_first_call() {
    let message = assistant_turn(&["call-1", "call-2"]);

    assert_eq!(key_of(&message).as_deref(), Some("assistant:call-1"));
}

/// A turn that asks for nothing has no durable identity: there is no
/// call to name it by.
#[test]
fn a_plain_assistant_turn_has_no_identity() {
    let message = Message::assistant("just talking");

    assert_eq!(key_of(&message), None);
}

/// A user message has no identity either — it is never shadowed on
/// its own.
#[test]
fn a_user_message_has_no_identity() {
    let message = Message::user("go");

    assert_eq!(key_of(&message), None);
}

/// The turn that made the call and the result that answers it name
/// the same call. The prefix is what keeps them apart: without it the
/// ledger would find two rows for one key and refuse to prove the
/// span.
#[test]
fn an_assistant_row_and_its_result_do_not_collide() {
    let ledger = SurfaceLedger::new();
    ledger.record(1, &assistant_row("call-1"));
    ledger.record(2, &tool_row("call-1"));

    let resolved = ledger
        .resolve(&[
            Some("assistant:call-1".to_string()),
            Some("tool:call-1".to_string()),
        ])
        .expect("both rows resolve: the keys do not collide");

    assert_eq!(resolved.source_event_seqs, vec![1, 2]);
}

/// The raw-log extraction and the typed one must name the same call:
/// the ledger's key and the balance cache's ids come from the two
/// paths, and a turn that named two different calls would break the
/// pairing a compaction relies on.
#[test]
fn the_typed_and_the_raw_extraction_name_the_same_call() {
    let message = assistant_turn(&["call-1", "call-2"]);
    let raw = serde_json::to_value(&message).expect("the message serialises");
    let mut ids = Vec::new();
    crate::tool_pairing::collect_tool_use_ids(&raw, &mut ids);

    assert_eq!(
        ids.first().map(String::as_str),
        crate::tool_pairing::first_tool_use_id(&message),
        "the two representations name the same call",
    );
}
