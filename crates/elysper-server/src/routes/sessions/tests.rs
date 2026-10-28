use axum::http::StatusCode;
use elysper::session::{SessionSink, manager::SERVER_DEFAULT_USER_ID};

use super::*;

/// Seed one row through a writer — the way every row reaches a
/// session's log. The writer stamps the row's `seq` and `ts`,
/// and the flush lands it.
async fn seed_row(sink: &Arc<dyn SessionSink>, row: Value) {
    let writer = elysper::session::LogWriter::new(
        Arc::clone(sink),
        Arc::new(elysper::core::SystemClock),
    );
    writer.append(row).await.expect("seed append");
    writer.flush().await.expect("seed flush");
}

#[test]
fn parse_status_filter_accepts_all_known_labels() {
    for (label, expected) in [
        ("unspecified", "unspecified"),
        ("submitted", "submitted"),
        ("working", "working"),
        ("completed", "completed"),
        ("failed", "failed"),
        ("canceled", "canceled"),
        ("input_required", "input_required"),
        ("rejected", "rejected"),
        ("auth_required", "auth_required"),
    ] {
        assert_eq!(parse_status_filter(label).unwrap(), expected);
    }
}

#[test]
fn parse_status_filter_rejects_unknown_label() {
    let err = parse_status_filter("not_a_state").expect_err("unknown label");
    assert!(matches!(err, elysper::core::Error::InvalidItem { .. }));
    assert!(err.to_string().contains("status filter 'not_a_state'"));
}

#[test]

fn parse_status_filter_is_case_sensitive() {
    let err = parse_status_filter("Completed").expect_err("uppercase");
    assert!(matches!(err, elysper::core::Error::InvalidItem { .. }));
}

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

/// Non-empty sink with no live controller ⇒ the run finished
/// ⇒ "completed". This is the regression guard for the bug
/// where every finished session was reported "working"
/// forever because `SessionEnded` is ephemeral and never
/// reaches the JSONL sink.
#[tokio::test]
async fn infer_status_reports_completed_without_live_controller() {
    let sink: Arc<dyn SessionSink> =
        Arc::new(elysper::session::in_memory::InMemorySessionSink::new("s1"));
    seed_row(
        &sink,
        serde_json::json!({
            "type": "Model",
            "data": {"text": "hi"},
        }),
    )
    .await;
    let rows = sink.read().await.expect("rows");
    let status = infer_status_from_parts(None, !rows.is_empty());
    assert_eq!(status, "completed");
}

/// A live Running controller ⇒ "working" even before any
/// durable event has been persisted.
#[tokio::test]
async fn infer_status_reports_working_for_running_controller() {
    let sink = elysper::session::in_memory::InMemorySessionSink::new("s1");
    let rows = sink.read().await.expect("rows");
    let status =
        infer_status_from_parts(Some(SessionState::Running), !rows.is_empty());
    assert_eq!(status, "working");
}

/// Empty sink + no live run ⇒ "unspecified" (never touched).
#[tokio::test]
async fn infer_status_reports_unspecified_for_untouched_session() {
    let sink = elysper::session::in_memory::InMemorySessionSink::new("s1");
    let rows = sink.read().await.expect("rows");
    let status = infer_status_from_parts(None, !rows.is_empty());
    assert_eq!(status, "unspecified");
}

/// A cancelled controller with durable history ⇒ "canceled".
#[tokio::test]
async fn infer_status_reports_canceled_for_cancelled_controller() {
    let sink: Arc<dyn SessionSink> =
        Arc::new(elysper::session::in_memory::InMemorySessionSink::new("s1"));
    seed_row(
        &sink,
        serde_json::json!({
            "type": "UserInput",
            "data": {"text": "go"},
        }),
    )
    .await;
    let rows = sink.read().await.expect("rows");
    let status = infer_status_from_parts(
        Some(SessionState::Cancelled),
        !rows.is_empty(),
    );
    assert_eq!(status, "canceled");
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
        status: "completed",
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
/// split (typed / legacy) with counts. Pin the field shape
/// so the JSON contract is locked.
#[test]
fn typed_event_response_serialises_with_counts() {
    let resp = TypedEventResponse {
        id: "abc".to_string(),
        typed_count: 2,
        legacy_count: 1,
        typed: Vec::new(),
        legacy: Vec::new(),
    };
    let v = serde_json::to_value(&resp).expect("serialise");
    assert_eq!(v["id"], "abc");
    assert_eq!(v["typed_count"], 2);
    assert_eq!(v["legacy_count"], 1);
    assert_eq!(v["typed"], serde_json::json!([]));
    assert_eq!(v["legacy"], serde_json::json!([]));
}

/// A hermetic `AppState` over a temp workspace: these tests build a
/// real `sessions/<user>/<id>/events.jsonl` tree, so the caller
/// keeps the tempdir alive.
async fn durable_state() -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp workspace");
    let sessions = elysper::session::manager::SessionRegistry::new(
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
            "kind": "elysper_session_meta",
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

    let status = delete_session(
        State(Arc::clone(&state)),
        AppPath("doomed".to_string()),
    )
    .await
    .expect("delete");
    assert_eq!(status, StatusCode::NO_CONTENT);
    assert!(
        !session_dir(dir.path(), "doomed").exists(),
        "the whole session directory must go, not just its log",
    );

    match get_session(State(Arc::clone(&state)), AppPath("doomed".to_string()))
        .await
    {
        Err(err) => assert_eq!(err.status_code, StatusCode::NOT_FOUND),
        Ok(_) => panic!("a deleted session must not be readable"),
    }
    let Json(list) = list_sessions(
        State(Arc::clone(&state)),
        AppQuery(SessionPageQuery::new()),
    )
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

    let status =
        delete_session(State(Arc::clone(&state)), AppPath("gated".to_string()))
            .await
            .expect("delete");
    assert_eq!(status, StatusCode::NO_CONTENT);
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

/// An id neither source knows is a 404 — and the delete creates
/// nothing on the way there.
#[tokio::test]
async fn delete_session_404s_for_an_unknown_id() {
    let (state, dir) = durable_state().await;
    match delete_session(
        State(Arc::clone(&state)),
        AppPath("never-existed".to_string()),
    )
    .await
    {
        Err(err) => assert_eq!(err.status_code, StatusCode::NOT_FOUND),
        Ok(status) => panic!("an unknown id must not 204: {status}"),
    }
    assert!(
        !session_dir(dir.path(), "never-existed").exists(),
        "a 404 must not leave an empty session directory behind",
    );
}

/// Export is the file, not the projection: append order kept, key
/// order in each row preserved, blank and torn lines dropped.
#[tokio::test]
async fn export_session_returns_the_raw_durable_rows() {
    use http_body_util::BodyExt as _;

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

    let resp = export_session(
        State(Arc::clone(&state)),
        AppPath("exportable".to_string()),
    )
    .await
    .expect("export");
    assert_eq!(resp.status(), StatusCode::OK);
    let header_value = |name: header::HeaderName| {
        resp.headers()
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::to_string)
    };
    assert_eq!(
        header_value(header::CONTENT_TYPE).as_deref(),
        Some("application/x-ndjson"),
    );
    assert_eq!(
        header_value(header::CONTENT_DISPOSITION).as_deref(),
        Some("attachment; filename=\"exportable.jsonl\""),
    );
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("export body")
        .to_bytes();
    assert_eq!(
        std::str::from_utf8(&bytes).expect("utf-8"),
        "{\"b\":1,\"a\":2}\n{\"type\":\"Model\"}\n",
    );
}

/// A registered session whose log is empty exports zero rows: it
/// is not an unknown id.
#[tokio::test]
async fn export_session_serves_a_registered_session_with_an_empty_log() {
    use http_body_util::BodyExt as _;

    let (state, _dir) = durable_state().await;
    state
        .session_manager
        .create_with_user(
            "blank".to_string(),
            SERVER_DEFAULT_USER_ID.to_string(),
        )
        .await
        .expect("register");

    let resp =
        export_session(State(Arc::clone(&state)), AppPath("blank".to_string()))
            .await
            .expect("export");
    assert_eq!(resp.status(), StatusCode::OK);
    let bytes = resp
        .into_body()
        .collect()
        .await
        .expect("export body")
        .to_bytes();
    assert!(bytes.is_empty(), "an empty log exports nothing: {bytes:?}");
}

/// Export of an id neither source knows is a 404.
#[tokio::test]
async fn export_session_404s_for_an_unknown_id() {
    let (state, _dir) = durable_state().await;
    match export_session(
        State(Arc::clone(&state)),
        AppPath("never-existed".to_string()),
    )
    .await
    {
        Err(err) => assert_eq!(err.status_code, StatusCode::NOT_FOUND),
        Ok(_) => panic!("an unknown id must not export"),
    }
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
                "data": {"text": "hi"},
            }),
        ],
    );

    let (status, Json(forked)) = fork_session(
        State(Arc::clone(&state)),
        AppPath("origin".to_string()),
        AppJson(ForkSessionRequest {
            from_stream_index: None,
        }),
    )
    .await
    .expect("fork");
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(forked.rows, 2);
    assert_eq!(forked.forked_from, "origin");
    assert_ne!(forked.session_id, "origin");

    let Json(child) = get_session(
        State(Arc::clone(&state)),
        AppPath(forked.session_id.clone()),
    )
    .await
    .expect("the child is a session");
    assert_eq!(child.history.len(), 2, "the child inherits the prefix");

    let Json(parent) =
        get_session(State(Arc::clone(&state)), AppPath("origin".to_string()))
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
        .append(serde_json::json!({
            "type": "UserInput",
            "data": {"text": "hello"},
        }))
        .await
        .expect("seed parent row 1");
    parent
        .append(serde_json::json!({
            "type": "Model",
            "data": {"text": "hi"},
        }))
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

    let (status, Json(forked)) = fork_session(
        State(Arc::clone(&state)),
        AppPath("meta-origin".to_string()),
        AppJson(ForkSessionRequest {
            from_stream_index: None,
        }),
    )
    .await
    .expect("fork");
    assert_eq!(status, StatusCode::CREATED);
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
                "data": {"text": "b"},
            }),
            serde_json::json!({
                "stream_index": 3,
                "type": "UserInput",
                "data": {"text": "c"},
            }),
        ],
    );

    let (status, Json(forked)) = fork_session(
        State(Arc::clone(&state)),
        AppPath("long".to_string()),
        AppJson(ForkSessionRequest {
            from_stream_index: Some(2),
        }),
    )
    .await
    .expect("fork");
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(forked.rows, 2, "only the rows at or below index 2");

    let Json(child) = get_session(
        State(Arc::clone(&state)),
        AppPath(forked.session_id.clone()),
    )
    .await
    .expect("the child is a session");
    assert_eq!(child.history.len(), 2);
    assert_eq!(child.history[1]["data"]["text"], "b");
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
                "data": {"text": "beta"},
            }),
        ],
    );

    let (_status, Json(forked)) = fork_session(
        State(Arc::clone(&state)),
        AppPath("base".to_string()),
        AppJson(ForkSessionRequest {
            from_stream_index: None,
        }),
    )
    .await
    .expect("fork");

    let child_writer = state
        .session_manager
        .writer(SERVER_DEFAULT_USER_ID, &forked.session_id);
    child_writer
        .append(serde_json::json!({
            "type": "UserInput",
            "data": {"text": "gamma"},
        }))
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

    let Json(child) = get_session(
        State(Arc::clone(&state)),
        AppPath(forked.session_id.clone()),
    )
    .await
    .expect("the child is a session");
    assert_eq!(child.history.len(), 3);
    let Json(parent) =
        get_session(State(Arc::clone(&state)), AppPath("base".to_string()))
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

    let (status, Json(forked)) = fork_session(
        State(Arc::clone(&state)),
        AppPath("fresh".to_string()),
        AppJson(ForkSessionRequest {
            from_stream_index: None,
        }),
    )
    .await
    .expect("fork");
    assert_eq!(status, StatusCode::CREATED);
    assert_eq!(forked.rows, 0);

    let Json(child) = get_session(
        State(Arc::clone(&state)),
        AppPath(forked.session_id.clone()),
    )
    .await
    .expect("the child is registered");
    assert!(child.history.is_empty());
}

/// Forking an id neither source knows is a 404, and creates nothing.
#[tokio::test]
async fn fork_session_404s_for_an_unknown_id() {
    let (state, dir) = durable_state().await;
    match fork_session(
        State(Arc::clone(&state)),
        AppPath("never-existed".to_string()),
        AppJson(ForkSessionRequest {
            from_stream_index: None,
        }),
    )
    .await
    {
        Err(err) => assert_eq!(err.status_code, StatusCode::NOT_FOUND),
        Ok(_) => panic!("an unknown parent must not fork"),
    }
    assert!(
        !session_dir(dir.path(), "never-existed").exists(),
        "a 404 must not leave an empty session directory behind",
    );
}

/// A turn that is still writing owns its log. `Completing` counts
/// as busy as well, even though `is_terminal` reports it terminal:
/// the flush is still appending.
#[test]
fn turn_in_flight_covers_running_and_completing_only() {
    use elysper::session::OperationState;

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

    let Json(list) = list_sessions(
        State(Arc::clone(&state)),
        AppQuery(SessionPageQuery::new()),
    )
    .await
    .expect("list");

    let row = list
        .data
        .iter()
        .find(|row| row.id == "earlier-run")
        .expect("a durable transcript must be listed");
    assert_eq!(row.status, "completed");
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
        elysper::session::turn_start(3),
        elysper::session::approval_asked(
            "a1", "c1", "shell", "sha256:x", "high", "default",
        ),
        elysper::session::turn_suspended(vec!["c1".into()]),
        elysper::session::turn_end_suspended(),
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

    let Json(list) = list_sessions(
        State(Arc::clone(&state)),
        AppQuery(SessionPageQuery::new()),
    )
    .await
    .expect("list");

    let row = list
        .data
        .iter()
        .find(|row| row.id == "paused-registered")
        .expect("the paused session is listed");
    assert_eq!(row.status, "input_required");
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

    let Json(list) = list_sessions(
        State(Arc::clone(&state)),
        AppQuery(SessionPageQuery::new()),
    )
    .await
    .expect("list");

    let row = list
        .data
        .iter()
        .find(|row| row.id == "paused-durable")
        .expect("the paused session is listed");
    assert_eq!(row.status, "input_required");
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
                "data": {"text": "hi"},
            }),
        ])
        .await
        .expect("seed the pre-restart transcript");

    let Json(list) = list_sessions(
        State(Arc::clone(&state)),
        AppQuery(SessionPageQuery::new()),
    )
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

    let Json(detail) = get_session(
        State(Arc::clone(&state)),
        AppPath("pre-restart".to_string()),
    )
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

    let Json(detail) = get_session(
        State(Arc::clone(&state)),
        AppPath("registered-detail".to_string()),
    )
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

    let Json(list) = list_sessions(
        State(Arc::clone(&state)),
        AppQuery(SessionPageQuery::new()),
    )
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

    let Json(list) = list_sessions(
        State(Arc::clone(&state)),
        AppQuery(SessionPageQuery::new()),
    )
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

    let Json(list) = list_sessions(
        State(Arc::clone(&state)),
        AppQuery(SessionPageQuery::new()),
    )
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

    let Json(list) = list_sessions(
        State(Arc::clone(&state)),
        AppQuery(SessionPageQuery::new()),
    )
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
/// the "View Detail" 404 reported for pre-restart sessions.
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
                "data": {"text": "hi"},
            }),
        ],
    );

    let Json(detail) = get_session(
        State(Arc::clone(&state)),
        AppPath("earlier-run".to_string()),
    )
    .await
    .expect("durable detail");

    assert_eq!(detail.id, "earlier-run");
    assert_eq!(detail.context_id, "earlier-run");
    assert_eq!(detail.status, "completed");
    assert_eq!(detail.history.len(), 2);
    assert_eq!(detail.history[0]["data"]["text"], "hello");
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
async fn get_session_404s_without_a_transcript() {
    let (state, dir) = durable_state().await;
    let unused = dir
        .path()
        .join("sessions")
        .join(SERVER_DEFAULT_USER_ID)
        .join("touched-but-never-used");
    std::fs::create_dir_all(&unused).expect("session dir");
    std::fs::write(unused.join("events.jsonl"), "").expect("empty log");

    for id in ["does-not-exist", "touched-but-never-used"] {
        let Err(err) =
            get_session(State(Arc::clone(&state)), AppPath(id.to_string()))
                .await
        else {
            panic!("{id} must 404");
        };
        assert_eq!(err.status_code, StatusCode::NOT_FOUND, "{id}");
        assert_eq!(err.code, "not_found", "{id}");
    }

    let Json(list) = list_sessions(
        State(Arc::clone(&state)),
        AppQuery(SessionPageQuery::new()),
    )
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
        let Err(err) =
            get_session(State(Arc::clone(&state)), AppPath(id.to_string()))
                .await
        else {
            panic!("{id} must be refused");
        };
        assert_eq!(err.status_code, StatusCode::BAD_REQUEST, "{id}");
        assert_eq!(err.code, "invalid_item", "{id}");
    }
}
