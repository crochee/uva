use uva::session::manager::SERVER_DEFAULT_USER_ID;

use super::*;

#[test]
fn parse_rfc3339_returns_none_for_invalid_input() {
    assert!(parse_rfc3339(None).is_none());
    assert!(parse_rfc3339(Some("not-a-date")).is_none());
}

/// The namespace test is "the id starts with the prefix", and the
/// prefix carries its own `-` separator. `anthropic-<uuid>` is the
/// mint; an id that merely shares the letters is not, and neither
/// is the empty string or a bare prefixless name.
#[test]
fn is_ephemeral_session_matches_only_the_separated_namespace() {
    for id in ["anthropic-1f9c", "anthropic-", "anthropic-0"] {
        assert!(is_ephemeral_session(id), "{id} is in the namespace");
    }
    for id in ["anthropicx-1", "anthropic", "xanthropic-1", "chat-1", ""] {
        assert!(!is_ephemeral_session(id), "{id} is not in the namespace");
    }
}

#[test]
fn parse_rfc3339_roundtrips_valid_timestamp() {
    let raw = "2026-08-22T10:00:00+00:00";
    let parsed = parse_rfc3339(Some(raw)).expect("valid");
    assert_eq!(parsed.to_rfc3339(), "2026-08-22T10:00:00+00:00");
}

/// `live_session_state` maps the DashMap key to the
/// controller state and returns `None` when absent.
#[tokio::test]
async fn live_session_state_none_when_controller_absent() {
    let map: DashMap<(String, String), Arc<SessionController>> = DashMap::new();
    assert_eq!(live_session_state(&map, "alice", "s-missing"), None);
}

#[test]
fn session_detail_serialises_with_legacy_artifacts_field() {
    let detail = SessionDetail {
        id: "abc".into(),
        status: SessionStatus::Completed,
        context_id: "abc".into(),
        created_at: None,
        updated_at: None,
        history: vec![
            serde_json::json!({"type": "Model", "data": {"text": "hi"}}),
        ],
        artifacts: Vec::new(),
        parent_session_id: None,
        agent_name: None,
    };
    let v = serde_json::to_value(&detail).expect("serialise");
    assert_eq!(v["id"], "abc");
    assert_eq!(v["status"], "completed");
    assert!(v["history"].is_array());
    // The frontend relies on the `artifacts` field being
    // present (even as `[]`) so it can iterate without
    // special-casing undefined.
    assert!(v["artifacts"].is_array());
}

/// R5-10: `TypedEventResponse` serialises the typed event
/// split (typed / legacy / unknown / broken) with counts. Pin the
/// field shape so the JSON contract is locked.
#[test]
fn typed_event_response_serialises_with_counts() {
    let resp = TypedEventResponse {
        id: "abc".to_string(),
        typed_count: 2,
        legacy_count: 1,
        unknown_count: 3,
        broken_count: 4,
        typed: Vec::new(),
        legacy: Vec::new(),
        unknown: Vec::new(),
        broken: Vec::new(),
    };
    let v = serde_json::to_value(&resp).expect("serialise");
    assert_eq!(v["id"], "abc");
    assert_eq!(v["typed_count"], 2);
    assert_eq!(v["legacy_count"], 1);
    assert_eq!(v["unknown_count"], 3);
    assert_eq!(v["broken_count"], 4);
    assert_eq!(v["typed"], serde_json::json!([]));
    assert_eq!(v["legacy"], serde_json::json!([]));
    assert_eq!(v["unknown"], serde_json::json!([]));
    assert_eq!(v["broken"], serde_json::json!([]));
}

/// The four buckets of the events response say what each row **is**.
///
/// A tag this build does not know is version skew — a row a newer
/// build wrote. One bucket for it and for the old dialects said
/// "history" about a row that is neither, and a client could not
/// tell a row it must keep from a row it must not interpret.
///
/// An old dialect is no longer history either: the row vocabulary
/// restores it to a current row, so it answers in `typed` — the
/// split reports what the row **is**, not how it was written.
#[tokio::test]
async fn the_events_response_restores_old_dialects_and_keeps_unknown_tags_apart()
 {
    let (state, dir) = durable_state().await;
    write_current_transcript(
        dir.path(),
        "mixed",
        &[
            serde_json::json!({
                "type": "user_message",
                "seq": 1,
                "ts": "2026-09-25T08:00:00+00:00",
                "data": {"role": "user", "content": "hi"},
            }),
            serde_json::json!({
                "type": "UserInput",
                "seq": 2,
                "ts": "2026-09-25T08:00:01+00:00",
                "data": {"text": "hi"},
            }),
            serde_json::json!({
                "type": "row_from_the_future",
                "seq": 3,
                "ts": "2026-09-25T08:00:02+00:00",
                "data": {},
            }),
        ],
    );
    state
        .session_manager
        .create_with_user(
            "mixed".to_string(),
            SERVER_DEFAULT_USER_ID.to_string(),
        )
        .await
        .expect("the session is registered");

    let response = list_session_events(Arc::clone(&state), "mixed".to_string())
        .await
        .expect("the events read");

    assert_eq!(
        response.typed_count, 2,
        "the typed row and the restored dialect row both decode"
    );
    assert_eq!(
        response.legacy_count, 0,
        "a dialect the vocabulary restores is a current row"
    );
    assert_eq!(response.unknown_count, 1, "the unknown tag is version skew");
    assert_eq!(response.broken_count, 0, "no row is unreadable");
    assert_eq!(response.unknown[0]["type"], "row_from_the_future");
    // The restored row is the user message the legacy envelope held.
    let restored = response
        .typed
        .iter()
        .find_map(|event| match event {
            uva::session::SessionEvent::UserMessage { data, .. } => {
                serde_json::from_value::<uva::provider::Message>(data.clone())
                    .ok()
            }
            _ => None,
        })
        .expect("the legacy prompt is a user message now");
    assert_eq!(
        restored.content.extract_text().as_deref(),
        Some("hi"),
        "the restored row carries the text the legacy envelope held"
    );
}

/// A known tag whose payload does not decode is a **defect**, not a
/// dialect: the row is in the log, this build knows what it is, and
/// the payload is broken. It never becomes history.
#[tokio::test]
async fn the_events_response_reports_a_broken_row_apart_from_history() {
    let (state, dir) = durable_state().await;
    write_current_transcript(
        dir.path(),
        "broken",
        &[
            serde_json::json!({
                "type": "user_message",
                "seq": 1,
                "ts": "2026-09-25T08:00:00+00:00",
                "data": {"role": "user", "content": "hi"},
            }),
            serde_json::json!({
                "type": "tool_result",
                "data": {"role": "tool"},
            }),
        ],
    );
    state
        .session_manager
        .create_with_user(
            "broken".to_string(),
            SERVER_DEFAULT_USER_ID.to_string(),
        )
        .await
        .expect("the session is registered");

    let response =
        list_session_events(Arc::clone(&state), "broken".to_string())
            .await
            .expect("the events read");

    assert_eq!(response.typed_count, 1, "the sound row decodes");
    assert_eq!(response.legacy_count, 0, "a broken row is not history");
    assert_eq!(response.unknown_count, 0, "its tag is known");
    assert_eq!(response.broken_count, 1, "the broken row is reported");
}

/// The `broken` bucket is the vocabulary's «this build cannot read
/// this row» answer, not a local re-take on it: a current row whose
/// payload does not decode **and** a row with no usable tag. A
/// dialect that restores and an unfamiliar tag stay readable — a
/// current row and version skew, in their own buckets.
///
/// The `legacy` bucket is what is left of the old dialects: a row
/// written in one the current vocabulary has no counterpart for
/// (the `Agent` envelope, whose delegated child lives in its own
/// session now).
#[test]
fn classify_rows_buckets_every_unreadable_row_as_broken() {
    let raw = vec![
        serde_json::json!({
            "type": "user_message",
            "seq": 1,
            "ts": "2026-09-25T08:00:00+00:00",
            "data": {"role": "user", "content": "hi"},
        }),
        serde_json::json!({"type": "UserInput", "data": {"text": "hi"}}),
        serde_json::json!({"type": "Agent", "data": {}}),
        serde_json::json!({"type": "a_tag_this_build_predates"}),
        serde_json::json!({"type": "feedback", "seq": "broken"}),
        serde_json::json!({"seq": 5}),
        serde_json::json!("not an object"),
    ];
    let rows = classify_rows(raw);

    assert_eq!(
        rows.typed.len(),
        2,
        "the sound row and the restored dialect row decode"
    );
    assert_eq!(
        rows.legacy.len(),
        1,
        "a dialect the vocabulary cannot restore is history"
    );
    assert_eq!(rows.legacy[0]["type"], "Agent");
    assert_eq!(rows.unknown.len(), 1, "the unfamiliar tag is version skew");
    assert_eq!(
        rows.broken,
        vec![
            serde_json::json!({"type": "feedback", "seq": "broken"}),
            serde_json::json!({"seq": 5}),
            serde_json::json!("not an object"),
        ],
        "a known tag that fails to decode, and every row with no usable tag",
    );
}

/// A hermetic `AppState` over a temp workspace: these tests build a
/// real `sessions/<user>/<id>/events.jsonl` tree, so the caller
/// keeps the tempdir alive.
async fn durable_state() -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp workspace");
    let sessions = uva::session::manager::SessionRegistry::new(
        dir.path().join("sessions"),
    );
    let state = AppState::for_test(sessions, dir.path().to_path_buf()).await;
    (Arc::new(state), dir)
}

/// Write `lines` to the transcript of `session_id`, the layout a
/// previous process leaves behind: headerless, a v0 log. The
/// listing's scan still reaches it through the tolerant legacy
/// window, and so does the detail view's streaming reader; the
/// registered loop's sink reads (`read_head` / `read_tail`)
/// refuse it, which is what [`write_current_transcript`] is for.
fn write_transcript(
    workspace: &std::path::Path,
    session_id: &str,
    lines: &[Value],
) {
    let dir = workspace
        .join("sessions")
        .join(SERVER_DEFAULT_USER_ID)
        .join(session_id);
    std::fs::create_dir_all(&dir).expect("session dir");
    let body: String = lines.iter().map(|line| format!("{line}\n")).collect();
    std::fs::write(dir.join("events.jsonl"), body).expect("transcript");
}

/// Write `lines` to the transcript of `session_id` in the layout
/// **this** build writes: the schema-version metadata header
/// first, then the rows.
///
/// A headerless transcript is a v0 log — a file this build
/// refuses to read, and therefore one no session may be opened
/// from. Only the detail view's streaming reader still tolerates
/// one (it skips the header when there is one), which is why
/// [`write_transcript`] can stay headerless for it; the listing's
/// scan and any test that opens the session through the
/// controller need the current layout.
fn write_current_transcript(
    workspace: &std::path::Path,
    session_id: &str,
    lines: &[Value],
) {
    let dir = workspace
        .join("sessions")
        .join(SERVER_DEFAULT_USER_ID)
        .join(session_id);
    std::fs::create_dir_all(&dir).expect("session dir");
    let header = serde_json::json!({
        "_meta": {
            "kind": "uva_session_meta",
            "schema_version": 2,
            "id": session_id,
        }
    });
    let mut body = serde_json::to_string(&header).expect("the header");
    body.push('\n');
    for line in lines {
        body.push_str(&serde_json::to_string(line).expect("a row"));
        body.push('\n');
    }
    std::fs::write(dir.join("events.jsonl"), body).expect("transcript");
}

/// The session directory for `session_id` under a temp workspace.
fn session_dir(
    workspace: &std::path::Path,
    session_id: &str,
) -> std::path::PathBuf {
    workspace
        .join("sessions")
        .join(SERVER_DEFAULT_USER_ID)
        .join(session_id)
}

/// `DELETE` removes the log *and* the directory, and the id stops
/// being addressable through either read surface.
#[tokio::test]
async fn delete_session_removes_the_transcript_and_the_registry_entry() {
    let (state, dir) = durable_state().await;
    write_transcript(
        dir.path(),
        "doomed",
        &[serde_json::json!({
            "type": "UserInput",
            "ts": "2026-09-25T08:00:00+00:00",
            "data": {"text": "hello"},
        })],
    );

    delete_session(Arc::clone(&state), "doomed".to_string())
        .await
        .expect("delete");
    assert!(
        !session_dir(dir.path(), "doomed").exists(),
        "the whole session directory must go, not just its log",
    );

    match get_session(Arc::clone(&state), "doomed".to_string()).await {
        Err(err) => assert_eq!(err.code, "not_found"),
        Ok(_) => panic!("a deleted session must not be readable"),
    }
    let list = list_sessions(Arc::clone(&state), SessionPageQuery::new())
        .await
        .expect("list");
    assert!(
        !list.data.iter().any(|row| row.id == "doomed"),
        "a deleted session must not stay in the listing",
    );
}

/// `DELETE` evicts the session's controller *and* its cold-open
/// gate.
///
/// The gate belongs to the controller, so it leaves with the
/// controller — through this path or through any other that drops
/// it. A map that only one eviction path cleaned would keep one
/// entry per session this process opened, and a re-created session
/// of the same name would queue behind a gate nobody holds.
#[tokio::test]
async fn deleting_a_session_evicts_its_cold_open_gate() {
    let (state, dir) = durable_state().await;
    let user = state.default_user_id().to_string();
    // The current layout: a session is opened from a log this
    // build can read.
    write_current_transcript(
        dir.path(),
        "gated",
        &[serde_json::json!({
            "type": "UserInput",
            "ts": "2026-09-25T08:00:00+00:00",
            "data": {"text": "hello"},
        })],
    );
    let controller = state
        .get_or_create_session_controller(&user, "gated")
        .await
        .expect("the session opens");
    let key = (user, "gated".to_string());
    assert!(state.has_spawn_gate(&key), "the open created a gate");
    assert!(state.active_sessions.get(&key).is_some());

    delete_session(Arc::clone(&state), "gated".to_string())
        .await
        .expect("delete");
    assert!(
        state.active_sessions.get(&key).is_none(),
        "and so must the controller"
    );
    // The handle the caller held is closed, not merely unreferenced
    // — and it is the last one to the controller, so the gate
    // leaves with it.
    drop(controller);
    assert!(
        !state.has_spawn_gate(&key),
        "the gate must leave with the controller"
    );
}

/// An id neither source knows is refused — and the delete creates
/// nothing on the way there.
#[tokio::test]
async fn delete_session_is_not_found_for_an_unknown_id() {
    let (state, dir) = durable_state().await;
    let error = delete_session(Arc::clone(&state), "never-existed".to_string())
        .await
        .expect_err("an unknown id must not delete");
    assert_eq!(error.code, "not_found");
    assert!(
        !session_dir(dir.path(), "never-existed").exists(),
        "a refused delete must not leave an empty session directory behind",
    );
}

/// Export is the file, not the projection: append order kept, key
/// order in each row preserved, blank and torn lines dropped.
#[tokio::test]
async fn export_session_returns_the_raw_durable_rows() {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

    let (state, dir) = durable_state().await;
    let session = session_dir(dir.path(), "exportable");
    std::fs::create_dir_all(&session).expect("session dir");
    // `b` before `a` is deliberate: a parse-then-reserialise
    // projection would reorder it, and a torn final line must not
    // reach the archive.
    std::fs::write(
        session.join("events.jsonl"),
        "{\"b\":1,\"a\":2}\n\n{\"type\":\"Model\"}\n{\"torn\":\n",
    )
    .expect("transcript");

    let export = export_session(Arc::clone(&state), "exportable".to_string())
        .await
        .expect("export");
    assert_eq!(export.content_type, "application/x-ndjson");
    assert_eq!(export.filename, "exportable.jsonl");
    let bytes = BASE64
        .decode(export.content_base64)
        .expect("the artifact decodes");
    assert_eq!(
        std::str::from_utf8(&bytes).expect("utf-8"),
        "{\"b\":1,\"a\":2}\n{\"type\":\"Model\"}\n",
    );
}

/// A registered session whose log is empty exports zero rows: it
/// is not an unknown id.
#[tokio::test]
async fn export_session_serves_a_registered_session_with_an_empty_log() {
    use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};

    let (state, _dir) = durable_state().await;
    state
        .session_manager
        .create_with_user(
            "blank".to_string(),
            SERVER_DEFAULT_USER_ID.to_string(),
        )
        .await
        .expect("register");

    let export = export_session(Arc::clone(&state), "blank".to_string())
        .await
        .expect("export");
    let bytes = BASE64
        .decode(export.content_base64)
        .expect("the artifact decodes");
    assert!(bytes.is_empty(), "an empty log exports nothing: {bytes:?}");
}

/// Export of an id neither source knows is not found.
#[tokio::test]
async fn export_session_is_not_found_for_an_unknown_id() {
    let (state, _dir) = durable_state().await;
    let error = export_session(Arc::clone(&state), "never-existed".to_string())
        .await
        .expect_err("an unknown id must not export");
    assert_eq!(error.code, "not_found");
}

/// Fork copies the parent's transcript into a new session that is
/// addressable on both read surfaces, and leaves the parent alone.
#[tokio::test]
async fn fork_session_copies_the_transcript_into_an_addressable_child() {
    let (state, dir) = durable_state().await;
    write_transcript(
        dir.path(),
        "origin",
        &[
            serde_json::json!({
                "type": "UserInput",
                "ts": "2026-09-25T08:00:00+00:00",
                "data": {"text": "hello"},
            }),
            serde_json::json!({
                "type": "Model",
                "ts": "2026-09-25T08:00:05+00:00",
                "data": {"type": "text", "text": "hi"},
            }),
        ],
    );

    let forked = fork_session(
        Arc::clone(&state),
        "origin".to_string(),
        ForkSessionRequest {
            from_stream_index: None,
        },
    )
    .await
    .expect("fork");
    assert_eq!(forked.rows, 2);
    assert_eq!(forked.forked_from, "origin");
    assert_ne!(forked.session_id, "origin");

    let child = get_session(Arc::clone(&state), forked.session_id.clone())
        .await
        .expect("the child is a session");
    assert_eq!(child.history.len(), 2, "the child inherits the prefix");

    let parent = get_session(Arc::clone(&state), "origin".to_string())
        .await
        .expect("the parent still reads");
    assert_eq!(parent.history.len(), 2, "the parent is untouched");
}

/// A parent written **through the session's writer** carries a
/// schema-version metadata header on its first line. The fork
/// must copy the parent's events, not that header: a copied
/// header would land mid-log in the child, where `read_filtered`
/// (which strips only the first line) parses it as a phantom
/// event and inflates `last_event_seq`. The headerless
/// `write_transcript` fixture above cannot exercise this path,
/// so this test seeds the parent through the registry's writer.
#[tokio::test]
async fn fork_session_drops_the_parent_metadata_header() {
    let (state, _dir) = durable_state().await;
    let parent = state
        .session_manager
        .writer(SERVER_DEFAULT_USER_ID, "meta-origin");
    parent
        .append_event(&uva::session::user_message(
            serde_json::to_value(uva::provider::Message::user("hello"))
                .expect("a user message serialises"),
            None,
        ))
        .await
        .expect("seed parent row 1");
    parent
        .append_event(&uva::session::assistant_message(
            serde_json::to_value(uva::provider::Message::assistant("hi"))
                .expect("an assistant message serialises"),
        ))
        .await
        .expect("seed parent row 2");
    parent.flush().await.expect("seed parent flush");
    let parent_seq = state
        .session_manager
        .sink(SERVER_DEFAULT_USER_ID, "meta-origin")
        .snapshot()
        .await
        .expect("parent snapshot");
    assert_eq!(
        parent_seq.last_event_seq, 2,
        "the parent's header must not count as an event",
    );

    let forked = fork_session(
        Arc::clone(&state),
        "meta-origin".to_string(),
        ForkSessionRequest {
            from_stream_index: None,
        },
    )
    .await
    .expect("fork");
    assert_eq!(
        forked.rows, 2,
        "the child inherits the parent's 2 events, not its header",
    );

    let child = state
        .session_manager
        .sink(SERVER_DEFAULT_USER_ID, &forked.session_id);
    let child_rows = child.read().await.expect("child read");
    assert_eq!(
        child_rows.len(),
        2,
        "no phantom `_meta` row in the child: {child_rows:?}",
    );
    assert!(
        child_rows.iter().all(|row| row.get("_meta").is_none()),
        "the child must hold events only: {child_rows:?}",
    );
    assert_eq!(
        child
            .snapshot()
            .await
            .expect("child snapshot")
            .last_event_seq,
        2,
        "the child's seq counts its own header + the 2 events only",
    );
}

/// `from_stream_index` forks as of a point: the rows past it stay
/// behind.
#[tokio::test]
async fn fork_session_truncates_at_the_requested_stream_index() {
    let (state, dir) = durable_state().await;
    write_transcript(
        dir.path(),
        "long",
        &[
            serde_json::json!({
                "stream_index": 1,
                "type": "UserInput",
                "data": {"text": "a"},
            }),
            serde_json::json!({
                "stream_index": 2,
                "type": "Model",
                "data": {"type": "text", "text": "b"},
            }),
            serde_json::json!({
                "stream_index": 3,
                "type": "UserInput",
                "data": {"text": "c"},
            }),
        ],
    );

    let forked = fork_session(
        Arc::clone(&state),
        "long".to_string(),
        ForkSessionRequest {
            from_stream_index: Some(2),
        },
    )
    .await
    .expect("fork");
    assert_eq!(forked.rows, 2, "only the rows at or below index 2");

    let child = get_session(Arc::clone(&state), forked.session_id.clone())
        .await
        .expect("the child is a session");
    assert_eq!(child.history.len(), 2);
    assert_eq!(child.history[1]["type"], "assistant_message");
    assert_eq!(child.history[1]["data"]["content"]["Single"]["text"], "b");
}

/// The child's future is its own: a row appended after the fork
/// lands in the child's log only, at the ordinal *after* the
/// re-stamped inherited rows — the child's counter follows the
/// child's own sequence.
#[tokio::test]
async fn fork_session_gives_the_child_an_independent_future() {
    let (state, dir) = durable_state().await;
    write_transcript(
        dir.path(),
        "base",
        &[
            serde_json::json!({
                "type": "UserInput",
                "ts": "2026-09-25T08:00:00+00:00",
                "data": {"text": "alpha"},
            }),
            serde_json::json!({
                "type": "Model",
                "ts": "2026-09-25T08:00:05+00:00",
                "data": {"type": "text", "text": "beta"},
            }),
        ],
    );

    let forked = fork_session(
        Arc::clone(&state),
        "base".to_string(),
        ForkSessionRequest {
            from_stream_index: None,
        },
    )
    .await
    .expect("fork");

    let child_writer = state
        .session_manager
        .writer(SERVER_DEFAULT_USER_ID, &forked.session_id);
    child_writer
        .append_event(&uva::session::user_message(
            serde_json::to_value(uva::provider::Message::user("gamma"))
                .expect("a user message serialises"),
            None,
        ))
        .await
        .expect("append to the child");
    child_writer.flush().await.expect("child flush");
    let snapshot = state
        .session_manager
        .sink(SERVER_DEFAULT_USER_ID, &forked.session_id)
        .snapshot()
        .await
        .expect("snapshot");
    assert_eq!(
        snapshot.last_event_seq, 3,
        "the inherited rows occupy 1..=2"
    );

    let child = get_session(Arc::clone(&state), forked.session_id.clone())
        .await
        .expect("the child is a session");
    assert_eq!(child.history.len(), 3);
    let parent = get_session(Arc::clone(&state), "base".to_string())
        .await
        .expect("the parent still reads");
    assert_eq!(
        parent.history.len(),
        2,
        "the parent must not gain the child's row",
    );
}

/// A registered session with no rows forks into an empty child: it
/// is not an unknown id.
#[tokio::test]
async fn fork_session_of_a_registered_session_with_an_empty_log() {
    let (state, _dir) = durable_state().await;
    state
        .session_manager
        .create_with_user(
            "fresh".to_string(),
            SERVER_DEFAULT_USER_ID.to_string(),
        )
        .await
        .expect("register");

    let forked = fork_session(
        Arc::clone(&state),
        "fresh".to_string(),
        ForkSessionRequest {
            from_stream_index: None,
        },
    )
    .await
    .expect("fork");
    assert_eq!(forked.rows, 0);

    let child = get_session(Arc::clone(&state), forked.session_id.clone())
        .await
        .expect("the child is registered");
    assert!(child.history.is_empty());
}

/// Forking an id neither source knows is refused, and creates
/// nothing.
#[tokio::test]
async fn fork_session_is_not_found_for_an_unknown_id() {
    let (state, dir) = durable_state().await;
    let error = fork_session(
        Arc::clone(&state),
        "never-existed".to_string(),
        ForkSessionRequest {
            from_stream_index: None,
        },
    )
    .await
    .expect_err("an unknown parent must not fork");
    assert_eq!(error.code, "not_found");
    assert!(
        !session_dir(dir.path(), "never-existed").exists(),
        "a refused fork must not leave an empty session directory behind",
    );
}

/// A turn that is still writing owns its log. `Completing` counts
/// as busy as well, even though `is_terminal` reports it terminal:
/// the flush is still appending.
#[test]
fn turn_in_flight_covers_running_and_completing_only() {
    use uva::session::OperationState;

    assert!(turn_in_flight(&OperationState::Running { iteration: 0 }));
    assert!(turn_in_flight(&OperationState::Completing));
    assert!(!turn_in_flight(&OperationState::Idle));
    assert!(!turn_in_flight(&OperationState::Failed {
        error_message: "boom".to_string(),
    }));
    assert!(!turn_in_flight(&OperationState::Cancelled {
        reason: "stop".to_string(),
    }));
}

/// The listing reaches a conversation recorded by an earlier
/// process: the registry is empty after a restart, so before the
/// merge every pre-restart conversation vanished from the Sessions
/// page.
#[tokio::test]
async fn list_sessions_includes_a_session_known_only_from_disk() {
    let (state, dir) = durable_state().await;
    write_transcript(
        dir.path(),
        "earlier-run",
        &[
            serde_json::json!({
                "type": "UserInput",
                "ts": "2026-09-19T08:00:00+00:00",
                "data": {"text": "hello"},
            }),
            serde_json::json!({
                "type": "Model",
                "ts": "2026-09-19T08:00:05+00:00",
                "data": {"text": "hi"},
            }),
        ],
    );

    let list = list_sessions(Arc::clone(&state), SessionPageQuery::new())
        .await
        .expect("list");

    let row = list
        .data
        .iter()
        .find(|row| row.id == "earlier-run")
        .expect("a durable transcript must be listed");
    assert_eq!(row.status, SessionStatus::Completed);
    assert_eq!(row.context_id.as_deref(), Some("earlier-run"));
    // The creation time is the transcript's first event, not the
    // time of the request.
    assert_eq!(
        row.created_at.expect("first event ts").to_rfc3339(),
        "2026-09-19T08:00:00+00:00"
    );
}

/// The rows of a turn that asked for approval and was suspended
/// waiting for it — the log shape `open_requests` judges, on both
/// of the listing's paths.
fn suspended_ask_rows() -> Vec<Value> {
    [
        uva::session::turn_start(3),
        uva::session::approval_asked(
            "a1", "c1", "shell", "sha256:x", "high", "default",
        ),
        uva::session::turn_suspended(vec!["c1".into()]),
        uva::session::turn_end_suspended(),
    ]
    .into_iter()
    .map(|event| serde_json::to_value(&event).expect("the row"))
    .collect()
}

/// A registered session suspended on an undecided approval is the
/// one status the log must win over the controller's in-memory
/// view: the listing reports `input_required` (Review Focus
/// item 4). The ask is unmatched and sits at the log's tail, so
/// the bounded tail window judges it exactly.
#[tokio::test]
async fn list_sessions_reports_input_required_for_a_registered_session_awaiting_approval()
 {
    let (state, dir) = durable_state().await;
    state
        .session_manager
        .create_with_user(
            "paused-registered".to_string(),
            state.default_user_id().to_string(),
        )
        .await
        .expect("register the paused session");
    write_current_transcript(
        dir.path(),
        "paused-registered",
        &suspended_ask_rows(),
    );

    let list = list_sessions(Arc::clone(&state), SessionPageQuery::new())
        .await
        .expect("list");

    let row = list
        .data
        .iter()
        .find(|row| row.id == "paused-registered")
        .expect("the paused session is listed");
    assert_eq!(row.status, SessionStatus::InputRequired);
}

/// The same judgement for a session only the disk knows: after a
/// restart no controller holds the pause, so the transcript's
/// tail window is the only source — and the scan reports it.
#[tokio::test]
async fn list_sessions_reports_input_required_for_a_disk_only_session_awaiting_approval()
 {
    let (state, dir) = durable_state().await;
    write_current_transcript(
        dir.path(),
        "paused-durable",
        &suspended_ask_rows(),
    );

    let list = list_sessions(Arc::clone(&state), SessionPageQuery::new())
        .await
        .expect("list");

    let row = list
        .data
        .iter()
        .find(|row| row.id == "paused-durable")
        .expect("the paused session is listed");
    assert_eq!(row.status, SessionStatus::InputRequired);
}

/// A disk-only transcript written **through the session's
/// writer** opens with the sink's schema-version metadata
/// header, and no projection may count it as an event: the
/// listing's `read_head` answers the session's opening row from
/// the log's first **event**, and `get_session` folds
/// `transcript_events` (which drops the header) into `history`.
/// A header left in would report no creation time, no subagent
/// attribution, and a phantom first history entry.
/// `write_transcript` writes headerless fixtures, so the
/// neighbouring tests cannot catch this.
#[tokio::test]
async fn disk_only_projections_skip_the_metadata_header() {
    let (state, _dir) = durable_state().await;
    // The writer writes the header on the first batch; the sink
    // alone does NOT register the session in
    // `SessionRegistry::sessions`, so this stays disk-only — the
    // exact shape the restart path produces. The rows go in as a
    // copy so their `ts` — what the projections read — is the
    // fixture's, not the writer's clock.
    let writer = state
        .session_manager
        .writer(SERVER_DEFAULT_USER_ID, "pre-restart");
    writer
        .append_copied(vec![
            serde_json::json!({
                "seq": 1,
                "type": "UserInput",
                "ts": "2026-09-19T08:00:00+00:00",
                "data": {"text": "hello"},
            }),
            serde_json::json!({
                "seq": 2,
                "type": "Model",
                "ts": "2026-09-19T08:00:05+00:00",
                "data": {"type": "text", "text": "hi"},
            }),
        ])
        .await
        .expect("seed the pre-restart transcript");

    let list = list_sessions(Arc::clone(&state), SessionPageQuery::new())
        .await
        .expect("list");
    let row = list
        .data
        .iter()
        .find(|row| row.id == "pre-restart")
        .expect("a durable transcript must be listed");
    assert_eq!(
        row.created_at.expect("first *event* ts").to_rfc3339(),
        "2026-09-19T08:00:00+00:00",
        "the header row has no `ts`; it must not shadow the first event",
    );

    let detail = get_session(Arc::clone(&state), "pre-restart".to_string())
        .await
        .expect("the disk-only session is readable");
    assert_eq!(
        detail.history.len(),
        2,
        "history must hold the 2 events, not the header row",
    );
    assert!(
        detail.history.iter().all(|row| row.get("_meta").is_none()),
        "no `_meta` row may reach the history projection",
    );
    assert_eq!(
        detail.created_at.expect("first *event* ts").to_rfc3339(),
        "2026-09-19T08:00:00+00:00",
    );
}

/// The **registered** detail view reads the log's head row as the
/// creation time — the same row the listing reads, and the same
/// row the disk-only detail view reads. It used to read the
/// *last* event instead, so the listing and the detail view
/// reported two different `created_at` values for one session, and
/// the web detail page rendered "Created" as the last activity.
///
/// The listing and the detail view must agree, and the update
/// time must still be the last event.
#[tokio::test]
async fn the_registered_detail_reports_the_head_row_as_created_at() {
    let (state, dir) = durable_state().await;
    state
        .session_manager
        .create_with_user(
            "registered-detail".to_string(),
            SERVER_DEFAULT_USER_ID.to_string(),
        )
        .await
        .expect("register the session");
    write_current_transcript(
        dir.path(),
        "registered-detail",
        &[
            serde_json::json!({
                "type": "UserInput",
                "ts": "2026-09-19T08:00:00+00:00",
                "data": {"text": "hello"},
            }),
            serde_json::json!({
                "type": "Model",
                "ts": "2026-09-19T08:05:00+00:00",
                "data": {"text": "hi"},
            }),
        ],
    );

    let detail =
        get_session(Arc::clone(&state), "registered-detail".to_string())
            .await
            .expect("the registered session is readable");
    assert_eq!(
        detail.created_at.expect("the head row's ts").to_rfc3339(),
        "2026-09-19T08:00:00+00:00",
        "the creation time is the log's first event",
    );
    assert_eq!(
        detail.updated_at.expect("the last row's ts").to_rfc3339(),
        "2026-09-19T08:05:00+00:00",
        "the update time is the log's last event",
    );

    let list = list_sessions(Arc::clone(&state), SessionPageQuery::new())
        .await
        .expect("list");
    let row = list
        .data
        .iter()
        .find(|row| row.id == "registered-detail")
        .expect("the registered session is listed");
    assert_eq!(
        row.created_at.expect("the head row's ts").to_rfc3339(),
        "2026-09-19T08:00:00+00:00",
        "the listing and the detail view must not disagree",
    );
    assert_eq!(
        row.created_at.map(|ts| ts.to_rfc3339()),
        detail.created_at.map(|ts| ts.to_rfc3339()),
        "one session, one creation time",
    );
}

/// A session the process registered *and* has a transcript for is
/// one row: the listing merges the two sources, it does not stack
/// them.
#[tokio::test]
async fn list_sessions_lists_a_registered_session_once() {
    let (state, dir) = durable_state().await;
    write_transcript(
        dir.path(),
        "both-sources",
        &[serde_json::json!({
            "type": "UserInput",
            "ts": "2026-09-19T08:00:00+00:00",
            "data": {"text": "hello"},
        })],
    );
    state
        .session_manager
        .create_with_user(
            "both-sources".to_string(),
            SERVER_DEFAULT_USER_ID.to_string(),
        )
        .await
        .expect("register");

    let list = list_sessions(Arc::clone(&state), SessionPageQuery::new())
        .await
        .expect("list");

    assert_eq!(
        list.data
            .iter()
            .filter(|row| row.id == "both-sources")
            .count(),
        1,
        "a session in both sources must be listed once"
    );
    assert_eq!(list.total, Some(1));
}

/// A legacy log whose events carry no timestamp is still a session:
/// it is listed, with an unknown creation time rather than a
/// manufactured one.
#[tokio::test]
async fn list_sessions_reports_no_created_at_for_an_untimed_log() {
    let (state, dir) = durable_state().await;
    write_transcript(
        dir.path(),
        "legacy-log",
        &[serde_json::json!({
            "type": "UserInput",
            "data": {"text": "hello"},
        })],
    );

    let list = list_sessions(Arc::clone(&state), SessionPageQuery::new())
        .await
        .expect("list");

    let row = list
        .data
        .iter()
        .find(|row| row.id == "legacy-log")
        .expect("an untimed log is still a session");
    assert_eq!(row.created_at, None);
}

/// A log whose last row was torn by a crash mid-append must not
/// vanish from the listing: `read_tail` fails on the torn piece
/// **on purpose**, and the scan treats that as "no tail", the
/// same fallback the registered loop has — the head row still
/// names the session.
#[tokio::test]
async fn list_sessions_keeps_a_session_whose_tail_row_is_torn() {
    let (state, dir) = durable_state().await;
    write_current_transcript(
        dir.path(),
        "torn-tail",
        &[
            serde_json::json!({
                "type": "UserInput",
                "ts": "2026-09-19T10:00:00+00:00",
                "data": {"text": "hello"},
            }),
            serde_json::json!({
                "type": "Model",
                "ts": "2026-09-19T10:00:05+00:00",
                "data": {"text": "hi"},
            }),
        ],
    );
    // Tear the last row: a crash between the bytes and the
    // newline leaves a half-written line the reader must refuse.
    let path = dir
        .path()
        .join("sessions")
        .join(SERVER_DEFAULT_USER_ID)
        .join("torn-tail")
        .join("events.jsonl");
    let bytes = std::fs::read(&path).expect("the transcript");
    std::fs::write(&path, &bytes[..bytes.len() - 5]).expect("tear the tail");

    let list = list_sessions(Arc::clone(&state), SessionPageQuery::new())
        .await
        .expect("list");

    let row = list
        .data
        .iter()
        .find(|row| row.id == "torn-tail")
        .expect("a torn tail must not hide the session");
    // The head row survived the tear, so the session still has
    // its creation time.
    assert_eq!(
        row.created_at.expect("the head ts").to_rfc3339(),
        "2026-09-19T10:00:00+00:00"
    );
}

/// The detail view renders a transcript the registry never saw —
/// the "View Detail" miss reported for pre-restart sessions.
#[tokio::test]
async fn get_session_reads_a_transcript_the_registry_never_saw() {
    let (state, dir) = durable_state().await;
    write_transcript(
        dir.path(),
        "earlier-run",
        &[
            serde_json::json!({
                "type": "UserInput",
                "ts": "2026-09-19T08:00:00+00:00",
                "data": {"text": "hello"},
            }),
            serde_json::json!({
                "type": "Model",
                "ts": "2026-09-19T08:00:05+00:00",
                "data": {"type": "text", "text": "hi"},
            }),
        ],
    );

    let detail = get_session(Arc::clone(&state), "earlier-run".to_string())
        .await
        .expect("durable detail");

    assert_eq!(detail.id, "earlier-run");
    assert_eq!(detail.context_id, "earlier-run");
    assert_eq!(detail.status, SessionStatus::Completed);
    assert_eq!(detail.history.len(), 2);
    assert_eq!(detail.history[0]["type"], "user_message");
    assert_eq!(
        detail.history[0]["data"]["content"]["Single"]["text"],
        "hello"
    );
    assert_eq!(
        detail.created_at.expect("first event ts").to_rfc3339(),
        "2026-09-19T08:00:00+00:00"
    );
    assert_eq!(
        detail.updated_at.expect("last event ts").to_rfc3339(),
        "2026-09-19T08:00:05+00:00"
    );
    assert!(detail.artifacts.is_empty());
}

/// Neither source knows the id. A directory whose log is empty is
/// not a session with no history — there is no transcript to
/// render, so it is unknown, and it is never listed as a
/// placeholder row.
#[tokio::test]
async fn get_session_is_not_found_without_a_transcript() {
    let (state, dir) = durable_state().await;
    let unused = dir
        .path()
        .join("sessions")
        .join(SERVER_DEFAULT_USER_ID)
        .join("touched-but-never-used");
    std::fs::create_dir_all(&unused).expect("session dir");
    std::fs::write(unused.join("events.jsonl"), "").expect("empty log");

    for id in ["does-not-exist", "touched-but-never-used"] {
        let Err(err) = get_session(Arc::clone(&state), id.to_string()).await
        else {
            panic!("{id} must be refused as not found");
        };
        assert_eq!(err.code, "not_found", "{id}");
    }

    let list = list_sessions(Arc::clone(&state), SessionPageQuery::new())
        .await
        .expect("list");
    assert!(
        list.data.is_empty(),
        "an empty transcript must not be listed"
    );
}

/// A traversal id can never address a file outside the sessions
/// root, even one that exists: the id is rejected, not resolved.
#[tokio::test]
async fn get_session_rejects_a_traversal_id() {
    let (state, dir) = durable_state().await;
    // `<root>/sessions/dev/../escape/events.jsonl` — where
    // `../escape` lands if the id is joined without a check.
    let escape = dir.path().join("sessions").join("escape");
    std::fs::create_dir_all(&escape).expect("escape dir");
    std::fs::write(
        escape.join("events.jsonl"),
        "{\"type\":\"UserInput\",\"ts\":\"2026-09-19T08:00:00+00:00\"}\n",
    )
    .expect("escape transcript");

    // `..%2f..%2fetc%2fpasswd` reaches the handler undecoded as a
    // raw path parameter, so both spellings must be refused.
    for id in ["../escape", "../../etc/passwd", "..%2f..%2fetc%2fpasswd"] {
        let Err(err) = get_session(Arc::clone(&state), id.to_string()).await
        else {
            panic!("{id} must be refused");
        };
        assert_eq!(err.code, "invalid_item", "{id}");
    }
}

/// The detail view publishes every dialect row in the current
/// vocabulary: legacy `UserInput` / `Model` rows reach the caller as
/// `user_message` / `assistant_message`, their text in the current
/// payload shape, and attachment references lifted to the row's top
/// level.
#[tokio::test]
async fn get_session_publishes_dialect_rows_in_the_current_vocabulary() {
    let (state, dir) = durable_state().await;
    write_transcript(
        dir.path(),
        "dialect-log",
        &[
            serde_json::json!({
                "type": "UserInput",
                "ts": "2026-09-19T08:00:00+00:00",
                "data": {
                    "text": "hello",
                    "attachments": [
                        {"kind": "file", "name": "a.md",
                         "mime_type": "text/markdown", "byte_len": 12}
                    ]
                },
            }),
            serde_json::json!({
                "type": "Model",
                "ts": "2026-09-19T08:00:05+00:00",
                "data": {"type": "text", "text": "hi"},
            }),
        ],
    );

    let detail = get_session(Arc::clone(&state), "dialect-log".to_string())
        .await
        .expect("the dialect log reads");

    assert_eq!(detail.history.len(), 2);
    assert!(
        detail
            .history
            .iter()
            .all(|row| restore_row(row).is_current()),
        "every published row is already in the current vocabulary"
    );
    assert_eq!(detail.history[0]["type"], "user_message");
    assert_eq!(
        detail.history[0]["data"]["content"]["Single"]["text"],
        "hello"
    );
    assert_eq!(
        detail.history[0]["attachments"][0]["name"], "a.md",
        "the dialect payload's attachment refs are lifted to the row"
    );
    assert_eq!(detail.history[1]["type"], "assistant_message");
    assert_eq!(detail.history[1]["data"]["content"]["Single"]["text"], "hi");
}

/// Rows with no current counterpart — an `Agent` delegation row, a
/// tag this build does not know — are not published: the online
/// surface never sees a dialect row, while `sessions/events` keeps
/// reporting them in its `legacy` / `unknown` buckets.
#[tokio::test]
async fn get_session_does_not_publish_rows_without_a_current_shape() {
    let (state, dir) = durable_state().await;
    write_transcript(
        dir.path(),
        "no-counterpart",
        &[
            serde_json::json!({
                "type": "UserInput",
                "ts": "2026-09-19T08:00:00+00:00",
                "data": {"text": "hello"},
            }),
            serde_json::json!({
                "type": "Agent",
                "ts": "2026-09-19T08:00:01+00:00",
                "data": {},
            }),
            serde_json::json!({
                "type": "NotATagThisBuildKnows",
                "ts": "2026-09-19T08:00:02+00:00",
                "data": {"text": "?"},
            }),
        ],
    );

    let detail = get_session(Arc::clone(&state), "no-counterpart".to_string())
        .await
        .expect("the log reads");

    assert_eq!(
        detail.history.len(),
        1,
        "only the row with a current counterpart is published"
    );
    assert_eq!(detail.history[0]["type"], "user_message");
    // The dropped rows still moved the facts: the update time is the
    // last raw row's, not the last published row's.
    assert_eq!(
        detail.updated_at.expect("last raw ts").to_rfc3339(),
        "2026-09-19T08:00:02+00:00"
    );
}

/// The listing's claimed default — newest first — is the order it
/// actually answers with.
///
/// The default used to be substituted into the whitelist check while
/// the ordering read a different value, so a sortless request came back
/// **oldest first** — while this route's own documentation and
/// `SessionSummary` both promised the opposite. The ids are the reverse
/// of their timestamps on purpose: an implementation that fell back to
/// the id tiebreaker would answer in a visibly different order.
#[tokio::test]
async fn the_default_order_is_newest_first() {
    let (state, dir) = durable_state().await;
    for (id, ts) in [
        ("zzz", "2026-09-27T08:00:00+00:00"),
        ("mmm", "2026-09-26T08:00:00+00:00"),
        ("aaa", "2026-09-25T08:00:00+00:00"),
    ] {
        write_transcript(
            dir.path(),
            id,
            &[serde_json::json!({
                "type": "UserInput",
                "ts": ts,
                "data": {"text": "hi"},
            })],
        );
    }

    let defaulted = list_sessions(Arc::clone(&state), SessionPageQuery::new())
        .await
        .expect("the default list");
    let ids: Vec<&str> =
        defaulted.data.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(
        ids,
        ["zzz", "mmm", "aaa"],
        "a sortless list is newest first"
    );

    let ascending = list_sessions(
        Arc::clone(&state),
        SessionPageQuery::new()
            .with_page(crate::api::PageQuery::new().with_sort("created_at")),
    )
    .await
    .expect("the named-key list");
    let ids: Vec<&str> =
        ascending.data.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(
        ids,
        ["aaa", "mmm", "zzz"],
        "`sort=created_at` names the same key ascending"
    );

    // Every row shares a status here, so `-status` is answered by the
    // id tiebreaker — turned with the key, which is the declared
    // semantic.
    let by_status = list_sessions(
        Arc::clone(&state),
        SessionPageQuery::new()
            .with_page(crate::api::PageQuery::new().with_sort("-status")),
    )
    .await
    .expect("the status list");
    let ids: Vec<&str> =
        by_status.data.iter().map(|row| row.id.as_str()).collect();
    assert_eq!(ids, ["zzz", "mmm", "aaa"], "`-status` turns the tiebreak");
}

/// The session listing validates its page parameters like every other
/// list method.
///
/// `sessions/list` was the one list route whose inner page struct
/// carried no `nested` validator, so `cursor: ""` passed here and was
/// refused by the other six.
#[test]
fn an_empty_cursor_is_refused_like_every_other_list_method() {
    let err = crate::rpc::params::<SessionPageQuery>(
        serde_json::json!({ "cursor": "" }),
    )
    .expect_err("an empty cursor is not a page");
    assert_eq!(err.code, "validation", "got {err:?}");

    assert!(
        crate::rpc::params::<SessionPageQuery>(
            serde_json::json!({ "cursor": "bm90LWVtcHR5" }),
        )
        .is_ok(),
        "a non-empty cursor still passes"
    );
}
