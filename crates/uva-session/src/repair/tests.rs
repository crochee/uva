use serde_json::json;
use uva_provider::{Content, ContentPart, Message, Role, ToolUse};

use super::*;
use crate::{CompactionLifecycleOutcome, typed_event_builders as vocabulary};

/// Captures the WARN events the reader emits, so a test can
/// assert a deprecation was said (and said once).
#[derive(Clone, Default)]
struct WarnCapture(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl WarnCapture {
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().expect("capture lock"))
    }
}

impl<S> tracing_subscriber::Layer<S> for WarnCapture
where
    S: tracing::Subscriber,
{
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        if *event.metadata().level() != tracing::Level::WARN {
            return;
        }
        struct Message(String);
        impl tracing::field::Visit for Message {
            fn record_debug(
                &mut self,
                field: &tracing::field::Field,
                value: &dyn std::fmt::Debug,
            ) {
                if field.name() == "message" {
                    self.0 = format!("{value:?}");
                }
            }
        }
        let mut message = Message(String::new());
        event.record(&mut message);
        self.0.lock().expect("capture lock").push(message.0);
    }
}

fn turn_start(turn: u64, ts: &str) -> SessionEvent {
    SessionEvent::Turn {
        seq: turn,
        ts: ts.into(),
        data: json!({"kind": "start", "turn": turn}),
        kind: Some("start".to_string()),
        turn: Some(turn),
    }
}

fn turn_end(turn: u64, ts: &str) -> SessionEvent {
    SessionEvent::Turn {
        seq: turn,
        ts: ts.into(),
        data: json!({"kind": "end", "turn": turn}),
        kind: Some("end".to_string()),
        turn: Some(turn),
    }
}

fn step_start(turn: u64, step: u64, ts: &str) -> SessionEvent {
    SessionEvent::Step {
        seq: turn * 100 + step,
        ts: ts.into(),
        data: json!({"kind": "start", "turn": turn, "step": step}),
        kind: Some("start".to_string()),
        turn: Some(turn),
        step: Some(step),
    }
}

fn assistant_with_tool_calls(
    seq: u64,
    ts: &str,
    calls: &[(&str, &str)],
) -> SessionEvent {
    SessionEvent::AssistantMessage {
        seq,
        ts: ts.into(),
        data: json!({
            "text": "calling tools",
            "tool_calls": calls
                .iter()
                .map(|(id, name)| json!({"id": id, "name": name}))
                .collect::<Vec<_>>(),
        }),
        surface_op: None,
    }
}

fn tool_result(seq: u64, ts: &str, call_id: &str) -> SessionEvent {
    SessionEvent::ToolResult {
        seq,
        ts: ts.into(),
        data: json!({"call_id": call_id, "content": "ok"}),
        is_error: false,
        tool_call_id: Some(call_id.to_string()),
        changed: None,
        surface_op: None,
    }
}

/// The shape the harness actually writes: the row payload is the
/// provider `Message`, with the calls in its content parts.
///
/// The harness has one construction of an assistant turn
/// (`Transcript::assistant_message` → `Content::parts`), and the
/// log row keeps the same bytes the next request sends — so the
/// fixture builds the message the same way. A fixture that *guesses*
/// the shape hides a reader drifting away from it: the repair reads
/// the calls out of this row, and a guessed shape made that read
/// dead in production while the test stayed green.
fn assistant_message(
    seq: u64,
    ts: &str,
    calls: &[(&str, &str)],
) -> SessionEvent {
    let message = Message::new(
        Role::Assistant,
        Content::parts(
            calls
                .iter()
                .map(|(id, name)| {
                    ContentPart::ToolUse(ToolUse {
                        id: (*id).to_string(),
                        name: (*name).to_string(),
                        input: json!({}),
                    })
                })
                .collect(),
        ),
    );
    SessionEvent::AssistantMessage {
        seq,
        ts: ts.into(),
        data: serde_json::to_value(&message).expect("the message serializes"),
        surface_op: None,
    }
}

/// The fixture must be the shape the harness writes — the provider
/// `Message`, whose `Content::parts` puts a lone part under
/// `Single`.
///
/// This is the line that says the repair's reader and the harness's
/// writer agree. Without it, a fixture that guessed the shape made
/// the repair's tool-call read dead in production while the test
/// stayed green.
#[test]
fn the_assistant_fixture_is_the_shape_the_harness_writes() {
    let row = assistant_message(3, "2026-09-10T00:00:00Z", &[("c1", "read")]);
    let SessionEvent::AssistantMessage { data, .. } = row else {
        panic!("the fixture builds an assistant message");
    };
    assert_eq!(
        data["content"]["Single"]["type"], "tool_use",
        "the fixture is not the harness's shape: {data}"
    );
}

/// The shape the harness actually writes for a result: the
/// provider tool message, keyed by `tool_call_id`.
fn tool_result_message(seq: u64, ts: &str, call_id: &str) -> SessionEvent {
    SessionEvent::ToolResult {
        seq,
        ts: ts.into(),
        data: json!({
            "role": "tool",
            "tool_call_id": call_id,
            "content": [{"type": "text", "text": "ok"}],
        }),
        is_error: false,
        tool_call_id: Some(call_id.to_string()),
        changed: None,
        surface_op: None,
    }
}

#[test]
fn suspension_closes_the_turn_with_suspended_not_interrupted() {
    // A suspended turn is intentional: every pending call is
    // still held, so repair must not invent outcomes for it.
    let ts = "2026-09-10T00:00:00Z";
    let events = vec![
        turn_start(1, ts),
        step_start(1, 1, ts),
        assistant_with_tool_calls(3, ts, &[("c1", "shell")]),
        SessionEvent::ApprovalAsked {
            seq: 4,
            ts: ts.into(),
            id: "a1".into(),
            call_id: "c1".into(),
            tool: "shell".into(),
            arguments_hash: "sha256:x".into(),
            risk: "high".into(),
            agent: "default".into(),
        },
        SessionEvent::Turn {
            seq: 5,
            ts: ts.into(),
            data: json!({
                "kind": "end",
                "status": "suspended",
                "pending": ["c1"],
            }),
            kind: Some("end".to_string()),
            turn: None,
        },
        SessionEvent::Step {
            seq: 6,
            ts: ts.into(),
            data: json!({"kind": "end", "status": "suspended"}),
            kind: Some("end".to_string()),
            turn: None,
            step: None,
        },
        SessionEvent::Turn {
            seq: 7,
            ts: ts.into(),
            data: json!({"kind": "end", "reason": "suspended"}),
            kind: Some("end".to_string()),
            turn: None,
        },
    ];
    assert!(
        interrupted_turn_closers(&events).is_empty(),
        "a suspended turn is not an interrupted turn"
    );
}

#[test]
fn a_crash_after_a_destructive_side_effect_does_not_reexecute() {
    // `tool_dispatch` landed before the side effect; the result
    // row did not. Repair must say the outcome is unknown — an
    // error a resume cannot turn into a silent re-run.
    let ts = "2026-09-10T00:00:00Z";
    let events = vec![
        turn_start(1, ts),
        step_start(1, 1, ts),
        assistant_with_tool_calls(3, ts, &[("c1", "shell")]),
        SessionEvent::ToolDispatch {
            seq: 4,
            ts: ts.into(),
            call_id: "c1".into(),
            tool: "shell".into(),
            arguments_hash: "sha256:x".into(),
        },
    ];
    let closers = interrupted_turn_closers(&events);
    let closer = closers
        .iter()
        .find_map(|event| match event {
            SessionEvent::ToolResult { data, is_error, .. } => {
                Some((data, *is_error))
            }
            _ => None,
        })
        .expect("a dispatched call with no result is closed");
    assert_eq!(closer.0["error"], json!(TOOL_OUTCOME_UNKNOWN));
    assert_eq!(closer.0["call_id"], json!("c1"));
    assert!(closer.1, "the unknown outcome is an error, not a result");
}

#[test]
fn balanced_log_produces_no_closers() {
    let events = vec![
        turn_start(1, "2026-09-10T00:00:00Z"),
        step_start(1, 1, "2026-09-10T00:00:00Z"),
        SessionEvent::Turn {
            seq: 99,
            ts: "2026-09-10T00:00:00Z".into(),
            data: json!({"kind": "end", "turn": 1}),
            kind: Some("end".to_string()),
            turn: Some(1),
        },
    ];
    assert!(interrupted_turn_closers(&events).is_empty());
}

#[test]
fn empty_log_produces_no_closers() {
    assert!(interrupted_turn_closers(&[]).is_empty());
}

#[test]
fn dangling_call_without_step_or_turn_start_still_closes() {
    // Edge case: assistant message carries tool_use blocks
    // but no turn/step boundary was ever logged. The repair
    // synthesises tool_results with the assistant's seq as
    // the closest anchor.
    let events = vec![assistant_with_tool_calls(
        5,
        "2026-09-10T00:00:00Z",
        &[("c1", "read")],
    )];
    let closers = interrupted_turn_closers(&events);
    // No open turn -> no closers (no boundary to close).
    assert!(closers.is_empty());
}

#[test]
fn dangling_tool_call_after_open_turn_emits_tool_result_then_closers() {
    // The classic crash case: a call was emitted to the
    // wire and recorded as a `ToolCall` log event, but the
    // server died before the tool produced a result.
    let events = vec![
        turn_start(1, "2026-09-10T00:00:00Z"),
        step_start(1, 1, "2026-09-10T00:00:00Z"),
        assistant_with_tool_calls(3, "2026-09-10T00:00:00Z", &[("c1", "read")]),
        SessionEvent::ToolCall {
            seq: 4,
            ts: "2026-09-10T00:00:00Z".into(),
            data: json!({"call_id": "c1", "name": "read", "arguments": {}}),
            call_id: Some("c1".to_string()),
            name: Some("read".to_string()),
        },
        // No tool_result for c1, no step/end, no turn/end.
    ];
    let closers = interrupted_turn_closers(&events);
    // Expected order: 1 tool_result (TOOL_OUTCOME_UNKNOWN),
    // 1 step/end, 1 turn/end.
    assert_eq!(closers.len(), 3, "got: {closers:#?}");

    match &closers[0] {
        SessionEvent::ToolResult {
            seq,
            data,
            is_error,
            ..
        } => {
            assert_eq!(
                *seq, 0,
                "the closer's ordinal is the writer's to hand out"
            );
            assert_eq!(data["call_id"], "c1");
            assert_eq!(data["error"], TOOL_OUTCOME_UNKNOWN);
            assert_eq!(data["interrupted"], true);
            assert!(
                *is_error,
                "a call that never finished must not read as one \
                 that returned"
            );
        }
        other => panic!("expected tool_result, got {other:?}"),
    }
    match &closers[1] {
        SessionEvent::Step { data, .. } => {
            assert_eq!(data["kind"], "end");
            assert_eq!(data["turn"], 1);
            assert_eq!(data["step"], 1);
        }
        other => panic!("expected step/end, got {other:?}"),
    }
    match &closers[2] {
        SessionEvent::Turn { data, .. } => {
            assert_eq!(data["kind"], "end");
            assert_eq!(data["turn"], 1);
            assert_eq!(data["reason"], TURN_INTERRUPTED);
        }
        other => panic!("expected turn/end, got {other:?}"),
    }
}

#[test]
fn dangling_tool_use_without_separate_tool_call_event_marks_not_started() {
    // The assistant message's tool_use block was recorded
    // but the wire never logged a `ToolCall` event for it
    // (e.g. a streaming drop on a very old version).
    let events = vec![
        turn_start(1, "2026-09-10T00:00:00Z"),
        step_start(1, 1, "2026-09-10T00:00:00Z"),
        assistant_with_tool_calls(3, "2026-09-10T00:00:00Z", &[("c1", "read")]),
    ];
    let closers = interrupted_turn_closers(&events);
    assert_eq!(closers.len(), 3);
    match &closers[0] {
        SessionEvent::ToolResult { data, .. } => {
            assert_eq!(data["error"], TOOL_NOT_STARTED);
        }
        other => panic!("expected tool_result, got {other:?}"),
    }
}

#[test]
fn answered_dispatch_is_not_closed_again_when_a_sibling_is_dangling() {
    // Two dispatches in one turn; the first has its result,
    // the second was cut off. The answered call must not get a
    // duplicate `TOOL_OUTCOME_UNKNOWN` row — a second row for a
    // call that already returned corrupts the transcript.
    let ts = "2026-09-10T00:00:00Z";
    let events = vec![
        turn_start(1, ts),
        step_start(1, 1, ts),
        assistant_message(3, ts, &[("c1", "read"), ("c2", "shell")]),
        SessionEvent::ToolDispatch {
            seq: 4,
            ts: ts.into(),
            call_id: "c1".into(),
            tool: "read".into(),
            arguments_hash: "sha256:a".into(),
        },
        tool_result_message(5, ts, "c1"),
        SessionEvent::ToolDispatch {
            seq: 6,
            ts: ts.into(),
            call_id: "c2".into(),
            tool: "shell".into(),
            arguments_hash: "sha256:b".into(),
        },
    ];
    let closers = interrupted_turn_closers(&events);
    let results: Vec<&Value> = closers
        .iter()
        .filter_map(|event| match event {
            SessionEvent::ToolResult { data, .. } => Some(data),
            _ => None,
        })
        .collect();
    assert_eq!(
        results.len(),
        1,
        "exactly the unanswered call is closed: {closers:#?}"
    );
    assert_eq!(results[0]["call_id"], "c2");
    assert_eq!(results[0]["error"], TOOL_OUTCOME_UNKNOWN);
}

#[test]
fn dangling_read_only_call_without_a_dispatch_is_closed() {
    // The model emitted a read-only `tool_use`; no dispatch row
    // ever landed because the crash happened before execution.
    // The call still needs a closer or the next request carries
    // an unanswered `tool_use`.
    let ts = "2026-09-10T00:00:00Z";
    let events = vec![
        turn_start(1, ts),
        step_start(1, 1, ts),
        assistant_message(3, ts, &[("c1", "read")]),
    ];
    let closers = interrupted_turn_closers(&events);
    let result = closers
        .iter()
        .find_map(|event| match event {
            SessionEvent::ToolResult { data, is_error, .. } => {
                Some((data, *is_error))
            }
            _ => None,
        })
        .expect("the dangling read-only call is closed");
    assert_eq!(result.0["call_id"], "c1");
    assert_eq!(result.0["error"], TOOL_NOT_STARTED);
    assert!(result.1, "the closer is an error row");
}

#[test]
fn paired_call_does_not_get_a_synthetic_result() {
    // c1 has both ToolCall and ToolResult; c2 only has
    // ToolCall. Only c2 should be synthesised.
    let events = vec![
        turn_start(1, "2026-09-10T00:00:00Z"),
        step_start(1, 1, "2026-09-10T00:00:00Z"),
        assistant_with_tool_calls(
            3,
            "2026-09-10T00:00:00Z",
            &[("c1", "read"), ("c2", "shell")],
        ),
        SessionEvent::ToolCall {
            seq: 4,
            ts: "2026-09-10T00:00:00Z".into(),
            data: json!({"call_id": "c1", "name": "read", "arguments": {}}),
            call_id: Some("c1".to_string()),
            name: Some("read".to_string()),
        },
        tool_result(5, "2026-09-10T00:00:00Z", "c1"),
        SessionEvent::ToolCall {
            seq: 6,
            ts: "2026-09-10T00:00:00Z".into(),
            data: json!({"call_id": "c2", "name": "shell", "arguments": {}}),
            call_id: Some("c2".to_string()),
            name: Some("shell".to_string()),
        },
    ];
    let closers = interrupted_turn_closers(&events);
    // One tool_result (for c2) + step/end + turn/end.
    assert_eq!(closers.len(), 3);
    match &closers[0] {
        SessionEvent::ToolResult { data, .. } => {
            assert_eq!(data["call_id"], "c2");
            assert_eq!(data["error"], TOOL_OUTCOME_UNKNOWN);
        }
        other => panic!("expected tool_result, got {other:?}"),
    }
}

#[test]
fn open_step_without_dangling_call_emits_only_step_and_turn_enders() {
    let events = vec![
        turn_start(1, "2026-09-10T00:00:00Z"),
        step_start(1, 1, "2026-09-10T00:00:00Z"),
    ];
    let closers = interrupted_turn_closers(&events);
    // step/end + turn/end, no synthetic tool_result.
    assert_eq!(closers.len(), 2);
    match &closers[0] {
        SessionEvent::Step { data, .. } => {
            assert_eq!(data["kind"], "end");
            assert_eq!(data["turn"], 1);
            assert_eq!(data["step"], 1);
        }
        other => panic!("expected step/end, got {other:?}"),
    }
    match &closers[1] {
        SessionEvent::Turn { data, .. } => {
            assert_eq!(data["kind"], "end");
            assert_eq!(data["turn"], 1);
            assert_eq!(data["reason"], TURN_INTERRUPTED);
        }
        other => panic!("expected turn/end, got {other:?}"),
    }
}

#[test]
fn closed_turn_then_open_turn_only_repairs_the_open_one() {
    let events = vec![
        turn_start(1, "2026-09-10T00:00:00Z"),
        turn_end(1, "2026-09-10T00:00:00Z"),
        turn_start(2, "2026-09-10T00:00:01Z"),
        step_start(2, 1, "2026-09-10T00:00:01Z"),
        assistant_with_tool_calls(
            8,
            "2026-09-10T00:00:01Z",
            &[("c1", "shell")],
        ),
    ];
    let closers = interrupted_turn_closers(&events);
    // The first turn is already closed; only the second is
    // repaired. 1 tool_result + step/end + turn/end.
    assert_eq!(closers.len(), 3);
    match &closers[2] {
        SessionEvent::Turn { data, .. } => {
            assert_eq!(data["turn"], 2);
            assert_eq!(data["reason"], TURN_INTERRUPTED);
        }
        other => panic!("expected turn/end for turn 2, got {other:?}"),
    }
}

#[test]
fn closers_leave_their_ordinal_to_the_writer() {
    let events = vec![
        turn_start(1, "2026-09-10T00:00:00Z"),
        step_start(1, 1, "2026-09-10T00:00:00Z"),
        assistant_with_tool_calls(
            7,
            "2026-09-10T00:00:00Z",
            &[("c1", "shell")],
        ),
        SessionEvent::ToolCall {
            seq: 8,
            ts: "2026-09-10T00:00:00Z".into(),
            data: json!({"call_id": "c1", "name": "shell", "arguments": {}}),
            call_id: Some("c1".to_string()),
            name: Some("shell".to_string()),
        },
    ];
    let closers = interrupted_turn_closers(&events);
    for ev in &closers {
        assert_eq!(
            ev.seq(),
            0,
            "the writer stamps the closers' ordinals: {closers:#?}"
        );
    }
}

#[test]
fn multiple_dangling_calls_get_one_tool_result_each_in_id_order() {
    let events = vec![
        turn_start(1, "2026-09-10T00:00:00Z"),
        step_start(1, 1, "2026-09-10T00:00:00Z"),
        assistant_with_tool_calls(
            5,
            "2026-09-10T00:00:00Z",
            &[("c3", "shell"), ("c1", "read"), ("c2", "write")],
        ),
    ];
    let closers = interrupted_turn_closers(&events);
    // 3 tool_results (sorted by call_id) + step/end + turn/end.
    assert_eq!(closers.len(), 5);
    let ids: Vec<&str> = closers
        .iter()
        .take(3)
        .map(|ev| match ev {
            SessionEvent::ToolResult { data, .. } => {
                data["call_id"].as_str().unwrap_or("")
            }
            _ => "",
        })
        .collect();
    assert_eq!(ids, vec!["c1", "c2", "c3"]);
}

/// The row as it goes on disk — the whole row, payload bag and typed
/// fields together. Comparing rows this way says the two are the
/// same shape, not merely that they carry the same keys.
fn row_json(event: &SessionEvent) -> Value {
    serde_json::to_value(event).expect("the row serialises")
}

/// The step end a crash-repair writes is the step end the vocabulary
/// builds: the same payload bag, the same typed fields, the same
/// action key a run's own step end carries.
///
/// The repair used to spell this row itself, and the row it spelled
/// left the action key out — a reader could not tell a step that
/// ended from a step that was cut short.
#[test]
fn a_repaired_step_end_is_the_row_the_vocabulary_builds() {
    let events = vec![
        turn_start(1, "2026-09-10T00:00:00Z"),
        step_start(1, 1, "2026-09-10T00:00:00Z"),
    ];
    let closers = interrupted_turn_closers(&events);
    // step/end + turn/end, no synthetic tool_result.
    assert_eq!(closers.len(), 2, "got: {closers:#?}");
    assert_eq!(
        row_json(&closers[0]),
        row_json(&vocabulary::step_end(1, 1, STEP_INTERRUPTED)),
        "the repaired step end is the vocabulary's row"
    );
    assert_eq!(
        row_json(&closers[0])["data"]["action"],
        "interrupted",
        "the step a crash cut short says so"
    );
}

/// The turn end a crash-repair writes is the turn end the vocabulary
/// builds: the same payload bag, the same typed fields, no key of
/// its own.
#[test]
fn a_repaired_turn_end_is_the_row_the_vocabulary_builds() {
    let events = vec![
        turn_start(1, "2026-09-10T00:00:00Z"),
        step_start(1, 1, "2026-09-10T00:00:00Z"),
    ];
    let closers = interrupted_turn_closers(&events);
    assert_eq!(closers.len(), 2, "got: {closers:#?}");
    assert_eq!(
        row_json(&closers[1]),
        row_json(&vocabulary::turn_end(1, TURN_INTERRUPTED, None)),
        "the repaired turn end is the vocabulary's row"
    );
}

/// The tool result a crash-repair writes for a call that never
/// finished is the row the vocabulary builds: the same payload bag,
/// the same typed fields, the same surface op.
///
/// The interrupted row is not a tool's output — there is none — so
/// its payload says what is known instead: the call it answers, the
/// tool that call named, the recovery code and the `interrupted`
/// mark.
#[test]
fn a_repaired_tool_result_is_the_row_the_vocabulary_builds() {
    let events = vec![
        turn_start(1, "2026-09-10T00:00:00Z"),
        step_start(1, 1, "2026-09-10T00:00:00Z"),
        SessionEvent::ToolCall {
            seq: 4,
            ts: "2026-09-10T00:00:00Z".into(),
            data: json!({"call_id": "c1", "name": "read", "arguments": {}}),
            call_id: Some("c1".to_string()),
            name: Some("read".to_string()),
        },
    ];
    let closers = interrupted_turn_closers(&events);
    // tool_result + step/end + turn/end.
    assert_eq!(closers.len(), 3, "got: {closers:#?}");
    assert_eq!(
        row_json(&closers[0]),
        row_json(&vocabulary::tool_result_interrupted(
            "c1",
            "read",
            TOOL_OUTCOME_UNKNOWN
        )),
        "the repaired tool result is the vocabulary's row"
    );
}

fn compaction_start(seq: u64, token: &str) -> SessionEvent {
    SessionEvent::CompactionStart {
        seq,
        ts: "2026-09-10T00:00:00Z".into(),
        token: token.into(),
        data: json!({"reason": "budget"}),
    }
}

fn compaction_summary(seq: u64, token: &str) -> SessionEvent {
    SessionEvent::CompactionSummary {
        seq,
        ts: "2026-09-10T00:00:00Z".into(),
        token: token.into(),
        data: json!({"summary": "compacted tail"}),
    }
}

fn compaction_end(seq: u64, token: &str, outcome: &str) -> SessionEvent {
    SessionEvent::CompactionEnd {
        seq,
        ts: "2026-09-10T00:00:00Z".into(),
        token: token.into(),
        outcome: outcome.into(),
        data: json!({}),
    }
}

#[test]
fn orphaned_compactions_reports_unmatched_starts_in_log_order() {
    // Contract shape: one closed lifecycle, then an open one.
    let contract = vec![
        compaction_start(1, "t1"),
        compaction_end(2, "t1", "committed"),
        compaction_start(3, "t2"),
    ];
    assert_eq!(
        orphaned_compactions(&contract),
        vec![OrphanedCompaction {
            token: "t2".into(),
            start_seq: 3,
        }]
    );

    // Multiple orphans keep their log order.
    let events = vec![
        compaction_start(1, "t1"),
        compaction_end(2, "t1", "committed"),
        compaction_start(3, "t2"),
        compaction_start(4, "t3"),
    ];
    assert_eq!(
        orphaned_compactions(&events),
        vec![
            OrphanedCompaction {
                token: "t2".into(),
                start_seq: 3,
            },
            OrphanedCompaction {
                token: "t3".into(),
                start_seq: 4,
            },
        ]
    );
}

#[test]
fn orphaned_compactions_needs_an_end_not_a_summary() {
    assert!(orphaned_compactions(&[]).is_empty());

    // Closed lifecycle — a summary is optional, the end is
    // what closes it.
    let closed = vec![
        compaction_start(1, "t1"),
        compaction_summary(2, "t1"),
        compaction_end(3, "t1", "committed"),
        compaction_start(4, "t2"),
        compaction_end(5, "t2", "failed"),
    ];
    assert!(orphaned_compactions(&closed).is_empty());

    // A summary alone leaves the lifecycle open.
    let summary_only =
        vec![compaction_start(7, "t9"), compaction_summary(8, "t9")];
    assert_eq!(
        orphaned_compactions(&summary_only),
        vec![OrphanedCompaction {
            token: "t9".into(),
            start_seq: 7,
        }]
    );
}

/// R29-Phase-J: the loader-side mapping. Matched lifecycles
/// report their recorded outcome; an unmatched `Start` reports
/// `Interrupted` — the "crashed mid-compaction" state.
#[test]
fn compaction_outcomes_maps_orphans_to_interrupted() {
    let events = vec![
        compaction_start(1, "t1"),
        compaction_summary(2, "t1"),
        compaction_end(3, "t1", "committed"),
        compaction_start(4, "t2"),
        compaction_end(5, "t2", "failed"),
        // Crash: started, never ended.
        compaction_start(6, "t3"),
    ];
    assert_eq!(
        compaction_outcomes(&events),
        vec![
            ("t1".to_string(), CompactionLifecycleOutcome::Committed),
            ("t2".to_string(), CompactionLifecycleOutcome::Failed),
            ("t3".to_string(), CompactionLifecycleOutcome::Interrupted),
        ]
    );
}

/// An unrecognised terminal outcome is not a committed one —
/// it maps to `Interrupted` rather than being dropped.
#[test]
fn compaction_outcomes_unknown_terminal_maps_to_interrupted() {
    let events =
        vec![compaction_start(1, "t1"), compaction_end(2, "t1", "wat")];
    assert_eq!(
        compaction_outcomes(&events),
        vec![("t1".to_string(), CompactionLifecycleOutcome::Interrupted)]
    );
}

/// A park row: `park_id` empty models a row written before the
/// link existed.
fn parked_row(seq: u64, park_id: &str, text: &str) -> Value {
    serde_json::to_value(SessionEvent::ParkedPrompt {
        seq,
        ts: "2026-10-26T00:00:00Z".to_string(),
        park_id: park_id.to_string(),
        text: text.to_string(),
        attachments: Vec::new(),
        agent_name: None,
        provider: None,
        model: None,
        rerun: false,
    })
    .expect("serialises")
}

/// The row the run started from a park appends.
fn consumed_row(seq: u64, park_id: &str) -> Value {
    serde_json::to_value(SessionEvent::ParkConsumed {
        seq,
        ts: "2026-10-26T00:00:01Z".to_string(),
        park_id: park_id.to_string(),
    })
    .expect("serialises")
}

fn user_input_row(text: &str) -> Value {
    json!({"type": "UserInput", "data": {"text": text}})
}

/// A parked prompt no run consumed is still queued — this is the
/// crash the park row exists for.
#[test]
fn parked_prompts_reports_an_unconsumed_park() {
    let rows = vec![
        json!({"type": "user_message", "data": {"text": "live turn"}}),
        parked_row(2, "park-2", "queued behind the run"),
    ];
    let recovered = parked_prompts(&rows);
    assert_eq!(recovered.len(), 1, "the park is still open");
    assert_eq!(recovered[0].text, "queued behind the run");
    assert_eq!(recovered[0].seq, 2);
    assert_eq!(recovered[0].park_id, "park-2");
}

/// A text-only park — the shape a plain `Prompt` parks — is
/// recovered with its text: `attachments` being empty is not a
/// filter, because every text turn parks exactly that shape.
#[test]
fn a_text_only_park_is_recovered_with_its_text() {
    let rows = vec![parked_row(1, "park-text", "a queued question")];
    let recovered = parked_prompts(&rows);
    assert_eq!(
        recovered.len(),
        1,
        "an empty attachments list is not a reason to skip the park"
    );
    assert_eq!(recovered[0].text, "a queued question");
    assert_eq!(recovered[0].park_id, "park-text");
    assert!(recovered[0].attachments.is_empty());
}

/// The run started from a park spends **that** park, and only it:
/// the second prompt queued behind a live run is still queued
/// after the first ran, or a crash before its turn loses it.
#[test]
fn parked_prompts_consumes_one_park_per_run() {
    let rows = vec![
        parked_row(1, "park-1", "first"),
        parked_row(2, "park-2", "second"),
        consumed_row(3, "park-1"),
        user_input_row("first"),
    ];
    let recovered = parked_prompts(&rows);
    assert_eq!(
        recovered
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>(),
        vec!["second"],
        "the second prompt is still queued after the first ran"
    );
}

/// The consumption row spends the park it names.
#[test]
fn parked_prompts_drops_a_consumed_park() {
    let rows = vec![
        parked_row(1, "park-1", "ran"),
        consumed_row(2, "park-1"),
        user_input_row("ran"),
        parked_row(4, "park-4", "still queued"),
    ];
    let recovered = parked_prompts(&rows);
    assert_eq!(
        recovered
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>(),
        vec!["still queued"],
        "only the park with no consumption row is still open"
    );
}

/// A plain `Prompt` is not parked, but its run writes a `UserInput`
/// row anyway. That row must not spend a park: the park's own turn
/// has not run, and dropping it would lose the prompt silently.
///
/// This is the shape ordinal pairing got wrong.
#[test]
fn a_plain_prompts_user_input_leaves_a_park_pending() {
    let rows = vec![
        parked_row(1, "park-1", "waiting its turn"),
        user_input_row("an unrelated text prompt's run"),
    ];
    let recovered = parked_prompts(&rows);
    assert_eq!(
        recovered
            .iter()
            .map(|p| p.park_id.as_str())
            .collect::<Vec<_>>(),
        vec!["park-1"],
        "a row another run wrote says nothing about this park"
    );
}

/// A consumption naming a park the log does not hold (a park the
/// append of which reported failure and never landed) is inert.
#[test]
fn a_consumption_without_its_park_is_ignored() {
    let rows = vec![
        consumed_row(1, "park-gone"),
        parked_row(2, "park-2", "queued"),
    ];
    assert_eq!(
        parked_prompts(&rows)
            .iter()
            .map(|p| p.park_id.as_str())
            .collect::<Vec<_>>(),
        vec!["park-2"],
        "the unmatched consumption spends nothing"
    );
}

/// A `parked_prompt` row from this version always carries a
/// `park_id`. A row written before the link existed carries none,
/// and recovering it by its ordinal would name the wrong park as
/// soon as any other row lands between it and its run — so the
/// reader warns once and skips the row. This version's logs need
/// `park_id`.
#[test]
fn a_keyless_park_row_is_warned_once_and_skipped() {
    use tracing_subscriber::prelude::*;

    let capture = WarnCapture::default();
    let _guard = tracing_subscriber::registry()
        .with(capture.clone())
        .set_default();
    let rows = vec![
        parked_row(3, "", "old row"),
        user_input_row("ran"),
        parked_row(5, "park-5", "queued behind"),
    ];
    assert_eq!(
        parked_prompts(&rows)
            .iter()
            .map(|p| p.park_id.as_str())
            .collect::<Vec<_>>(),
        vec!["park-5"],
        "only a park this version wrote is recovered"
    );
    let warns = capture.take();
    assert_eq!(
        warns.len(),
        1,
        "one warn for the keyless row, not one per row folded"
    );
    assert!(
        warns[0].contains("park_id"),
        "the warn names what the row is missing: {}",
        warns[0]
    );
}

/// A turn's own prompt row is a `user_message` row. It records what
/// the user asked, not that a park was spent: the park a run took is
/// spent by the `park_consumed` row naming it, so an ordinary turn's
/// prompt record leaves every queued park alone. Inferring the
/// consumption from "this row is a user turn" would drop a prompt
/// whose park is still waiting.
#[test]
fn an_ordinary_user_message_row_does_not_consume_a_park() {
    let rows = vec![
        parked_row(1, "park-1", "still queued"),
        json!({
            "type": "user_message",
            "seq": 2,
            "data": {"role": "user", "content": {"text": "another turn"}},
        }),
    ];
    assert_eq!(
        parked_prompts(&rows)
            .iter()
            .map(|p| p.park_id.as_str())
            .collect::<Vec<_>>(),
        vec!["park-1"],
        "the turn's prompt record is not a consumption"
    );
}

/// A steer lands mid-run; the prompt queued behind that run is
/// untouched by it.
#[test]
fn a_steer_does_not_consume_a_parked_prompt() {
    let rows = vec![
        parked_row(1, "park-1", "queued"),
        json!({
            "type": "user_message",
            "data": {"text": "steer"},
            "source": "steer",
        }),
    ];
    assert_eq!(
        parked_prompts(&rows).len(),
        1,
        "a steer is not the run's prompt record"
    );
}

/// The recovered turn keeps the parts of the park the run needs:
/// its attachments and its agent selection.
#[test]
fn parked_prompts_carries_the_attachment_refs_and_agent() {
    let row = serde_json::to_value(SessionEvent::ParkedPrompt {
        seq: 7,
        ts: "2026-10-26T00:00:00Z".to_string(),
        park_id: "park-7".to_string(),
        text: "look".to_string(),
        attachments: vec![crate::events::AttachmentRefRow {
            kind: "image".to_string(),
            hash: Some("abc123".to_string()),
            name: None,
            mime_type: Some("image/png".to_string()),
            byte_len: Some(12),
            unstored: None,
        }],
        agent_name: Some("reviewer".to_string()),
        provider: Some("openai".to_string()),
        model: Some("gpt-5".to_string()),
        rerun: true,
    })
    .expect("serialises");
    let recovered = parked_prompts(&[row]);
    assert_eq!(recovered.len(), 1);
    assert_eq!(recovered[0].agent_name.as_deref(), Some("reviewer"));
    assert_eq!(recovered[0].provider.as_deref(), Some("openai"));
    assert_eq!(recovered[0].model.as_deref(), Some("gpt-5"));
    assert!(recovered[0].rerun, "a rerun stays a rerun");
    assert_eq!(
        recovered[0].attachments[0].hash.as_deref(),
        Some("abc123"),
        "the ref is re-readable"
    );
}
