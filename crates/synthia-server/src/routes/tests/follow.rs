//! `follow` wire contract: the first frame is a complete baseline,
//! a reconnect replaces the window rather than merging it, and the
//! paused state is derived from the log.

use std::{sync::Arc, time::Duration};

use axum::{
    body::Body,
    extract::State,
    response::{IntoResponse, Response},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use synthia::{
    provider::Message,
    session::{SessionEvent, manager::SessionRegistry},
};

use crate::{
    api::{AppPath, AppQuery},
    routes::follow::{FollowQuery, follow},
    session::controller::SessionState,
    state::AppState,
};

const SESSION: &str = "follow-session";

struct Fixture {
    state: Arc<AppState>,
    user: String,
    _temp: tempfile::TempDir,
}

async fn fixture() -> Fixture {
    let temp = tempfile::TempDir::new().expect("a temp workspace");
    let registry = SessionRegistry::new(temp.path().to_path_buf());
    let state =
        Arc::new(AppState::for_test(registry, temp.path().to_path_buf()).await);
    let user = state.default_user_id().to_string();
    state
        .session_manager
        .create_with_user(SESSION.to_string(), user.clone())
        .await
        .expect("the session is created");
    // Open the controller before the fixture writes its rows: the
    // first open repairs a dangling turn, and that repair row would
    // otherwise move the ordinal the test asserts on.
    state
        .get_or_create_session_controller(&user, SESSION)
        .await
        .expect("a controller");
    Fixture {
        state,
        user,
        _temp: temp,
    }
}

impl Fixture {
    async fn append(&self, event: &SessionEvent) {
        let writer = self.state.session_manager.writer(&self.user, SESSION);
        writer.append_event(event).await.expect("append the row");
        writer.flush().await.expect("flush the row");
    }

    async fn open(&self, from: Option<u64>) -> Frames {
        let response: Response = follow(
            State(Arc::clone(&self.state)),
            AppPath(SESSION.to_string()),
            AppQuery(FollowQuery { from }),
        )
        .await
        .unwrap_or_else(|error| error.into_response());
        Frames::new(response.into_body())
    }
}

/// A minimal SSE reader: enough to assert the frame contract without
/// pulling a client library into the server's tests.
struct Frames {
    body: Body,
    buffer: String,
}

impl Frames {
    fn new(body: Body) -> Self {
        Self {
            body,
            buffer: String::new(),
        }
    }

    /// The next data-bearing frame, or `None` when the stream ended.
    async fn next(&mut self) -> Option<(String, Value)> {
        loop {
            if let Some(index) = self.buffer.find("\n\n") {
                let frame: String = self.buffer.drain(..index + 2).collect();
                if let Some(parsed) = parse_frame(&frame) {
                    return Some(parsed);
                }
                continue;
            }
            let frame =
                tokio::time::timeout(Duration::from_secs(2), self.body.frame())
                    .await
                    .expect("a frame before the deadline")
                    .expect("the body is open")
                    .ok()?;
            let chunk = frame.into_data().ok()?;
            self.buffer.push_str(&String::from_utf8_lossy(&chunk));
        }
    }

    /// Drain frames for `window`, returning the ones that arrived.
    async fn drain_for(&mut self, window: Duration) -> Vec<(String, Value)> {
        let deadline = tokio::time::Instant::now() + window;
        let mut frames = Vec::new();
        loop {
            let left =
                deadline.saturating_duration_since(tokio::time::Instant::now());
            if left.is_zero() {
                return frames;
            }
            match tokio::time::timeout(left, self.next()).await {
                Ok(Some(frame)) => frames.push(frame),
                Ok(None) | Err(_) => return frames,
            }
        }
    }
}

fn parse_frame(frame: &str) -> Option<(String, Value)> {
    let mut event = None;
    let mut data = String::new();
    for line in frame.lines() {
        if let Some(name) = line.strip_prefix("event:") {
            event = Some(name.trim().to_string());
        } else if let Some(payload) = line.strip_prefix("data:") {
            data.push_str(payload.trim());
        }
        // `:` comment lines (keep-alive) carry no data.
    }
    let event = event?;
    let value = serde_json::from_str(&data).ok()?;
    Some((event, value))
}

fn seq_of(row: &Value) -> u64 {
    row.get("seq").and_then(Value::as_u64).unwrap_or(0)
}

fn user_row(text: &str) -> SessionEvent {
    synthia::session::user_message(
        serde_json::to_value(Message::user(text)).expect("a message"),
        None,
    )
}

#[tokio::test]
async fn follow_first_frame_is_a_complete_baseline() {
    let fixture = fixture().await;
    fixture.append(&user_row("hello")).await;
    fixture
        .append(&synthia::session::usage(120, 30, 150, None, None, None))
        .await;
    fixture.append(&synthia::session::turn_start(0)).await;
    let head = fixture.open(None).await;
    let mut head = head;
    let (name, baseline) = head.next().await.expect("a baseline frame");

    assert_eq!(name, "baseline");
    assert_eq!(baseline["header"]["session_id"], SESSION);
    assert_eq!(baseline["header"]["state"], "idle");
    assert_eq!(
        baseline["cursor"], 3,
        "the cursor is the last durable ordinal"
    );
    let rows = baseline["rows"].as_array().expect("rows is an array");
    assert_eq!(rows.len(), 3, "the baseline carries the durable window");
    assert_eq!(seq_of(&rows[2]), 3);
    assert!(
        baseline["live"].is_object(),
        "the live projection is always present (possibly empty)"
    );
    assert_eq!(baseline["live"]["running"], false);
    assert_eq!(
        baseline["context"]["window"], 128_000,
        "the context meter names the model's window"
    );
    assert!(
        baseline["context"]["used"].as_u64().unwrap_or(0) > 0,
        "the usage row prices the next request"
    );
    assert_eq!(baseline["pending_interrupts"], json!([]));
    assert_eq!(
        baseline["request_snapshot_hash"], "",
        "a log with no snapshot has no fingerprint"
    );
}

#[tokio::test]
async fn follow_rejects_a_cursor_past_the_tail() {
    let fixture = fixture().await;
    fixture.append(&user_row("hello")).await;
    let response: Response = follow(
        State(Arc::clone(&fixture.state)),
        AppPath(SESSION.to_string()),
        AppQuery(FollowQuery { from: Some(9) }),
    )
    .await
    .expect("a response")
    .into_response();
    assert_eq!(response.status(), axum::http::StatusCode::GONE);
    let body = response
        .into_body()
        .collect()
        .await
        .expect("a body")
        .to_bytes();
    let value: Value =
        serde_json::from_slice(&body).expect("a JSON error body");
    assert_eq!(value["code"], "follow_cursor_truncated");
    assert_eq!(value["cursor"], 1);
}

#[tokio::test]
async fn reconnect_mid_stream_replaces_the_window_without_duplicates() {
    let fixture = fixture().await;
    fixture.append(&user_row("one")).await;
    fixture.append(&user_row("two")).await;

    let mut first = fixture.open(None).await;
    let (_, baseline) = first.next().await.expect("a baseline frame");
    assert_eq!(baseline["cursor"], 2);

    fixture.append(&user_row("three")).await;
    let mut seen = None;
    for _ in 0..10 {
        let Some((name, frame)) = first.next().await else {
            break;
        };
        if name == "row" {
            seen = Some(frame);
            break;
        }
    }
    let delivered = seen.expect("the appended row reaches the stream");
    assert_eq!(delivered["seq"], 3);
    // The connection dies here — a client that saw 1..3 reconnects.
    drop(first);

    let mut second =
        fixture.open(Some(delivered["seq"].as_u64().unwrap())).await;
    let (_, baseline) = second.next().await.expect("a fresh baseline");
    assert_eq!(
        baseline["cursor"], 3,
        "the reconnect baseline covers the row the old stream delivered"
    );
    let rows = baseline["rows"].as_array().expect("rows");
    let seqs: Vec<u64> = rows.iter().map(seq_of).collect();
    assert_eq!(seqs, vec![1, 2, 3], "the window replaces, never merges");

    // Increments start after the cursor: nothing at or below it is
    // replayed, so applying the baseline then the deltas cannot
    // duplicate a row.
    let later = second.drain_for(Duration::from_millis(250)).await;
    for (name, frame) in later {
        if name == "row" {
            assert!(
                frame["seq"].as_u64().unwrap_or(0) > 3,
                "a redundant row was replayed after the baseline: {frame}"
            );
        }
    }
}

#[tokio::test]
async fn the_paused_state_is_derived_from_the_log() {
    let fixture = fixture().await;
    fixture.append(&user_row("go")).await;
    fixture.append(&synthia::session::turn_start(0)).await;
    fixture
        .append(&synthia::session::approval_asked(
            "approval-c1",
            "c1",
            "shell",
            "sha256:args",
            "high",
            "default",
        ))
        .await;
    fixture
        .append(&synthia::session::turn_suspended(vec!["c1".into()]))
        .await;
    fixture
        .append(&synthia::session::turn_end_suspended())
        .await;

    let controller = fixture
        .state
        .get_or_create_session_controller(&fixture.user, SESSION)
        .await
        .expect("a controller");
    assert_eq!(
        controller.session_state().await,
        SessionState::AwaitingApproval,
        "an open ask in the last suspended segment pauses the session"
    );

    let mut frames = fixture.open(None).await;
    let (_, baseline) = frames.next().await.expect("a baseline");
    assert_eq!(baseline["header"]["state"], "awaiting_approval");
    let pending = baseline["pending_interrupts"]
        .as_array()
        .expect("pending_interrupts");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["call_id"], "c1");
}

// ---------------------------------------------------------------------------
// resume / compact / usage
// ---------------------------------------------------------------------------

impl Fixture {
    /// Write a suspended turn whose two held calls are still open.
    async fn suspend_two_calls(&self) -> synthia::harness::ResumeToken {
        self.suspend_two_calls_with(true).await
    }

    /// The same suspended turn, but the log carries no request
    /// snapshot: a log without one cannot have issued a valid token.
    async fn suspend_two_calls_without_snapshot(
        &self,
    ) -> synthia::harness::ResumeToken {
        self.suspend_two_calls_with(false).await
    }

    /// The suspended turn both helpers write. `snapshot` says whether
    /// the request-envelope row is stamped, as a real run stamps it.
    async fn suspend_two_calls_with(
        &self,
        snapshot: bool,
    ) -> synthia::harness::ResumeToken {
        self.append(&user_row("go")).await;
        self.append(&synthia::session::turn_start(0)).await;
        if snapshot {
            self.append(&synthia::session::request_snapshot(
                "fake",
                "fake-model",
                "params",
                "tools",
                "system",
                Vec::new(),
                "policy",
            ))
            .await;
        }
        for (id, call) in [("a1", "c1"), ("a2", "c2")] {
            self.append(&synthia::session::approval_asked(
                id,
                call,
                "shell",
                "sha256:args",
                "high",
                "default",
            ))
            .await;
        }
        self.append(&synthia::session::turn_suspended(vec![
            "c1".into(),
            "c2".into(),
        ]))
        .await;
        self.append(&synthia::session::turn_end_suspended()).await;
        let writer = self.state.session_manager.writer(&self.user, SESSION);
        let rows = writer.read().await.expect("rows");
        let tail = rows
            .iter()
            .filter_map(|row| row.get("seq").and_then(Value::as_u64))
            .max()
            .unwrap_or(rows.len() as u64);
        let hash = synthia::harness::latest_request_snapshot_hash(&rows)
            .unwrap_or_default();
        synthia::harness::ResumeToken::new(
            tail,
            vec!["c1".into(), "c2".into()],
            hash,
        )
    }
}

/// Write a suspended turn whose two held calls were actually recorded
/// in an assistant message, so a resume can execute them and the
/// durable dispatch pairing is observable.
async fn suspended_with_tool_calls(
    fixture: &Fixture,
) -> synthia::harness::ResumeToken {
    fixture.append(&user_row("go")).await;
    fixture.append(&synthia::session::turn_start(0)).await;
    record_two_tool_calls(fixture).await;
    fixture
        .append(&synthia::session::request_snapshot(
            "fake",
            "fake-model",
            "params",
            "tools",
            "system",
            Vec::new(),
            "policy",
        ))
        .await;
    for (id, call) in [("a1", "c1"), ("a2", "c2")] {
        fixture
            .append(&synthia::session::approval_asked(
                id,
                call,
                "read",
                "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
                "high",
                "default",
            ))
            .await;
    }
    fixture
        .append(&synthia::session::turn_suspended(vec![
            "c1".into(),
            "c2".into(),
        ]))
        .await;
    fixture
        .append(&synthia::session::turn_end_suspended())
        .await;
    let writer = fixture.state.session_manager.writer(&fixture.user, SESSION);
    let rows = writer.read().await.expect("rows");
    let tail = rows
        .iter()
        .filter_map(|row| row.get("seq").and_then(Value::as_u64))
        .max()
        .unwrap_or(rows.len() as u64);
    let hash = synthia::harness::latest_request_snapshot_hash(&rows)
        .unwrap_or_default();
    synthia::harness::ResumeToken::new(
        tail,
        vec!["c1".into(), "c2".into()],
        hash,
    )
}

/// Wait for both decisions and return the rows.
async fn await_decisions(
    writer: &Arc<synthia::session::LogWriter>,
) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let rows = writer.read().await.expect("rows");
        let decided = rows
            .iter()
            .filter(|row| row["type"] == "approval_decided")
            .count();
        if decided == 2 || tokio::time::Instant::now() > deadline {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn error_of<T>(
    result: Result<T, crate::api::AppError>,
) -> (axum::http::StatusCode, &'static str) {
    match result {
        Ok(_) => panic!("expected an error response"),
        Err(error) => (error.status_code, error.code),
    }
}

/// Post a resume for `token`, answering both held calls.
///
/// The decisions cover the two interrupts the fixture suspends, so a
/// request that reaches the coverage check is accepted; a request
/// that fails earlier carries the code that failure is about.
async fn post_resume(
    fixture: &Fixture,
    token: &synthia::harness::ResumeToken,
    allow_drift: bool,
) -> Result<axum::http::StatusCode, crate::api::AppError> {
    crate::routes::follow::resume(
        State(Arc::clone(&fixture.state)),
        AppPath(SESSION.to_string()),
        crate::api::AppJson(crate::routes::follow::ResumeRequest {
            resume_token: token.clone(),
            interrupts: vec![
                crate::routes::follow::InterruptDecision {
                    id: "a1".to_string(),
                    status: synthia::harness::ResumeStatus::Resolved,
                    payload: None,
                },
                crate::routes::follow::InterruptDecision {
                    id: "a2".to_string(),
                    status: synthia::harness::ResumeStatus::Rejected,
                    payload: Some(json!({"reason": "no"})),
                },
            ],
            allow_drift,
        }),
    )
    .await
}

#[tokio::test]
async fn resume_must_cover_every_open_interrupt() {
    let fixture = fixture().await;
    let token = fixture.suspend_two_calls().await;
    let result = crate::routes::follow::resume(
        State(Arc::clone(&fixture.state)),
        AppPath(SESSION.to_string()),
        crate::api::AppJson(crate::routes::follow::ResumeRequest {
            resume_token: token,
            interrupts: vec![crate::routes::follow::InterruptDecision {
                id: "a1".to_string(),
                status: synthia::harness::ResumeStatus::Resolved,
                payload: None,
            }],
            allow_drift: false,
        }),
    )
    .await;
    let (status, code) = error_of(result).await;
    assert_eq!(status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(code, "incomplete_resume");
}

#[tokio::test]
async fn a_resume_conflict_names_a_log_that_moved_on() {
    let fixture = fixture().await;
    let mut token = fixture.suspend_two_calls().await;
    token.last_seq += 1;
    let (status, code) =
        error_of(post_resume(&fixture, &token, false).await).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert_eq!(code, "resume_conflict");
}

#[tokio::test]
async fn a_resume_conflict_names_a_pending_set_the_log_does_not_hold() {
    let fixture = fixture().await;
    let mut token = fixture.suspend_two_calls().await;
    // The log holds c1 and c2; the token names c1 and a call the log
    // never asked about.
    token.pending = vec!["c1".into(), "c9".into()];
    let (status, code) =
        error_of(post_resume(&fixture, &token, false).await).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert_eq!(code, "resume_conflict");
}

#[tokio::test]
async fn a_resume_without_a_request_snapshot_is_refused() {
    let fixture = fixture().await;
    let token = fixture.suspend_two_calls_without_snapshot().await;
    let (status, code) =
        error_of(post_resume(&fixture, &token, false).await).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert_eq!(code, "resume_no_snapshot");
}

#[tokio::test]
async fn resume_replays_an_applied_token_as_a_success() {
    let fixture = fixture().await;
    let token = suspended_with_tool_calls(&fixture).await;
    assert_eq!(
        post_resume(&fixture, &token, true).await.expect("accepted"),
        axum::http::StatusCode::ACCEPTED
    );
    let writer = fixture.state.session_manager.writer(&fixture.user, SESSION);
    let rows = await_decisions(&writer).await;
    assert_eq!(
        rows.iter()
            .filter(|row| row["type"] == "approval_decided")
            .count(),
        2,
        "the run recorded both decisions"
    );
    // The client never saw the answer and sends the same token again.
    // The decisions are in the log, so the retry is a replay: it must
    // not become a second decision for one call, and it must not be
    // reported as a conflict either.
    assert_eq!(
        post_resume(&fixture, &token, true)
            .await
            .expect("the replay is a success"),
        axum::http::StatusCode::OK
    );
}

#[tokio::test]
async fn resume_accepts_a_complete_decision_set() {
    let fixture = fixture().await;
    let token = suspended_with_tool_calls(&fixture).await;
    let result = crate::routes::follow::resume(
        State(Arc::clone(&fixture.state)),
        AppPath(SESSION.to_string()),
        crate::api::AppJson(crate::routes::follow::ResumeRequest {
            resume_token: token,
            interrupts: vec![
                crate::routes::follow::InterruptDecision {
                    id: "a1".to_string(),
                    status: synthia::harness::ResumeStatus::Resolved,
                    payload: None,
                },
                crate::routes::follow::InterruptDecision {
                    id: "a2".to_string(),
                    status: synthia::harness::ResumeStatus::Cancelled,
                    payload: None,
                },
            ],
            allow_drift: true,
        }),
    )
    .await;
    assert_eq!(result.expect("accepted"), axum::http::StatusCode::ACCEPTED);
    let writer = fixture.state.session_manager.writer(&fixture.user, SESSION);
    let rows = await_decisions(&writer).await;
    let dispatched: Vec<&str> = rows
        .iter()
        .filter(|row| row["type"] == "tool_dispatch")
        .filter_map(|row| row["call_id"].as_str())
        .collect();
    assert_eq!(
        dispatched,
        vec!["c1"],
        "the resolved call ran; the cancelled one did not"
    );
    let decided = |call_id: &str| {
        rows.iter()
            .find(|row| {
                row["type"] == "approval_decided" && row["call_id"] == call_id
            })
            .unwrap_or_else(|| panic!("a decision for {call_id}"))
    };
    assert_eq!(decided("c1")["status"], "approved");
    assert_eq!(decided("c2")["status"], "cancelled");
}

#[tokio::test]
async fn compact_is_rejected_while_suspended() {
    let fixture = fixture().await;
    let _token = fixture.suspend_two_calls().await;
    let result = crate::routes::follow::compact(
        State(Arc::clone(&fixture.state)),
        AppPath(SESSION.to_string()),
    )
    .await;
    let (status, code) = error_of(result).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert_eq!(code, "session_suspended");
}

#[tokio::test]
async fn compact_without_a_configured_policy_is_a_conflict() {
    let fixture = fixture().await;
    fixture.append(&user_row("hello")).await;
    let result = crate::routes::follow::compact(
        State(Arc::clone(&fixture.state)),
        AppPath(SESSION.to_string()),
    )
    .await;
    let (status, code) = error_of(result).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert_eq!(code, "compaction_not_configured");
}

#[tokio::test]
async fn session_usage_is_scoped_to_the_session() {
    let fixture = fixture().await;
    fixture.append(&user_row("mine")).await;
    fixture.append(&synthia::session::turn_start(0)).await;
    fixture
        .append(&synthia::session::usage(100, 10, 110, None, None, None))
        .await;
    fixture
        .append(&synthia::session::turn_end(0, "completed"))
        .await;
    let other = "other-session";
    fixture
        .state
        .session_manager
        .create_with_user(other.to_string(), fixture.user.clone())
        .await
        .expect("the other session is created");
    let other_writer =
        fixture.state.session_manager.writer(&fixture.user, other);
    other_writer
        .append_event(&synthia::session::usage(7, 3, 10, None, None, None))
        .await
        .expect("append the other session's usage");
    other_writer.flush().await.expect("flush");

    let mine = crate::routes::follow::usage(
        State(Arc::clone(&fixture.state)),
        AppPath(SESSION.to_string()),
    )
    .await
    .expect("usage")
    .0;
    assert_eq!(mine.input_tokens, 100);
    assert_eq!(mine.output_tokens, 10);
    assert_eq!(mine.turns, 1);
    assert_eq!(mine.context_window, Some(128_000));

    let theirs = crate::routes::follow::usage(
        State(Arc::clone(&fixture.state)),
        AppPath(other.to_string()),
    )
    .await
    .expect("usage")
    .0;
    assert_eq!(
        theirs.input_tokens, 7,
        "the other session's tokens are not counted here"
    );
    assert_eq!(theirs.output_tokens, 3);
    assert_eq!(theirs.turns, 0);
}

#[tokio::test]
async fn a_queued_prompt_does_not_start_a_run_while_suspended() {
    let fixture = fixture().await;
    let _token = fixture.suspend_two_calls().await;
    let controller = fixture
        .state
        .get_or_create_session_controller(&fixture.user, SESSION)
        .await
        .expect("a controller");
    controller
        .submit(crate::session::controller::SessionOp::Prompt {
            content: "another question".to_string(),
            priority: 1,
        })
        .await
        .expect("the prompt is queued");
    tokio::time::sleep(Duration::from_millis(300)).await;

    let writer = fixture.state.session_manager.writer(&fixture.user, SESSION);
    let rows = writer.read().await.expect("rows");
    assert!(
        !rows
            .iter()
            .any(|row| row.get("type").and_then(Value::as_str)
                == Some("assistant_message")),
        "the queued prompt must not start a run over an open approval"
    );
    assert_eq!(
        controller.session_state().await,
        SessionState::AwaitingApproval,
        "the session stays paused until the interrupts are answered"
    );
}

#[tokio::test]
async fn a_suspension_pushes_an_interrupts_frame() {
    let fixture = fixture().await;
    let controller = fixture
        .state
        .get_or_create_session_controller(&fixture.user, SESSION)
        .await
        .expect("a controller");
    let mut frames = fixture.open(None).await;
    let (_, baseline) = frames.next().await.expect("a baseline");
    assert_eq!(baseline["pending_interrupts"], json!([]));

    // The durable rows land before the live event, exactly as the
    // harness's suspension boundary orders them.
    fixture.append(&synthia::session::turn_start(0)).await;
    fixture
        .append(&synthia::session::approval_asked(
            "approval-c1",
            "c1",
            "shell",
            "sha256:args",
            "high",
            "default",
        ))
        .await;
    fixture
        .append(&synthia::session::turn_suspended(vec!["c1".into()]))
        .await;
    fixture
        .append(&synthia::session::turn_end_suspended())
        .await;
    controller.broadcast_event(&synthia::harness::AgentEvent::System(
        synthia::harness::SystemEvent::Suspended(
            synthia::harness::Suspension::AwaitingApproval {
                interrupts: vec![synthia::harness::ApprovalRequest::new(
                    "approval-c1",
                    "c1",
                    "shell",
                    "sha256:args",
                    synthia::tool::Risk::High,
                    "default",
                )],
                resume: synthia::harness::ResumeToken::new(
                    0,
                    vec!["c1".into()],
                    "",
                ),
            },
        ),
    ));

    let mut found = None;
    for _ in 0..10 {
        let Some((name, frame)) = frames.next().await else {
            break;
        };
        if name == "interrupts" {
            found = Some(frame);
            break;
        }
    }
    let frame = found.expect("the suspension reaches the follower");
    let pending = frame["pending_interrupts"].as_array().expect("pending");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0]["id"], "approval-c1");
}

// ---------------------------------------------------------------------------
// Decision binding, live frames, and the compaction gate
// ---------------------------------------------------------------------------

/// Append an assistant message whose two tool calls the resume can
/// execute, so the durable pairing is observable at the route.
async fn record_two_tool_calls(fixture: &Fixture) {
    use synthia::provider::{Content, ContentPart, ToolUse};
    let message = synthia::provider::Message {
        role: synthia::provider::Role::Assistant,
        content: Content::Multi(vec![
            ContentPart::ToolUse(ToolUse {
                id: "c1".to_string(),
                name: "read".to_string(),
                input: json!({}),
            }),
            ContentPart::ToolUse(ToolUse {
                id: "c2".to_string(),
                name: "read".to_string(),
                input: json!({}),
            }),
        ]),
        tool_call_id: None,
        name: None,
        ..Default::default()
    };
    let event = synthia::session::SessionEvent::AssistantMessage {
        seq: 0,
        ts: String::new(),
        data: serde_json::to_value(message).expect("message serializes"),
        surface_op: None,
    };
    fixture.append(&event).await;
}

#[tokio::test]
async fn resume_decisions_pair_by_id_not_position() {
    let fixture = fixture().await;
    let token = suspended_with_tool_calls(&fixture).await;
    // Reject c2 first, resolve c1 second: the list order is the
    // client's, the binding is the id's.
    crate::routes::follow::resume(
        State(Arc::clone(&fixture.state)),
        AppPath(SESSION.to_string()),
        crate::api::AppJson(crate::routes::follow::ResumeRequest {
            resume_token: token,
            interrupts: vec![
                crate::routes::follow::InterruptDecision {
                    id: "a2".to_string(),
                    status: synthia::harness::ResumeStatus::Rejected,
                    payload: Some(json!({"reason": "not this one"})),
                },
                crate::routes::follow::InterruptDecision {
                    id: "a1".to_string(),
                    status: synthia::harness::ResumeStatus::Resolved,
                    payload: None,
                },
            ],
            allow_drift: true,
        }),
    )
    .await
    .expect("accepted");

    let writer = fixture.state.session_manager.writer(&fixture.user, SESSION);
    let rows = await_decisions(&writer).await;
    let decided = |call_id: &str| {
        rows.iter()
            .find(|row| {
                row["type"] == "approval_decided" && row["call_id"] == call_id
            })
            .unwrap_or_else(|| panic!("a decision for {call_id}"))
    };
    assert_eq!(decided("c1")["status"], "approved");
    assert_eq!(decided("c2")["status"], "denied");
    assert_eq!(
        decided("c2")["payload"]["reason"],
        "not this one",
        "the payload stays with its own call"
    );
    let dispatched: Vec<&str> = rows
        .iter()
        .filter(|row| row["type"] == "tool_dispatch")
        .filter_map(|row| row["call_id"].as_str())
        .collect();
    assert_eq!(
        dispatched,
        vec!["c1"],
        "the rejected call writes no dispatch intent"
    );
    let denied = rows
        .iter()
        .find(|row| {
            row["type"] == "tool_result" && row["data"]["tool_call_id"] == "c2"
        })
        .expect("a result for the rejected call");
    assert_eq!(denied["is_error"], json!(true));
}

#[tokio::test]
async fn a_duplicate_decision_is_unprocessable() {
    let fixture = fixture().await;
    let token = fixture.suspend_two_calls().await;
    let result = crate::routes::follow::resume(
        State(Arc::clone(&fixture.state)),
        AppPath(SESSION.to_string()),
        crate::api::AppJson(crate::routes::follow::ResumeRequest {
            resume_token: token,
            interrupts: vec![
                crate::routes::follow::InterruptDecision {
                    id: "a1".to_string(),
                    status: synthia::harness::ResumeStatus::Resolved,
                    payload: None,
                },
                crate::routes::follow::InterruptDecision {
                    id: "a1".to_string(),
                    status: synthia::harness::ResumeStatus::Rejected,
                    payload: None,
                },
            ],
            allow_drift: false,
        }),
    )
    .await;
    let (status, code) = error_of(result).await;
    assert_eq!(status, axum::http::StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(code, "incomplete_resume");
}

#[tokio::test]
async fn live_reasoning_is_not_duplicated() {
    use synthia::{
        harness::AgentEvent,
        provider::{ContentPart, ReasoningContent, SamplingResult},
    };
    let fixture = fixture().await;
    let controller = fixture
        .state
        .get_or_create_session_controller(&fixture.user, SESSION)
        .await
        .expect("a controller");
    controller.reset_live_for_test();
    controller.tap_live_for_test(&AgentEvent::Model(ContentPart::Reasoning(
        ReasoningContent {
            text: "chain of thought".to_string(),
            signature: None,
        },
    )));
    controller.tap_live_for_test(&AgentEvent::ModelDone(SamplingResult {
        reasoning: "chain of thought".to_string(),
        ..SamplingResult::default()
    }));
    assert_eq!(
        controller.live_snapshot().reasoning,
        "chain of thought",
        "the committed reasoning must not repeat the streamed text"
    );

    controller.tap_live_for_test(&AgentEvent::Model(ContentPart::Text(
        synthia::provider::TextContent {
            text: "hello".to_string(),
            cache_control: None,
        },
    )));
    controller.tap_live_for_test(&AgentEvent::ModelDone(SamplingResult {
        text: "hello".to_string(),
        ..SamplingResult::default()
    }));
    assert_eq!(controller.live_snapshot().text, "hello");
}

#[tokio::test]
async fn a_durable_row_names_the_turn_it_supersedes() {
    use synthia::{harness::AgentEvent, provider::ContentPart};
    let fixture = fixture().await;
    let controller = fixture
        .state
        .get_or_create_session_controller(&fixture.user, SESSION)
        .await
        .expect("a controller");
    controller.reset_live_for_test();
    let turn_id = controller.live_snapshot().turn_id;
    assert!(!turn_id.is_empty(), "a run gets a turn id");

    let mut frames = fixture.open(None).await;
    let (_, baseline) = frames.next().await.expect("a baseline");
    assert_eq!(baseline["live"]["turn_id"], turn_id);

    controller.tap_live_for_test(&AgentEvent::Model(ContentPart::Text(
        synthia::provider::TextContent {
            text: "streaming".to_string(),
            cache_control: None,
        },
    )));
    let (name, live) = frames.next().await.expect("a live frame");
    assert_eq!(name, "live");
    assert_eq!(live["turn_id"], turn_id);
    assert_eq!(live["text"], "streaming");

    // The prompt and the step boundary are part of the running turn but
    // are NOT content the projection renders: labelling them with the
    // turn id retired the projection on the first poll, blanking the
    // streaming answer until the next live frame.
    fixture.append(&user_row("a durable row")).await;
    let user_frame = next_row_of_type(&mut frames, "user_message").await;
    assert_eq!(
        user_frame["turn_id"],
        Value::Null,
        "a prompt row does not supersede the projection"
    );

    fixture.append(&synthia::session::step_start(0, 0)).await;
    let step_frame = next_row_of_type(&mut frames, "step").await;
    assert_eq!(
        step_frame["turn_id"],
        Value::Null,
        "a step boundary does not supersede the projection"
    );

    // The assistant row is what the projection was rendering, so it is
    // labelled with the turn it supersedes.
    let writer = fixture.state.session_manager.writer(&fixture.user, SESSION);
    writer
        .append(json!({
            "type": "assistant_message",
            "data": {
                "role": "assistant",
                "content": {"Multi": [{"type": "text", "text": "done"}]},
            },
        }))
        .await
        .expect("append the assistant row");
    writer.flush().await.expect("flush the assistant row");
    let assistant_frame =
        next_row_of_type(&mut frames, "assistant_message").await;
    assert_eq!(
        assistant_frame["turn_id"], turn_id,
        "the assistant row names the turn whose projection it supersedes"
    );
}

/// The next `row` frame whose durable row has `kind`.
async fn next_row_of_type(frames: &mut Frames, kind: &str) -> Value {
    loop {
        let (name, frame) = frames.next().await.expect("a row frame");
        if name == "row" && frame["row"]["type"] == json!(kind) {
            return frame;
        }
    }
}

#[tokio::test]
async fn the_status_surface_reports_a_paused_session() {
    let fixture = fixture().await;
    let _token = fixture.suspend_two_calls().await;
    let detail = crate::routes::sessions::get_session(
        State(Arc::clone(&fixture.state)),
        AppPath(SESSION.to_string()),
    )
    .await
    .expect("detail")
    .0;
    assert_eq!(detail.status, "input_required");
}

/// A run that never produces an event until its token fires — the
/// "still working" shape compact must refuse.
struct BlockingFactory;

impl crate::session::controller::RunStreamFactory for BlockingFactory {
    fn run_stream(
        &self,
        _config: crate::agent_run_config::AgentRunConfig,
        _overrides: crate::agent_run_config::RunOverrides,
        _input: synthia::harness::AgentInput,
        _cancel: Arc<synthia::core::AtomicCancelToken>,
    ) -> std::pin::Pin<
        Box<
            dyn futures::Stream<Item = synthia::harness::AgentEvent>
                + Send
                + 'static,
        >,
    > {
        // Never settles, never yields: the controller stays Running.
        Box::pin(futures::stream::pending::<synthia::harness::AgentEvent>())
    }
}

#[tokio::test]
async fn compact_is_rejected_while_running() {
    let temp = tempfile::TempDir::new().expect("a temp workspace");
    let registry = SessionRegistry::new(temp.path().to_path_buf());
    let state =
        Arc::new(AppState::for_test(registry, temp.path().to_path_buf()).await);
    let user = state.default_user_id().to_string();
    state
        .session_manager
        .create_with_user(SESSION.to_string(), user.clone())
        .await
        .expect("the session is created");
    let deps = crate::session::controller::RunDependencies::new(
        Arc::clone(&state.default_provider),
        Arc::clone(&state.tool_registry),
        state.workspace_root.clone(),
        state.system_prompt.clone(),
    );
    let controller = crate::session::controller::SessionController::spawn(
        user.clone(),
        SESSION,
        state.input_queue.clone(),
        state.session_manager.sink(&user, SESSION),
        state.session_manager.writer(&user, SESSION),
        deps,
        Duration::from_secs(300),
        Arc::new(BlockingFactory),
    );
    state
        .active_sessions
        .insert((user.clone(), SESSION.to_string()), Arc::clone(&controller));
    controller
        .submit(crate::session::controller::SessionOp::Prompt {
            content: "work".to_string(),
            priority: 1,
        })
        .await
        .expect("queued");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    while controller.state() != SessionState::Running {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run never started"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let result = crate::routes::follow::compact(
        State(Arc::clone(&state)),
        AppPath(SESSION.to_string()),
    )
    .await;
    let (status, code) = error_of(result).await;
    assert_eq!(status, axum::http::StatusCode::CONFLICT);
    assert_eq!(code, "session_busy");
}

#[tokio::test]
async fn a_run_cannot_start_while_the_compaction_gate_is_held() {
    let fixture = fixture().await;
    let controller = fixture
        .state
        .get_or_create_session_controller(&fixture.user, SESSION)
        .await
        .expect("a controller");
    let gate = controller
        .compaction_gate()
        .await
        .expect("the owner has a gate");
    controller
        .submit(crate::session::controller::SessionOp::Prompt {
            content: "start me".to_string(),
            priority: 1,
        })
        .await
        .expect("queued");
    tokio::time::sleep(Duration::from_millis(200)).await;

    let writer = fixture.state.session_manager.writer(&fixture.user, SESSION);
    let rows = writer.read().await.expect("rows");
    assert!(
        !rows.iter().any(|row| row["type"] == "request_header"),
        "a run started while a compaction held the gate"
    );
    assert_eq!(controller.state(), SessionState::Idle);

    drop(gate);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        let rows = writer.read().await.expect("rows");
        if rows.iter().any(|row| row["type"] == "request_header") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the run did not start after the gate was released"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn a_settled_tool_call_advances_the_live_projection() {
    use synthia::{harness::AgentEvent, provider::ToolUse, tool::ToolOutput};
    let fixture = fixture().await;
    let controller = fixture
        .state
        .get_or_create_session_controller(&fixture.user, SESSION)
        .await
        .expect("a controller");
    controller.reset_live_for_test();
    controller.tap_live_for_test(&AgentEvent::Model(
        synthia::provider::ContentPart::ToolUse(ToolUse {
            id: "c1".to_string(),
            name: "read".to_string(),
            input: json!({"path": "src/lib.rs"}),
        }),
    ));
    let proposed = controller.live_snapshot();
    assert_eq!(proposed.tool_calls.len(), 1);
    assert_eq!(proposed.tool_calls[0].status, "running");

    controller.tap_live_for_test(&AgentEvent::System(
        synthia::harness::SystemEvent::ToolProgress {
            tool_name: "read".to_string(),
            call_id: "c1".to_string(),
            output: ToolOutput::text("file body"),
        },
    ));
    let settled = controller.live_snapshot();
    assert!(
        settled.live_seq > proposed.live_seq,
        "settling a call must advance the frame counter"
    );
    assert_eq!(settled.tool_calls[0].status, "completed");
    assert_eq!(
        settled.tool_calls[0].output.as_deref(),
        Some("file body"),
        "the settled output reaches the client"
    );
}

// ---------------------------------------------------------------------------
// Fix round 2: disk-only status, ordinal holes, settled frames, straddle
// ---------------------------------------------------------------------------

/// A fixture whose deployment has a compaction policy, so `/compact`
/// reaches the planner instead of the "not configured" guard.
async fn fixture_with_compaction() -> Fixture {
    let temp = tempfile::TempDir::new().expect("a temp workspace");
    let registry = SessionRegistry::new(temp.path().to_path_buf());
    let mut state =
        AppState::for_test(registry, temp.path().to_path_buf()).await;
    state.default_compaction =
        Some(synthia::context::CompactionSettings::default());
    let state = Arc::new(state);
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
    Fixture {
        state,
        user,
        _temp: temp,
    }
}

/// Forget the session in memory but leave its transcript on disk — the
/// shape a restarted server sees.
async fn forget_in_memory(fixture: &Fixture) {
    fixture
        .state
        .active_sessions
        .remove(&(fixture.user.clone(), SESSION.to_string()));
    fixture.state.session_manager.remove(SESSION).await;
}

#[tokio::test]
async fn a_restarted_session_reports_the_pause_in_the_list_and_detail() {
    let fixture = fixture().await;
    let _token = fixture.suspend_two_calls().await;
    forget_in_memory(&fixture).await;

    let detail = crate::routes::sessions::get_session(
        State(Arc::clone(&fixture.state)),
        AppPath(SESSION.to_string()),
    )
    .await
    .expect("detail")
    .0;
    assert_eq!(
        detail.status, "input_required",
        "the disk-only detail derives the pause from the transcript"
    );

    let list = crate::routes::sessions::list_sessions(
        State(Arc::clone(&fixture.state)),
        crate::api::AppQuery(crate::api::SessionPageQuery::new()),
    )
    .await
    .expect("list")
    .0;
    let row = list
        .data
        .iter()
        .find(|summary| summary.id == SESSION)
        .expect("the disk-only session is listed");
    assert_eq!(
        row.status, "input_required",
        "the disk-only list derives the pause from the transcript"
    );
}

#[tokio::test]
async fn a_hole_in_the_ordinals_does_not_stall_the_tail() {
    let fixture = fixture().await;
    fixture.append(&user_row("one")).await;
    fixture.append(&user_row("two")).await;
    fixture.append(&user_row("three")).await;
    // Burn an ordinal without writing a row: the stamped seq and the
    // append position now disagree by one.
    let sink = fixture.state.session_manager.sink(&fixture.user, SESSION);
    sink.alloc_seq();
    fixture.append(&user_row("after the hole")).await;

    let mut frames = fixture.open(None).await;
    let (_, baseline) = frames.next().await.expect("a baseline");
    assert_eq!(baseline["cursor"], 5, "the cursor is the stamped tail");
    assert_eq!(baseline["rows"].as_array().expect("rows").len(), 4);

    fixture.append(&user_row("the next row")).await;
    let mut delivered = None;
    for _ in 0..10 {
        let Some((name, frame)) = frames.next().await else {
            break;
        };
        if name == "row" {
            delivered = Some(frame);
            break;
        }
    }
    let frame = delivered.expect("the row past the hole reaches the tail");
    assert_eq!(
        frame["seq"], 6,
        "the stamped ordinal of the row after the hole"
    );
    assert_eq!(
        frame["row"]["data"]["content"]["Single"]["text"],
        "the next row"
    );
}

#[tokio::test]
async fn the_last_live_frame_reports_the_turn_settled() {
    use synthia::harness::{AgentEvent, SessionEndReason, SystemEvent};
    let fixture = fixture().await;
    let controller = fixture
        .state
        .get_or_create_session_controller(&fixture.user, SESSION)
        .await
        .expect("a controller");
    controller.reset_live_for_test();
    let turn_id = controller.live_snapshot().turn_id;
    let mut frames = fixture.open(None).await;
    let _ = frames.next().await.expect("a baseline");

    controller.tap_live_for_test(&AgentEvent::Model(
        synthia::provider::ContentPart::Text(synthia::provider::TextContent {
            text: "working".to_string(),
            cache_control: None,
        }),
    ));
    let (name, live) = frames.next().await.expect("a live frame");
    assert_eq!(name, "live");
    assert_eq!(live["running"], true);

    controller.tap_live_for_test(&AgentEvent::System(
        SystemEvent::SessionEnded {
            reason: SessionEndReason::Completed,
        },
    ));
    let (name, settled) = frames.next().await.expect("a settled live frame");
    assert_eq!(name, "live");
    assert_eq!(
        settled["running"], false,
        "the settling run reaches the client as a live frame"
    );
    assert_eq!(settled["turn_id"], turn_id);
}

#[tokio::test]
async fn a_batch_straddling_two_runs_labels_only_the_current_turn() {
    let fixture = fixture().await;
    let controller = fixture
        .state
        .get_or_create_session_controller(&fixture.user, SESSION)
        .await
        .expect("a controller");
    controller.reset_live_for_test();
    let current = controller.live_snapshot().turn_id;
    let previous = json!({"type": "user_message", "seq": 1,
        "data": {"role": "user"}});
    let start = serde_json::to_value(synthia::session::turn_start(1))
        .expect("a turn row");
    let fresh =
        json!({"type": "user_message", "seq": 3, "data": {"role": "user"}});

    let labels = crate::routes::follow::turn_labels(
        &[previous, start.clone(), fresh],
        &current,
        true,
    );
    assert_eq!(
        labels,
        vec![None, None, None],
        "neither the previous run's tail nor the new prompt is labelled"
    );

    let mid_turn = [json!({"type": "assistant_message", "seq": 4})];
    assert_eq!(
        crate::routes::follow::turn_labels(&mid_turn, &current, true),
        vec![Some(current.as_str())],
        "a mid-turn batch belongs to the running turn"
    );
    assert_eq!(
        crate::routes::follow::turn_labels(&mid_turn, &current, false),
        vec![None],
        "a settled turn has no projection left to supersede"
    );
}

/// Only the rows the live projection renders carry the live turn id.
///
/// A user prompt, a `step` boundary or a `turn` boundary must not
/// supersede the projection: those rows land within the first poll of a
/// run, and retiring the projection there blanked the streaming answer
/// until the next live frame (the client had nothing else rendering it).
#[tokio::test]
async fn only_assistant_and_tool_result_rows_supersede_the_live_projection() {
    let current = "turn-live";
    let start = serde_json::to_value(synthia::session::turn_start(1))
        .expect("a turn row");
    let step = serde_json::to_value(synthia::session::step_start(1, 0))
        .expect("a step row");
    let rows = vec![
        start,
        json!({"type": "user_message", "seq": 2, "data": {"role": "user"}}),
        step,
        json!({"type": "assistant_message", "seq": 4}),
        json!({"type": "tool_result", "seq": 5}),
        json!({"type": "usage", "seq": 6}),
        json!({"type": "turn", "seq": 7, "data": {"phase": "end"}}),
    ];
    let labels = crate::routes::follow::turn_labels(&rows, current, true);
    assert_eq!(
        labels,
        vec![None, None, None, Some(current), Some(current), None, None],
        "only content rows supersede the projection"
    );
}

#[tokio::test]
async fn compact_with_a_policy_and_nothing_to_compact_is_a_metadata_200() {
    let fixture = fixture_with_compaction().await;
    fixture.append(&user_row("hello")).await;
    let response = crate::routes::follow::compact(
        State(Arc::clone(&fixture.state)),
        AppPath(SESSION.to_string()),
    )
    .await
    .expect("a 200 with metadata, not a 409")
    .0;
    assert!(!response.committed);
    assert_eq!(response.reason, Some("under_budget"));
    assert!(response.start.is_none() && response.end.is_none());

    let writer = fixture.state.session_manager.writer(&fixture.user, SESSION);
    let rows = writer.read().await.expect("rows");
    assert!(
        !rows.iter().any(|row| row["type"] == "compaction"),
        "a skipped pass writes no replace"
    );
}

#[tokio::test]
async fn the_baseline_is_the_log_tail_and_the_head_is_read_separately() {
    // Task 16: the baseline no longer reads the whole log. It carries
    // the last `FOLLOW_WINDOW` rows, read from the end of the file, and
    // the two whole-log header facts come from a bounded head read.
    let fixture = fixture().await;
    let total = crate::routes::follow::FOLLOW_WINDOW + 50;
    for index in 0..total {
        fixture.append(&user_row(&format!("row {index}"))).await;
    }
    let rows = fixture
        .state
        .session_manager
        .sink(&fixture.user, SESSION)
        .read()
        .await
        .expect("the log");
    let first_ts = rows[0]["ts"].as_str().map(str::to_string);
    let last_seq = seq_of(rows.last().expect("a tail row"));

    let mut frames = fixture.open(None).await;
    let (name, baseline) = frames.next().await.expect("a baseline");
    assert_eq!(name, "baseline");
    let window = baseline["rows"].as_array().expect("rows").clone();
    assert_eq!(
        window.len(),
        crate::routes::follow::FOLLOW_WINDOW,
        "the window is exactly FOLLOW_WINDOW rows"
    );
    assert_eq!(
        window[0]["data"]["content"]["Single"]["text"],
        format!("row {}", total - crate::routes::follow::FOLLOW_WINDOW),
        "the window starts where the last FOLLOW_WINDOW rows do"
    );
    assert_eq!(baseline["cursor"], last_seq, "the cursor is the log tail");
    assert_eq!(
        baseline["header"]["created_at"],
        first_ts.expect("a head ts"),
        "created_at is the log's first row, not the window's"
    );
}

#[tokio::test]
async fn a_long_turn_still_prices_the_envelope_the_log_holds() {
    // A turn writes its `request_header` and `request_snapshot` at the
    // start; a turn long enough to cross `FOLLOW_WINDOW` leaves both
    // outside the tail. The baseline must answer with the log's
    // envelope anyway — the window alone would price the next request
    // against the *default* model's window and report an empty
    // fingerprint, which on the wire means "this log has no envelope".
    let fixture = fixture().await;
    fixture.append(&synthia::session::turn_start(0)).await;
    fixture
        .append(&synthia::session::request_header(
            "fake",
            "fake-model",
            "sha256:tools",
            "start",
        ))
        .await;
    fixture
        .append(&synthia::session::request_snapshot(
            "fake",
            "fake-model",
            "sha256:params",
            "sha256:tools",
            "sha256:system",
            vec!["identity".to_string()],
            "sha256:policy",
        ))
        .await;
    fixture
        .append(&synthia::session::usage(400, 100, 500, None, None, None))
        .await;
    // The turn runs on, past the window.
    for index in 0..(crate::routes::follow::FOLLOW_WINDOW + 20) {
        fixture
            .append(&user_row(&format!("iteration {index}")))
            .await;
    }

    let full = fixture
        .state
        .session_manager
        .sink(&fixture.user, SESSION)
        .read()
        .await
        .expect("the whole log");
    let expected_context =
        crate::routes::follow::context_meter(&fixture.state, &full);
    let expected_hash = synthia::harness::latest_request_snapshot_hash(&full)
        .expect("the fixture carries an envelope");
    assert_ne!(expected_hash, "");

    let mut frames = fixture.open(None).await;
    let (name, baseline) = frames.next().await.expect("a baseline");
    assert_eq!(name, "baseline");
    assert_eq!(
        baseline["request_snapshot_hash"], expected_hash,
        "the fingerprint is the log's newest envelope, not the window's"
    );
    assert_eq!(
        baseline["context"],
        serde_json::to_value(&expected_context).expect("a meter"),
        "the context meter is the full-log fold"
    );
    assert_eq!(
        baseline["context"]["window"],
        serde_json::json!(expected_context.window),
        "priced against the model the log ran with"
    );
}

/// A sink over a real log that counts **whole-log** reads.
///
/// `build_baseline`'s fast path is a promise not to pay for the
/// session's history on a subscription; a counter is the only way to
/// assert an absence of work.
struct ReadSpy {
    inner: Arc<dyn synthia::session::SessionSink>,
    reads: Arc<std::sync::atomic::AtomicUsize>,
}

impl ReadSpy {
    fn new(inner: Arc<dyn synthia::session::SessionSink>) -> Self {
        Self {
            inner,
            reads: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        }
    }

    fn reads(&self) -> usize {
        self.reads.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl synthia::session::SessionSink for ReadSpy {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn alloc_seq(&self) -> u64 {
        self.inner.alloc_seq()
    }

    async fn append(
        &self,
        event: &Value,
    ) -> Result<(), synthia::session::SessionError> {
        self.inner.append(event).await
    }

    async fn read(&self) -> Result<Vec<Value>, synthia::session::SessionError> {
        self.reads.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.inner.read().await
    }

    async fn read_from(
        &self,
        from: u64,
    ) -> Result<Vec<Value>, synthia::session::SessionError> {
        self.inner.read_from(from).await
    }

    async fn read_tail(
        &self,
        window: usize,
    ) -> Result<Vec<Value>, synthia::session::SessionError> {
        self.inner.read_tail(window).await
    }

    async fn read_head(
        &self,
    ) -> Result<Option<Value>, synthia::session::SessionError> {
        self.inner.read_head().await
    }

    async fn snapshot(
        &self,
    ) -> Result<synthia::session::SessionSnapshot, synthia::session::SessionError>
    {
        self.inner.snapshot().await
    }

    async fn close(
        &self,
        reason: synthia::session::SessionEndReason,
    ) -> Result<(), synthia::session::SessionError> {
        self.inner.close(reason).await
    }
}

/// Run `build_baseline` over the fixture's current log with a counting
/// sink, and return the baseline plus the number of whole-log reads it
/// took.
async fn baseline_over_spy(
    fixture: &Fixture,
) -> (crate::routes::follow::FollowSnapshot, usize) {
    let controller = fixture
        .state
        .active_sessions
        .get(&(fixture.user.clone(), SESSION.to_string()))
        .map(|entry| entry.value().clone())
        .expect("the fixture opened a controller");
    let sink: Arc<dyn synthia::session::SessionSink> =
        fixture.state.session_manager.sink(&fixture.user, SESSION);
    let rows = sink
        .read_tail(crate::routes::follow::FOLLOW_WINDOW)
        .await
        .expect("the tail");
    let head = sink.read_head().await.expect("the head");
    let spy = ReadSpy::new(Arc::clone(&sink));
    let snapshot = crate::routes::follow::build_baseline(
        &fixture.state,
        &controller,
        &fixture.user,
        SESSION,
        &spy,
        rows,
        head,
    )
    .await;
    (snapshot, spy.reads())
}

/// A window that holds the last turn **and** a usage anchor answers
/// every fold on its own: the baseline must not pay for the whole log.
///
/// This is the assertion the predicate's fast path needs — inverted, it
/// would still pass every fallback test.
#[tokio::test]
async fn a_provable_window_does_not_read_the_whole_log() {
    let fixture = fixture().await;
    fixture.append(&synthia::session::turn_start(0)).await;
    fixture
        .append(&synthia::session::request_header(
            "fake",
            "fake-model",
            "sha256:tools",
            "start",
        ))
        .await;
    fixture.append(&user_row("a short turn")).await;
    fixture
        .append(&synthia::session::usage(400, 100, 500, None, None, None))
        .await;

    let (snapshot, reads) = baseline_over_spy(&fixture).await;
    assert_eq!(
        reads, 0,
        "the window proves the turn and carries its usage anchor; \
         nothing needs the whole log"
    );
    // The numbers the fast path reports are the whole-log ones.
    let full = fixture
        .state
        .session_manager
        .sink(&fixture.user, SESSION)
        .read()
        .await
        .expect("the whole log");
    let expected = crate::routes::follow::context_meter(&fixture.state, &full);
    assert_eq!(
        (snapshot.context.used, snapshot.context.window),
        (expected.used, expected.window),
        "the anchored window prices the same request the log does"
    );
}

/// A window that holds the last turn but **no usage anchor** cannot
/// answer the context meter: its fold has no baseline, so it would
/// price the window's own surface while `/usage` prices the whole
/// surface. The predicate must send it to the whole log.
#[tokio::test]
async fn a_window_without_a_usage_anchor_reads_the_whole_log() {
    let fixture = fixture().await;
    // A first turn that priced a request and answered it …
    fixture.append(&synthia::session::turn_start(0)).await;
    fixture
        .append(&synthia::session::request_header(
            "fake",
            "fake-model",
            "sha256:tools",
            "start",
        ))
        .await;
    fixture.append(&user_row("the first turn")).await;
    fixture
        .append(&synthia::session::usage(
            1_000, 100, 1_100, None, None, None,
        ))
        .await;
    fixture.append(&user_row("first answer")).await;
    fixture
        .append(&synthia::session::turn_end(0, "completed"))
        .await;
    // … then a second turn that has run past its header with no usage
    // report of its own yet: the header reset the anchor, so the window
    // holds a turn but no baseline.
    fixture.append(&synthia::session::turn_start(1)).await;
    fixture
        .append(&synthia::session::request_header(
            "fake",
            "fake-model",
            "sha256:tools",
            "start",
        ))
        .await;
    fixture.append(&user_row("the second turn")).await;

    let (snapshot, reads) = baseline_over_spy(&fixture).await;
    assert_eq!(reads, 1, "without an anchor the meter needs the whole log");
    let full = fixture
        .state
        .session_manager
        .sink(&fixture.user, SESSION)
        .read()
        .await
        .expect("the whole log");
    let expected = crate::routes::follow::context_meter(&fixture.state, &full);
    assert_eq!(
        (snapshot.context.used, snapshot.context.window),
        (expected.used, expected.window),
        "the baseline prices the whole surface, as /usage does"
    );
    // And the window-only number would have been a different one: the
    // last turn's start is where `read_tail` would have cut.
    let start = full
        .iter()
        .rposition(|row| row["type"] == "turn")
        .expect("the fixture writes turn rows");
    let window =
        crate::routes::follow::context_meter(&fixture.state, &full[start..]);
    assert_ne!(
        window.used, expected.used,
        "the window's own fold disagrees, which is why it is not used"
    );
}
