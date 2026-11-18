//! Interface tests for 归一 — the read boundary's answer to «what
//! row is this, in any format?». Everything goes through the
//! module's public face: [`restore_row`], [`Rows`] and the types
//! they hand back.

use serde_json::{Value, json};
use uva_provider::{Content, Message, Role};

use super::{CurrentRow, DECODE_ATTEMPTS, RowDefect, Rows, restore_row};
use crate::{
    events::{RowTag, SessionEvent},
    jsonl::SessionMetadataHeader,
};

/// The JSONL metadata header row exactly as the writer emits it —
/// the one legal row besides events a log carries.
fn header_row() -> Value {
    serde_json::to_value(SessionMetadataHeader::fresh("s-restore"))
        .expect("the header serialises")
}

/// A legacy `UserInput` envelope: the pre-typed controller's user
/// prompt.
fn user_input_row() -> Value {
    json!({"type": "UserInput", "data": {"text": "hello"}})
}

/// A legacy `Model` envelope: one streamed model part.
fn model_row(part: Value) -> Value {
    json!({"type": "Model", "data": part})
}

/// A legacy `Message` envelope: a whole provider message, in the
/// message's own wire shape.
fn message_row(role: &str) -> Value {
    let message = match role {
        "user" => Message::user("hi"),
        "assistant" => Message::assistant("hi"),
        "tool" => Message::tool(Content::text("hi"), "call_1"),
        _ => Message::new(Role::System, Content::text("hi")),
    };
    json!({
        "type": "Message",
        "data": serde_json::to_value(&message).expect("a message serialises")
    })
}

/// A current-vocabulary row.
fn typed_row(tag: RowTag, data: Value) -> Value {
    let mut row = json!({"type": tag.as_wire(), "data": data, "seq": 7, "ts": "2026-01-01T00:00:00Z"});
    if let Some(object) = row.as_object_mut() {
        object.insert("seq".into(), Value::from(7));
    }
    row
}

/// A dialect row restores to the current row it stands for.
///
/// The pairing is executed, not declared: a read site asks «is this
/// a user prompt» and gets one answer, whichever dialect the row was
/// written in.
#[test]
fn a_dialect_row_restores_to_its_current_form() {
    assert_eq!(
        restore_row(&user_input_row()).tag(),
        Some(RowTag::UserMessage)
    );
    assert_eq!(
        restore_row(&model_row(json!({"type": "text", "text": "hi"}))).tag(),
        Some(RowTag::AssistantMessage)
    );
    assert_eq!(
        restore_row(&model_row(json!({
            "type": "tool_use",
            "id": "call_1",
            "name": "read",
            "input": {}
        })))
        .tag(),
        Some(RowTag::AssistantMessage)
    );
    assert_eq!(
        restore_row(&model_row(json!({
            "type": "tool_result",
            "tool_use_id": "call_1",
            "content": [{"type": "text", "text": "body"}],
            "is_error": false
        })))
        .tag(),
        Some(RowTag::ToolResult)
    );
}

/// A role-polymorphic dialect is decided by the payload, not by the
/// dialect name.
///
/// `Message` holds a whole provider message, so the role in it says
/// which current row the envelope stands for. This is the case omp
/// shipped a real bug on: a tag that says «not a user message» with
/// a payload that says «user message».
#[test]
fn a_role_polymorphic_dialect_is_decided_by_the_payload() {
    assert_eq!(
        restore_row(&message_row("user")).tag(),
        Some(RowTag::UserMessage)
    );
    assert_eq!(
        restore_row(&message_row("assistant")).tag(),
        Some(RowTag::AssistantMessage)
    );
    assert_eq!(
        restore_row(&message_row("tool")).tag(),
        Some(RowTag::ToolResult)
    );
    // A system message has no current counterpart of its own; the
    // current vocabulary folds it with the user turn.
    assert_eq!(
        restore_row(&message_row("system")).tag(),
        Some(RowTag::UserMessage)
    );
}

/// A dialect with no current form says so, instead of pretending.
#[test]
fn a_dialect_with_no_current_form_is_a_defect() {
    let agent = json!({"type": "Agent", "data": {"child": "s-1"}});
    assert_eq!(restore_row(&agent).defect(), Some(RowDefect::NoCounterpart));
    assert_eq!(restore_row(&agent).tag(), None);

    // A model part the current vocabulary cannot name — an image.
    let image = model_row(
        json!({"type": "image", "data": "AAA", "mime_type": "image/png"}),
    );
    assert_eq!(restore_row(&image).defect(), Some(RowDefect::NoCounterpart));
}

/// A reasoning part is assistant content: the browser draws the
/// thinking segment for it today, so the restore must not drop it.
#[test]
fn a_reasoning_part_restores_to_an_assistant_message() {
    let row = model_row(json!({
        "type": "reasoning", "text": "thinking out loud"
    }));
    assert_eq!(restore_row(&row).tag(), Some(RowTag::AssistantMessage));
    let message = restore_row(&row)
        .message()
        .expect("the message carries the reasoning part");
    assert_eq!(
        serde_json::to_value(&message.content).unwrap()["Single"]["type"],
        "reasoning"
    );
}

/// A file-only turn is a turn: the browser draws a bubble with a chip
/// for it today, and the counterpart does not hinge on whether the
/// refs decode — a strange ref must not sink the prompt with them.
#[test]
fn a_file_only_user_input_row_restores() {
    let row = json!({
        "type": "UserInput",
        "data": {"attachments": [
            {"kind": "file", "name": "a.md", "byte_len": 12}
        ]}
    });
    assert_eq!(restore_row(&row).tag(), Some(RowTag::UserMessage));

    // The refs are junk JSON values, not decodable refs at all.
    let junk = json!({
        "type": "UserInput",
        "data": {"text": "hi", "attachments": ["junk"]}
    });
    assert_eq!(
        restore_row(&junk).tag(),
        Some(RowTag::UserMessage),
        "the counterpart never hinges on the refs decoding"
    );

    // Neither text nor refs: nothing the browser renders today.
    let empty = json!({"type": "UserInput", "data": {}});
    assert_eq!(restore_row(&empty).defect(), Some(RowDefect::NoCounterpart));
}

/// The `Message` dialect accepts the loose hand-written spelling the
/// current-row decoder accepts: the vocabulary is not stricter about
/// a dialect payload than about a current row's.
#[test]
fn a_loose_message_row_restores_to_the_roles_row() {
    let loose = json!({
        "type": "Message",
        "data": {
            "role": "assistant",
            "content": [{"type": "text", "text": "hand-written"}
            ]
        }
    });
    assert_eq!(restore_row(&loose).tag(), Some(RowTag::AssistantMessage));

    // A role the browser does not dispatch on stays a defect — the
    // fallback decoder is deliberately not used.
    let strange = json!({
        "type": "Message",
        "data": {"role": "orchestrator", "text": "hello"}
    });
    assert_eq!(
        restore_row(&strange).defect(),
        Some(RowDefect::NoCounterpart)
    );
}

/// The restored event carries the row's own facts — the ordinal, the
/// timestamp, the steer source, the attachment refs, the failure
/// flag — instead of the placeholders a bare rewrite would stamp.
#[test]
fn the_restored_event_carries_the_rows_own_facts() {
    let row = json!({
        "type": "UserInput",
        "seq": 9,
        "ts": "2026-02-02T00:00:00Z",
        "source": "steer",
        "data": {
            "text": "hello",
            "attachments": [
                {"kind": "file", "name": "a.md", "byte_len": 12}
            ]
        }
    });
    let SessionEvent::UserMessage {
        seq,
        ts,
        source,
        attachments,
        ..
    } = restore_row(&row).event().expect("the row restores")
    else {
        panic!("a UserInput row restores to a UserMessage event");
    };
    assert_eq!(seq, 9);
    assert_eq!(ts, "2026-02-02T00:00:00Z");
    assert_eq!(source.as_deref(), Some("steer"));
    assert_eq!(attachments.len(), 1);
    assert_eq!(attachments[0].name.as_deref(), Some("a.md"));

    let failed = json!({
        "type": "Model",
        "seq": 10,
        "data": {
            "type": "tool_result",
            "tool_use_id": "call_1",
            "content": [{"type": "text", "text": "boom"}],
            "is_error": true
        }
    });
    let SessionEvent::ToolResult { is_error, seq, .. } =
        restore_row(&failed).event().expect("the row restores")
    else {
        panic!("a tool-result Model row restores to a ToolResult event");
    };
    assert!(is_error, "the part's own flag rides the row");
    assert_eq!(seq, 10);
}

/// A typed row keeps its own tag: the payload never overrides it.
#[test]
fn a_typed_row_keeps_its_tag() {
    let row = typed_row(
        RowTag::UserMessage,
        json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
    );
    assert_eq!(restore_row(&row).tag(), Some(RowTag::UserMessage));
    assert_eq!(restore_row(&row).defect(), None);
}

/// An unknown tag and a typeless row are two different defects.
#[test]
fn an_unknown_tag_and_a_typeless_row_are_different_defects() {
    let unknown = json!({"type": "future_row", "data": {}});
    assert_eq!(
        restore_row(&unknown).defect(),
        Some(RowDefect::UnknownTag("future_row"))
    );

    let header = header_row();
    assert_eq!(restore_row(&header).defect(), Some(RowDefect::NoTag));
}

/// A log this build cannot interpret is refused — not silently
/// skipped.
///
/// Skipping an unknown row could rebuild a session that is not the
/// one that was logged.
#[test]
fn reading_a_log_refuses_an_unknown_type() {
    let rows = vec![
        typed_row(
            RowTag::UserMessage,
            json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
        ),
        json!({"type": "future_row", "data": {}}),
    ];
    let error = Rows::read(&rows).expect_err("the log carries an unknown type");
    assert_eq!(error.index, 1);
    assert_eq!(error.tag, "future_row");
}

/// A row marked ignorable may be skipped by a build that does not
/// know it.
#[test]
fn an_ignorable_row_does_not_refuse_the_log() {
    let rows = vec![
        typed_row(
            RowTag::UserMessage,
            json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
        ),
        json!({"type": "future_row", "data": {}, "ignorable": true}),
    ];
    let log = Rows::read(&rows).expect("an ignorable row is safe to skip");
    assert_eq!(log.iter().count(), 2);
}

/// Reading by tag compares the tag before the payload is decoded.
#[test]
fn the_tag_is_compared_before_the_payload_is_decoded() {
    let rows = vec![
        typed_row(
            RowTag::UserMessage,
            json!({"role": "user", "content": [{"type": "text", "text": "one"}]}),
        ),
        typed_row(
            RowTag::AssistantMessage,
            json!({"role": "assistant", "content": [{"type": "text", "text": "two"}]}),
        ),
        typed_row(
            RowTag::UserMessage,
            json!({"role": "user", "content": [{"type": "text", "text": "three"}]}),
        ),
    ];
    let log = Rows::read(&rows).expect("every row is current vocabulary");
    DECODE_ATTEMPTS.with(|count| count.set(0));
    let found: Vec<_> = log
        .of(RowTag::UserMessage)
        .filter_map(|row| row.event())
        .collect();
    assert_eq!(found.len(), 2);
    assert_eq!(
        DECODE_ATTEMPTS.with(|count| count.get()),
        2,
        "only the rows the tag selected paid for a decode"
    );
}

/// The last row of a kind is the last one that *carries* the kind.
///
/// A row whose payload does not decode is still the last row of that
/// kind — it is a defect, not a newer answer. A reader that wants
/// the last one that decodes says so.
#[test]
fn the_last_row_of_a_kind_is_the_last_one_that_carries_it() {
    let rows = vec![
        typed_row(
            RowTag::RequestSnapshot,
            json!({"provider": "openai", "model": "gpt"}),
        ),
        typed_row(RowTag::RequestSnapshot, json!({"broken": true})),
    ];
    let log = Rows::read(&rows).expect("every row is current vocabulary");
    let last = log
        .last_of(RowTag::RequestSnapshot)
        .expect("a row of that kind");
    assert!(
        last.event().is_none(),
        "the broken tail row does not decode"
    );
}

/// A dialect row restores into the current event shape, so a reader
/// never has to know it was written in a dialect.
#[test]
fn a_dialect_row_restores_to_the_current_event_shape() {
    let event = restore_row(&user_input_row())
        .event()
        .expect("the envelope restores");
    assert!(
        matches!(event, SessionEvent::UserMessage { .. }),
        "the UserInput envelope is a user_message row now"
    );
}

/// A dialect row projects to the message it carries.
#[test]
fn a_dialect_row_projects_to_the_message_it_carries() {
    let message = restore_row(&user_input_row())
        .message()
        .expect("the envelope carries a message");
    assert_eq!(message.content.extract_text().as_deref(), Some("hello"));
}

/// A known tag whose payload does not decode is a defect, not a
/// dialect: the row is still a current row.
#[test]
fn a_broken_payload_is_not_a_dialect() {
    let row = typed_row(RowTag::UserMessage, json!({"not": "a message"}));
    let current = restore_row(&row);
    assert_eq!(current.tag(), Some(RowTag::UserMessage));
    assert_eq!(current.defect(), None);
    assert!(
        current.message().is_none(),
        "the payload carries no message"
    );
}

/// The surface membership comes from the declared role, not from a
/// guess.
#[test]
fn the_surface_membership_comes_from_the_declared_role() {
    let message_row = typed_row(
        RowTag::UserMessage,
        json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
    );
    assert!(restore_row(&message_row).is_surface_row());

    let log_row = typed_row(RowTag::Usage, json!({"prompt_tokens": 1}));
    assert!(!restore_row(&log_row).is_surface_row());
}

/// A current row carries the ordinal the log gave it; a dialect row
/// carries none of its own.
#[test]
fn the_ordinal_is_the_row_s_own() {
    let row = typed_row(
        RowTag::UserMessage,
        json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
    );
    assert_eq!(restore_row(&row).seq(), Some(7));
    assert_eq!(restore_row(&user_input_row()).seq(), None);
}

/// The restoration is total: any input at all yields a row that says
/// what it is.
#[test]
fn the_restoration_is_total() {
    for row in [
        Value::Null,
        json!([]),
        json!({}),
        json!({"type": 42}),
        json!({"type": ""}),
        json!({"type": "nope"}),
    ] {
        let current = restore_row(&row);
        assert!(
            current.tag().is_some() || current.defect().is_some(),
            "every row is either a current row or names its defect"
        );
    }
}

/// A current row answers «is this row kind X» in one call, with no
/// dialect arm a reader could forget.
#[test]
fn a_current_row_answers_which_kind_it_is() {
    let envelope = user_input_row();
    let row: CurrentRow<'_> = restore_row(&envelope);
    assert!(row.restores_to(RowTag::UserMessage));
    assert!(!row.restores_to(RowTag::AssistantMessage));
}
