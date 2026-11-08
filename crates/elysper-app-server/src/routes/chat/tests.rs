use serde_json::json;

use super::*;

fn text_part(s: &str) -> ContentPart {
    ContentPart::Text(elysper::provider::TextContent {
        text: s.to_string(),
        cache_control: None,
    })
}

#[test]
fn build_text_request_routes_to_prompt_op() {
    let req = SendMessageRequest {
        text: "hi".into(),
        attachments: vec![WireAttachment {
            kind: "image".into(),
            data_base64: Some("BASE64".into()),
            mime_type: Some("image/png".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let parts = build_parts(&req, None);
    assert_eq!(parts.len(), 2);
}

#[test]
fn wire_attachment_attachment_hash_serde_round_trip() {
    // Round-trip an attachment_ref through serde. The chat
    // pipeline serialises the hash so the agent runtime can
    // resolve it through `AppState::attachment_store` later;
    // R12-3 wires the actual resolve step.
    let json = r#"{
            "kind": "attachment_ref",
            "mime_type": "image/png",
            "attachment_hash": "abc123def456"
        }"#;
    let a: WireAttachment = serde_json::from_str(json).unwrap();
    assert_eq!(a.kind, "attachment_ref");
    assert_eq!(a.attachment_hash.as_deref(), Some("abc123def456"));
    let back = serde_json::to_value(&a).unwrap();
    assert_eq!(back["attachment_hash"], "abc123def456");
    assert_eq!(back["mime_type"], "image/png");
}

#[test]
fn build_parts_skips_empty_text() {
    let req = SendMessageRequest {
        text: String::new(),
        attachments: vec![WireAttachment {
            kind: "audio".into(),
            data_base64: Some("AAAA".into()),
            mime_type: Some("audio/wav".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let parts = build_parts(&req, None);
    assert_eq!(parts.len(), 1);
    assert!(matches!(&parts[0], ContentPart::Audio(_)));
}

/// The controller's real user-prompt row is
/// `{"type":"UserInput","data":{"text":…}}`. It MUST be
/// recoverable, because it is the only shape the running system
/// writes — the previous matcher looked for
/// `{"role":…,"parts":[…]}` instead, found nothing in any real
/// log, and silently turned every regenerate into a cancel.
#[test]
fn extract_last_user_parts_reads_the_controller_user_input_row() {
    let events = vec![
        json!({"type": "UserInput", "data": {"text": "first"}}),
        json!({"type": "Model", "data": {"type": "text", "text": "reply"}}),
        json!({"type": "UserInput", "data": {"text": "second"}}),
    ];
    let parts = extract_last_user_parts(&events).expect("a user turn");
    assert_eq!(parts.len(), 1);
    match &parts[0] {
        ContentPart::Text(t) => assert_eq!(t.text, "second"),
        other => panic!("expected the text part, got {other:?}"),
    }
}

/// The lossless `Message` envelope (`SessionMemory::append`)
/// decodes too, so a log written by that adapter regenerates.
#[test]
fn extract_last_user_parts_reads_the_message_envelope() {
    let events = vec![json!({
        "type": "Message",
        "data": serde_json::to_value(Message::user("from memory"))
            .unwrap(),
    })];
    let parts = extract_last_user_parts(&events).expect("a user turn");
    match &parts[0] {
        ContentPart::Text(t) => assert_eq!(t.text, "from memory"),
        other => panic!("expected the text part, got {other:?}"),
    }
}

/// The typed `user_message` row carries a multi-part content
/// array; every part has to survive, not just the text.
#[test]
fn extract_last_user_parts_keeps_every_part_of_a_typed_row() {
    let events = vec![json!({
        "type": "user_message",
        "seq": 1,
        "data": {
            "role": "user",
            "content": [
                {"type": "text", "text": "what is this?"},
                {
                    "type": "image",
                    "data": "QUJD",
                    "mime_type": "image/png",
                    "detail": null,
                }
            ],
        }
    })];
    let parts = extract_last_user_parts(&events).expect("a user turn");
    assert_eq!(parts.len(), 2, "got {parts:?}");
    assert!(matches!(&parts[1], ContentPart::Image(_)));
}

/// A log with no user turn must yield `None` — that is the signal
/// `regenerate` uses to answer 202 without rerunning a turn that
/// does not exist.
#[test]
fn extract_last_user_parts_returns_none_without_a_user_turn() {
    let assistant_only =
        vec![json!({"type": "Model", "data": {"type": "text", "text": "hi"}})];
    assert!(extract_last_user_parts(&assistant_only).is_none());
    assert!(extract_last_user_parts(&[]).is_none());
    // A user-role *tool* row is not a user turn.
    let tool = vec![json!({
        "type": "Model",
        "data": {
            "type": "tool_result",
            "tool_use_id": "c1",
            "content": [{"type": "text", "text": "out"}],
        }
    })];
    assert!(extract_last_user_parts(&tool).is_none());
}

#[test]
fn build_text_only_request_routes_to_prompt_op() {
    // Sanity: text-only with no attachments should fall into
    // the text branch of `send_message`.
    let req = SendMessageRequest {
        text: "hello".into(),
        attachments: vec![],
        agent_name: None,
        model: None,
        delivery: Delivery::Queue,
    };
    let parts = build_parts(&req, None);
    assert_eq!(parts.len(), 1);
    assert_eq!(parts, vec![text_part("hello")]);
}

#[test]
fn build_multimodal_request_routes_to_prompt_multi() {
    let req = SendMessageRequest {
        text: "what is this?".into(),
        attachments: vec![WireAttachment {
            kind: "image".into(),
            data_base64: Some("IMG".into()),
            mime_type: Some("image/jpeg".into()),
            filename: Some("cat.jpg".into()),
            ..Default::default()
        }],
        agent_name: None,
        model: None,
        delivery: Delivery::Queue,
    };
    let parts = build_parts(&req, None);
    assert!(parts.iter().any(|p| !matches!(p, ContentPart::Text(_))));
}

#[test]
fn send_message_request_deserialises_model_field() {
    // The chat UI's model selector posts the selection as
    // `"model": "<provider>/<model>"`. The server must accept
    // the field (without failing the request) and route it to
    // the agent runtime — round-tripping through serde
    // proves the wire shape stays in sync with the React
    // page.
    let json = serde_json::json!({
        "text": "hi",
        "attachments": [],
        "agent_name": null,
        "model": "anthropic/claude-opus",
    });
    let req: SendMessageRequest = serde_json::from_value(json)
        .expect("SendMessageRequest must accept the `model` field");
    assert_eq!(req.model.as_deref(), Some("anthropic/claude-opus"));

    // The field is optional — older clients that omit it
    // (e.g. the regenerate/edit endpoints) still parse.
    let json_no_model = serde_json::json!({
        "text": "hi",
        "attachments": [],
        "agent_name": null,
    });
    let req_no_model: SendMessageRequest =
        serde_json::from_value(json_no_model)
            .expect("SendMessageRequest must tolerate a missing `model`");
    assert!(req_no_model.model.is_none());
}

/// R13-1: `kind = "attachment_ref"` resolves the hash through
/// the R11 attachment store and inlines the bytes as an
/// `Image` part — the full hash→store→bytes→wire round trip.
#[test]
fn attachment_ref_resolves_through_store_into_image_part() {
    // 1x1 transparent PNG (same fixture as the attachment
    // crate's tests).
    let png: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D,
        0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
        0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00,
        0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
        0x00, 0x00, 0x03, 0x00, 0x01, 0x6F, 0x32, 0x4D, 0x0E, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ];
    let store = elysper::attachment::AttachmentStore::default();
    let saved = store.save_image(&png, "image/png").unwrap();

    let req = SendMessageRequest {
        text: "what is this?".into(),
        attachments: vec![WireAttachment {
            kind: "attachment_ref".into(),
            attachment_hash: Some(saved.hash.clone()),
            mime_type: Some("image/png".into()),
            ..Default::default()
        }],
        ..Default::default()
    };
    let parts = build_parts(&req, Some(&store));
    // Text + resolved Image.
    assert_eq!(parts.len(), 2);
    match &parts[1] {
        ContentPart::Image(img) => {
            assert_eq!(img.mime_type, "image/png");
            let decoded = base64::engine::general_purpose::STANDARD
                .decode(&img.data)
                .unwrap();
            assert_eq!(decoded, png, "bytes must round-trip losslessly");
        }
        other => panic!("expected Image part, got {other:?}"),
    }
}

/// R13-1: an unresolvable hash (never uploaded) is skipped —
//—
/// the text part still ships so the turn isn't lost.
#[test]
fn attachment_ref_unknown_hash_skips_not_fails() {
    let store = elysper::attachment::AttachmentStore::default();
    let req = SendMessageRequest {
        text: "hello".into(),
        attachments: vec![WireAttachment {
            kind: "attachment_ref".into(),
            attachment_hash: Some("deadbeef".repeat(8)),
            ..Default::default()
        }],
        ..Default::default()
    };
    let parts = build_parts(&req, Some(&store));
    // Only the text part; the dead ref was skipped.
    assert_eq!(parts.len(), 1);
    assert!(matches!(&parts[0], ContentPart::Text(_)));
}

#[cfg(test)]
mod attachment_admission_tests {
    use crate::routes::chat::{
        SendMessageRequest,
        WireAttachment,
        admit_attachments,
        has_multimodal_input,
    };

    fn request(kind: &str) -> SendMessageRequest {
        SendMessageRequest {
            text: "look".into(),
            attachments: vec![WireAttachment {
                kind: kind.into(),
                data_base64: Some("BASE64".into()),
                mime_type: Some("image/png".into()),
                ..Default::default()
            }],
            ..Default::default()
        }
    }

    #[test]
    fn image_and_audio_need_the_attachments_service() {
        for kind in ["image", "audio", "attachment_ref"] {
            let req = request(kind);
            assert!(has_multimodal_input(&req), "{kind} is multimodal");
            assert!(
                admit_attachments(&req, false).is_err(),
                "{kind} is refused without the service"
            );
            assert!(
                admit_attachments(&req, true).is_ok(),
                "{kind} is admitted with the service"
            );
        }
    }

    #[test]
    fn text_only_turns_do_not_need_the_service() {
        let req = SendMessageRequest {
            text: "hello".into(),
            ..Default::default()
        };
        assert!(!has_multimodal_input(&req));
        assert!(admit_attachments(&req, false).is_ok());
    }
}
