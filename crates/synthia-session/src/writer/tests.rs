//! `LogWriter` unit tests: the queue never drops a row, one flush is
//! one `append_many`, and every row carries a real ordinal.

use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering},
};

use async_trait::async_trait;
use serde_json::{Value, json};
use synthia_core::SystemClock;
use synthia_provider::{Content, Message};
use tempfile::Builder;

use super::LogWriter;
use crate::{
    CompactionCheckpoint,
    CompactionLifecycleView,
    MAX_PLUGIN_KIND_BYTES,
    MAX_PLUGIN_PAYLOAD_BYTES,
    SessionEndReason,
    SessionError,
    SessionEvent,
    SessionSink,
    SessionSnapshot,
    in_memory::InMemorySessionSink,
    jsonl::JsonlSessionSink,
    log_surface::{SurfaceLedger, fold_log_surface, try_fold_log_surface},
    typed_event_sink::CompactionRecordView,
};

/// A sink that counts the calls it receives, so a test can assert
/// that one flush is one `append_many` — and that a flush never
/// falls back to per-row appends.
#[derive(Default)]
struct CountingSink {
    rows: std::sync::Mutex<Vec<Value>>,
    appends: AtomicUsize,
    batches: AtomicUsize,
    /// How many rows each batch carried.
    batch_sizes: std::sync::Mutex<Vec<usize>>,
    next_seq: AtomicU64,
    /// How many of the next `append_many` calls fail.
    failures: AtomicUsize,
}

impl CountingSink {
    fn new() -> Self {
        Self::default()
    }

    /// How many times `append` was called.
    fn append_calls(&self) -> usize {
        self.appends.load(Ordering::SeqCst)
    }

    /// How many times `append_many` was called.
    fn append_many_calls(&self) -> usize {
        self.batches.load(Ordering::SeqCst)
    }

    /// How many rows each batch carried, in the order the batches
    /// landed.
    fn batch_sizes(&self) -> Vec<usize> {
        self.batch_sizes
            .lock()
            .expect("CountingSink poisoned")
            .clone()
    }

    /// Make the next `count` batches fail.
    fn fail_next(&self, count: usize) {
        self.failures.store(count, Ordering::SeqCst);
    }
}

#[async_trait]
impl SessionSink for CountingSink {
    fn id(&self) -> &str {
        "counting"
    }

    fn alloc_seq(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::SeqCst) + 1
    }

    async fn append(&self, event: &Value) -> Result<(), SessionError> {
        self.appends.fetch_add(1, Ordering::SeqCst);
        self.rows
            .lock()
            .expect("CountingSink poisoned")
            .push(event.clone());
        Ok(())
    }

    async fn append_many(&self, events: &[Value]) -> Result<(), SessionError> {
        self.batches.fetch_add(1, Ordering::SeqCst);
        if self.failures.load(Ordering::SeqCst) > 0 {
            self.failures.fetch_sub(1, Ordering::SeqCst);
            return Err(SessionError::AppendFailed(
                "the sink refused the batch".to_string(),
            ));
        }
        self.batch_sizes
            .lock()
            .expect("CountingSink poisoned")
            .push(events.len());
        let mut rows = self.rows.lock().expect("CountingSink poisoned");
        rows.extend(events.iter().cloned());
        Ok(())
    }

    async fn read(&self) -> Result<Vec<Value>, SessionError> {
        Ok(self.rows.lock().expect("CountingSink poisoned").clone())
    }

    async fn read_from(&self, from: u64) -> Result<Vec<Value>, SessionError> {
        let rows = self.rows.lock().expect("CountingSink poisoned");
        let skip = usize::try_from(from).unwrap_or(usize::MAX);
        Ok(rows.iter().skip(skip).cloned().collect())
    }

    async fn snapshot(&self) -> Result<SessionSnapshot, SessionError> {
        let last = self.next_seq.load(Ordering::SeqCst);
        Ok(SessionSnapshot {
            session_id: "counting".to_string(),
            last_event_seq: last,
            last_stream_index: last,
            bytes_on_disk: 0,
        })
    }

    async fn close(
        &self,
        _reason: SessionEndReason,
    ) -> Result<(), SessionError> {
        Ok(())
    }
}

/// The writer is shared behind an `Arc` across the tasks that write
/// one session's log, so it must be `Send + Sync`.
fn assert_send_sync<T: Send + Sync>() {}

#[test]
fn the_writer_is_send_and_sync() {
    assert_send_sync::<LogWriter>();
}

/// A fresh directory for a sink's log.
fn temp_dir() -> std::path::PathBuf {
    let tmp = Builder::new().disable_cleanup(true).tempdir().unwrap();
    let path = tmp.path().to_path_buf();
    drop(tmp);
    path
}

/// The ordinals the sink holds, in the order it holds them.
async fn stored_seqs(sink: &InMemorySessionSink) -> Vec<u64> {
    seqs_of(&sink.read().await.expect("read"))
}

/// The ordinals the rows carry, in the order they carry them.
fn seqs_of(rows: &[Value]) -> Vec<u64> {
    rows.iter().filter_map(|row| row["seq"].as_u64()).collect()
}

/// A queue that holds one row forces the slow path on every append
/// after the first. Every row must still land: a dropped row would
/// leave a hole in the log's ordinals, and the fold's provenance
/// check then rejects every compaction that cites a row after the
/// hole.
#[tokio::test]
async fn a_saturated_queue_never_drops_a_row() {
    let sink = Arc::new(CountingSink::new());
    let writer =
        LogWriter::with_capacity(sink.clone(), Arc::new(SystemClock), 1);
    for _ in 0..64 {
        writer
            .append(json!({
                "type": "Model",
                "data": {"type": "text", "text": "x"},
            }))
            .await
            .unwrap();
    }

    // The queue never holds more than its capacity: with room for one
    // row, every second append finds the queue full and flushes it.
    // A queue that grew instead would show one batch at the end, not
    // one per pair of appends.
    assert!(
        sink.append_many_calls() >= 32,
        "a full queue triggers the flush that frees it: {} flushes \
         for 64 appends",
        sink.append_many_calls(),
    );
    assert!(
        sink.batch_sizes().iter().all(|size| *size <= 2),
        "no batch holds more than the queue's capacity plus the row \
         that did not fit: {:?}",
        sink.batch_sizes(),
    );

    writer.flush().await.unwrap();
    let rows = sink.read().await.unwrap();
    assert_eq!(rows.len(), 64, "a durable row is never dropped");
    assert_eq!(
        seqs_of(&rows),
        (1..=64).collect::<Vec<u64>>(),
        "ordinals are gap-free",
    );
}

/// The flush is the durability promise, and it writes the whole batch
/// in one call: a per-row append would cost one fsync per row.
#[tokio::test]
async fn one_flush_is_one_append_many() {
    let sink = Arc::new(CountingSink::new());
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));
    for _ in 0..10 {
        writer.append(json!({"type": "Model"})).await.unwrap();
    }
    writer.flush().await.unwrap();

    assert_eq!(sink.append_many_calls(), 1);
    assert_eq!(sink.append_calls(), 0);
}

/// Rows are not durable until the flush: the queue is the buffer, and
/// only the flush writes to the sink.
#[tokio::test]
async fn rows_reach_the_sink_only_at_the_flush() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));
    writer.append(json!({"type": "Model"})).await.unwrap();

    assert!(
        sink.read().await.expect("read").is_empty(),
        "an unflushed row must not be in the sink",
    );

    writer.flush().await.unwrap();
    assert_eq!(stored_seqs(&sink).await, vec![1]);
}

/// The payload bound lives at the log boundary, not in a
/// constructor: a `plugin` row assembled through the public variant
/// literal is capped on every ingress path, because variant fields
/// are public and a constructor-only cap is advisory.
#[tokio::test]
async fn the_writer_bounds_a_plugin_row_built_by_hand() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));

    let huge = "x".repeat(MAX_PLUGIN_PAYLOAD_BYTES * 2);
    let long_kind = "k".repeat(MAX_PLUGIN_KIND_BYTES * 4);
    writer
        .append(json!({
            "type": "plugin",
            "kind": long_kind,
            "payload": {"blob": huge},
        }))
        .await
        .unwrap();
    // The synchronous path, with the degenerate kind.
    writer.append_nowait(json!({
        "type": "plugin",
        "kind": "",
        "payload": {"ok": true},
    }));
    writer.flush().await.unwrap();

    let rows = sink.read().await.expect("read");
    let first = &rows[0];
    let stored = serde_json::to_string(&first["payload"]).expect("serialises");
    assert!(
        stored.len() <= MAX_PLUGIN_PAYLOAD_BYTES,
        "the row carries at most the cap: {}",
        stored.len()
    );
    assert!(
        first["original_bytes"]
            .as_u64()
            .is_some_and(|n| n > MAX_PLUGIN_PAYLOAD_BYTES as u64),
        "the cap records what it cut: {}",
        first["original_bytes"],
    );
    let kind = serde_json::to_string(&first["kind"]).expect("serialises");
    assert!(kind.len() <= MAX_PLUGIN_KIND_BYTES, "kind: {}", kind.len());

    let second = &rows[1];
    assert_eq!(second["kind"], json!("unnamed"));
    assert_eq!(second["payload"], json!({"ok": true}));
}

/// The writer owns the session's log, so the log is read through it:
/// a caller that holds only the writer can still project the log.
#[tokio::test]
async fn the_writer_reads_the_log_it_owns() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));
    writer
        .append(json!({"type": "UserInput", "data": {"text": "hi"}}))
        .await
        .unwrap();
    writer.flush().await.unwrap();

    let rows = writer.read().await.expect("read");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["seq"], 1);

    // A row the writer has buffered is not in the log yet: the read
    // sees what the last flush wrote.
    writer
        .append(json!({"type": "UserInput", "data": {"text": "pending"}}))
        .await
        .unwrap();
    assert_eq!(writer.read().await.expect("read").len(), 1);
}

/// The written order follows the ordinal, not the order two
/// producers happened to reach the queue in.
#[tokio::test]
async fn rows_are_written_in_ordinal_order() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));

    let first = writer.append(json!({"type": "Model"})).await.unwrap();
    let second = writer.append(json!({"type": "Model"})).await.unwrap();
    assert_eq!((first, second), (1, 2));

    writer.flush().await.unwrap();
    assert_eq!(stored_seqs(&sink).await, vec![1, 2]);
}

/// A synchronous producer — the provider's chunk callback — cannot
/// wait for room. Its row waits in the writer's spill instead of
/// being dropped, and lands at the next flush.
#[tokio::test]
async fn a_sync_append_never_drops_a_row() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer =
        LogWriter::with_capacity(sink.clone(), Arc::new(SystemClock), 1);
    for index in 0..8 {
        writer.append_nowait(json!({"type": "Model", "i": index}));
    }

    // The queue holds one row; the rest wait in the spill.
    assert!(
        !writer.spill.lock().is_empty(),
        "a row that does not fit waits in the spill",
    );

    writer.flush().await.unwrap();

    let rows = sink.read().await.unwrap();
    assert_eq!(rows.len(), 8, "a spilled row is never dropped");
    assert_eq!(
        stored_seqs(&sink).await,
        (1..=8).collect::<Vec<u64>>(),
        "every row keeps the ordinal it was given",
    );
}

/// A typed event carries `seq: 0` from its builder. The writer is
/// what knows the ordinal, so the writer is what stamps it: a row
/// that kept the builder's `0` would collide with every other
/// builder-defaulted row in the log.
#[tokio::test]
async fn a_typed_event_never_keeps_seq_zero() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));
    let event = SessionEvent::Usage {
        seq: 0,
        ts: String::new(),
        data: json!({"index": 1}),
        usage: None,
    };

    let seq = writer.append_event(&event).await.unwrap();
    writer.flush().await.unwrap();

    let rows = sink.read().await.unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0]["seq"].as_u64(),
        Some(seq),
        "the writer's ordinal replaces the builder's default",
    );
    assert_ne!(rows[0]["seq"].as_u64(), Some(0));
    assert!(
        rows[0]["ts"].as_str().is_some_and(|ts| !ts.is_empty()),
        "the writer stamps the clock's timestamp",
    );
}

/// The writer's ordinals must satisfy the fold's provenance rule: a
/// compaction row cites the seqs of the rows it shadows, and the
/// fold rejects a citation that is not older than the compaction
/// itself.
///
/// The fixture is the plan's: a user row at 5, an assistant row at 9,
/// and a compaction at 12 that cites `[5, 9]`. The gaps are ordinals
/// the session's earlier rows used — the writer continues the
/// session's numbering, it does not restart it.
#[tokio::test]
async fn a_written_compaction_row_folds() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    for _ in 0..4 {
        sink.alloc_seq();
    }
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));

    let user_seq = writer
        .append(json!({"type": "UserInput", "data": {"text": "hello"}}))
        .await
        .unwrap();
    assert_eq!(user_seq, 5);
    for _ in 0..3 {
        sink.alloc_seq();
    }
    let assistant_seq = writer
        .append(json!({
            "type": "Model",
            "data": {"type": "text", "text": "working"},
        }))
        .await
        .unwrap();
    assert_eq!(assistant_seq, 9);
    for _ in 0..2 {
        sink.alloc_seq();
    }
    let compaction_seq = writer
        .append(json!({
            "type": "compaction",
            "data": {"summary": "condensed"},
            "surface_op": {
                "start": 0,
                "end": 2,
                "source_event_seqs": [user_seq, assistant_seq],
            },
        }))
        .await
        .unwrap();
    assert_eq!(compaction_seq, 12);
    writer.flush().await.unwrap();

    let rows = sink.read().await.unwrap();
    let lenient = fold_log_surface(&rows);
    let strict = try_fold_log_surface(&rows).expect("the log folds");

    assert_eq!(
        lenient.surface_seqs,
        vec![12],
        "the compaction replaces both rows it cites",
    );
    assert_eq!(strict.surface_seqs, lenient.surface_seqs);
    assert_eq!(strict.messages, lenient.messages);
}

/// The ledger is what a compaction checkpoint resolves its span
/// against, and the fold is what the reader reproduces. They must see
/// the same rows, in the same order.
#[tokio::test]
async fn the_ledger_sees_the_rows_the_fold_sees() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let ledger = Arc::new(SurfaceLedger::new());
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock))
        .with_ledger(ledger.clone());

    writer
        .append(json!({"type": "UserInput", "data": {"text": "hello"}}))
        .await
        .unwrap();
    writer
        .append(json!({
            "type": "Model",
            "data": {"type": "text", "text": "hi"},
        }))
        .await
        .unwrap();
    writer.flush().await.unwrap();

    let rows = sink.read().await.unwrap();
    let folded = fold_log_surface(&rows);
    assert_eq!(ledger.len(), folded.messages.len());
}

/// A resumed session seeds its ledger from the history, numbered the
/// way the fold numbers it: a stamped history uses each row's own
/// `seq`, an old one counts positions.
#[tokio::test]
async fn a_seeded_ledger_numbers_history_like_the_fold() {
    let ledger = Arc::new(SurfaceLedger::new());
    let writer = LogWriter::new(
        Arc::new(InMemorySessionSink::new("s")),
        Arc::new(SystemClock),
    )
    .with_ledger(ledger.clone());

    let message = Message::tool(Content::text("ok"), "call-1");
    let data = serde_json::to_value(&message).unwrap();
    writer.seed_ledger(&[
        json!({"type": "UserInput", "seq": 5, "data": {"text": "hello"}}),
        json!({"type": "Message", "seq": 9, "data": data}),
    ]);

    let resolved = ledger
        .resolve(&[Some("tool:call-1".to_string())])
        .expect("the key resolves");
    assert_eq!(
        resolved.source_event_seqs,
        vec![9],
        "the seeded ledger reads the row's own ordinal",
    );
}

/// A failed write must not lose the rows: the batch goes back to the
/// writer's spill and the next flush writes it under the same
/// ordinals.
#[tokio::test]
async fn a_failed_flush_reports_the_error_and_keeps_the_rows() {
    let sink = Arc::new(CountingSink::new());
    sink.fail_next(1);
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));
    writer.append(json!({"type": "Model"})).await.unwrap();

    let error = writer.flush().await.unwrap_err();
    assert!(
        matches!(error, SessionError::AppendFailed(_)),
        "the sink's failure reaches the caller",
    );

    writer.flush().await.unwrap();
    let rows = sink.read().await.unwrap();
    assert_eq!(rows.len(), 1, "the refused row is still owed");
    assert_eq!(rows[0]["seq"].as_u64(), Some(1));
}

/// A sink that holds every batch until the test opens the gate, so
/// two flushes can be made to overlap on purpose.
struct GatedSink {
    /// The ordinals of each batch, in the order the batches landed.
    batches: std::sync::Mutex<Vec<Vec<u64>>>,
    /// The sink takes one permit per batch it announces, and waits
    /// for one gate permit before it writes the batch.
    entered: tokio::sync::Semaphore,
    gate: tokio::sync::Semaphore,
    next_seq: AtomicU64,
}

impl GatedSink {
    fn new() -> Self {
        Self {
            batches: std::sync::Mutex::new(Vec::new()),
            entered: tokio::sync::Semaphore::new(0),
            gate: tokio::sync::Semaphore::new(0),
            next_seq: AtomicU64::new(0),
        }
    }

    /// Wait until a batch is inside the sink.
    async fn wait_for_entry(&self) {
        self.entered
            .acquire()
            .await
            .expect("the semaphore is open")
            .forget();
    }

    /// Let one batch through.
    fn open_the_gate(&self) {
        self.gate.add_permits(1);
    }

    /// The ordinals of the batches written so far.
    fn batches(&self) -> Vec<Vec<u64>> {
        self.batches.lock().expect("GatedSink poisoned").clone()
    }
}

#[async_trait]
impl SessionSink for GatedSink {
    fn id(&self) -> &str {
        "gated"
    }

    fn alloc_seq(&self) -> u64 {
        self.next_seq.fetch_add(1, Ordering::SeqCst) + 1
    }

    async fn append(&self, event: &Value) -> Result<(), SessionError> {
        self.append_many(std::slice::from_ref(event)).await
    }

    async fn append_many(&self, events: &[Value]) -> Result<(), SessionError> {
        self.entered.add_permits(1);
        self.gate
            .acquire()
            .await
            .expect("the semaphore is open")
            .forget();
        let ordinals: Vec<u64> = events
            .iter()
            .filter_map(|row| row["seq"].as_u64())
            .collect();
        self.batches
            .lock()
            .expect("GatedSink poisoned")
            .push(ordinals);
        Ok(())
    }

    async fn read(&self) -> Result<Vec<Value>, SessionError> {
        Ok(Vec::new())
    }

    async fn read_from(&self, _from: u64) -> Result<Vec<Value>, SessionError> {
        Ok(Vec::new())
    }

    async fn snapshot(&self) -> Result<SessionSnapshot, SessionError> {
        let last = self.next_seq.load(Ordering::SeqCst);
        Ok(SessionSnapshot {
            session_id: "gated".to_string(),
            last_event_seq: last,
            last_stream_index: last,
            bytes_on_disk: 0,
        })
    }

    async fn close(
        &self,
        _reason: SessionEndReason,
    ) -> Result<(), SessionError> {
        Ok(())
    }
}

/// Two flushes that overlap must not write their batches in
/// whichever order they won the race: the file's order is the
/// ordinal order, and the reader reproduces it.
#[tokio::test]
async fn concurrent_flushes_keep_the_ordinal_order() {
    let sink = Arc::new(GatedSink::new());
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    for _ in 0..4 {
        writer.append(json!({"type": "Model"})).await.unwrap();
    }

    let first = {
        let writer = writer.clone();
        tokio::spawn(async move { writer.flush().await })
    };
    // The first flush is inside the sink, holding its batch.
    sink.wait_for_entry().await;
    for _ in 0..4 {
        writer.append(json!({"type": "Model"})).await.unwrap();
    }
    let second = {
        let writer = writer.clone();
        tokio::spawn(async move { writer.flush().await })
    };

    sink.open_the_gate();
    sink.open_the_gate();
    first.await.unwrap().unwrap();
    second.await.unwrap().unwrap();

    assert_eq!(
        sink.batches(),
        vec![vec![1, 2, 3, 4], vec![5, 6, 7, 8]],
        "the file's order is the ordinal order",
    );
}

/// A restart must not hand out an ordinal the log already holds. The
/// log below has a hole: ordinal 2 was reserved and never written, so
/// the row count (2) is not the highest ordinal (3) the log holds.
#[tokio::test]
async fn restart_does_not_reuse_a_burned_ordinal() {
    let dir = temp_dir().join("s_burned");
    let sink = JsonlSessionSink::new("s_burned", dir.clone());
    sink.alloc_seq();
    sink.append(&json!({"type": "Model", "seq": 1}))
        .await
        .unwrap();
    sink.alloc_seq();
    sink.alloc_seq();
    sink.append(&json!({"type": "Model", "seq": 3}))
        .await
        .unwrap();
    drop(sink);

    let reopened = JsonlSessionSink::new("s_burned", dir);
    assert_eq!(
        reopened.alloc_seq(),
        4,
        "the counter continues from the log's highest ordinal",
    );
}

/// An old log — one whose rows carry no `seq` — keeps the counter it
/// always had: one past its last row.
#[tokio::test]
async fn an_old_log_keeps_counting_its_rows() {
    let dir = temp_dir().join("s_old");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("events.jsonl"), "{\"i\":0}\n{\"i\":1}\n").unwrap();

    let sink = JsonlSessionSink::new("s_old", dir);
    assert_eq!(sink.alloc_seq(), 3, "an old log counts its rows");
}

/// Repair *adds* the closers a resumed run owes; it never truncates
/// what is already in the log, and the writer hands out the closers'
/// ordinals.
#[tokio::test]
async fn repair_appends_closers_and_keeps_the_log() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));
    writer.append(json!({"type": "Model"})).await.unwrap();
    writer.flush().await.unwrap();

    let closer = SessionEvent::Usage {
        seq: 0,
        ts: String::new(),
        data: json!({"index": 2}),
        usage: None,
    };
    writer.repair(vec![closer]).await.unwrap();

    assert_eq!(stored_seqs(&sink).await, vec![1, 2]);
}

/// The run's rows: a prompt, an assistant line, and the two tool
/// results a compaction will shadow.
///
/// The tool results carry the call id — the only durable identity a
/// compaction record can cite.
fn run_rows() -> Vec<Value> {
    vec![
        json!({"type": "UserInput", "data": {"text": "hello"}}),
        json!({"type": "Model", "data": {"type": "text", "text": "working"}}),
        tool_result_row("c1"),
        tool_result_row("c2"),
    ]
}

/// One tool-result row, in the shape the controller persists.
fn tool_result_row(call_id: &str) -> Value {
    json!({
        "type": "Model",
        "data": {
            "type": "tool_result",
            "tool_use_id": call_id,
            "tool_name": "read",
            "content": [{
                "type": "text",
                "text": format!("raw output of {call_id}"),
            }],
            "structured_content": null,
            "is_error": null,
        }
    })
}

/// A compaction record replacing the rows `calls` name.
///
/// The keys are the durable identities the manager reports: a
/// tool-result row is named by the call it answers.
fn record(calls: &[&str], summary: &str) -> CompactionRecordView {
    CompactionRecordView {
        start: 2,
        end: 2 + calls.len(),
        source_indices: (2..2 + calls.len()).collect(),
        source_keys: calls
            .iter()
            .map(|call| Some(format!("tool:{call}")))
            .collect(),
        summary_text: summary.to_string(),
    }
}

/// The writer is the session's and outlives the run that resolves
/// its compactions; the ledger that indexes the surface is the run's.
/// `set_ledger` swaps the ledger the writer feeds, so a row lands in
/// the ledger of the run that wrote it.
#[tokio::test]
async fn a_shared_writer_feeds_the_ledger_it_was_given() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let first = Arc::new(SurfaceLedger::new());
    let second = Arc::new(SurfaceLedger::new());
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));

    writer.set_ledger(Arc::clone(&first));
    writer
        .append(json!({"type": "Model", "data": {"type": "text", "text": "a"}}))
        .await
        .unwrap();
    writer.flush().await.unwrap();
    assert_eq!(first.len(), 1, "the first ledger saw the row");

    writer.set_ledger(Arc::clone(&second));
    writer
        .append(json!({"type": "Model", "data": {"type": "text", "text": "b"}}))
        .await
        .unwrap();
    writer.flush().await.unwrap();
    assert_eq!(first.len(), 1, "the old ledger is fed no more");
    assert_eq!(second.len(), 1, "the new ledger saw the row");
}

/// A compaction checkpoint's callbacks are synchronous: the
/// summarising manager fires them from the agent's own task, and a
/// callback cannot await. A row that does not fit in the writer's
/// queue must wait in the spill instead of being dropped — a dropped
/// compaction row leaves the surface without the replacement it
/// promised, and the reader replays the shadowed span.
#[tokio::test]
async fn compaction_row_survives_a_full_queue() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let ledger = Arc::new(SurfaceLedger::new());
    let writer = Arc::new(
        LogWriter::with_capacity(sink.clone(), Arc::new(SystemClock), 1)
            .with_ledger(Arc::clone(&ledger)),
    );

    // The run's rows land first: the ledger now knows where the
    // shadowed rows sit and which ordinals identify them.
    for row in run_rows() {
        writer.append(row).await.unwrap();
    }
    writer.flush().await.unwrap();

    let checkpoint =
        CompactionCheckpoint::new(Arc::clone(&writer), Arc::clone(&ledger));
    // The queue holds one row: the lifecycle row takes that slot and
    // the compaction record — the row that must not be lost — waits
    // in the spill.
    checkpoint.lifecycle_callback()(CompactionLifecycleView::Start {
        token: "t1".to_string(),
    });
    checkpoint.record_callback()(record(&["c1", "c2"], "compacted tail"));

    writer.flush().await.unwrap();

    let rows = sink.read().await.unwrap();
    assert_eq!(rows.len(), 6, "no row is dropped: {rows:?}");
    let compaction = rows
        .iter()
        .find(|row| row["type"] == "compaction")
        .expect("the checkpoint's record is in the log");
    assert_eq!(
        compaction["surface_op"]["source_event_seqs"],
        json!([3, 4]),
        "the record cites the rows it shadows"
    );
    // The row's provenance resolves: the strict fold accepts it.
    try_fold_log_surface(&rows).expect("the written log folds");
}

/// The request header says why it is there: `initial` for the
/// session's first one, `same` when the envelope repeats, `change`
/// when it drifted. The writer remembers the last envelope it
/// stamped, so a run's header carries the drift marker even though
/// every run stamps one.
#[tokio::test]
async fn the_request_header_says_whether_the_envelope_drifted() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));

    writer.append_request_header("anthropic", "opus", "aaaa");
    writer.append_request_header("anthropic", "opus", "aaaa");
    writer.append_request_header("openai", "gpt", "bbbb");
    writer.flush().await.expect("flush");

    let rows = sink.read().await.expect("read");
    let reasons: Vec<&str> = rows
        .iter()
        .map(|row| row["data"]["reason"].as_str().expect("a reason"))
        .collect();
    assert_eq!(reasons, vec!["initial", "same", "change"]);
    assert!(
        rows.iter().all(|row| row["type"] == "request_header"),
        "every row is the header: {rows:?}"
    );
}

/// A sink that writes the batch and then reports the barrier as
/// failed — the shape of a backend whose bytes reached the file but
/// whose durability barrier did not hold.
///
/// The rows land in the production backend, so the retry that follows
/// is answered by the production sink: a row the log already holds is
/// not written a second time.
struct WriteThenFailSink {
    /// The backend the rows actually land in.
    inner: JsonlSessionSink,
    /// Whether the next batch reports its barrier as failed.
    fail_next: AtomicBool,
}

impl WriteThenFailSink {
    fn new(dir: std::path::PathBuf) -> Self {
        Self {
            inner: JsonlSessionSink::new("write-then-fail", dir),
            fail_next: AtomicBool::new(false),
        }
    }

    /// Report the next batch's barrier as failed.
    fn fail_next_batch(&self) {
        self.fail_next.store(true, Ordering::SeqCst);
    }

    /// The rows the log holds.
    async fn rows(&self) -> Vec<Value> {
        self.inner.read().await.expect("read the log")
    }
}

#[async_trait]
impl SessionSink for WriteThenFailSink {
    fn id(&self) -> &str {
        "write-then-fail"
    }

    fn alloc_seq(&self) -> u64 {
        self.inner.alloc_seq()
    }

    async fn append(&self, event: &Value) -> Result<(), SessionError> {
        self.inner.append(event).await
    }

    async fn append_many(&self, events: &[Value]) -> Result<(), SessionError> {
        // The bytes land ...
        self.inner.append_many(events).await?;
        // ... and the barrier fails.
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(SessionError::AppendFailed(
                "the durability barrier failed".to_string(),
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

/// A flush whose barrier failed must not write the row twice: the
/// retry finds the row in the log and leaves it there.
#[tokio::test]
async fn a_retry_after_a_failed_flush_does_not_duplicate_a_row() {
    let sink = Arc::new(WriteThenFailSink::new(temp_dir().join("s_retry")));
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));
    writer.append(json!({"type": "Model"})).await.unwrap();

    // The first flush: the bytes land, the barrier fails.
    sink.fail_next_batch();
    let error = writer.flush().await.unwrap_err();
    assert!(
        matches!(error, SessionError::AppendFailed(_)),
        "the barrier's failure reaches the caller",
    );

    // The retry: the row is already in the log.
    writer.flush().await.unwrap();

    let rows = sink.rows().await;
    assert_eq!(rows.len(), 1, "the row is written once: {rows:?}");
    assert_eq!(rows[0]["seq"].as_u64(), Some(1));
}

/// The log's rows are written once: a row the log already holds is
/// not written again, and a row without an ordinal is written — what
/// cannot be identified cannot be skipped.
#[tokio::test]
async fn a_row_the_log_already_holds_is_not_written_again() {
    let dir = temp_dir().join("s_once");
    let sink = JsonlSessionSink::new("s_once", dir);
    let row = json!({"type": "Model", "seq": 1});
    sink.append_many(std::slice::from_ref(&row)).await.unwrap();
    sink.append_many(&[row.clone(), json!({"type": "Model"})])
        .await
        .unwrap();

    let rows = sink.read().await.unwrap();
    assert_eq!(
        rows.len(),
        2,
        "the identified row is written once, the anonymous row is written: \
         {rows:?}"
    );
    assert_eq!(rows[0]["seq"].as_u64(), Some(1));
}

/// An ordinal is 1-based: a row that carries `seq: 0` carries no
/// identity — a typed event's builder leaves it behind — and is
/// written like a row with no ordinal at all.
#[tokio::test]
async fn a_row_without_a_valid_ordinal_is_written() {
    let dir = temp_dir().join("s_unstamped");
    let sink = JsonlSessionSink::new("s_unstamped", dir);
    sink.append_many(&[json!({"type": "Model", "seq": 0})])
        .await
        .unwrap();

    let rows = sink.read().await.unwrap();
    assert_eq!(rows.len(), 1, "the row is written: {rows:?}");
}

/// A write whose bytes reached the file but whose barrier failed
/// leaves the rows in the log: the retry must not write them again.
///
/// `/dev/null` takes the write and refuses the barrier, which is
/// that shape exactly.
#[cfg(unix)]
#[tokio::test]
async fn a_failed_barrier_leaves_the_rows_written() {
    let dir = temp_dir().join("s_barrier");
    std::fs::create_dir_all(&dir).unwrap();
    std::os::unix::fs::symlink("/dev/null", dir.join("events.jsonl")).unwrap();
    let sink = JsonlSessionSink::new("s_barrier", dir);
    let row = json!({"type": "Model", "seq": 1});
    let written = serde_json::to_string(&row).unwrap().len() as u64 + 1;

    // The bytes land, the barrier fails.
    let error = sink
        .append_many(std::slice::from_ref(&row))
        .await
        .unwrap_err();
    assert!(
        matches!(error, SessionError::AppendFailed(_)),
        "the barrier's failure reaches the caller",
    );

    // The retry finds the row already written.
    sink.append_many(&[row])
        .await
        .expect("the retry writes nothing");
    let snapshot = sink.snapshot().await.unwrap();
    assert_eq!(snapshot.bytes_on_disk, written, "the row is written once");
}

/// A copy is one durable barrier no matter its size: the rows never
/// enter the queue, so a copy longer than the queue's capacity is
/// still written by a single `append_many`.
#[tokio::test]
async fn a_copy_is_one_append_many_regardless_of_size() {
    let sink = Arc::new(CountingSink::new());
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));
    let rows: Vec<Value> = (0..600)
        .map(|i| {
            json!({
                "seq": i + 1,
                "type": "Model",
                "ts": "2026-10-25T08:00:00+00:00",
                "data": {"text": i},
            })
        })
        .collect();

    let seqs = writer.append_copied(rows).await.unwrap();

    assert_eq!(
        sink.append_many_calls(),
        1,
        "the whole copy is one barrier, not one per queue-load"
    );
    assert_eq!(sink.append_calls(), 0, "no per-row fallback");
    assert_eq!(seqs, (1..=600).collect::<Vec<u64>>());
    let written = sink.read().await.unwrap();
    assert_eq!(written.len(), 600);
    assert_eq!(written[0]["seq"], 1, "the copy takes this log's ordinals");
    assert_eq!(
        written[0]["ts"], "2026-10-25T08:00:00+00:00",
        "the copy keeps the row's own timestamp"
    );
}
