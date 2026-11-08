//! Durable parked prompts: a prompt queued for the next run has to
//! survive the node that parked it.
//!
//! Every queued turn — a plain text, a multimodal payload, a rerun —
//! parks its lossless parts on the controller, and that queue lives
//! in process memory, so a crash between the park and the run used
//! to lose the turn. The `parked_prompt` row is the park: appended
//! and flushed before the op returns, so the log a restarted node
//! reads still holds the prompt.
//!
//! The bytes never ride the log. A multimodal part is stored in the
//! attachment store first (content-addressed) and the row carries a
//! re-readable ref — the hash is what lets a reloaded prompt fetch
//! the bytes back.

use std::{sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use elysper::{
    attachment::AttachmentStore,
    core::SystemClock,
    session::{
        LogWriter,
        SessionEndReason,
        SessionError,
        SessionEvent,
        SessionSink,
        SessionSnapshot,
        jsonl::JsonlSessionSink,
    },
};
use serde_json::Value;
use tempfile::TempDir;

/// A fresh session directory.
fn session_dir() -> TempDir {
    tempfile::tempdir().expect("a temp dir")
}

/// A writer over the JSONL sink rooted at `dir`.
fn writer(dir: &TempDir) -> (Arc<JsonlSessionSink>, Arc<LogWriter>) {
    let sink = Arc::new(JsonlSessionSink::new("parked", dir.path()));
    let writer = Arc::new(LogWriter::new(
        Arc::clone(&sink) as Arc<dyn SessionSink>,
        Arc::new(SystemClock),
    ));
    (sink, writer)
}

/// Poll a fresh read until `done`, so the assertion does not race the
/// writer's flush.
async fn wait_for_rows(
    sink: &JsonlSessionSink,
    done: impl Fn(&[Value]) -> bool,
) -> Vec<Value> {
    for _ in 0..50 {
        let rows = sink.read().await.expect("a readable log");
        if done(&rows) {
            return rows;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    sink.read().await.expect("a readable log")
}

/// A 1×1 transparent PNG.
fn png_bytes() -> Vec<u8> {
    vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D,
        0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01,
        0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89, 0x00, 0x00, 0x00,
        0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63, 0xF8, 0xCF, 0xC0, 0x00,
        0x00, 0x00, 0x03, 0x00, 0x01, 0x6F, 0x32, 0x4D, 0x0E, 0x00, 0x00, 0x00,
        0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE, 0x42, 0x60, 0x82,
    ]
}

/// A parked prompt's row is on disk after the flush, and a *fresh
/// sink over the same directory* — the restart — still reads it.
#[tokio::test]
async fn a_parked_prompt_is_durable() {
    let dir = session_dir();
    let (_sink, writer) = writer(&dir);
    writer
        .append_event(&SessionEvent::ParkedPrompt {
            seq: 0,
            ts: String::new(),
            park_id: "park-durable".to_string(),
            text: "a queued question".to_string(),
            attachments: Vec::new(),
            agent_name: Some("reviewer".to_string()),
            provider: None,
            model: None,
            rerun: false,
        })
        .await
        .expect("the park row queues");
    writer.flush().await.expect("the park row is durable");

    // The restart: a sink that never saw the append.
    let reopened = JsonlSessionSink::new("parked", dir.path());
    let rows = wait_for_rows(&reopened, |rows| !rows.is_empty()).await;
    let row = rows
        .iter()
        .find(|row| row["type"] == "parked_prompt")
        .unwrap_or_else(|| panic!("parked prompt row missing: {rows:?}"));
    assert_eq!(row["text"], "a queued question");
    assert_eq!(
        row["agent_name"], "reviewer",
        "the turn's agent selection survives the crash"
    );
    assert_eq!(
        row.get("rerun"),
        None,
        "an ordinary prompt carries no rerun flag"
    );
    assert_eq!(
        row.get("attachments"),
        None,
        "a text-only prompt carries no attachment field"
    );
}

/// The row's attachment ref resolves back to the bytes through the
/// store — the log itself never carries them.
#[tokio::test]
async fn a_parked_prompt_attachment_ref_resolves_through_the_store() {
    let dir = session_dir();
    let (_sink, writer) = writer(&dir);
    let store = AttachmentStore::default();
    let bytes = png_bytes();
    let saved = store.save_image(&bytes, "image/png").expect("save");

    writer
        .append_event(&SessionEvent::ParkedPrompt {
            seq: 0,
            ts: String::new(),
            park_id: "park-image".to_string(),
            text: "look at this".to_string(),
            attachments: vec![elysper::session::AttachmentRefRow {
                kind: "image".to_string(),
                hash: Some(saved.hash.clone()),
                mime_type: Some("image/png".to_string()),
                byte_len: Some(bytes.len() as u64),
                name: None,
                unstored: None,
            }],
            agent_name: None,
            provider: None,
            model: None,
            rerun: false,
        })
        .await
        .expect("the park row queues");
    writer.flush().await.expect("the park row is durable");

    let reopened = JsonlSessionSink::new("parked", dir.path());
    let rows = wait_for_rows(&reopened, |rows| !rows.is_empty()).await;
    let row = rows
        .iter()
        .find(|row| row["type"] == "parked_prompt")
        .unwrap_or_else(|| panic!("parked prompt row missing: {rows:?}"));

    // The bytes are not in the row …
    let encoded = format!("\"{}\"", BASE64.encode(&bytes));
    assert!(
        !row.to_string().contains(&encoded),
        "the row must carry the ref, never the bytes: {row}"
    );
    // … and the ref fetches them back.
    let hash = row["attachments"][0]["hash"]
        .as_str()
        .expect("the ref carries its content hash");
    assert_eq!(hash, saved.hash);
    let fetched = store.read_base64_raw(hash).expect("the ref resolves");
    assert_eq!(fetched, bytes, "the ref resolves back to the bytes");
}

/// The park the controller makes is a row: submitting a multimodal
/// prompt writes it before the run it starts can consume it.
#[tokio::test]
async fn a_multimodal_prompt_parks_a_durable_row() {
    use std::{sync::Arc, time::Duration};

    use elysper::{
        harness::{SessionEndReason, SystemEvent},
        provider::{ContentPart, ImageContent, ImageDetail, TextContent},
    };
    use parking_lot::Mutex;

    use super::{
        super::SessionOp,
        support::{RecordedCall, VecFactory},
    };

    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(
            vec![super::super::AgentEvent::System(
                SystemEvent::SessionEnded {
                    reason: SessionEndReason::Completed,
                },
            )],
            Arc::clone(&calls),
        ));
    // A *filesystem* store, like a deployment's: the park's bytes have to
    // survive this process, not only this store handle.
    let attachments = tempfile::tempdir().expect("an attachments dir");
    let store =
        Arc::new(AttachmentStore::new_fs(attachments.path()).expect("a store"));
    let deps =
        super::support::test_deps().with_attachment_store(Arc::clone(&store));
    let (controller, manager, _temp) =
        super::support::make_manager_and_controller_with_deps(
            Duration::from_secs(60),
            factory,
            deps,
        )
        .await;

    controller
        .submit(SessionOp::PromptMulti {
            parts: vec![
                ContentPart::Text(TextContent {
                    text: "look at this".to_string(),
                    cache_control: None,
                }),
                ContentPart::Image(ImageContent {
                    // "QUJD" decodes to b"ABC".
                    data: "QUJD".to_string(),
                    mime_type: "image/png".to_string(),
                    detail: Some(ImageDetail::Auto),
                }),
            ],
            agent_name: Some("reviewer".to_string()),
            priority: 0,
        })
        .await
        .expect("the prompt is accepted");

    let row = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let rows =
                manager.sink("alice", "s1").read().await.unwrap_or_default();
            if let Some(row) = rows
                .iter()
                .find(|row| row["type"] == "parked_prompt")
                .cloned()
            {
                return row;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the park row is durable");

    assert_eq!(row["text"], "look at this");
    assert_eq!(row["agent_name"], "reviewer");
    assert_eq!(row["attachments"][0]["kind"], "image");
    assert_eq!(row["attachments"][0]["byte_len"], 3);
    let hash = row["attachments"][0]["hash"]
        .as_str()
        .unwrap_or_else(|| panic!("the ref must be re-readable: {row}"));
    assert_eq!(
        store
            .read_base64_raw(hash)
            .expect("the parking store resolves"),
        b"ABC"
    );

    // The claim is that a *restart* can read it back: a fresh sink over
    // the same directory, and a fresh store over the same root. The
    // caching store above would answer from its own cache.
    let restarted = AttachmentStore::new_fs(attachments.path())
        .expect("a fresh store over the same root");
    assert_eq!(
        restarted
            .read_base64_raw(hash)
            .expect("a fresh store resolves the ref"),
        b"ABC",
        "the bytes are on disk, addressed by the row's hash"
    );
}

/// Poll `sink` until a row contains `needle`, then return the log.
async fn wait_for_row(sink: &dyn SessionSink, needle: &str) -> Vec<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
    loop {
        let rows = sink.read().await.unwrap_or_default();
        if rows.iter().any(|row| row.to_string().contains(needle)) {
            return rows;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!("{needle:?} never landed; the log holds {rows:?}");
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A sink whose **first** `parked_prompt` append lands the row and
/// reports failure.
///
/// The shape the durability layer has to survive: `append` returned an
/// error but the bytes are on disk, so any reader — this process or the
/// one that takes over — sees the park. Later appends succeed: a sink
/// that fails forever would poison the writer's flush and no row after
/// the park could be written at all. Everything delegates to a real
/// JSONL sink, so the log is a real log.
struct LyingAppendSink {
    inner: Arc<JsonlSessionSink>,
    lied: std::sync::atomic::AtomicBool,
}

impl LyingAppendSink {
    fn new(inner: Arc<JsonlSessionSink>) -> Self {
        Self {
            inner,
            lied: std::sync::atomic::AtomicBool::new(false),
        }
    }
}

#[async_trait::async_trait]
impl SessionSink for LyingAppendSink {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn alloc_seq(&self) -> u64 {
        self.inner.alloc_seq()
    }

    async fn append(&self, event: &Value) -> Result<(), SessionError> {
        let is_park =
            event.get("type").and_then(Value::as_str) == Some("parked_prompt");
        self.inner.append(event).await?;
        if is_park && !self.lied.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(SessionError::AppendFailed(
                "the flush reported failure after the row landed".to_string(),
            ));
        }
        Ok(())
    }

    async fn read(&self) -> Result<Vec<Value>, SessionError> {
        self.inner.read().await
    }

    async fn read_from(&self, from: u64) -> Result<Vec<Value>, SessionError> {
        self.inner.read_from(from).await
    }

    async fn read_tail(
        &self,
        window: usize,
    ) -> Result<Vec<Value>, SessionError> {
        self.inner.read_tail(window).await
    }

    async fn read_head(&self) -> Result<Option<Value>, SessionError> {
        self.inner.read_head().await
    }

    async fn snapshot(&self) -> Result<SessionSnapshot, SessionError> {
        self.inner.snapshot().await
    }

    async fn close(
        &self,
        reason: SessionEndReason,
    ) -> Result<(), SessionError> {
        self.inner.close(reason).await
    }
}

/// A text `Prompt` is its own park, and its run spends exactly that
/// park: the consumption row names the prompt's own id, never the
/// id of a park some earlier process left queued in the log. A
/// positional pairing spent the waiting park and its turn was then
/// lost if the node crashed before it ran.
#[tokio::test]
async fn a_plain_prompts_turn_does_not_spend_a_park() {
    use elysper::harness::SystemEvent;
    use parking_lot::Mutex;

    use super::{
        super::SessionOp,
        support::{RecordedCall, VecFactory, make_manager_and_controller},
    };

    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(
            vec![super::super::AgentEvent::System(
                SystemEvent::SessionEnded {
                    reason: elysper::harness::SessionEndReason::Completed,
                },
            )],
            Arc::clone(&calls),
        ));
    let (controller, manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    // A park an earlier process left behind: the controller is already
    // open, so the entry is pending in the log only. Written through the
    // session's own writer.
    let writer = manager.writer("alice", "s1");
    writer
        .append_event(&SessionEvent::ParkedPrompt {
            seq: 0,
            ts: String::new(),
            park_id: "park-waiting".to_string(),
            text: "a multimodal turn queued behind the run".to_string(),
            attachments: Vec::new(),
            agent_name: None,
            provider: None,
            model: None,
            rerun: false,
        })
        .await
        .expect("the park row queues");
    writer.flush().await.expect("the park row is durable");

    controller
        .submit(SessionOp::Prompt {
            content: "a text-only prompt".to_string(),
            priority: 0,
        })
        .await
        .expect("the prompt is accepted");

    let rows = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let rows =
                manager.sink("alice", "s1").read().await.unwrap_or_default();
            if rows.iter().any(|row| {
                row["type"] == "UserInput"
                    && row["data"]["text"] == "a text-only prompt"
            }) {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the text run recorded its turn");
    let own_park = rows
        .iter()
        .find(|row| {
            row["type"] == "parked_prompt"
                && row["text"] == "a text-only prompt"
        })
        .unwrap_or_else(|| panic!("the prompt parked a row: {rows:?}"));
    let own_id = own_park["park_id"].as_str().expect("the park names its id");
    assert!(
        rows.iter()
            .any(|row| row["type"] == "park_consumed"
                && row["park_id"] == own_id),
        "the run spends the park it started from: {rows:?}"
    );
    assert_eq!(
        elysper::session::parked_prompts(&rows)
            .iter()
            .map(|park| park.park_id.as_str())
            .collect::<Vec<_>>(),
        vec!["park-waiting"],
        "the unrelated run's row leaves the park queued"
    );
}

/// The reverse shape: the park's append **reported** failure but the row
/// landed. The park still carries the id it was written under, so the run
/// it starts spends exactly that row — and a restart finds no pending
/// park instead of running the turn a second time.
#[tokio::test]
async fn a_park_whose_append_failed_is_still_spent_by_its_run() {
    use elysper::{
        core::SystemClock,
        harness::SystemEvent,
        provider::{ContentPart, TextContent},
        session::LogWriter,
    };
    use parking_lot::Mutex;

    use super::{
        super::SessionOp,
        support::{RecordedCall, VecFactory, test_deps},
    };

    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(
            vec![super::super::AgentEvent::System(
                SystemEvent::SessionEnded {
                    reason: elysper::harness::SessionEndReason::Completed,
                },
            )],
            Arc::clone(&calls),
        ));
    let dir = tempfile::tempdir().expect("a session dir");
    let jsonl = Arc::new(JsonlSessionSink::new("s1", dir.path()));
    let sink: Arc<dyn SessionSink> =
        Arc::new(LyingAppendSink::new(Arc::clone(&jsonl)));
    let writer =
        Arc::new(LogWriter::new(Arc::clone(&sink), Arc::new(SystemClock)));
    let controller = super::super::SessionController::spawn(
        "alice",
        "s1",
        Arc::clone(&sink),
        writer,
        test_deps(),
        Duration::from_secs(60),
        factory,
    );

    controller
        .submit(SessionOp::PromptMulti {
            parts: vec![ContentPart::Text(TextContent {
                text: "the turn that must not run twice".to_string(),
                cache_control: None,
            })],
            agent_name: None,
            priority: 0,
        })
        .await
        .expect("the prompt is accepted");

    let _ =
        wait_for_row(jsonl.as_ref(), "the turn that must not run twice").await;
    // The consumption is a row the run task appends; wait for it rather
    // than assume the ordering of the run's own appends.
    let rows = wait_for_row(jsonl.as_ref(), "park_consumed").await;

    let park = rows
        .iter()
        .find(|row| row["type"] == "parked_prompt")
        .unwrap_or_else(|| {
            panic!("the park row landed despite the reported error: {rows:?}")
        });
    let park_id = park["park_id"].as_str().expect("the park row names its id");
    assert!(
        rows.iter()
            .any(|row| row["type"] == "park_consumed"
                && row["park_id"] == park_id),
        "the run spent the park it was started from: {rows:?}"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| {
                row["type"] == "UserInput"
                    && row["data"]["text"] == "the turn that must not run twice"
            })
            .count(),
        1,
        "the turn is written once: {rows:?}"
    );
    // The verdict the restart reaches. A flush error can leave the park
    // row on disk more than once (the writer re-sends the failed batch);
    // every copy names the same id, so the one consumption spends them
    // all and no turn runs twice.
    assert!(
        elysper::session::parked_prompts(&rows).is_empty(),
        "the restart finds nothing to re-run: {rows:?}"
    );
}

/// A sink whose reads fail until the test opens them.
///
/// The shape a node that dies mid-queue leaves behind: appends (the
/// park rows) still land, but the controller cannot read its own
/// log, so `maybe_start_run` refuses and the queued prompts stay
/// queued. Everything delegates to a real JSONL sink, so the log is
/// a real log and its rows are readable once the gate opens.
struct GatedReadSink {
    inner: Arc<JsonlSessionSink>,
    blocked: std::sync::atomic::AtomicBool,
}

impl GatedReadSink {
    fn blocking(inner: Arc<JsonlSessionSink>) -> Self {
        Self {
            inner,
            blocked: std::sync::atomic::AtomicBool::new(true),
        }
    }

    /// Let reads through again — the node recovering.
    fn open(&self) {
        self.blocked
            .store(false, std::sync::atomic::Ordering::SeqCst);
    }

    fn gated(&self) -> Result<(), SessionError> {
        if self.blocked.load(std::sync::atomic::Ordering::SeqCst) {
            Err(SessionError::ReadFailed("the log is gated".to_string()))
        } else {
            Ok(())
        }
    }
}

#[async_trait::async_trait]
impl SessionSink for GatedReadSink {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn alloc_seq(&self) -> u64 {
        self.inner.alloc_seq()
    }

    async fn append(&self, event: &Value) -> Result<(), SessionError> {
        self.inner.append(event).await
    }

    async fn read(&self) -> Result<Vec<Value>, SessionError> {
        self.gated()?;
        self.inner.read().await
    }

    async fn read_from(&self, from: u64) -> Result<Vec<Value>, SessionError> {
        self.gated()?;
        self.inner.read_from(from).await
    }

    async fn read_tail(
        &self,
        window: usize,
    ) -> Result<Vec<Value>, SessionError> {
        self.gated()?;
        self.inner.read_tail(window).await
    }

    async fn read_head(&self) -> Result<Option<Value>, SessionError> {
        self.gated()?;
        self.inner.read_head().await
    }

    async fn snapshot(&self) -> Result<SessionSnapshot, SessionError> {
        self.inner.snapshot().await
    }

    async fn close(
        &self,
        reason: SessionEndReason,
    ) -> Result<(), SessionError> {
        self.inner.close(reason).await
    }
}

/// A plain-text `Prompt` is a durable park: the `parked_prompt` row
/// is appended and flushed before the op returns, so a node that
/// dies before the turn runs leaves the prompt in the log. The read
/// gate models the death — no turn can start while the controller
/// cannot read its log — and the recovery fold, the re-queue on a
/// fresh controller, and the turns that controller then runs model
/// the takeover that follows it.
#[tokio::test]
async fn text_prompt_is_parked_durably_and_survives_a_rebuild() {
    use elysper::provider::{ContentPart, TextContent};
    use parking_lot::Mutex;

    use super::{
        super::SessionOp,
        support::{RecordedCall, VecFactory, test_deps},
    };
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(
            vec![super::super::AgentEvent::System(
                elysper::harness::SystemEvent::SessionEnded {
                    reason: elysper::harness::SessionEndReason::Completed,
                },
            )],
            Arc::clone(&calls),
        ));
    let dir = session_dir();
    let jsonl = Arc::new(JsonlSessionSink::new("s1", dir.path()));
    let gated = Arc::new(GatedReadSink::blocking(Arc::clone(&jsonl)));
    let sink: Arc<dyn SessionSink> = Arc::clone(&gated) as Arc<dyn SessionSink>;
    let writer =
        Arc::new(LogWriter::new(Arc::clone(&sink), Arc::new(SystemClock)));
    let controller = super::super::SessionController::spawn(
        "alice",
        "s1",
        Arc::clone(&sink),
        Arc::clone(&writer),
        test_deps(),
        Duration::from_secs(60),
        Arc::clone(&factory),
    );

    controller
        .submit(SessionOp::Prompt {
            content: "first question".to_string(),
            priority: 0,
        })
        .await
        .expect("the first prompt is accepted");
    controller
        .submit(SessionOp::Prompt {
            content: "second question".to_string(),
            priority: 0,
        })
        .await
        .expect("the second prompt is accepted");

    // Both prompts are durable rows: each text is its own row's, a
    // text turn carries no attachments, and no turn ran.
    let rows = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let rows = jsonl.read().await.unwrap_or_default();
            if rows
                .iter()
                .filter(|row| row["type"] == "parked_prompt")
                .count()
                == 2
            {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("both prompts park durable rows");
    let parks: Vec<&Value> = rows
        .iter()
        .filter(|row| row["type"] == "parked_prompt")
        .collect();
    assert_eq!(
        parks
            .iter()
            .map(|row| row["text"].as_str().unwrap_or_default())
            .collect::<Vec<_>>(),
        vec!["first question", "second question"],
        "each prompt parks its own row, in submission order: {rows:?}"
    );
    assert!(
        parks.iter().all(|row| row.get("attachments").is_none()),
        "a text turn carries no attachment field: {rows:?}"
    );
    assert_eq!(
        calls.lock().len(),
        0,
        "no turn ran: both parks are still queued"
    );

    // The rebuild a taking-over node performs: fold the parks back
    // out of the log (nothing consumed them), re-queue each one on a
    // fresh controller over the same session, and let the recovered
    // turns run — the re-queue is only real if they do.
    gated.open();
    let rows = jsonl
        .read()
        .await
        .expect("the log reads once the gate opens");
    let recovered = elysper::session::parked_prompts(&rows);
    assert_eq!(
        recovered
            .iter()
            .map(|park| park.text.as_str())
            .collect::<Vec<_>>(),
        vec!["first question", "second question"],
        "the fold recovers both parks, in order"
    );
    let rebuilt = super::super::SessionController::spawn(
        "alice",
        "s1",
        Arc::clone(&sink),
        Arc::clone(&writer),
        test_deps(),
        Duration::from_secs(60),
        Arc::clone(&factory),
    );
    for park in &recovered {
        rebuilt.restore_parked_prompt(
            vec![ContentPart::Text(TextContent {
                text: park.text.clone(),
                cache_control: None,
            })],
            park.park_id.clone(),
            park.agent_name.clone(),
            None,
            park.rerun,
        );
    }
    rebuilt
        .submit(SessionOp::StartPending)
        .await
        .expect("the recovered turns start");
    let rows = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let rows = jsonl.read().await.unwrap_or_default();
            let turns: Vec<&str> = rows
                .iter()
                .filter(|row| row["type"] == "UserInput")
                .filter_map(|row| row["data"]["text"].as_str())
                .collect();
            if turns.len() == 2 {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the rebuilt controller runs both recovered turns");
    let turns: Vec<&str> = rows
        .iter()
        .filter(|row| row["type"] == "UserInput")
        .filter_map(|row| row["data"]["text"].as_str())
        .collect();
    assert_eq!(
        turns,
        vec!["first question", "second question"],
        "the recovered turns ran, in order, on the rebuilt node"
    );
    assert_eq!(
        calls.lock().len(),
        2,
        "one run per recovered park, on the controller that re-queued them"
    );
    assert!(
        elysper::session::parked_prompts(&rows).is_empty(),
        "the rebuilt node spent every park it ran: {rows:?}"
    );
}

/// One queued message is one turn: two prompts that pile up while
/// no turn can start each run as their own turn when the queue
/// drains — the first is not folded into the second's history.
#[tokio::test]
async fn each_queued_text_prompt_becomes_its_own_turn() {
    use elysper::provider::ContentPart;
    use parking_lot::Mutex;

    use super::{
        super::SessionOp,
        support::{RecordedCall, VecFactory, test_deps, wait_for_runs},
    };

    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(
            vec![super::super::AgentEvent::System(
                elysper::harness::SystemEvent::SessionEnded {
                    reason: elysper::harness::SessionEndReason::Completed,
                },
            )],
            Arc::clone(&calls),
        ));
    let dir = session_dir();
    let jsonl = Arc::new(JsonlSessionSink::new("s1", dir.path()));
    let gated = Arc::new(GatedReadSink::blocking(Arc::clone(&jsonl)));
    let sink: Arc<dyn SessionSink> = Arc::clone(&gated) as Arc<dyn SessionSink>;
    let writer =
        Arc::new(LogWriter::new(Arc::clone(&sink), Arc::new(SystemClock)));
    let controller = super::super::SessionController::spawn(
        "alice",
        "s1",
        Arc::clone(&sink),
        Arc::clone(&writer),
        test_deps(),
        Duration::from_secs(60),
        factory,
    );

    // Both prompts queue while the log is unreadable: no turn can
    // start, so the two messages pile up.
    controller
        .submit(SessionOp::Prompt {
            content: "first message".to_string(),
            priority: 0,
        })
        .await
        .expect("the first prompt is accepted");
    controller
        .submit(SessionOp::Prompt {
            content: "second message".to_string(),
            priority: 0,
        })
        .await
        .expect("the second prompt is accepted");

    // The node recovers: the log reads again and the queued turns
    // start, one run per parked message.
    gated.open();
    controller
        .submit(SessionOp::StartPending)
        .await
        .expect("the queued turns start");

    wait_for_runs(&calls, 2).await;
    // The guard is scoped: the assertions below await the log.
    let turn_texts = {
        let runs = calls.lock();
        assert_eq!(
            runs.len(),
            2,
            "one queued message is one turn, not a fold into the last"
        );
        runs.iter()
            .map(|run| match run.input.content.first() {
                Some(ContentPart::Text(text)) => text.text.clone(),
                other => panic!(
                    "a text turn's prompt is its text content: {other:?}"
                ),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(
        turn_texts,
        vec!["first message".to_string(), "second message".to_string()],
        "each message is its own turn's prompt, in order"
    );

    // Both turns land in the log as their own user rows, in order,
    // and neither park is left waiting.
    let rows = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let rows = jsonl.read().await.unwrap_or_default();
            if rows.iter().filter(|row| row["type"] == "UserInput").count() == 2
            {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("both turns land their user rows");
    let turns: Vec<&str> = rows
        .iter()
        .filter(|row| row["type"] == "UserInput")
        .filter_map(|row| row["data"]["text"].as_str())
        .collect();
    assert_eq!(
        turns,
        vec!["first message", "second message"],
        "each message becomes its own user turn, in order: {rows:?}"
    );
    assert!(
        elysper::session::parked_prompts(&rows).is_empty(),
        "both parks are spent by their own runs: {rows:?}"
    );
}

/// Cancelling spends the parks it drops: one `park_consumed` row per
/// dropped turn, through the session's one writer, before the cancel
/// returns. The park rows themselves stay in the log — without their
/// consumption rows a node that takes the session over would replay
/// turns the user just cancelled.
#[tokio::test]
async fn cancelling_spends_the_parks_it_drops() {
    use std::sync::atomic::AtomicUsize;

    use parking_lot::Mutex;

    use super::{
        super::{SessionOp, SessionState},
        support::{
            BlockingFactory,
            RecordedCall,
            make_manager_and_controller,
            wait_for_runs,
        },
    };

    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(BlockingFactory {
            calls: Arc::clone(&calls),
            progress_count: Arc::new(AtomicUsize::new(0)),
        });
    let (controller, manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    // The first prompt's run stays live (the factory blocks until
    // cancelled), so the second prompt parks behind it.
    controller
        .submit(SessionOp::Prompt {
            content: "the live turn".to_string(),
            priority: 0,
        })
        .await
        .expect("the first prompt is accepted");
    wait_for_runs(&calls, 1).await;
    controller
        .submit(SessionOp::Prompt {
            content: "the cancelled turn".to_string(),
            priority: 0,
        })
        .await
        .expect("the second prompt is accepted");
    let park_id = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let rows =
                manager.sink("alice", "s1").read().await.unwrap_or_default();
            if let Some(id) = rows
                .iter()
                .find(|row| {
                    row["type"] == "parked_prompt"
                        && row["text"] == "the cancelled turn"
                })
                .and_then(|row| row["park_id"].as_str())
            {
                return id.to_string();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the queued prompt parks a durable row");

    controller.cancel().await.expect("the cancel is accepted");
    tokio::time::timeout(Duration::from_millis(500), async {
        while controller.state() != SessionState::Cancelled
            && controller.state() != SessionState::Idle
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the cancelled run settles");

    // The verdict a taking-over node reaches: every park is spent —
    // the live one by its run, the dropped one by the cancel.
    let rows = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let rows =
                manager.sink("alice", "s1").read().await.unwrap_or_default();
            if elysper::session::parked_prompts(&rows).is_empty() {
                return rows;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("both parks are spent");
    assert!(
        rows.iter()
            .any(|row| row["type"] == "park_consumed"
                && row["park_id"] == park_id),
        "the cancel spent the park it dropped: {rows:?}"
    );
    assert_eq!(calls.lock().len(), 1, "the cancelled turn never ran");
}

/// A steer must not queue behind a manual compaction pass: the
/// pass gate serialises run starts only, so the steer's live probe
/// (`append_live_steer`, which takes the steer gate before it reads
/// the state) decides while the pass still holds its gate, and the
/// message parks as the next turn. Before the split the pass held
/// the steer gate itself and this deadline expired.
#[tokio::test]
async fn a_steer_is_not_blocked_by_a_manual_compaction_pass() {
    use std::sync::atomic::AtomicUsize;

    use super::{
        super::SteerDelivery,
        support::{BlockingFactory, RecordedCall, make_manager_and_controller},
    };

    let calls = Arc::new(parking_lot::Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(BlockingFactory {
            calls: Arc::clone(&calls),
            progress_count: Arc::new(AtomicUsize::new(0)),
        });
    let (controller, _manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    // The gate a `/compact` request holds for its whole pass.
    let _pass = controller
        .compaction_gate()
        .await
        .expect("the owner has a gate");

    let delivery = tokio::time::timeout(
        Duration::from_millis(1_000),
        controller.steer("steer behind a pass".to_string(), None),
    )
    .await
    .expect("the steer decides while the pass holds its gate")
    .expect("the steer is accepted");

    assert!(
        matches!(delivery, SteerDelivery::Queue),
        "no run is live, so the steer becomes the next queued turn"
    );
}

/// A sink that holds every batch once told to, so a test can park a
/// flush at the sink barrier and observe which side of it the
/// cancel's state flip lands on.
struct GatedAppendSink {
    rows: parking_lot::Mutex<Vec<serde_json::Value>>,
    blocking: std::sync::atomic::AtomicBool,
    entered: tokio::sync::Semaphore,
    gate: tokio::sync::Semaphore,
    next_seq: std::sync::atomic::AtomicU64,
}

impl GatedAppendSink {
    fn new() -> Self {
        Self {
            rows: parking_lot::Mutex::new(Vec::new()),
            blocking: std::sync::atomic::AtomicBool::new(false),
            entered: tokio::sync::Semaphore::new(0),
            gate: tokio::sync::Semaphore::new(0),
            next_seq: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Hold every batch from now on.
    fn hold_batches(&self) {
        self.blocking
            .store(true, std::sync::atomic::Ordering::SeqCst);
    }

    /// Wait until a batch is parked inside the sink.
    async fn wait_for_entry(&self) {
        self.entered
            .acquire()
            .await
            .expect("the semaphore is open")
            .forget();
    }

    /// Let parked batches through.
    fn open_the_gate(&self) {
        self.gate.add_permits(8);
    }

    fn rows(&self) -> Vec<serde_json::Value> {
        self.rows.lock().clone()
    }
}

#[async_trait::async_trait]
impl elysper::session::SessionSink for GatedAppendSink {
    fn id(&self) -> &str {
        "gated-append"
    }

    fn alloc_seq(&self) -> u64 {
        self.next_seq
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1
    }

    async fn append(
        &self,
        event: &serde_json::Value,
    ) -> Result<(), elysper::session::SessionError> {
        self.append_many(std::slice::from_ref(event)).await
    }

    async fn append_many(
        &self,
        events: &[serde_json::Value],
    ) -> Result<(), elysper::session::SessionError> {
        if self.blocking.load(std::sync::atomic::Ordering::SeqCst) {
            self.entered.add_permits(1);
            self.gate
                .acquire()
                .await
                .expect("the gate is open")
                .forget();
        }
        self.rows.lock().extend(events.iter().cloned());
        Ok(())
    }

    async fn read(
        &self,
    ) -> Result<Vec<serde_json::Value>, elysper::session::SessionError> {
        Ok(self.rows())
    }

    async fn read_from(
        &self,
        from: u64,
    ) -> Result<Vec<serde_json::Value>, elysper::session::SessionError> {
        Ok(self
            .rows()
            .into_iter()
            .filter(|row| row["seq"].as_u64().unwrap_or(0) > from)
            .collect())
    }

    async fn snapshot(
        &self,
    ) -> Result<elysper::session::SessionSnapshot, elysper::session::SessionError>
    {
        let last = self.next_seq.load(std::sync::atomic::Ordering::SeqCst);
        Ok(elysper::session::SessionSnapshot {
            session_id: "gated-append".to_string(),
            last_event_seq: last,
            last_stream_index: last,
            bytes_on_disk: 0,
        })
    }

    async fn close(
        &self,
        _reason: elysper::session::SessionEndReason,
    ) -> Result<(), elysper::session::SessionError> {
        Ok(())
    }
}

/// The cancel's `park_consumed` rows are durable **before** the
/// cancelled state flips: the flip is what a taker (or a crash) sees
/// first, so a `Cancelled` session must never carry an unspent park.
/// The sink holds the consumption flush at its barrier, and the
/// assertion is made while the rows are provably not yet durable —
/// the flip must still be waiting behind them.
#[tokio::test]
async fn the_cancelled_state_never_outlives_its_park_consumed_rows() {
    use std::sync::atomic::AtomicUsize;

    use super::{
        super::{SessionOp, SessionState},
        support::{BlockingFactory, RecordedCall, wait_for_runs},
    };
    let calls = Arc::new(parking_lot::Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(BlockingFactory {
            calls: Arc::clone(&calls),
            progress_count: Arc::new(AtomicUsize::new(0)),
        });
    let sink = Arc::new(GatedAppendSink::new());
    let writer = Arc::new(elysper::session::LogWriter::new(
        Arc::clone(&sink) as Arc<dyn elysper::session::SessionSink>,
        Arc::new(elysper::core::SystemClock),
    ));
    let controller = super::super::SessionController::spawn(
        "alice",
        "s1",
        Arc::clone(&sink) as Arc<dyn elysper::session::SessionSink>,
        writer,
        super::support::test_deps(),
        Duration::from_secs(60),
        factory,
    );

    controller
        .submit(SessionOp::Prompt {
            content: "the live turn".to_string(),
            priority: 0,
        })
        .await
        .expect("the first prompt is accepted");
    wait_for_runs(&calls, 1).await;
    controller
        .submit(SessionOp::Prompt {
            content: "the dropped turn".to_string(),
            priority: 0,
        })
        .await
        .expect("the second prompt is accepted");
    let park_id = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if let Some(id) = sink
                .rows()
                .iter()
                .find(|row| {
                    row["type"] == "parked_prompt"
                        && row["text"] == "the dropped turn"
                })
                .and_then(|row| row["park_id"].as_str())
            {
                return id.to_string();
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the queued prompt parks a durable row");

    // From here every batch parks at the sink barrier: the cancel's
    // consumption flush is now observable as "entered but not
    // durable".
    sink.hold_batches();
    controller.cancel().await.expect("the cancel is accepted");
    tokio::time::timeout(Duration::from_secs(5), sink.wait_for_entry())
        .await
        .expect("the cancel's consumption flush reaches the sink");

    assert_ne!(
        controller.state(),
        SessionState::Cancelled,
        "the state flips only after the consumption rows are durable"
    );
    assert!(
        !sink
            .rows()
            .iter()
            .any(|row| row["type"] == "park_consumed"
                && row["park_id"] == park_id),
        "the row is provably still in flight at the barrier"
    );

    sink.open_the_gate();
    tokio::time::timeout(Duration::from_secs(2), async {
        while controller.state() != SessionState::Cancelled {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the cancel completes once the rows land");
    assert!(
        sink.rows()
            .iter()
            .any(|row| row["type"] == "park_consumed"
                && row["park_id"] == park_id),
        "the consumption row is durable with the flip"
    );
}

/// The cancel's consumption rows are written with the steer gate
/// **free**: one fsync per dropped park must not serialise every
/// steer and every run start behind the disk. The sink parks the
/// consumption flush at its barrier — the disk work is provably in
/// flight — and the steer gate must still be lockable while it is.
///
/// The ordering a takeover depends on (the rows durable *before* the
/// state flips) is the sibling test's subject; this one is about the
/// gate's scope.
#[tokio::test]
async fn the_cancels_park_spending_does_not_hold_the_steer_gate() {
    use std::sync::atomic::AtomicUsize;

    use super::{
        super::{SessionOp, SessionState},
        support::{BlockingFactory, RecordedCall, wait_for_runs},
    };
    let calls = Arc::new(parking_lot::Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(BlockingFactory {
            calls: Arc::clone(&calls),
            progress_count: Arc::new(AtomicUsize::new(0)),
        });
    let sink = Arc::new(GatedAppendSink::new());
    let writer = Arc::new(elysper::session::LogWriter::new(
        Arc::clone(&sink) as Arc<dyn elysper::session::SessionSink>,
        Arc::new(elysper::core::SystemClock),
    ));
    let controller = super::super::SessionController::spawn(
        "alice",
        "s1",
        Arc::clone(&sink) as Arc<dyn elysper::session::SessionSink>,
        writer,
        super::support::test_deps(),
        Duration::from_secs(60),
        factory,
    );

    controller
        .submit(SessionOp::Prompt {
            content: "the live turn".to_string(),
            priority: 0,
        })
        .await
        .expect("the first prompt is accepted");
    wait_for_runs(&calls, 1).await;
    controller
        .submit(SessionOp::Prompt {
            content: "the dropped turn".to_string(),
            priority: 0,
        })
        .await
        .expect("the second prompt is accepted");
    // The park row must be durable before the barrier closes, or the
    // barrier would hold *this* row instead of the cancel's.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if sink.rows().iter().any(|row| {
                row["type"] == "parked_prompt"
                    && row["text"] == "the dropped turn"
            }) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the queued prompt parks a durable row");

    sink.hold_batches();
    controller.cancel().await.expect("the cancel is accepted");
    tokio::time::timeout(Duration::from_secs(5), sink.wait_for_entry())
        .await
        .expect("the cancel's consumption flush reaches the sink");

    // The disk work is in flight and the steer gate must be free: a
    // gate held across the fsync would serialise every steer and
    // every run start behind the disk.
    let inner = controller
        .inner
        .as_ref()
        .expect("the owning controller keeps its inner state");
    // The guard is dropped at the end of the statement: the cancel's
    // own second gate section (the state flip) must not queue behind
    // this probe.
    let _ = tokio::time::timeout(
        Duration::from_millis(500),
        inner.steer_gate.lock(),
    )
    .await
    .expect("the steer gate must not be held across the fsync");

    sink.open_the_gate();
    tokio::time::timeout(Duration::from_secs(2), async {
        while controller.state() != SessionState::Cancelled {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the cancel completes once the rows land");
}

/// The durable submit answers only after the park row is on disk.
///
/// The row is the promise: a caller that was told "queued" must be
/// able to find the turn in the log the moment the answer arrives.
/// (The plain submit answers as soon as the operation is queued; the
/// turn becomes durable later, when the loop parks it.)
#[tokio::test]
async fn the_durable_submit_answers_only_after_the_park_row_is_on_disk() {
    use std::sync::atomic::AtomicUsize;

    use super::{
        super::SessionOp,
        support::{BlockingFactory, RecordedCall, make_manager_and_controller},
    };
    let calls = Arc::new(parking_lot::Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(BlockingFactory {
            calls: Arc::clone(&calls),
            progress_count: Arc::new(AtomicUsize::new(0)),
        });
    let (controller, manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    controller
        .submit_durable(SessionOp::Prompt {
            content: "the durable turn".to_string(),
            priority: 0,
        })
        .await
        .expect("the turn is durable");

    let rows = manager
        .sink("alice", "s1")
        .read()
        .await
        .expect("the log reads");
    assert!(
        rows.iter().any(|row| row["type"] == "parked_prompt"
            && row["text"] == "the durable turn"),
        "the park row must be on disk when the submit answers: {rows:?}",
    );
}

/// The durable submit must not deadlock on a busy controller: the
/// wait holds nothing the loop needs, so a controller that is running
/// a turn still parks — and confirms — the next one.
#[tokio::test]
async fn the_durable_submit_answers_while_a_run_is_live() {
    use std::sync::atomic::AtomicUsize;

    use super::{
        super::SessionOp,
        support::{
            BlockingFactory,
            RecordedCall,
            make_manager_and_controller,
            wait_for_runs,
        },
    };
    let calls = Arc::new(parking_lot::Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(BlockingFactory {
            calls: Arc::clone(&calls),
            progress_count: Arc::new(AtomicUsize::new(0)),
        });
    let (controller, manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    // The first prompt's run stays live (the factory blocks until it
    // is cancelled), so the controller is busy when the second turn
    // arrives.
    controller
        .submit(SessionOp::Prompt {
            content: "the live turn".to_string(),
            priority: 0,
        })
        .await
        .expect("the first prompt is accepted");
    wait_for_runs(&calls, 1).await;

    tokio::time::timeout(
        Duration::from_secs(5),
        controller.submit_durable(SessionOp::Prompt {
            content: "the queued turn".to_string(),
            priority: 0,
        }),
    )
    .await
    .expect("the durable submit must not wait on the live run")
    .expect("the queued turn is durable");

    let rows = manager
        .sink("alice", "s1")
        .read()
        .await
        .expect("the log reads");
    assert!(
        rows.iter().any(|row| row["type"] == "parked_prompt"
            && row["text"] == "the queued turn"),
        "the park row must be on disk when the submit answers: {rows:?}",
    );
}
