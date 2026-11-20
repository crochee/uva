use serde_json::json;

use super::*;

fn text_part(s: &str) -> ContentPart {
    ContentPart::Text(uva::provider::TextContent {
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
    let store = uva::attachment::AttachmentStore::default();
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
    let store = uva::attachment::AttachmentStore::default();
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

// ---------------------------------------------------------------------------
// The feedback target
// ---------------------------------------------------------------------------

/// The feedback target is the rated row's **ordinal** — the `seq`
/// the session's writer stamped on it.
///
/// The rating is about an assistant message, so the server resolves
/// the ordinal against the session's own rows and refuses a target
/// that names no row or names a row that is not an answer. A rating
/// that names nothing is noise in an append-only audit log.
#[cfg(test)]
mod feedback_target_tests {
    use std::sync::Arc;

    use serde_json::Value;
    use uva::session::{SessionEvent, manager::SessionRegistry};

    use super::{FeedbackRequest, feedback};
    use crate::state::AppState;

    /// The session the fixture writes its rows into.
    const SESSION: &str = "feedback-session";

    /// A state whose session log holds one user turn and one
    /// assistant answer — the two rows a rating can name.
    ///
    /// The rows are written through the session's own writer, so they
    /// carry the ordinals the writer stamped. The controller is
    /// opened first: a cold open may repair a dangling turn, and
    /// that repair row would move the ordinals the test reads.
    async fn fixture() -> (Arc<AppState>, tempfile::TempDir, String) {
        let temp = tempfile::TempDir::new().expect("a temp workspace");
        let registry = SessionRegistry::new(temp.path().to_path_buf());
        let state = Arc::new(
            AppState::for_test(registry, temp.path().to_path_buf()).await,
        );
        let user = state.default_user_id().to_string();
        state
            .session_manager
            .create_with_user(SESSION.to_string(), user.clone())
            .await
            .expect("the session is created");
        state
            .get_or_create_session_controller(&user, SESSION)
            .await
            .expect("a controller");
        let writer = state.session_manager.writer(&user, SESSION);
        writer
            .append_event(&SessionEvent::UserMessage {
                seq: 0,
                ts: String::new(),
                data: serde_json::json!({"role": "user", "content": []}),
                source: None,
                surface_op: None,
                attachments: Vec::new(),
            })
            .await
            .expect("the user row is written");
        writer
            .append_event(&SessionEvent::AssistantMessage {
                seq: 0,
                ts: String::new(),
                data: serde_json::json!({"role": "assistant", "content": []}),
                surface_op: None,
            })
            .await
            .expect("the assistant row is written");
        writer.flush().await.expect("the rows are flushed");
        (state, temp, user)
    }

    /// The ordinal the log gave the row of that kind — the number a
    /// client saw on the wire.
    async fn ordinal_of(state: &AppState, user: &str, kind: &str) -> u64 {
        let rows = state
            .session_manager
            .sink(user, SESSION)
            .read()
            .await
            .expect("the log reads");
        rows.iter()
            .find(|row| row.get("type").and_then(Value::as_str) == Some(kind))
            .and_then(|row| row.get("seq").and_then(Value::as_u64))
            .unwrap_or_else(|| panic!("the log holds a `{kind}` row: {rows:?}"))
    }

    /// The rows the session's log holds.
    async fn rows_of(state: &AppState, user: &str) -> Vec<Value> {
        state
            .session_manager
            .sink(user, SESSION)
            .read()
            .await
            .expect("the log reads")
    }

    /// A target that names no row is refused: the log holds no such
    /// ordinal, so the rating would name nothing.
    #[tokio::test]
    async fn a_target_that_names_no_row_is_refused() {
        let (state, _temp, user) = fixture().await;
        let target = "9999".to_string();
        let error = feedback(
            Arc::clone(&state),
            target.clone(),
            FeedbackRequest {
                thumbs_up: true,
                session_id: SESSION.to_string(),
            },
        )
        .await
        .expect_err("a target that names no row is refused");
        assert_eq!(
            error.code, "feedback_target_unknown",
            "the refusal carries the family's machine code"
        );
        let rows = rows_of(&state, &user).await;
        assert!(
            !rows
                .iter()
                .any(|row| row.get("type").and_then(Value::as_str)
                    == Some("feedback")),
            "a refused rating writes no row: {rows:?}"
        );
    }

    /// A target that names a row that is not an answer is refused: a
    /// rating is about an assistant message.
    #[tokio::test]
    async fn a_target_that_names_a_non_assistant_row_is_refused() {
        let (state, _temp, user) = fixture().await;
        let target = ordinal_of(&state, &user, "user_message").await;
        let error = feedback(
            Arc::clone(&state),
            target.to_string(),
            FeedbackRequest {
                thumbs_up: true,
                session_id: SESSION.to_string(),
            },
        )
        .await
        .expect_err("a target that names a user turn is refused");
        assert_eq!(
            error.code, "feedback_target_unknown",
            "the refusal carries the family's machine code"
        );
        let rows = rows_of(&state, &user).await;
        assert!(
            !rows
                .iter()
                .any(|row| row.get("type").and_then(Value::as_str)
                    == Some("feedback")),
            "a refused rating writes no row: {rows:?}"
        );
    }

    /// A target that names the assistant's answer is persisted as it
    /// always was: the rating lands beside the answer it is about.
    #[tokio::test]
    async fn a_target_that_names_an_assistant_row_is_persisted() {
        let (state, _temp, user) = fixture().await;
        let target = ordinal_of(&state, &user, "assistant_message").await;
        feedback(
            Arc::clone(&state),
            target.to_string(),
            FeedbackRequest {
                thumbs_up: true,
                session_id: SESSION.to_string(),
            },
        )
        .await
        .expect("a target that names an answer is accepted");
        let rows = rows_of(&state, &user).await;
        let feedback = rows
            .iter()
            .find(|row| {
                row.get("type").and_then(Value::as_str) == Some("feedback")
            })
            .unwrap_or_else(|| panic!("the rating is on disk: {rows:?}"));
        assert_eq!(
            feedback.get("message_id").and_then(Value::as_str),
            Some(target.to_string().as_str()),
            "the rating names the row it is about"
        );
        assert_eq!(
            feedback.get("thumbs_up").and_then(Value::as_bool),
            Some(true),
            "the rating is on disk"
        );
    }
}

// ---------------------------------------------------------------------------
// The create_on_demand policy the chat surface asks for by name
// ---------------------------------------------------------------------------

mod create_on_demand_tests {
    use std::sync::Arc;

    use uva::session::manager::SessionRegistry;

    use super::{CreateSessionRequest, create_session, regenerate};
    use crate::state::AppState;

    /// A chat state with no session opened in it yet.
    async fn chat_state() -> (Arc<AppState>, tempfile::TempDir, String) {
        let temp = tempfile::TempDir::new().expect("a temp workspace");
        let registry = SessionRegistry::new(temp.path().to_path_buf());
        let state = Arc::new(
            AppState::for_test(registry, temp.path().to_path_buf()).await,
        );
        let user = state.default_user_id().to_string();
        (state, temp, user)
    }

    /// A session the chat surface is asked to create is durable and
    /// registered: the directory on disk **and** the registry entry.
    /// That two-layer side effect is the `create_on_demand` policy's
    /// contract, pinned here instead of left an accident of the
    /// resolve path.
    #[tokio::test]
    async fn a_session_the_chat_surface_creates_is_durable_and_registered() {
        let (state, temp, user) = chat_state().await;
        let answer = create_session(
            Arc::clone(&state),
            CreateSessionRequest {
                session_id: Some("brand-new".to_string()),
                agent_name: None,
            },
        )
        .await
        .expect("the session is created");
        assert_eq!(answer.session_id, "brand-new");
        assert!(
            temp.path().join(&user).join("brand-new").is_dir(),
            "the session's directory is minted on disk"
        );
        assert!(
            state.session_manager.get("brand-new").await.is_some(),
            "the registry holds the session from now on"
        );
    }

    /// Regenerate on an unknown session still answers the miss — the
    /// not-found that is never a cancel — and the session's
    /// existence is the policy's business now: the route asks for
    /// `create_on_demand` by name instead of reading the log first
    /// to steer around creation.
    #[tokio::test]
    async fn regenerate_on_an_unknown_session_leaves_creation_to_the_policy() {
        let (state, temp, user) = chat_state().await;
        let error = regenerate(Arc::clone(&state), "never-seen".to_string())
            .await
            .expect_err("a session with no user turn is a miss");
        assert_eq!(error.code, "not_found");
        assert_eq!(
            error.message,
            "not found: no user turn to regenerate in this session",
            "the miss is reported, never turned into a cancel"
        );
        assert!(
            state.session_manager.get("never-seen").await.is_some(),
            "the create_on_demand policy minted the registry entry"
        );
        assert!(
            temp.path().join(&user).join("never-seen").is_dir(),
            "the session's directory was minted by the policy"
        );
    }

    /// Two failures can sit on one regenerate — a log whose schema
    /// version this build does not read, and no user turn to replay
    /// — and the policy decides which answer wins: the controller
    /// is opened **before** the log is read for the turn, so the
    /// schema refusal is the answer, not the no-user-turn 404 the
    /// read-first patch used to give. The new priority is the more
    /// honest one: "this build cannot open the log" is a fact
    /// about the session, while "no user turn" would be a claim
    /// about a log this build cannot even read.
    #[tokio::test]
    async fn an_unopenable_log_outranks_the_missing_user_turn_on_regenerate() {
        let (state, temp, user) = chat_state().await;
        // A log from a newer build: the header names a schema
        // version this build refuses to read, and it holds no user
        // turn — both failures present at once.
        let dir = temp.path().join(&user).join("from-the-future");
        std::fs::create_dir_all(&dir).expect("the session's directory");
        std::fs::write(
            dir.join("events.jsonl"),
            "{\"_meta\":{\"kind\":\"uva_session_meta\",\
             \"schema_version\":9999,\"id\":\"from-the-future\"}}\n",
        )
        .expect("the transcript");
        let error =
            regenerate(Arc::clone(&state), "from-the-future".to_string())
                .await
                .expect_err("a log this build cannot open is refused");
        assert_eq!(
            error.code, "session",
            "opening the controller comes first; its refusal is the answer"
        );
        assert!(
            error.message.contains("schema version 9999"),
            "the refusal names the version it found: {}",
            error.message
        );
        assert!(
            !error.message.contains("no user turn"),
            "the schema refusal outranks the no-user-turn miss"
        );
    }

    /// The chat surface asks for its policy by name: the default
    /// resolve path and its creation-avoiding patch are gone from
    /// the surface's source.
    #[test]
    fn the_chat_surface_asks_for_its_policy_by_name() {
        let source = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("src/routes/chat.rs");
        let text = std::fs::read_to_string(&source).expect("readable source");
        assert!(
            !text.contains("get_or_create_session_controller"),
            "the chat surface names the create_on_demand policy, not the \
             default resolve path"
        );
        assert!(
            !text.contains("eagerly creates"),
            "the read-the-log-first patch comment is gone; the policy owns \
             the read-before-create timing"
        );
    }
}
