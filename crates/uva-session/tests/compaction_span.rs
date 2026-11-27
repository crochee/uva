//! A compaction's span must name the rows it cites.
//!
//! The durable fold reads a `SurfaceOp::Replace` as a *span* of the
//! surface plus the seqs of the rows it shadows. The two are one
//! description of the same rows: `source_event_seqs[i]` is the seq
//! of the surface row at `start + i`. A range that names the right
//! rows in the wrong coordinates passes every provenance check and
//! shadows the wrong rows — silently, because the fold had no way to
//! tell.
//!
//! These tests pin the tie: the strict fold refuses a replace whose
//! citation does not name its span, and the lenient fold skips it
//! and replays the shadowed span instead.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::{Value, json};
use uva_core::{CancelToken, SystemClock};
use uva_provider::{Content, ContentPart, Message, Role, ToolUse};
use uva_session::{
    CompactionError,
    CompactionOutcome,
    CompactionPass,
    CompactionPlan,
    Compactor,
    FoldError,
    LogWriter,
    PlannedCompaction,
    ProvenancedHistory,
    SessionEvent,
    SummaryInput,
    SurfaceLedger,
    SurfaceOp,
    TokenBudget,
    TokenMeter,
    TokenMeterError,
    begin_compaction,
    finish_compaction,
    fold_log_surface,
    in_memory::InMemorySessionSink,
    plan_compaction,
    try_fold_log_surface,
};

fn row(tag: &str, seq: u64, data: Message) -> Value {
    json!({"type": tag, "seq": seq, "data": data})
}

fn user_row(seq: u64, text: &str) -> Value {
    row("user_message", seq, Message::user(text))
}

fn assistant_row(seq: u64, text: &str) -> Value {
    row("assistant_message", seq, Message::assistant(text))
}

fn assistant_tool_row(seq: u64, call_id: &str) -> Value {
    let parts = vec![ContentPart::ToolUse(ToolUse {
        id: call_id.into(),
        name: "read".into(),
        input: json!({"path": "a.rs"}),
    })];
    row(
        "assistant_message",
        seq,
        Message::new(uva_provider::Role::Assistant, Content::parts(parts)),
    )
}

fn tool_row(seq: u64, call_id: &str) -> Value {
    row(
        "tool_result",
        seq,
        Message::tool(Content::text("ok"), call_id),
    )
}

/// A compaction row whose span and citation are given by hand: the
/// point of the test is the pair, not the summary.
fn compaction_row(seq: u64, start: usize, end: usize, cited: &[u64]) -> Value {
    json!({
        "type": "compaction",
        "seq": seq,
        "data": {"summary": "summary"},
        "surface_op": {
            "start": start,
            "end": end,
            "source_event_seqs": cited,
        },
    })
}

/// The typed twin of [`user_row`] — the meter and the ledger's
/// engine read typed events, so the tests that pin them use these.
fn user_event(seq: u64, text: &str) -> SessionEvent {
    SessionEvent::UserMessage {
        seq,
        ts: "2026-11-14T00:00:00Z".into(),
        data: serde_json::to_value(Message::user(text)).expect("ser"),
        source: None,
        surface_op: None,
        attachments: Vec::new(),
    }
}

/// The typed twin of [`assistant_row`].
fn assistant_event(seq: u64, text: &str) -> SessionEvent {
    SessionEvent::AssistantMessage {
        seq,
        ts: "2026-11-14T00:00:01Z".into(),
        data: serde_json::to_value(Message::assistant(text)).expect("ser"),
        surface_op: None,
    }
}

/// The typed twin of [`tool_row`].
fn tool_event(seq: u64, call_id: &str) -> SessionEvent {
    SessionEvent::ToolResult {
        seq,
        ts: "2026-11-14T00:00:02Z".into(),
        data: serde_json::to_value(Message::tool(Content::text("ok"), call_id))
            .expect("ser"),
        is_error: false,
        tool_call_id: Some(call_id.into()),
        changed: None,
        checkpoint: None,
        surface_op: None,
    }
}

/// The typed twin of [`compaction_row`].
fn compaction_event(
    seq: u64,
    start: usize,
    end: usize,
    cited: Vec<u64>,
) -> SessionEvent {
    SessionEvent::Compaction {
        seq,
        ts: "2026-11-14T00:00:03Z".into(),
        data: json!({"summary": "summary"}),
        surface_op: SurfaceOp::Replace {
            start,
            end,
            source_event_seqs: cited,
        },
    }
}

/// The strict fold refuses a replace whose citation does not name the
/// rows its span covers.
///
/// The rows are `user(1), assistant(2), tool(3)`; the replace covers
/// `[0, 2)` but cites `[2, 3]` — the right rows, one position off.
/// Every other check passes: the range fits, and both cited seqs are
/// earlier than the replace's own. Only the tie between the span and
/// the citation catches it.
#[test]
fn a_replace_whose_citation_does_not_name_its_span_is_refused() {
    let rows = vec![
        user_row(1, "start"),
        assistant_row(2, "hi"),
        tool_row(3, "call-1"),
        compaction_row(4, 0, 2, &[2, 3]),
    ];

    let error = try_fold_log_surface(&rows)
        .expect_err("the citation does not name the span");
    assert_eq!(
        error,
        FoldError::SpanMismatch {
            seq: 4,
            offset: 0,
            row: 0,
            cited: 2,
            expected: 1,
        }
    );
}

/// The token meter refuses the same replace the fold refuses.
///
/// The meter prices the surface through the same engine as the fold,
/// so a citation that does not name its span must stop the meter
/// too — otherwise the meter would price a surface the durable fold
/// would refuse, and the two would disagree about what the log
/// says.
///
/// The meter is catch-up: the refused event stays unconsumed, so a
/// retry re-sees it.
#[test]
fn the_token_meter_refuses_a_replace_whose_citation_does_not_name_its_span() {
    let log = vec![
        user_event(1, "start"),
        assistant_event(2, "hi"),
        tool_event(3, "call-1"),
        compaction_event(4, 0, 2, vec![2, 3]),
    ];

    let mut meter = TokenMeter::new();
    let error = meter
        .observe(&log)
        .expect_err("the citation does not name the span");
    assert_eq!(
        error,
        TokenMeterError::Fold(FoldError::SpanMismatch {
            seq: 4,
            offset: 0,
            row: 0,
            cited: 2,
            expected: 1,
        })
    );
    assert_eq!(
        meter.measure().log_revision,
        3,
        "the refused event stays unconsumed, so a retry re-sees it"
    );
}

/// The ledger ignores the same replace the fold refuses.
///
/// The ledger is the incremental twin of the durable fold: a
/// compaction it cannot resolve must leave its rows as they were
/// rather than splice the wrong span. The rows the ledger holds are
/// the rows a later compaction's `Replace` cites, so a wrong splice
/// here would shadow rows nobody named.
#[test]
fn the_ledger_ignores_a_compaction_whose_citation_does_not_name_its_span() {
    let ledger = SurfaceLedger::new();
    ledger.record(1, &user_row(1, "start"));
    ledger.record(2, &assistant_row(2, "hi"));
    ledger.record(3, &tool_row(3, "call-1"));
    assert_eq!(ledger.len(), 3);

    ledger.record(4, &compaction_row(4, 0, 2, &[2, 3]));
    assert_eq!(
        ledger.len(),
        3,
        "the refused replace leaves the ledger's rows alone"
    );

    // The ledger is not ignoring every compaction: one that names
    // the rows it shadows does change the ledger.
    ledger.record(5, &compaction_row(5, 1, 3, &[2, 3]));
    assert_eq!(
        ledger.len(),
        2,
        "the accepted replace shadows two rows with one"
    );
}

/// The lenient fold skips the same replace and replays the span it
/// shadowed: the caller still gets a usable surface, and the rows
/// the replace would have shadowed survive.
#[test]
fn the_lenient_fold_replays_the_span_a_refused_replace_shadowed() {
    let rows = vec![
        user_row(1, "start"),
        assistant_row(2, "hi"),
        tool_row(3, "call-1"),
        compaction_row(4, 0, 2, &[2, 3]),
    ];

    let surface = fold_log_surface(&rows);
    assert_eq!(
        surface.messages.len(),
        3,
        "the shadowed span is replayed, not replaced"
    );
    assert_eq!(surface.surface_seqs, vec![1, 2, 3]);
}

/// A replace that cites a different number of seqs than its span
/// covers cannot be tied to the rows it names either.
#[test]
fn a_replace_whose_citation_is_the_wrong_width_is_refused() {
    let rows = vec![
        user_row(1, "start"),
        assistant_row(2, "hi"),
        compaction_row(3, 0, 2, &[1]),
    ];

    let error = try_fold_log_surface(&rows)
        .expect_err("the citation does not cover the span");
    assert_eq!(
        error,
        FoldError::CitationCount {
            seq: 3,
            width: 2,
            cited: 1,
        }
    );
}

/// A replace that names the rows it shadows is accepted: the tie is
/// a check, not a new restriction.
#[test]
fn a_replace_that_names_its_span_is_accepted() {
    let rows = vec![
        user_row(1, "start"),
        assistant_row(2, "hi"),
        tool_row(3, "call-1"),
        compaction_row(4, 1, 3, &[2, 3]),
    ];

    let surface =
        try_fold_log_surface(&rows).expect("the citation names the span");
    assert_eq!(surface.messages.len(), 2);
    assert_eq!(surface.surface_seqs, vec![1, 4]);
}

/// The tool-pairing view of the same log: an assistant turn that
/// asks for a tool, and the result that answers it.
#[test]
fn a_steered_log_folds_to_the_rows_it_names() {
    let rows = vec![
        user_row(1, "start"),
        assistant_tool_row(2, "call-1"),
        tool_row(3, "call-1"),
    ];

    let surface = try_fold_log_surface(&rows).expect("the log folds");
    assert_eq!(surface.surface_seqs, vec![1, 2, 3]);
    assert_eq!(surface.messages.len(), 3);
}

// --- the steered scenario -------------------------------------------

/// A compactor that compacts exactly the span it is told to.
struct FixedSpan {
    range: std::ops::Range<usize>,
}

impl Compactor for FixedSpan {
    fn plan(
        &self,
        _history: &[Message],
        _budget: TokenBudget,
    ) -> Option<CompactionPlan> {
        Some(CompactionPlan {
            range: self.range.clone(),
            shadowed_seqs: Vec::new(),
            keep_recent: 0,
            shadowed: Vec::new(),
            shadowed_tokens: 0,
        })
    }

    fn summarize(
        &self,
        _plan: &CompactionPlan,
        _input: &SummaryInput,
        _cancel: Arc<dyn CancelToken>,
    ) -> BoxFuture<'static, Result<CompactionOutcome, CompactionError>> {
        Box::pin(async {
            Ok(CompactionOutcome {
                summary: "summary".to_string(),
                shadowed_seqs: Vec::new(),
                shadowed_tokens: 0,
                provider: None,
                model: None,
                usage: None,
            })
        })
    }
}

/// A steered log: the steer lands while the tool batch is in flight,
/// so its row sits between the assistant's `tool_use` and the result
/// that answers it.
fn steered_events() -> Vec<SessionEvent> {
    let parts = vec![ContentPart::ToolUse(ToolUse {
        id: "call-1".into(),
        name: "read".into(),
        input: json!({"path": "a.rs"}),
    })];
    vec![
        SessionEvent::UserMessage {
            seq: 1,
            ts: "2026-11-14T00:00:00Z".into(),
            data: serde_json::to_value(Message::user("start")).expect("ser"),
            source: None,
            surface_op: None,
            attachments: Vec::new(),
        },
        SessionEvent::AssistantMessage {
            seq: 2,
            ts: "2026-11-14T00:00:01Z".into(),
            data: serde_json::to_value(Message::new(
                Role::Assistant,
                Content::parts(parts),
            ))
            .expect("ser"),
            surface_op: None,
        },
        SessionEvent::UserMessage {
            seq: 3,
            ts: "2026-11-14T00:00:02Z".into(),
            data: serde_json::to_value(Message::user(
                "steer: focus on the tests",
            ))
            .expect("ser"),
            source: None,
            surface_op: None,
            attachments: Vec::new(),
        },
        SessionEvent::ToolResult {
            seq: 4,
            changed: None,
            checkpoint: None,
            ts: "2026-11-14T00:00:03Z".into(),
            data: serde_json::to_value(Message::tool(
                Content::text("ok"),
                "call-1",
            ))
            .expect("ser"),
            is_error: false,
            tool_call_id: Some("call-1".into()),
            surface_op: None,
        },
    ]
}

/// The steer waits for the tool result that answers the open call:
/// the fold's order is the order the next LLM pass sees.
#[test]
fn a_steer_waits_for_the_tool_result_it_landed_between() {
    let rows: Vec<Value> = steered_events()
        .into_iter()
        .map(|event| serde_json::to_value(event).expect("ser"))
        .collect();

    let surface = try_fold_log_surface(&rows).expect("the log folds");
    assert_eq!(
        surface.surface_seqs,
        vec![1, 2, 4, 3],
        "the steer waits for the result that answers the call"
    );
}

/// The compaction a resumed steered run writes: it shadows the
/// assistant's `tool_use` and the result that answers it, and the
/// steer stays.
///
/// The shadowed rows are the model's rows 2 and 4, which sit at
/// positions 1 and 2 of the model-visible surface — the surface the
/// plan was made over and the surface the fold reproduces. The steer
/// is row 3, at position 3, and it survives: a compaction that
/// shadowed it would leave the `tool_result` without the `tool_use`
/// that answers it.
#[tokio::test]
async fn a_compaction_over_a_steered_log_shadows_exactly_its_rows() {
    let sink = Arc::new(InMemorySessionSink::new("s1"));
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));
    for event in steered_events() {
        writer.append_event(&event).await.expect("append");
    }
    writer.flush().await.expect("flush");

    let rows = writer.read().await.expect("read");
    let history =
        ProvenancedHistory::from_log(&rows).expect("the log projects");
    assert_eq!(
        history.ordinals(),
        &[1, 2, 4, 3],
        "the plan is made over the model-visible order"
    );

    // The plan shadows the tool_use and its result — the model's rows
    // 2 and 4.
    let compactor = FixedSpan { range: 1..3 };
    let plan = match plan_compaction(
        history.messages(),
        history.ordinals(),
        &compactor,
        TokenBudget::for_window(1_000),
    )
    .expect("the plan binds")
    {
        PlannedCompaction::Plan(plan) => plan,
        _ => panic!("the span is not empty"),
    };
    assert_eq!(plan.range, 1..3);
    assert_eq!(
        plan.shadowed_seqs,
        vec![2, 4],
        "the plan cites the rows it shadows"
    );

    let token = begin_compaction(&writer, &plan, "context_pressure")
        .await
        .expect("the audit row is written");
    let outcome = CompactionOutcome {
        summary: "summary".to_string(),
        shadowed_seqs: plan.shadowed_seqs.clone(),
        shadowed_tokens: plan.shadowed_tokens,
        provider: None,
        model: None,
        usage: None,
    };
    let seq = finish_compaction(
        &writer,
        &token,
        "context_pressure",
        &plan,
        CompactionPass::Committed(&outcome),
    )
    .await
    .expect("the replace is written");
    assert!(seq.is_some(), "the pass committed");

    let rows = writer.read().await.expect("read");
    let surface = try_fold_log_surface(&rows).expect("the log folds");
    assert_eq!(
        surface.surface_seqs,
        vec![1, 7, 3],
        "the tool_use and its result are shadowed; the steer survives"
    );

    // The resumed view and the fold agree: the transcript the next
    // run reads is the surface the fold produced.
    let resumed =
        ProvenancedHistory::from_log(&rows).expect("the log projects");
    assert_eq!(resumed.ordinals(), &[1, 7, 3]);
    assert_eq!(resumed.messages().len(), 3);

    // No orphaned tool result: the row that answers the tool_use was
    // shadowed with it.
    let tool_results = resumed
        .messages()
        .iter()
        .filter(|message| message.role == Role::Tool)
        .count();
    assert_eq!(
        tool_results, 0,
        "the shadowed tool_use takes its result with it"
    );
    let steers = resumed
        .messages()
        .iter()
        .filter(|message| {
            message.role == Role::User
                && message
                    .content
                    .extract_text()
                    .is_some_and(|text| text.contains("steer"))
        })
        .count();
    assert_eq!(steers, 1, "the steer survives the compaction");
}

/// The migration: a compaction row written in the old coordinates.
///
/// Before the fold learned the model-visible order, the fold read a
/// range as a position in the **log** order while the harness wrote
/// it as a position in the **model** order. On a steered log the two
/// disagree, and the row names rows it does not shadow.
///
/// The migration is fail-closed: such a row is refused. The strict
/// fold says so; the lenient fold skips it and replays the span it
/// shadowed, so the session sees more history rather than a history
/// the log does not support.
#[test]
fn a_compaction_written_in_the_old_coordinates_is_refused() {
    let mut rows: Vec<Value> = steered_events()
        .into_iter()
        .map(|event| serde_json::to_value(event).expect("ser"))
        .collect();
    // The old coordinates: the log order's positions 1..3 name the
    // assistant's `tool_use` (row 2) and the steer (row 3).
    rows.push(compaction_row(5, 1, 3, &[2, 3]));

    let error = try_fold_log_surface(&rows)
        .expect_err("the old coordinates do not name the fold's rows");
    assert_eq!(
        error,
        FoldError::SpanMismatch {
            seq: 5,
            offset: 1,
            row: 2,
            cited: 3,
            expected: 4,
        }
    );
}

/// The lenient fold's half of the migration: the compaction is
/// skipped and the span it shadowed is replayed, so the session keeps
/// the history the log actually supports.
#[test]
fn the_lenient_fold_replays_the_span_an_old_coordinate_compaction_shadowed() {
    let mut rows: Vec<Value> = steered_events()
        .into_iter()
        .map(|event| serde_json::to_value(event).expect("ser"))
        .collect();
    rows.push(compaction_row(5, 1, 3, &[2, 3]));

    let surface = fold_log_surface(&rows);
    assert_eq!(
        surface.surface_seqs,
        vec![1, 2, 4, 3],
        "the compaction is skipped; the model sees the whole history"
    );
}
