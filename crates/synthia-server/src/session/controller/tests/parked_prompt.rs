//! Durable parked prompts: a prompt queued for the next run has to
//! survive the node that parked it.
//!
//! `PromptMulti` parks its lossless parts on the controller — the
//! text queue cannot carry them — and that slot lives in process
//! memory, so a crash between the park and the run used to lose the
//! turn. The `parked_prompt` row is the park: appended and flushed
//! before the op returns, so the log a restarted node reads still
//! holds the prompt.
//!
//! The bytes never ride the log. A multimodal part is stored in the
//! attachment store first (content-addressed) and the row carries a
//! re-readable ref — the hash is what lets a reloaded prompt fetch
//! the bytes back.

use std::{sync::Arc, time::Duration};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use serde_json::Value;
use synthia::{
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
            attachments: vec![synthia::session::AttachmentRefRow {
                kind: "image".to_string(),
                hash: Some(saved.hash.clone()),
                mime_type: Some("image/png".to_string()),
                byte_len: Some(bytes.len() as u64),
                name: None,
                unstored: None,
            }],
            agent_name: None,
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

    use parking_lot::Mutex;
    use synthia::{
        harness::{SessionEndReason, SystemEvent},
        provider::{ContentPart, ImageContent, ImageDetail, TextContent},
    };

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
            None,
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

/// A plain `Prompt` run writes a `UserInput` row without ever parking.
/// That row must not spend a park — pairing consumers with parks by
/// position did, and the park's own turn was then lost if the node
/// crashed before it ran.
#[tokio::test]
async fn a_plain_prompts_turn_does_not_spend_a_park() {
    use std::sync::Arc;

    use parking_lot::Mutex;
    use synthia::harness::SystemEvent;

    use super::{
        super::SessionOp,
        support::{RecordedCall, VecFactory, make_manager_and_controller},
    };

    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(
            vec![super::super::AgentEvent::System(
                SystemEvent::SessionEnded {
                    reason: synthia::harness::SessionEndReason::Completed,
                },
            )],
            Arc::clone(&calls),
            None,
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

    let rows = wait_for_row(
        manager.sink("alice", "s1").as_ref(),
        "a text-only prompt",
    )
    .await;
    assert!(
        rows.iter().any(|row| row["type"] == "UserInput"
            && row["data"]["text"] == "a text-only prompt"),
        "the text run recorded its turn: {rows:?}"
    );
    assert!(
        rows.iter().all(|row| row["type"] != "park_consumed"),
        "a run that started from no park writes no consumption row"
    );
    assert_eq!(
        synthia::session::parked_prompts(&rows)
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
    use std::sync::Arc;

    use parking_lot::Mutex;
    use synthia::{
        core::SystemClock,
        harness::SystemEvent,
        provider::{ContentPart, TextContent},
        session::LogWriter,
    };

    use super::{
        super::SessionOp,
        support::{RecordedCall, VecFactory, test_deps},
    };

    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn super::super::RunStreamFactory> =
        Arc::new(VecFactory::new(
            vec![super::super::AgentEvent::System(
                SystemEvent::SessionEnded {
                    reason: synthia::harness::SessionEndReason::Completed,
                },
            )],
            Arc::clone(&calls),
            None,
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
        Default::default(),
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
        synthia::session::parked_prompts(&rows).is_empty(),
        "the restart finds nothing to re-run: {rows:?}"
    );
}
