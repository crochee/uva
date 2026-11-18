//! Interface tests for 发布 — the publication boundary's answer to
//! «what row does a reader see?». Everything goes through the
//! module's public face: [`publish_row`] and [`publish_rows`].

use serde_json::{Value, json};
use uva_provider::{Content, Message};

use super::{publish_row, publish_rows};

/// A legacy `UserInput` envelope carrying text and attachment refs.
fn user_input_row() -> Value {
    json!({
        "type": "UserInput",
        "seq": 3,
        "ts": "2026-01-01T00:00:00Z",
        "data": {
            "text": "hello",
            "attachments": [
                {"kind": "file", "name": "a.md",
                 "mime_type": "text/markdown", "byte_len": 12}
            ]
        }
    })
}

/// A legacy `Model` envelope: one streamed model part.
fn model_row(part: Value) -> Value {
    json!({"type": "Model", "seq": 8, "data": part})
}

/// A current-vocabulary row.
fn typed_row() -> Value {
    json!({
        "type": "warning",
        "seq": 42,
        "ts": "2026-01-01T00:00:00Z",
        "data": {"kind": "test", "message": "hi"}
    })
}

/// A current row is published byte-identical, and its rows are moved
/// — a modern log costs the publication nothing.
#[test]
fn a_current_row_is_published_as_it_is() {
    let row = typed_row();
    let before = row.clone();
    let published = publish_row(row).expect("a current row publishes");
    assert_eq!(published, before);
}

/// A current row whose payload does not decode is still published:
/// its *name* is current — dropping it would drop transcript content
/// the reader renders today.
#[test]
fn a_current_row_with_a_broken_payload_still_publishes() {
    let row = json!({
        "type": "user_message",
        "data": "not the payload shape at all"
    });
    let before = row.clone();
    let published =
        publish_row(row).expect("the tag is current, so it publishes");
    assert_eq!(published, before);
}

/// A dialect row publishes as the current row it stands for: the
/// restored tag, the current payload spelling, and the attachment
/// refs hoisted to the row's top level — the place the current
/// `user_message` row keeps them.
#[test]
fn a_user_input_row_publishes_as_a_user_message() {
    let published = publish_row(user_input_row()).expect("the row restores");
    assert_eq!(published["type"], "user_message");
    assert_eq!(
        published["data"]["role"], "user",
        "the payload is the provider message",
    );
    assert_eq!(
        published["data"]["content"]["Single"]["type"], "text",
        "the turn's text rides as the message's text part",
    );
    assert_eq!(published["data"]["content"]["Single"]["text"], "hello");
    assert_eq!(
        published["attachments"][0]["name"], "a.md",
        "the refs are hoisted to the row's top level",
    );
    assert!(
        published["data"].get("attachments").is_none(),
        "the refs no longer ride inside the payload"
    );
}

/// The published dialect row keeps the log row's own envelope — the
/// ordinal, the timestamp — **including its absence**: no `seq` key
/// is invented, and ordinal 0 is never claimed for a row the sink
/// never stamped.
#[test]
fn a_published_dialect_row_keeps_its_own_ordinal_and_timestamp() {
    let published = publish_row(user_input_row()).expect("the row restores");
    assert_eq!(published["seq"], 3);
    assert_eq!(published["ts"], "2026-01-01T00:00:00Z");

    let unstamped = json!({"type": "UserInput", "data": {"text": "x"}});
    let published = publish_row(unstamped).expect("the row restores");
    assert!(
        published.get("seq").is_none(),
        "an unstamped log row publishes with no seq key — never 0"
    );
    assert!(published.get("ts").is_none());
}

/// A `Model` row carrying text publishes as an `assistant_message`
/// whose message holds that one part.
#[test]
fn a_model_text_row_publishes_as_an_assistant_message() {
    let published = publish_row(model_row(json!({
        "type": "text", "text": "hi there"
    })))
    .expect("the row restores");
    assert_eq!(published["type"], "assistant_message");
    assert_eq!(published["data"]["role"], "assistant");
    assert_eq!(published["data"]["content"]["Single"]["text"], "hi there");
}

/// A `Model` row carrying a reasoning part publishes as an
/// `assistant_message` — the thinking bubble the browser draws for
/// that part today.
#[test]
fn a_model_reasoning_row_publishes_as_an_assistant_message() {
    let published = publish_row(model_row(json!({
        "type": "reasoning", "text": "thinking out loud"
    })))
    .expect("the row restores");
    assert_eq!(published["type"], "assistant_message");
    assert_eq!(published["data"]["content"]["Single"]["type"], "reasoning");
}

/// A `Model` row carrying a tool result publishes as a `tool_result`
/// row: the failure flag hoisted to the row's top level when the part
/// says so, and the call id beside it.
#[test]
fn a_model_tool_result_row_publishes_with_its_failure_flag() {
    let failed = json!({
        "type": "tool_result",
        "tool_use_id": "call_1",
        "content": [{"type": "text", "text": "boom"}],
        "is_error": true
    });
    let published = publish_row(model_row(failed)).expect("the row restores");
    assert_eq!(published["type"], "tool_result");
    assert_eq!(published["data"]["role"], "tool");
    assert_eq!(published["is_error"], true);
    assert_eq!(published["tool_call_id"], "call_1");

    let succeeded = json!({
        "type": "tool_result",
        "tool_use_id": "call_2",
        "content": [{"type": "text", "text": "ok"}]
    });
    let published =
        publish_row(model_row(succeeded)).expect("the row restores");
    assert!(
        published.get("is_error").is_none(),
        "a false flag is omitted — the way a current row spells it"
    );
}

/// A `UserInput` row with attachments and no text is a turn: the
/// browser draws a file-only bubble for it today, so it publishes.
#[test]
fn a_file_only_user_input_row_publishes() {
    let row = json!({
        "type": "UserInput",
        "seq": 11,
        "data": {
            "attachments": [
                {"kind": "file", "name": "notes.md",
                 "mime_type": "text/markdown", "byte_len": 12}
            ]
        }
    });
    let published = publish_row(row).expect("a file-only turn is a turn");
    assert_eq!(published["type"], "user_message");
    assert_eq!(published["attachments"][0]["name"], "notes.md");
}

/// The rows with no current form are not published: `Agent` (the
/// delegated sub-agent's trace — the child has its own log now), an
/// unknown tag, and a row with no type at all.
#[test]
fn rows_with_no_current_form_are_not_published() {
    let agent = json!({"type": "Agent", "data": [{"child_session_id": "c"}]});
    assert!(publish_row(agent).is_none(), "Agent has no counterpart");

    let unknown = json!({"type": "brand_new_tag", "data": {}});
    assert!(
        publish_row(unknown).is_none(),
        "an unknown tag is not a current row"
    );

    let untyped = json!({"data": {"text": "x"}, "seq": 1});
    assert!(
        publish_row(untyped).is_none(),
        "a row with no type has no current form"
    );
}

/// A `Message` row in the loose hand-written spelling publishes to
/// the role's current row: the vocabulary is not stricter about a
/// dialect payload than about a current row's.
#[test]
fn a_loose_message_row_publishes_to_the_roles_row() {
    let loose = json!({
        "type": "Message",
        "seq": 5,
        "data": {
            "role": "assistant",
            "content": [{"type": "text", "text": "hand-written"}]
        }
    });
    let published = publish_row(loose).expect("the loose spelling restores");
    assert_eq!(published["type"], "assistant_message");
    assert_eq!(
        published["data"]["content"]["Single"]["text"],
        "hand-written"
    );
}

/// `publish_rows` keeps the log's order and drops exactly the rows
/// without a current form.
#[test]
fn publish_rows_keeps_order_and_drops_the_rest() {
    let rows = vec![
        json!({"type": "UserInput", "seq": 1, "data": {"text": "one"}}),
        json!({"type": "Agent", "seq": 2, "data": []}),
        json!({"type": "Model", "seq": 3,
               "data": {"type": "text", "text": "two"}}),
    ];
    let published = publish_rows(rows);
    assert_eq!(
        published
            .iter()
            .map(|row| row["type"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["user_message", "assistant_message"],
        "order kept, the Agent row gone"
    );
    assert_eq!(published[0]["seq"], 1);
    assert_eq!(published[1]["seq"], 3);
}

/// A provider message round-trips through the publication: what the
/// reader decodes from the published row is the message the log held.
#[test]
fn the_published_payload_decodes_as_the_message() {
    let published = publish_row(user_input_row()).expect("the row restores");
    let data = published["data"].clone();
    let message: Message = serde_json::from_value(data)
        .expect("the published payload is a provider message");
    assert_eq!(message.content, Content::text("hello"));
}
