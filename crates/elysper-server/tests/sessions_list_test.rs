//! `GET /api/v1/sessions` — the user's own session listing.
//!
//! The listing is a management view of the user's conversations, so
//! the throwaway sessions `POST /v1/messages` mints for clients that
//! name no session must not appear in it: a single Cursor or Claude
//! Code session filled the page with a dozen `anthropic-<uuid>` rows
//! in one verification pass. What these tests hold is the boundary of
//! that exclusion — the exact namespace form, the lookalike that is
//! *not* excluded, and the direct lookup that is deliberately still
//! served.

use std::sync::Arc;

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use http_body_util::BodyExt;
use elysper_server::{create_router, state::AppState};
use tower::ServiceExt;

/// The user this deployment resolves an unauthenticated request to:
/// the fixture must write into *that* namespace, or the test would be
/// asserting the leak instead of the fix.
const RESOLVED_USER: &str = elysper::session::manager::SERVER_DEFAULT_USER_ID;

/// A finished unbound Messages turn: on disk, evicted from the
/// registry when the turn ended.
const EPHEMERAL_ON_DISK: &str = "anthropic-9f1c4c6e8a2b4d7f";

/// An unbound Messages turn still open, i.e. registered rather than
/// durable — the other of the listing's two sources.
const EPHEMERAL_REGISTERED: &str = "anthropic-c4f7a0d21b8e4e69";

/// An id that shares the prefix's letters without its separator.
const LOOKALIKE: &str = "anthropicx-1";

/// The user's own conversation.
const CONVERSATION: &str = "my-conversation";

/// One transcript under `<root>/<user>/<session>/events.jsonl`.
fn write_transcript(root: &std::path::Path, session: &str, text: &str) {
    let dir = root.join(RESOLVED_USER).join(session);
    std::fs::create_dir_all(&dir).unwrap();
    let header = metadata_header(session);
    let row = serde_json::json!({
        "type": "user_message",
        "seq": 1,
        "ts": "2026-09-19T09:00:00Z",
        "data": elysper::provider::Message::user(text),
    });
    std::fs::write(dir.join("events.jsonl"), format!("{header}\n{row}\n"))
        .unwrap();
}

/// The sink's metadata header line: every log the JSONL sink wrote
/// opens with one, and the sink's readers gate on it — a fixture
/// that omits it is not a log this build can read.
fn metadata_header(session: &str) -> serde_json::Value {
    serde_json::json!({
        "_meta": {
            "kind": "elysper_session_meta",
            "schema_version":
                elysper::session::CURRENT_SCHEMA_VERSION,
            "id": session,
        }
    })
}

/// The listing endpoints, over both of the sources they merge.
async fn app() -> (tempfile::TempDir, axum::Router) {
    let temp = tempfile::TempDir::new().unwrap();
    let sessions = temp.path().join("sessions");
    write_transcript(&sessions, EPHEMERAL_ON_DISK, "say hi");
    write_transcript(&sessions, LOOKALIKE, "a user's own session");
    write_transcript(&sessions, CONVERSATION, "how do access tokens expire");

    let registry = elysper::session::manager::SessionRegistry::new(sessions);
    let state = AppState::for_test(registry, temp.path().to_path_buf()).await;
    state
        .session_manager
        .create_with_user(
            EPHEMERAL_REGISTERED.to_string(),
            RESOLVED_USER.to_string(),
        )
        .await
        .expect("register a live minted session");
    state
        .session_manager
        .create_with_user(CONVERSATION.to_string(), RESOLVED_USER.to_string())
        .await
        .expect("register the user's session");
    (temp, create_router(Arc::new(state)).await)
}

async fn get(app: &axum::Router, uri: &str) -> (StatusCode, serde_json::Value) {
    let resp = app
        .clone()
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let json =
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null);
    (status, json)
}

/// The ids the listing returned, in order.
fn listed_ids(body: &serde_json::Value) -> Vec<&str> {
    body["data"]
        .as_array()
        .expect("data array")
        .iter()
        .map(|row| row["id"].as_str().expect("id"))
        .collect()
}

/// The listing carries the user's conversations and nothing minted for
/// a client that named no session — from either source: one minted
/// session is durable (its turn finished and closed) and one is
/// registered (its turn is still open), so a filter covering only one
/// of the listing's two loops leaves a row behind and fails this test.
#[tokio::test]
async fn the_listing_omits_both_sources_of_unbound_messages_session() {
    let (_temp, app) = app().await;
    let (status, body) = get(&app, "/api/v1/sessions").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let ids = listed_ids(&body);
    assert!(
        !ids.contains(&EPHEMERAL_ON_DISK),
        "a finished minted session must not be listed: {ids:?}"
    );
    assert!(
        !ids.contains(&EPHEMERAL_REGISTERED),
        "a live minted session must not be listed: {ids:?}"
    );
    assert!(ids.contains(&CONVERSATION), "listed: {ids:?}");
}

/// The exclusion is the namespace, not the letters: an id that merely
/// begins with `anthropic` is an ordinary session and stays listed.
/// This is the boundary the route implements (`anthropic-` — separator
/// included — not `anthropic`), asserted against the live endpoint so
/// a widened prefix cannot pass silently.
#[tokio::test]
async fn the_listing_keeps_an_id_that_only_shares_the_prefix_letters() {
    let (_temp, app) = app().await;
    let (status, body) = get(&app, "/api/v1/sessions").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let ids = listed_ids(&body);
    assert!(ids.contains(&LOOKALIKE), "listed: {ids:?}");
}

/// Naming an ephemeral session is a deliberate lookup, not passive
/// browsing, so the detail route still serves it — the fix hides a row
/// from the sessions page, it does not make the session unreachable.
#[tokio::test]
async fn an_ephemeral_session_is_still_reachable_by_id() {
    let (_temp, app) = app().await;
    let (status, body) =
        get(&app, &format!("/api/v1/sessions/{EPHEMERAL_ON_DISK}")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["id"].as_str(), Some(EPHEMERAL_ON_DISK));
    assert_eq!(body["history"].as_array().map(Vec::len), Some(1), "{body}");
}

// ---------------------------------------------------------------------------
// Bounded reads
// ---------------------------------------------------------------------------

use std::sync::atomic::{AtomicUsize, Ordering};

use elysper_server::routes::sessions::SESSION_LIST_TAIL_WINDOW;

/// The session whose log the bounded-read test writes: long enough
/// that a whole-file `read()` and a bounded tail window are easy to
/// tell apart.
const LONG_LOG_SESSION: &str = "long-log-session";
const LONG_LOG_ROWS: usize = 2000;

/// A sink that counts whole-log reads and records the widest tail
/// window it was asked for.
///
/// Everything delegates to the production JSONL sink, so the rows
/// land on disk exactly as the registry's own sink would write them,
/// and `read_tail` / `read_head` keep their bounded implementations —
/// the trait defaults would forward to `read()` and count themselves.
struct CountingSink {
    inner: elysper::session::jsonl::JsonlSessionSink,
    reads: AtomicUsize,
    tails: AtomicUsize,
    widest_tail: AtomicUsize,
}

impl CountingSink {
    fn new(session_id: &str, dir: std::path::PathBuf) -> Self {
        Self {
            inner: elysper::session::jsonl::JsonlSessionSink::new(
                session_id, dir,
            ),
            reads: AtomicUsize::new(0),
            tails: AtomicUsize::new(0),
            widest_tail: AtomicUsize::new(0),
        }
    }

    fn read_count(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }

    fn tail_count(&self) -> usize {
        self.tails.load(Ordering::SeqCst)
    }

    fn widest_tail_window(&self) -> usize {
        self.widest_tail.load(Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl elysper::session::SessionSink for CountingSink {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn alloc_seq(&self) -> u64 {
        self.inner.alloc_seq()
    }

    async fn append(
        &self,
        event: &serde_json::Value,
    ) -> Result<(), elysper::session::SessionError> {
        self.inner.append(event).await
    }

    async fn read(
        &self,
    ) -> Result<Vec<serde_json::Value>, elysper::session::SessionError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read().await
    }

    async fn read_from(
        &self,
        from: u64,
    ) -> Result<Vec<serde_json::Value>, elysper::session::SessionError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read_from(from).await
    }

    async fn read_tail(
        &self,
        window: usize,
    ) -> Result<Vec<serde_json::Value>, elysper::session::SessionError> {
        self.tails.fetch_add(1, Ordering::SeqCst);
        self.widest_tail.fetch_max(window, Ordering::SeqCst);
        self.inner.read_tail(window).await
    }

    async fn read_head(
        &self,
    ) -> Result<Option<serde_json::Value>, elysper::session::SessionError> {
        self.inner.read_head().await
    }

    async fn snapshot(
        &self,
    ) -> Result<elysper::session::SessionSnapshot, elysper::session::SessionError>
    {
        self.inner.snapshot().await
    }

    async fn close(
        &self,
        reason: elysper::session::SessionEndReason,
    ) -> Result<(), elysper::session::SessionError> {
        self.inner.close(reason).await
    }
}

/// Write a long transcript straight to disk: the rows predate the
/// counting sink, exactly like a session the process reopened.
fn write_long_transcript(root: &std::path::Path, session: &str, rows: usize) {
    let dir = root.join(RESOLVED_USER).join(session);
    std::fs::create_dir_all(&dir).unwrap();
    let mut log = format!("{}\n", metadata_header(session));
    for seq in 1..=rows {
        let row = serde_json::json!({
            "type": "user_message",
            "seq": seq,
            "ts": format!("2026-09-19T09:{:02}:{:02}Z", seq / 60, seq % 60),
            "data": {"role": "user", "content": "row"},
        });
        log.push_str(&format!("{row}\n"));
    }
    std::fs::write(dir.join("events.jsonl"), log).unwrap();
}

/// A session with a 2000-row log must cost the listing one head row
/// and one bounded tail window — never a whole-file `read()`. The
/// counting sink is installed through the registry, so the route
/// reads through it exactly as it would through its own sink.
#[tokio::test]
async fn listing_sessions_reads_a_bounded_window_per_session() {
    let temp = tempfile::TempDir::new().unwrap();
    let sessions = temp.path().join("sessions");
    write_long_transcript(&sessions, LONG_LOG_SESSION, LONG_LOG_ROWS);

    let registry =
        elysper::session::manager::SessionRegistry::new(sessions.clone());
    let state = AppState::for_test(registry, temp.path().to_path_buf()).await;
    state
        .session_manager
        .create_with_user(
            LONG_LOG_SESSION.to_string(),
            RESOLVED_USER.to_string(),
        )
        .await
        .expect("register the long-log session");
    let counting = Arc::new(CountingSink::new(
        LONG_LOG_SESSION,
        sessions.join(RESOLVED_USER).join(LONG_LOG_SESSION),
    ));
    state.session_manager.install_sink(
        RESOLVED_USER,
        LONG_LOG_SESSION,
        counting.clone(),
    );

    let app = create_router(Arc::new(state)).await;
    let (status, body) = get(&app, "/api/v1/sessions").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert!(
        listed_ids(&body).contains(&LONG_LOG_SESSION),
        "the long-log session is listed: {:?}",
        listed_ids(&body)
    );
    assert_eq!(
        counting.read_count(),
        0,
        "the listing must not whole-file-read a session's log"
    );
    assert!(
        counting.tail_count() >= 1,
        "the listing read the session's tail window"
    );
    assert!(
        counting.widest_tail_window() <= SESSION_LIST_TAIL_WINDOW,
        "tail window {} exceeds the bound {}",
        counting.widest_tail_window(),
        SESSION_LIST_TAIL_WINDOW
    );
}

/// The disk-only path (the one that matters after every restart)
/// carries the same bound: the scan's head-plus-tail reads never
/// whole-file-read the session's log through the `SessionSink`
/// surface. (That the sink's own **open** stays O(1) in the log's
/// length — no eager whole-file inspection in the constructor — is
/// pinned where it is observable, on the deferred seed's own cell:
/// `elysper-session`'s `opening_a_sink_defers_the_whole_log_pass`.)
#[tokio::test]
async fn the_disk_scan_reads_a_bounded_window_per_session() {
    const DISK_ONLY: &str = "disk-only-long-log";
    let temp = tempfile::TempDir::new().unwrap();
    let sessions = temp.path().join("sessions");
    write_long_transcript(&sessions, DISK_ONLY, LONG_LOG_ROWS);

    let registry =
        elysper::session::manager::SessionRegistry::new(sessions.clone());
    let state = AppState::for_test(registry, temp.path().to_path_buf()).await;
    // Not registered: the listing must reach the session through the
    // disk scan, reading via the registry's sink for it.
    let counting = Arc::new(CountingSink::new(
        DISK_ONLY,
        sessions.join(RESOLVED_USER).join(DISK_ONLY),
    ));
    state.session_manager.install_sink(
        RESOLVED_USER,
        DISK_ONLY,
        counting.clone(),
    );

    let app = create_router(Arc::new(state)).await;
    let (status, body) = get(&app, "/api/v1/sessions").await;
    assert_eq!(status, StatusCode::OK, "{body}");

    assert!(
        listed_ids(&body).contains(&DISK_ONLY),
        "the disk-only long-log session is listed: {:?}",
        listed_ids(&body)
    );
    assert_eq!(
        counting.read_count(),
        0,
        "the disk scan must not whole-file-read a session's log"
    );
    assert!(
        counting.widest_tail_window() <= SESSION_LIST_TAIL_WINDOW,
        "tail window {} exceeds the bound {}",
        counting.widest_tail_window(),
        SESSION_LIST_TAIL_WINDOW
    );
}
