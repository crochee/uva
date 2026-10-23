//! `LogWriter` unit tests: the queue never drops a row, the
//! written order follows the ordinal, and the flush is the only
//! durability promise.

use std::sync::Arc;

use serde_json::json;
use synthia_session::{
    SessionEvent,
    SessionSink,
    in_memory::InMemorySessionSink,
};

use super::{Durability, LogWriter, StampedRow};

// One usage row, the cheapest typed event to build.
fn usage_row(index: u64) -> SessionEvent {
    SessionEvent::Usage {
        seq: 0,
        ts: String::new(),
        data: json!({ "index": index }),
        usage: None,
    }
}

// A stamped row for the given event.
fn row(writer: &LogWriter, event: SessionEvent) -> StampedRow {
    let seq = writer.stamp();
    let value = writer.row_value(&event, seq);
    StampedRow { seq, value }
}

// The ordinals the sink holds, in the order it holds them.
async fn stored_seqs(sink: &InMemorySessionSink) -> Vec<u64> {
    sink.read()
        .await
        .expect("read")
        .iter()
        .filter_map(|row| row["seq"].as_u64())
        .collect()
}

// A queue that holds one row forces the slow path on every append
// after the first. Every row must still land: a dropped row would
// leave a hole in the ordinals, and the fold's provenance check
// rejects every compaction that cites a row after the hole.
#[tokio::test]
async fn a_saturated_queue_never_drops_a_durable_row() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer =
        LogWriter::with_capacity(sink.clone(), 1, Durability::StepBoundary);

    for index in 0..64u64 {
        writer.append(row(&writer, usage_row(index))).await;
    }
    writer.flush().await.expect("flush");

    let seqs = stored_seqs(&sink).await;
    assert_eq!(
        seqs,
        (1..=64).collect::<Vec<u64>>(),
        "every row must land under its own ordinal"
    );
}

// The ordinal is what the reader reproduces, so the written order
// follows the ordinal — not the order two producers happened to
// reach the queue in.
#[tokio::test]
async fn rows_are_written_in_ordinal_order() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = LogWriter::new(sink.clone(), Durability::StepBoundary);

    let first = row(&writer, usage_row(1));
    let second = row(&writer, usage_row(2));
    writer.append(second).await;
    writer.append(first).await;
    writer.flush().await.expect("flush");

    assert_eq!(
        stored_seqs(&sink).await,
        vec![1, 2],
        "read order follows the stamped ordinal, not arrival"
    );
}

// Rows are not durable until the flush: the queue is the buffer,
// and only the flush writes to the sink.
#[tokio::test]
async fn rows_reach_the_sink_only_at_the_flush() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = LogWriter::new(sink.clone(), Durability::StepBoundary);

    writer.append(row(&writer, usage_row(1))).await;
    assert!(
        sink.read().await.expect("read").is_empty(),
        "an unflushed row must not be in the sink"
    );

    writer.flush().await.expect("flush");
    assert_eq!(stored_seqs(&sink).await, vec![1]);
}

// A synchronous producer — the provider's chunk callback — cannot
// wait for room. Its row waits in the writer's spill instead of
// being dropped, and lands at the next flush.
#[tokio::test]
async fn a_synchronous_producer_never_drops_its_row() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer =
        LogWriter::with_capacity(sink.clone(), 1, Durability::StepBoundary);

    for index in 0..8u64 {
        writer.append_nowait(row(&writer, usage_row(index)));
    }
    writer.flush().await.expect("flush");

    let seqs = stored_seqs(&sink).await;
    assert_eq!(seqs, (1..=8).collect::<Vec<u64>>());
}

// `Durability::Off` keeps the rows in memory: the writer stamps
// them, but nothing reaches the sink.
#[tokio::test]
async fn durability_off_never_writes_to_the_sink() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = LogWriter::new(sink.clone(), Durability::Off);

    writer.append(row(&writer, usage_row(1))).await;
    writer.flush().await.expect("flush");

    assert!(sink.read().await.expect("read").is_empty());
}

// Repair *adds* the closers a resumed run owes; it never truncates
// what is already in the log.
#[tokio::test]
async fn repair_appends_closers_and_keeps_the_log() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = LogWriter::new(sink.clone(), Durability::StepBoundary);

    writer.append(row(&writer, usage_row(1))).await;
    writer.flush().await.expect("flush");

    writer.repair(vec![usage_row(2)]).await.expect("repair");

    assert_eq!(stored_seqs(&sink).await, vec![1, 2]);
}
