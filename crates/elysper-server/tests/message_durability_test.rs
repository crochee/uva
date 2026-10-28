//! The chat message route's answer is a promise about the disk.
//!
//! `POST /api/v1/chat/sessions/{id}/messages` answers `200
//! {"queued": true}` — and the promise that answer carries is that
//! the message is **already on disk**: the controller parks the turn
//! as a durable `parked_prompt` row and flushes it before the route
//! answers. A crash between the answer and the run must not lose the
//! message.
//!
//! The test reads the log the moment the response lands. Without the
//! ack path the row lands later — the controller loop parks it — and
//! the read races that loop, so the assertion is a real one.
//!
//! Deterministic: the fixture's `AppState::for_test` wires a
//! `FakeProvider`, so no run touches the network.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use elysper::session::manager::{SERVER_DEFAULT_USER_ID, SessionRegistry};
use elysper_server::{create_router, state::AppState};
use tower::ServiceExt;

struct Fixture {
    app: axum::Router,
    _temp: tempfile::TempDir,
    state: Arc<AppState>,
}

async fn make_fixture() -> Fixture {
    let temp = tempfile::TempDir::new().unwrap();
    let sessions_root = temp.path().to_path_buf();
    let registry = SessionRegistry::new(sessions_root.clone());
    let state =
        Arc::new(AppState::for_test(registry, sessions_root.clone()).await);
    let app = create_router(Arc::clone(&state)).await;
    Fixture {
        app,
        _temp: temp,
        state,
    }
}

async fn create_session(fix: &Fixture, session_id: &str) {
    let req = Request::builder()
        .uri("/api/v1/chat/sessions".to_string())
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(json!({"session_id": session_id}).to_string()))
        .unwrap();
    let resp = fix.app.clone().oneshot(req).await.unwrap();
    assert_eq!(
        resp.status(),
        StatusCode::OK,
        "create_session must seed the controller"
    );
}

async fn post_message(
    fix: &Fixture,
    session_id: &str,
    text: &str,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .uri(format!("/api/v1/chat/sessions/{session_id}/messages"))
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(json!({"text": text}).to_string()))
        .unwrap();
    let resp = fix.app.clone().oneshot(req).await.unwrap();
    let status = resp.status();
    let body = resp
        .into_body()
        .collect()
        .await
        .expect("the response body reads")
        .to_bytes();
    let body: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    (status, body)
}

async fn read_rows(fix: &Fixture, session_id: &str) -> Vec<Value> {
    fix.state
        .session_manager
        .sink(SERVER_DEFAULT_USER_ID, session_id)
        .read()
        .await
        .unwrap()
}

fn parked_texts(rows: &[Value]) -> Vec<String> {
    rows.iter()
        .filter(|row| row["type"] == "parked_prompt")
        .filter_map(|row| row["text"].as_str().map(str::to_string))
        .collect()
}

/// The `200` is answered after the turn is on disk: the
/// `parked_prompt` row is in the log the moment the response lands.
#[tokio::test]
async fn the_message_route_answers_after_the_turn_is_on_disk() {
    let fix = make_fixture().await;
    let session_id = "durable-message";
    create_session(&fix, session_id).await;

    let (status, body) = post_message(&fix, session_id, "a durable turn").await;
    assert_eq!(status, StatusCode::OK, "the message is accepted: {body}");
    assert_eq!(body["queued"], json!(true), "the turn is queued");

    // No waiting, no polling: the row must be there already.
    let rows = read_rows(&fix, session_id).await;
    assert!(
        parked_texts(&rows)
            .iter()
            .any(|text| text == "a durable turn"),
        "the parked_prompt row must be on disk when the route answers: \
         {rows:?}",
    );
}

/// The same promise for a second message: one queued message = one
/// durable park, and the answer for the second one still waits for
/// its own row.
#[tokio::test]
async fn every_queued_message_gets_its_own_durable_park() {
    let fix = make_fixture().await;
    let session_id = "durable-messages";
    create_session(&fix, session_id).await;

    let (status, _) = post_message(&fix, session_id, "first").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = post_message(&fix, session_id, "second").await;
    assert_eq!(status, StatusCode::OK);

    let rows = read_rows(&fix, session_id).await;
    let parked = parked_texts(&rows);
    assert!(
        parked.iter().any(|text| text == "first"),
        "the first turn is on disk: {rows:?}",
    );
    assert!(
        parked.iter().any(|text| text == "second"),
        "the second turn is on disk: {rows:?}",
    );
}
