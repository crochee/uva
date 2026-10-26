//! `RunLog` — the run's handle on the session's one writer.
//!
//! The writer is the only thing that can tell a compaction
//! checkpoint where its shadowed rows are: it stamps every row
//! with the ordinal the reader will reproduce and feeds the same
//! rows, under the same ordinals, to the ledger.
use std::sync::Arc;

use synthia::session::{
    LogWriter,
    SessionSink,
    SurfaceLedger,
    try_fold_log_surface,
};

use super::super::run_log::RunLog;

#[tokio::test]
async fn run_log_feeds_the_ledger_with_log_ordinals() {
    let sink: Arc<dyn SessionSink> =
        Arc::new(synthia::session::in_memory::InMemorySessionSink::new("s1"));
    let ledger = Arc::new(SurfaceLedger::new());
    let writer = Arc::new(
        LogWriter::new(Arc::clone(&sink), Arc::new(synthia::core::SystemClock))
            .with_ledger(Arc::clone(&ledger)),
    );
    let mut log = RunLog::new(Arc::clone(&writer));

    log.append(serde_json::json!({
        "type": "UserInput",
        "data": {"text": "hi"},
    }))
    .await
    .unwrap();
    log.append(serde_json::json!({
        "type": "Model",
        "data": {
            "type": "tool_result",
            "tool_use_id": "c1",
            "content": [{"type": "text", "text": "out"}],
        }
    }))
    .await
    .unwrap();
    writer.flush().await.unwrap();

    assert_eq!(ledger.len(), 2);
    let resolved = ledger
        .resolve(&[Some("tool:c1".to_string())])
        .expect("the tool row resolves");
    assert_eq!(resolved.start, 1, "the tool row is the second surface row");
    assert_eq!(resolved.source_event_seqs, vec![2]);

    // A typed row is stamped with the ordinal it will have in the
    // log, which is what a checkpoint cites.
    let event = synthia::session::SessionEvent::CompactionEnd {
        seq: 0,
        ts: String::new(),
        token: "t".to_string(),
        outcome: "failed".to_string(),
        data: serde_json::json!({}),
    };
    log.append_typed(serde_json::to_value(&event).unwrap())
        .await
        .unwrap();
    writer.flush().await.unwrap();
    let rows = sink.read().await.unwrap();
    assert_eq!(rows[2]["seq"], 3);
    assert!(
        rows[2]["ts"].as_str().is_some_and(|ts| !ts.is_empty()),
        "the wall clock is stamped too"
    );
    // The log is fully stamped, so the strict fold reads each row's
    // own ordinal — the same numbers the ledger recorded.
    try_fold_log_surface(&rows).expect("a fully stamped log folds");
}

/// The ledger must record the ordinal the **sink's counter** gave
/// the row, not `previous + 1`.
///
/// The controller's op loop appends out-of-band — `Feedback` rows,
/// and the shutdown marker, which can land while a run is still
/// writing. A locally counted value would file the next run row one
/// slot too low, and a checkpoint citing it would fail
/// `validate_replace`'s `cited >= self_seq` check: the lenient fold
/// then replays the span the checkpoint had replaced, and the strict
/// fold errors. The out-of-band rows go through the same writer now,
/// so the interleave below is the production shape: one ordinal
/// source, and the ledger, the fold and the counter agree.
#[tokio::test]
async fn run_log_takes_its_ordinal_from_the_sink_not_a_local_counter() {
    let sink: Arc<dyn SessionSink> =
        Arc::new(synthia::session::in_memory::InMemorySessionSink::new("s1"));
    let writer = Arc::new(LogWriter::new(
        Arc::clone(&sink),
        Arc::new(synthia::core::SystemClock),
    ));
    // Seed the writer as the run task does: the previous run left
    // one row, and the run seeds its ledger from what is on disk.
    writer
        .append(serde_json::json!({
            "type": "UserInput",
            "data": {"text": "old"},
        }))
        .await
        .unwrap();
    writer.flush().await.unwrap();

    let ledger = Arc::new(SurfaceLedger::new());
    writer.set_ledger(Arc::clone(&ledger));
    let existing = sink.read().await.unwrap();
    writer.seed_ledger(&existing);

    let mut log = RunLog::new(Arc::clone(&writer));
    log.append(serde_json::json!({
        "type": "UserInput",
        "data": {"text": "run row"},
    }))
    .await
    .unwrap();

    // The op loop's out-of-band row lands between two of the run's
    // own — through the same writer, so it draws from the same
    // counter and the two cannot collide.
    writer
        .append(serde_json::json!({
            "kind": "feedback",
            "message_id": "m1",
            "thumbs_up": true,
        }))
        .await
        .unwrap();

    log.append(serde_json::json!({
        "type": "Model",
        "data": {
            "type": "tool_result",
            "tool_use_id": "c9",
            "content": [{"type": "text", "text": "out"}],
        }
    }))
    .await
    .unwrap();
    writer.flush().await.unwrap();

    let rows = sink.read().await.unwrap();
    assert_eq!(rows.len(), 4, "seed + run + feedback + run");
    // The tool row is physically row 4. The ledger must say so —
    // a local counter would have said 3 (the feedback row's
    // ordinal) and made any checkpoint citing it unresolvable.
    let resolved = ledger
        .resolve(&[Some("tool:c9".to_string())])
        .expect("the tool row resolves");
    assert_eq!(
        resolved.source_event_seqs,
        vec![4],
        "the ledger records the counter's ordinal, not previous + 1"
    );
    try_fold_log_surface(&rows).expect("a fully stamped log folds");
}
