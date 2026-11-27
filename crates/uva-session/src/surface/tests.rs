/// `FoldError` satisfies the std error conventions: it
/// renders a message naming the offending event, and it can be
/// erased into a boxed error (so a consumer can `?` it).
#[test]
fn fold_error_is_a_std_error_with_a_useful_message() {
    let err = FoldError::BadProvenance { seq: 7, bad_seq: 9 };
    let msg = err.to_string();
    assert!(msg.contains('7') && msg.contains('9'), "got {msg:?}");

    let bad_range = FoldError::RangeOutOfBounds {
        seq: 3,
        start: 0,
        end: 99,
        surface_len: 4,
    };
    assert!(bad_range.to_string().contains("99"), "got {bad_range:?}");

    let boxed: Box<dyn std::error::Error + Send + Sync> = Box::new(err);
    assert!(!boxed.to_string().is_empty());
}

use serde_json::json;

use super::*;
use crate::events::SurfaceOp;

fn ev(tag: &str, seq: u64, data: Value, op: Option<SurfaceOp>) -> SessionEvent {
    match tag {
        "user_message" => SessionEvent::UserMessage {
            seq,
            ts: "2026-09-10T00:00:00Z".into(),
            data,
            source: None,
            surface_op: op,
            attachments: Vec::new(),
        },
        "assistant_message" => SessionEvent::AssistantMessage {
            seq,
            ts: "2026-09-10T00:00:00Z".into(),
            data,
            surface_op: op,
        },
        "tool_result" => SessionEvent::ToolResult {
            seq,
            ts: "2026-09-10T00:00:00Z".into(),
            data,
            is_error: false,
            tool_call_id: None,
            changed: None,
            checkpoint: None,
            surface_op: op,
        },
        "compaction" => SessionEvent::Compaction {
            seq,
            ts: "2026-09-10T00:00:00Z".into(),
            data,
            surface_op: op.expect("compaction requires surface_op"),
        },
        other => panic!("unexpected tag {other}"),
    }
}

#[test]
fn append_only_log_folds_to_messages_in_order() {
    let events = vec![
        ev(
            "user_message",
            1,
            json!({"role": "user", "text": "hi"}),
            Some(SurfaceOp::append()),
        ),
        ev(
            "assistant_message",
            2,
            json!({"role": "assistant", "text": "hello"}),
            Some(SurfaceOp::append()),
        ),
        ev(
            "tool_result",
            3,
            json!({"role": "tool", "content": "ok"}),
            Some(SurfaceOp::append()),
        ),
    ];
    let s = fold_surface(&events).expect("fold");
    assert_eq!(s.messages.len(), 3);
    assert_eq!(s.surface_seqs, vec![1, 2, 3]);
    assert_eq!(s.messages[0]["text"], "hi");
    assert_eq!(s.messages[1]["text"], "hello");
    assert_eq!(s.messages[2]["content"], "ok");
}

#[test]
fn log_only_events_are_skipped() {
    let events = vec![
        ev(
            "user_message",
            1,
            json!({"role": "user", "text": "hi"}),
            Some(SurfaceOp::append()),
        ),
        SessionEvent::ToolCall {
            seq: 2,
            ts: "2026-09-10T00:00:00Z".into(),
            data: json!({"name": "read"}),
            call_id: None,
            name: Some("read".to_string()),
        },
        ev(
            "assistant_message",
            3,
            json!({"role": "assistant", "text": "ok"}),
            Some(SurfaceOp::append()),
        ),
    ];
    let s = fold_surface(&events).expect("fold");
    assert_eq!(s.messages.len(), 2);
    assert_eq!(s.surface_seqs, vec![1, 3]);
}

#[test]
fn missing_surface_op_treated_as_append_for_forward_compat() {
    let events = vec![ev("user_message", 1, json!({"text": "hi"}), None)];
    let s = fold_surface(&events).expect("fold");
    assert_eq!(s.messages.len(), 1);
    assert_eq!(s.surface_seqs, vec![1]);
}

#[test]
fn replace_swaps_surface_range_and_preserves_seqs() {
    let events = vec![
        ev(
            "user_message",
            1,
            json!({"text": "u1"}),
            Some(SurfaceOp::append()),
        ),
        ev(
            "assistant_message",
            2,
            json!({"text": "a1"}),
            Some(SurfaceOp::append()),
        ),
        ev(
            "tool_result",
            3,
            json!({"text": "tr1"}),
            Some(SurfaceOp::append()),
        ),
        // Compaction replaces surface[1..3] with a summary,
        // citing the seqs being shadowed.
        ev(
            "compaction",
            4,
            json!({"text": "summary"}),
            Some(SurfaceOp::Replace {
                start: 1,
                end: 3,
                source_event_seqs: vec![2, 3],
            }),
        ),
    ];
    let s = fold_surface(&events).expect("fold");
    assert_eq!(s.messages.len(), 2);
    assert_eq!(s.messages[0]["text"], "u1");
    assert_eq!(s.messages[1]["text"], "summary");
    assert_eq!(s.surface_seqs, vec![1, 4]);
}

#[test]
fn replace_rejects_out_of_bounds_range() {
    let events = vec![
        ev(
            "user_message",
            1,
            json!({"text": "u1"}),
            Some(SurfaceOp::append()),
        ),
        ev(
            "assistant_message",
            2,
            json!({"text": "summary"}),
            Some(SurfaceOp::Replace {
                start: 0,
                end: 5,
                source_event_seqs: vec![1],
            }),
        ),
    ];
    let err = fold_surface(&events).expect_err("must reject");
    assert_eq!(
        err,
        FoldError::RangeOutOfBounds {
            seq: 2,
            start: 0,
            end: 5,
            surface_len: 1,
        }
    );
}

#[test]
fn replace_rejects_provenance_violation_citing_future_seq() {
    let events = vec![
        ev(
            "user_message",
            1,
            json!({"text": "u1"}),
            Some(SurfaceOp::append()),
        ),
        // Bad: source_event_seqs contains 2, which is this
        // event's own seq.
        ev(
            "assistant_message",
            2,
            json!({"text": "summary"}),
            Some(SurfaceOp::Replace {
                start: 0,
                end: 1,
                source_event_seqs: vec![2],
            }),
        ),
    ];
    let err = fold_surface(&events).expect_err("must reject");
    assert_eq!(err, FoldError::BadProvenance { seq: 2, bad_seq: 2 });
}

#[test]
fn replace_rejects_provenance_violation_citing_unseen_seq() {
    let events = vec![
        ev(
            "user_message",
            1,
            json!({"text": "u1"}),
            Some(SurfaceOp::append()),
        ),
        // Bad: source_event_seqs contains 5, which has not
        // been appended yet (current max surface seq = 1).
        ev(
            "assistant_message",
            2,
            json!({"text": "summary"}),
            Some(SurfaceOp::Replace {
                start: 0,
                end: 1,
                source_event_seqs: vec![5],
            }),
        ),
    ];
    let err = fold_surface(&events).expect_err("must reject");
    assert_eq!(err, FoldError::BadProvenance { seq: 2, bad_seq: 5 });
}

#[test]
fn replace_at_tail_with_zero_length_is_valid_insert() {
    let events = vec![
        ev(
            "user_message",
            1,
            json!({"text": "u1"}),
            Some(SurfaceOp::append()),
        ),
        // Insert at end (start == end == len): zero-length
        // replacement, equivalent to append. The span covers no
        // row, so it cites none.
        ev(
            "tool_result",
            2,
            json!({"text": "tr1"}),
            Some(SurfaceOp::Replace {
                start: 1,
                end: 1,
                source_event_seqs: vec![],
            }),
        ),
    ];
    let s = fold_surface(&events).expect("fold");
    assert_eq!(s.messages.len(), 2);
    assert_eq!(s.messages[1]["text"], "tr1");
    assert_eq!(s.surface_seqs, vec![1, 2]);
}

#[test]
fn empty_log_produces_empty_surface() {
    let s = fold_surface(&[]).expect("fold");
    assert!(s.is_empty());
    assert_eq!(s.len(), 0);
}

#[test]
fn log_only_only_produces_empty_surface() {
    let events = vec![SessionEvent::ToolCall {
        seq: 1,
        ts: "2026-09-10T00:00:00Z".into(),
        data: json!({"name": "read"}),
        call_id: None,
        name: Some("read".to_string()),
    }];
    let s = fold_surface(&events).expect("fold");
    assert!(s.is_empty());
}

#[test]
fn replace_at_start_replaces_first_message() {
    let events = vec![
        ev(
            "user_message",
            1,
            json!({"text": "u1"}),
            Some(SurfaceOp::append()),
        ),
        ev(
            "assistant_message",
            2,
            json!({"text": "a1"}),
            Some(SurfaceOp::append()),
        ),
        // Rewrite user_message with a new one citing seq 1.
        ev(
            "user_message",
            3,
            json!({"text": "u1-edited"}),
            Some(SurfaceOp::Replace {
                start: 0,
                end: 1,
                source_event_seqs: vec![1],
            }),
        ),
    ];
    let s = fold_surface(&events).expect("fold");
    assert_eq!(s.messages.len(), 2);
    assert_eq!(s.messages[0]["text"], "u1-edited");
    assert_eq!(s.surface_seqs, vec![3, 2]);
}

fn chunk(chunk_seq: u64, data: Value) -> SessionEvent {
    SessionEvent::AssistantChunk {
        seq: 100 + chunk_seq,
        ts: "2026-09-10T00:00:00Z".into(),
        chunk_seq,
        finish_reason: None,
        data,
    }
}

#[test]
fn assemble_chunks_joins_deltas_in_chunk_seq_order() {
    // Handed over out of order; `chunk_seq` decides.
    let events = vec![
        chunk(3, json!({"delta": "d"})),
        chunk(0, json!({"delta": "a"})),
        chunk(4, json!({"delta": "e"})),
        chunk(1, json!({"delta": "b"})),
        chunk(2, json!({"delta": "c"})),
    ];
    assert_eq!(assemble_chunks(&events), "abcde");
}

#[test]
fn assemble_chunks_skips_non_text_chunks_and_other_events() {
    assert_eq!(assemble_chunks(&[]), "");
    let events = vec![
        ev(
            "assistant_message",
            1,
            json!({"text": "ignored"}),
            Some(SurfaceOp::append()),
        ),
        chunk(0, json!({"delta": "hi"})),
        chunk(1, json!({"call_id": "c1", "name": "read"})),
        chunk(2, json!({"delta": 7})),
        chunk(3, json!({"delta": " there"})),
    ];
    assert_eq!(assemble_chunks(&events), "hi there");
}
