//! Durable compaction checkpoint: a splice that survives a restart.
//!
//! Seam shown: `CompactionCheckpoint` bridges the summarising
//! manager's `CompactionRecordView` — in-memory message indices plus
//! the call ids of the shadowed tool results — to a durable
//! `SessionEvent::Compaction` row, but **only** when the run's
//! `SurfaceLedger` can prove where those rows sit in the log and
//! which seqs identify them. The session's `LogWriter` feeds that
//! ledger every row it appends, with the row's own ordinal; this
//! example runs it over the crate's own `InMemorySessionSink`, with
//! no agent, no provider call and no network.
//!
//! Run:
//!
//! ```text
//! cargo run -p synthia-session --example compaction_checkpoint
//! ```
//!
//! Look at:
//!
//! 1. the `compaction` row's `surface_op`: the span and cited seqs
//!    are the log's, not the manager's indices (the manager counts
//!    the system prompt the loop never logs, so it reports index 3
//!    where the durable surface holds the row at 2);
//! 2. the fold before vs after: the tool-result row is *replaced*,
//!    never replayed, and the surface keeps its length;
//! 3. the fail-soft run: a call id the log never carried writes no
//!    `compaction` row at all, yet the lifecycle rows still land
//!    and the surface still folds.

use std::sync::Arc;

use serde_json::Value;
use synthia_core::SystemClock;
use synthia_provider::{Content, Message};
use synthia_session::{
    CompactionCheckpoint,
    CompactionLifecycleOutcome,
    CompactionLifecycleView,
    CompactionRecordView,
    FoldedSurface,
    LogWriter,
    SessionEvent,
    SessionSink,
    SurfaceLedger,
    fold_log_surface,
    in_memory::InMemorySessionSink,
    try_fold_log_surface,
};

/// The run's rows: a prompt, one assistant line, and the tool result
/// the compaction will shadow.
///
/// Each row is the typed event the controller persists, serialised
/// to its JSONL form. The tool result carries the call id — the only
/// durable identity a record can cite.
fn run_rows() -> Vec<Value> {
    let user = SessionEvent::UserMessage {
        seq: 0,
        ts: String::new(),
        data: serde_json::to_value(Message::user("read the config file"))
            .expect("a user message serialises"),
        source: None,
        surface_op: None,
    };
    let assistant = SessionEvent::AssistantMessage {
        seq: 0,
        ts: String::new(),
        data: serde_json::to_value(Message::assistant("reading it now"))
            .expect("an assistant message serialises"),
        surface_op: None,
    };
    let tool_result = SessionEvent::ToolResult {
        seq: 0,
        ts: String::new(),
        data: serde_json::to_value(Message::tool(
            Content::text("raw output of call-1"),
            "call-1",
        ))
        .expect("a tool result serialises"),
        is_error: false,
        surface_op: None,
    };
    [user, assistant, tool_result]
        .into_iter()
        .map(|event| {
            serde_json::to_value(&event).expect("an event row serialises")
        })
        .collect()
}

/// Drive the manager's two callbacks for one attempt: the lifecycle
/// triple, then the record.
fn attempt(
    checkpoint: &Arc<CompactionCheckpoint>,
    token: &str,
    record: CompactionRecordView,
) {
    let lifecycle = checkpoint.lifecycle_callback();
    lifecycle(CompactionLifecycleView::Start {
        token: token.to_string(),
    });
    lifecycle(CompactionLifecycleView::Summary {
        token: token.to_string(),
        summary: record.summary_text.clone(),
        shadowed_positions: record
            .source_indices
            .iter()
            .map(|index| *index as u64)
            .collect(),
    });
    checkpoint.record_callback()(record);
    lifecycle(CompactionLifecycleView::End {
        token: token.to_string(),
        outcome: CompactionLifecycleOutcome::Committed,
    });
}

/// The run-end flush, in the server's order: the writer's queued
/// rows reach the log, then the records that parked are retried and
/// what resolved is written through the same writer. Returns
/// `(parked, resolved-and-written)`.
async fn flush(
    checkpoint: &Arc<CompactionCheckpoint>,
    writer: &Arc<LogWriter>,
) -> (usize, usize) {
    writer
        .flush()
        .await
        .expect("the writer's rows reach the log");
    let parked = checkpoint.pending_len();
    let resolved = checkpoint.resolve_pending();
    let written = resolved.len();
    for event in resolved {
        writer
            .append_event(&event)
            .await
            .expect("the resolved record is queued");
    }
    writer
        .flush()
        .await
        .expect("the resolved record reaches the log");
    (parked, written)
}

/// The first row whose `type` tag is `tag`.
fn find_row<'a>(rows: &'a [Value], tag: &str) -> Option<&'a Value> {
    rows.iter()
        .find(|row| row.get("type").and_then(Value::as_str) == Some(tag))
}

/// How many rows carry the `type` tag `tag`.
fn count_rows(rows: &[Value], tag: &str) -> usize {
    rows.iter()
        .filter(|row| row.get("type").and_then(Value::as_str) == Some(tag))
        .count()
}

/// A one-line preview of a folded message: its first text (or
/// summary) string, else the raw JSON, elided to fit the transcript.
fn preview(message: &Value) -> String {
    /// Depth-first search for the first string under `keys`.
    fn find(value: &Value, keys: [&str; 2]) -> Option<String> {
        match value {
            Value::Object(map) => {
                for key in keys {
                    if let Some(Value::String(text)) = map.get(key) {
                        return Some(text.clone());
                    }
                }
                map.values().find_map(|value| find(value, keys))
            }
            Value::Array(items) => {
                items.iter().find_map(|value| find(value, keys))
            }
            _ => None,
        }
    }
    let text = find(message, ["text", "summary"]).unwrap_or_else(|| {
        serde_json::to_string(message).expect("a message serialises")
    });
    let mut out: String = text.chars().take(48).collect();
    if text.chars().count() > 48 {
        out.push('…');
    }
    out
}

/// One line per folded message: surface seq, role, short preview.
fn print_surface(label: &str, folded: &FoldedSurface) {
    println!(
        "{label}: {} message(s), surface_seqs={:?}",
        folded.messages.len(),
        folded.surface_seqs,
    );
    for (index, (message, seq)) in
        folded.messages.iter().zip(&folded.surface_seqs).enumerate()
    {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("<none>");
        println!("  #{index} seq={seq} role={role} {}", preview(message));
    }
}

/// True when any folded message carries `needle`.
fn mentions(folded: &FoldedSurface, needle: &str) -> bool {
    folded.messages.iter().any(|message| {
        serde_json::to_string(message)
            .expect("a message serialises")
            .contains(needle)
    })
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    println!("== run 1: a record the ledger can prove ==");
    let ledger = Arc::new(SurfaceLedger::new());
    let sink = Arc::new(InMemorySessionSink::new("checkpoint-proven"));
    let writer = Arc::new(
        LogWriter::new(sink.clone(), Arc::new(SystemClock))
            .with_ledger(Arc::clone(&ledger)),
    );
    for row in run_rows() {
        writer
            .append(row)
            .await
            .expect("the writer queues the rows");
    }
    writer.flush().await.expect("the run's rows reach the log");
    let rows_before = sink.read().await.expect("the log reads");
    println!(
        "run rows: {} (tool result `call-1` at seq 3)",
        rows_before.len()
    );
    let before = fold_log_surface(&rows_before);
    print_surface("surface before the checkpoint", &before);

    let checkpoint =
        CompactionCheckpoint::new(Arc::clone(&writer), Arc::clone(&ledger));

    // What the manager reports: it counts the system prompt the
    // loop prepends but never logs, so the tool result is its index
    // 3 while the durable surface holds it at 2. `source_keys`
    // carries the identity the checkpoint actually trusts.
    attempt(
        &checkpoint,
        "t1",
        CompactionRecordView {
            start: 3,
            end: 4,
            source_indices: vec![3],
            source_keys: vec![Some("tool:call-1".to_string())],
            summary_text: "summary: read the config file -> ok".to_string(),
        },
    );
    println!(
        "records parked after the callback: {}",
        checkpoint.pending_len()
    );

    let (parked, written) = flush(&checkpoint, &writer).await;
    println!("flush: parked={parked} resolved-and-written={written}");

    let rows_after = sink.read().await.expect("the log reads");
    let compaction = find_row(&rows_after, "compaction")
        .expect("the checkpoint wrote a compaction row");
    let tag = compaction
        .get("type")
        .and_then(Value::as_str)
        .expect("the row carries a type tag");
    let op = compaction
        .get("surface_op")
        .expect("a compaction row carries a surface_op");
    println!(
        "compaction row: type={tag} seq={} surface_op={}",
        compaction.get("seq").expect("the writer stamped a seq"),
        serde_json::to_string(op).expect("the surface_op serialises"),
    );
    println!(
        "  manager index 3 -> start={} end={} source_event_seqs={}",
        op.get("start").expect("a replace carries a start"),
        op.get("end").expect("a replace carries an end"),
        op.get("source_event_seqs")
            .expect("a replace cites source seqs"),
    );

    let after = fold_log_surface(&rows_after);
    print_surface("surface after the checkpoint", &after);
    let strict = try_fold_log_surface(&rows_after)
        .expect("the checkpoint's log folds under the strict variant");
    assert_eq!(
        strict.surface_seqs, after.surface_seqs,
        "the lenient and strict folds must agree"
    );
    println!("strict fold agrees with the lenient fold");

    assert!(
        mentions(&before, "raw output of call-1"),
        "the pre-compaction log replays the tool result"
    );
    assert!(
        !mentions(&after, "raw output of call-1"),
        "the checkpoint must replace the tool result, not replay it"
    );
    assert!(
        mentions(&after, "summary: read the config file"),
        "the summary took the shadowed row's place"
    );
    assert_eq!(
        before.messages.len(),
        after.messages.len(),
        "a replace swaps one surface slot for one summary"
    );
    println!(
        "tool result in surface: before={} after={} (replaced, not \
         replayed)",
        mentions(&before, "raw output of call-1"),
        mentions(&after, "raw output of call-1"),
    );

    println!();
    println!("== run 2: a record the ledger cannot prove ==");
    let ghost_ledger = Arc::new(SurfaceLedger::new());
    let ghost_sink = Arc::new(InMemorySessionSink::new("checkpoint-unproven"));
    let ghost_writer = Arc::new(
        LogWriter::new(ghost_sink.clone(), Arc::new(SystemClock))
            .with_ledger(Arc::clone(&ghost_ledger)),
    );
    for row in run_rows() {
        ghost_writer
            .append(row)
            .await
            .expect("the writer queues the rows");
    }
    ghost_writer
        .flush()
        .await
        .expect("the run's rows reach the log");
    let ghost_keys = vec![Some("tool:call-ghost".to_string())];
    println!(
        "ledger lookup for `tool:call-ghost`: {}",
        match ghost_ledger.resolve(&ghost_keys) {
            Ok(_) => "resolved".to_string(),
            Err(gap) => gap.to_string(),
        }
    );

    let ghost_checkpoint = CompactionCheckpoint::new(
        Arc::clone(&ghost_writer),
        Arc::clone(&ghost_ledger),
    );
    // A call id no row carries: citing it would invent provenance,
    // so the record is parked instead of written.
    attempt(
        &ghost_checkpoint,
        "t2",
        CompactionRecordView {
            start: 3,
            end: 4,
            source_indices: vec![3],
            source_keys: ghost_keys,
            summary_text: "summary: cannot be proven".to_string(),
        },
    );
    println!(
        "records parked after the callback: {}",
        ghost_checkpoint.pending_len()
    );
    let (parked, written) = flush(&ghost_checkpoint, &ghost_writer).await;
    println!(
        "flush: parked={parked} resolved-and-written={written} \
         (an unprovable record is dropped)"
    );
    let ghost_rows = ghost_sink.read().await.expect("the log reads");
    println!(
        "compaction rows in the log: {}",
        count_rows(&ghost_rows, "compaction")
    );
    println!(
        "lifecycle rows written: start={} summary={} end={}",
        count_rows(&ghost_rows, "compaction_start"),
        count_rows(&ghost_rows, "compaction_summary"),
        count_rows(&ghost_rows, "compaction_end"),
    );
    assert_eq!(count_rows(&ghost_rows, "compaction"), 0);
    assert_eq!(count_rows(&ghost_rows, "compaction_start"), 1);
    assert_eq!(count_rows(&ghost_rows, "compaction_summary"), 1);
    assert_eq!(count_rows(&ghost_rows, "compaction_end"), 1);

    let ghost_surface = fold_log_surface(&ghost_rows);
    print_surface("surface after the fail-soft run", &ghost_surface);
    assert_eq!(
        ghost_surface.messages.len(),
        3,
        "no replace was applied; the pre-compaction rows still fold"
    );
    assert!(mentions(&ghost_surface, "raw output of call-1"));
    assert!(
        try_fold_log_surface(&ghost_rows).is_ok(),
        "the surface stays foldable"
    );
    println!("strict fold still succeeds: the surface stays foldable");

    println!("COMPACTION-CHECKPOINT: OK");
}
