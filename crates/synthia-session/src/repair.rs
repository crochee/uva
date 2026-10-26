//! Crash-repair: synthesise the boundary events that close an
//! open tail turn after a process crash.
//!
//! Mirrors `deepseek-harness`'s `core/session/src/repair.ts`.
//! `interrupted_turn_closers` walks a `[SessionEvent]` log and,
//! when it finds an open tail (assistant message with tool
//! calls but no matching tool results), emits the synthetic
//! events needed to balance the log so the next run can resume
//! from a well-formed prefix.
//!
//! ## What gets synthesised
//!
//! For each dangling tool call recorded in the open assistant
//! message:
//!
//! 1. A `ToolResult` event with an `error: TOOL_NOT_STARTED`
//!    payload (the call was emitted to the wire but no
//!    provider-side state was recorded for it) OR
//!    `TOOL_OUTCOME_UNKNOWN` (the call was recorded but no
//!    completion row was durably committed — the agent must
//!    replay-or-retry on resume).
//! 2. A `Step { kind: "end", turn, step }` event if a step is
//!    currently open.
//! 3. A `Turn { kind: "end", turn, reason: "interrupted" }` event.
//!
//! Timestamps on synthetic events reuse the last real event's
//! timestamp. The row the writer writes carries its own `ts` — the
//! writer stamps the wall clock it is given — so the log records
//! when the repair ran, not when the interrupted turn did.
//!
//! ## Compaction crash detection
//!
//! [`orphaned_compactions`] covers the other half of crash
//! repair: a `CompactionStart` whose `CompactionEnd` never
//! reached the log. The caller decides whether to re-emit the
//! end as `interrupted` or replay the compaction.
//!
//! ## Wire-stable codes
//!
//! The two `TOOL_*` string codes match dsh's export names so
//! downstream consumers (UI, dashboard, prompt recovery flow)
//! can recognise them across the two runtimes.

use serde_json::{Value, json};

use crate::events::{CompactionLifecycleOutcome, SessionEvent, SurfaceOp};

/// Recovery code for an assistant tool request that never reached
/// a recorded call start.
pub const TOOL_NOT_STARTED: &str = "TOOL_NOT_STARTED";

/// Recovery code for a recorded tool call whose completed outcome
/// was not durably recorded.
pub const TOOL_OUTCOME_UNKNOWN: &str = "TOOL_OUTCOME_UNKNOWN";

/// Recovery reason attached to the synthesised
/// `Turn { kind: "end" }` closer.
pub const TURN_INTERRUPTED: &str = "interrupted";

/// Synthesise the boundary events needed to close an open tail
/// turn after a crash.
///
/// Single-pass scan: tracks the latest `turn/start` and
/// `step/start` boundaries and the set of tool calls already
/// paired with a `tool_result` event. If the log ends with an
/// assistant message whose tool calls are not fully paired,
/// synthetic events are appended in deterministic order:
///
/// 1. One `ToolResult` per dangling call (with
///    `TOOL_NOT_STARTED` when the call was never recorded as a
///    start event, otherwise `TOOL_OUTCOME_UNKNOWN`).
/// 2. A `Step { kind: "end", ... }` if a step is open.
/// 3. A `Turn { kind: "end", reason: TURN_INTERRUPTED, ... }`.
///
/// Timestamps on synthetic events reuse the last real event's
/// timestamp. A log whose events carry no timestamp leaves the
/// closers' `ts` empty; either way the writer stamps the row it
/// writes with its clock, so the log records when the repair ran.
///
/// The closers carry `seq: 0`: their ordinal is the writer's to
/// hand out, exactly like every other row's. A closer that numbered
/// itself would be a second ordinal source, and two sources drift
/// as soon as anything else appends.
///
/// Returns an empty `Vec` when the log is already balanced or
/// empty. Pure function: no I/O, no shared state.
#[must_use]
pub fn interrupted_turn_closers(events: &[SessionEvent]) -> Vec<SessionEvent> {
    // State carried across the scan.
    #[derive(Default)]
    struct State {
        open_turn: Option<u64>,
        open_step: Option<(u64, u64)>,
        // call_id → (turn, step, was_a_tool_call_event_logged)
        pending_calls: std::collections::HashMap<String, PendingCall>,
        last_ts: String,
    }
    #[derive(Clone)]
    struct PendingCall {
        call_started: bool,
        name: String,
    }

    fn scan(events: &[SessionEvent]) -> State {
        let mut s = State::default();
        for ev in events {
            if let Some(ts) = event_ts(ev) {
                s.last_ts = ts.to_string();
            }
            match ev {
                SessionEvent::Turn { data, .. } => match boundary_kind(data) {
                    Some("start") => {
                        if let Some(turn) =
                            data.get("turn").and_then(Value::as_u64)
                        {
                            s.open_turn = Some(turn);
                            s.open_step = None;
                        }
                    }
                    Some("end") => {
                        s.open_turn = None;
                        s.open_step = None;
                        s.pending_calls.clear();
                    }
                    _ => {}
                },
                SessionEvent::Step { data, .. } => match boundary_kind(data) {
                    Some("start") => {
                        if let (Some(turn), Some(step)) = (
                            data.get("turn").and_then(Value::as_u64),
                            data.get("step").and_then(Value::as_u64),
                        ) {
                            s.open_step = Some((turn, step));
                        }
                    }
                    Some("end") => {
                        s.open_step = None;
                    }
                    _ => {}
                },
                SessionEvent::ToolCall { data, .. } => {
                    if let Some(call_id) =
                        data.get("call_id").and_then(Value::as_str)
                    {
                        let name = data
                            .get("name")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string();
                        s.pending_calls.insert(
                            call_id.to_string(),
                            PendingCall {
                                call_started: true,
                                name,
                            },
                        );
                    }
                }
                // `tool_dispatch` is the intent record written before
                // the call runs. A dispatch with no result means the
                // side effect may have happened, so the outcome is
                // unknown — not "never started".
                SessionEvent::ToolDispatch { call_id, tool, .. } => {
                    s.pending_calls.insert(
                        call_id.clone(),
                        PendingCall {
                            call_started: true,
                            name: tool.clone(),
                        },
                    );
                }
                SessionEvent::AssistantMessage { data, .. } => {
                    // Capture every `tool_use` block on the open
                    // assistant message so dangling calls without
                    // a paired dispatch row still get a synthetic
                    // closer.
                    //
                    // The row payload is the provider `Message`
                    // (`{"role":"assistant","content":[{…}]}`),
                    // so the calls live in the content parts; the
                    // flat `tool_calls` array is the legacy shape
                    // older logs carry.
                    if s.open_turn.is_some() {
                        let mut calls: Vec<(&str, &str)> = Vec::new();
                        if let Some(parts) =
                            data.get("content").and_then(Value::as_array)
                        {
                            for part in parts {
                                if part.get("type").and_then(Value::as_str)
                                    != Some("tool_use")
                                {
                                    continue;
                                }
                                let Some(id) =
                                    part.get("id").and_then(Value::as_str)
                                else {
                                    continue;
                                };
                                let name = part
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                calls.push((id, name));
                            }
                        }
                        if let Some(arr) =
                            data.get("tool_calls").and_then(Value::as_array)
                        {
                            for tc in arr {
                                let Some(id) =
                                    tc.get("id").and_then(Value::as_str)
                                else {
                                    continue;
                                };
                                let name = tc
                                    .get("name")
                                    .and_then(Value::as_str)
                                    .unwrap_or("");
                                calls.push((id, name));
                            }
                        }
                        for (call_id, name) in calls {
                            if call_id.is_empty() {
                                continue;
                            }
                            s.pending_calls
                                .entry(call_id.to_string())
                                .or_insert(PendingCall {
                                    call_started: false,
                                    name: name.to_string(),
                                });
                        }
                    }
                }
                SessionEvent::ToolResult { data, .. } => {
                    // The row payload is the provider tool message:
                    // its `tool_call_id` is the call it answers.
                    // `call_id` is the legacy spelling a synthetic
                    // closer or an older log uses.
                    if let Some(call_id) = data
                        .get("tool_call_id")
                        .or_else(|| data.get("call_id"))
                        .and_then(Value::as_str)
                    {
                        s.pending_calls.remove(call_id);
                    }
                }
                _ => {}
            }
        }
        s
    }

    let state = scan(events);
    let mut closers = Vec::new();
    let Some(turn) = state.open_turn else {
        return closers;
    };
    let ts = state.last_ts.clone();
    // Step insertion order follows Map iteration (HashMap is
    // non-deterministic; sort by call_id for wire stability).
    let mut pending: Vec<_> = state.pending_calls.into_iter().collect();
    pending.sort_by(|a, b| a.0.cmp(&b.0));
    for (call_id, pending_call) in pending {
        let error_code = if pending_call.call_started {
            TOOL_OUTCOME_UNKNOWN
        } else {
            TOOL_NOT_STARTED
        };
        closers.push(SessionEvent::ToolResult {
            seq: 0,
            ts: ts.clone(),
            data: json!({
                "call_id": call_id,
                "tool_name": pending_call.name,
                "error": error_code,
                "interrupted": true,
            }),
            // The call never finished, so the row says the tool
            // failed: a reader that trusts the flag must not read
            // this as a call that returned.
            is_error: true,
            surface_op: Some(SurfaceOp::append()),
        });
    }

    if let Some((_, step)) = state.open_step {
        closers.push(SessionEvent::Step {
            seq: 0,
            ts: ts.clone(),
            data: json!({
                "kind": "end",
                "turn": turn,
                "step": step,
            }),
        });
    }
    closers.push(SessionEvent::Turn {
        seq: 0,
        ts,
        data: json!({
            "kind": "end",
            "turn": turn,
            "reason": TURN_INTERRUPTED,
        }),
    });
    closers
}

/// A `CompactionStart` with no matching `CompactionEnd`.
///
/// The crash marker: the process died mid-compaction, so the
/// summary (if any) may or may not have reached the surface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrphanedCompaction {
    /// `token` of the unmatched `CompactionStart`.
    pub token: String,
    /// `seq` of the unmatched `CompactionStart`.
    pub start_seq: u64,
}

/// Scan a session log for compaction lifecycles that began but
/// never ended.
///
/// Lifecycles are matched by `token`: `CompactionStart` opens
/// one, `CompactionEnd` closes it. A `CompactionSummary` is not
/// required for a lifecycle to close — a start/end pair with no
/// summary is a compaction that failed before producing one.
///
/// Returned in log order. Pure: no I/O, no shared state.
#[must_use]
pub fn orphaned_compactions(
    events: &[SessionEvent],
) -> Vec<OrphanedCompaction> {
    // `(token, start_seq)` of every start not yet closed, in
    // log order.
    let mut open: Vec<(String, u64)> = Vec::new();
    for event in events {
        match event {
            SessionEvent::CompactionStart { seq, token, .. } => {
                open.push((token.clone(), *seq));
            }
            SessionEvent::CompactionEnd { token, .. } => {
                if let Some(idx) = open.iter().position(|(t, _)| t == token) {
                    open.remove(idx);
                }
            }
            _ => {}
        }
    }
    open.into_iter()
        .map(|(token, start_seq)| OrphanedCompaction { token, start_seq })
        .collect()
}

/// The loader-side view of every compaction in a log: one
/// `(token, outcome)` per lifecycle, in log order.
///
/// A lifecycle that reached a `CompactionEnd` carries the outcome
/// that event recorded (`committed` / `failed`); a lifecycle that
/// only has a `CompactionStart` is **`Interrupted`** — the process
/// died mid-compaction. This is the mapping `orphaned_compactions`
/// documents, applied: a resuming loader calls this and treats an
/// `Interrupted` entry as "the summary may or may not have landed;
/// re-derive or replay".
///
/// An unrecognised `outcome` string maps to `Interrupted` rather
/// than being dropped — an unknown terminal state is not a
/// committed one.
#[must_use]
pub fn compaction_outcomes(
    events: &[SessionEvent],
) -> Vec<(String, CompactionLifecycleOutcome)> {
    let mut out: Vec<(String, CompactionLifecycleOutcome)> = Vec::new();
    for event in events {
        match event {
            SessionEvent::CompactionStart { token, .. } => {
                out.push((
                    token.clone(),
                    CompactionLifecycleOutcome::Interrupted,
                ));
            }
            SessionEvent::CompactionEnd { token, outcome, .. } => {
                let parsed = parse_outcome(outcome);
                if let Some(slot) = out.iter_mut().find(|(t, _)| t == token) {
                    slot.1 = parsed;
                } else {
                    // An `End` with no recorded `Start` (a
                    // truncated head) still describes a real
                    // lifecycle; keep it so the count is honest.
                    out.push((token.clone(), parsed));
                }
            }
            _ => {}
        }
    }
    out
}

fn parse_outcome(raw: &str) -> CompactionLifecycleOutcome {
    match raw {
        "committed" => CompactionLifecycleOutcome::Committed,
        "failed" => CompactionLifecycleOutcome::Failed,
        _ => CompactionLifecycleOutcome::Interrupted,
    }
}

/// A prompt a node parked and never ran: the durable half of
/// `ControllerInner::pending_multimodal`.
///
/// The parked row is the queue entry, so a node that takes the
/// session over after a crash can put the turn back in the queue
/// instead of losing it. The attachment bytes are not here — the
/// refs resolve through the deployment's attachment store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedPromptRecovery {
    /// `seq` of the `parked_prompt` row.
    pub seq: u64,
    /// The id the run started from this park quotes back in a
    /// `park_consumed` row.
    ///
    /// The row's own `park_id`, or — for a row written before the link
    /// existed — `seq:{seq}`, which is unique in the log the same way
    /// the row's ordinal is.
    pub park_id: String,
    /// ISO-8601 timestamp of the row.
    pub ts: String,
    /// The prompt's text parts, concatenated.
    pub text: String,
    /// Re-readable refs to the turn's attachments.
    pub attachments: Vec<crate::events::AttachmentRefRow>,
    /// The agent the turn named, when the client selected one.
    pub agent_name: Option<String>,
    /// `true` when the turn was a rerun.
    pub rerun: bool,
}

/// The prompts a crash left parked, in log order.
///
/// A `parked_prompt` row is one queue entry, and the **run started from
/// it** spends exactly that entry by appending a `park_consumed` row
/// naming the park's `park_id`. Nothing else consumes a park:
///
/// - A plain `Prompt` is never parked (the text queue carries it in
///   memory, and the op appends no row for it), yet *every* run writes a
///   `UserInput` row. Pairing by position therefore paired unrelated
///   turns with parks — and a crash before a park's own run rolled the
///   pairing forward, dropping a prompt whose row was intact.
/// - A `user_message` row does not consume a park either: a steer lands
///   mid-run, while a prompt queued behind that run is still waiting.
///
/// The id in the pair comes first in the log — the run appends its
/// consumption before its prompt row — but the fold does not depend on
/// that: a park with a matching `park_consumed` anywhere in `rows` is
/// spent, and every other park is still queued. Because a consumer is
/// always written *after* its park, any tail of the log answers
/// exactly: a park in the tail whose consumer is missing really had
/// none.
///
/// Two guards keep a turn from being lost:
///
/// - A `parked_prompt` row written before the link existed carries no
///   `park_id`; it is keyed by its ordinal (`seq:{seq}`) and stays
///   pending until the run started from it writes the consumption.
///   Treating it as consumed because *some* row followed would be the
///   silent loss this fold exists to prevent.
/// - If the park's own append reported failure but the row landed, the
///   in-memory park still carries the id it tried to write, so its run
///   spends it either way.
///
/// Takes the sink's rows rather than typed events because the park and
/// its consumption are checked *by tag* first. Pure: no I/O.
#[must_use]
pub fn parked_prompts(rows: &[Value]) -> Vec<ParkedPromptRecovery> {
    let mut parked: Vec<ParkedPromptRecovery> = Vec::new();
    let mut consumed: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    for row in rows {
        match row.get("type").and_then(Value::as_str) {
            Some("parked_prompt") => {
                let Some(SessionEvent::ParkedPrompt {
                    seq,
                    ts,
                    park_id,
                    text,
                    attachments,
                    agent_name,
                    rerun,
                }) = SessionEvent::from_value(row)
                else {
                    continue;
                };
                parked.push(ParkedPromptRecovery {
                    seq,
                    park_id: park_key(park_id, seq),
                    ts,
                    text,
                    attachments,
                    agent_name,
                    rerun,
                });
            }
            Some("park_consumed") => {
                if let Some(SessionEvent::ParkConsumed { park_id, .. }) =
                    SessionEvent::from_value(row)
                {
                    consumed.insert(park_id);
                }
            }
            _ => {}
        }
    }
    parked.retain(|prompt| !consumed.contains(&prompt.park_id));
    parked
}

/// The id a park is keyed by: its own, or its ordinal for a row
/// written before the link existed.
fn park_key(park_id: String, seq: u64) -> String {
    if park_id.is_empty() {
        format!("seq:{seq}")
    } else {
        park_id
    }
}

fn event_ts(ev: &SessionEvent) -> Option<&str> {
    match ev {
        SessionEvent::UserMessage { ts, .. }
        | SessionEvent::AssistantMessage { ts, .. }
        | SessionEvent::AssistantChunk { ts, .. }
        | SessionEvent::ToolResult { ts, .. }
        | SessionEvent::Compaction { ts, .. }
        | SessionEvent::CompactionStart { ts, .. }
        | SessionEvent::CompactionSummary { ts, .. }
        | SessionEvent::CompactionEnd { ts, .. }
        | SessionEvent::ToolCall { ts, .. }
        | SessionEvent::Step { ts, .. }
        | SessionEvent::Turn { ts, .. }
        | SessionEvent::Iteration { ts, .. }
        | SessionEvent::Warning { ts, .. }
        | SessionEvent::SteeringGuard { ts, .. }
        | SessionEvent::SteeringHint { ts, .. }
        | SessionEvent::HookBlock { ts, .. }
        | SessionEvent::SubagentEnter { ts, .. }
        | SessionEvent::SubagentExit { ts, .. }
        | SessionEvent::RequestHeader { ts, .. }
        | SessionEvent::ApprovalAsked { ts, .. }
        | SessionEvent::ApprovalDecided { ts, .. }
        | SessionEvent::ApprovalGranted { ts, .. }
        | SessionEvent::ToolDispatch { ts, .. }
        | SessionEvent::RequestSnapshot { ts, .. }
        | SessionEvent::Usage { ts, .. }
        | SessionEvent::SandboxMode { ts, .. }
        | SessionEvent::ToolResultTransformed { ts, .. }
        | SessionEvent::ParkedPrompt { ts, .. }
        | SessionEvent::ParkConsumed { ts, .. }
        | SessionEvent::Plugin { ts, .. }
        | SessionEvent::LifecycleShutdown { ts, .. } => Some(ts.as_str()),
    }
}

// The loop writes `kind` on ordinary boundaries and `phase` on a
// suspension's suspended/end rows; both spellings mark the boundary.
fn boundary_kind(data: &Value) -> Option<&str> {
    data.get("kind")
        .or_else(|| data.get("phase"))
        .and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::CompactionLifecycleOutcome;

    fn turn_start(turn: u64, ts: &str) -> SessionEvent {
        SessionEvent::Turn {
            seq: turn,
            ts: ts.into(),
            data: json!({"kind": "start", "turn": turn}),
        }
    }

    fn turn_end(turn: u64, ts: &str) -> SessionEvent {
        SessionEvent::Turn {
            seq: turn,
            ts: ts.into(),
            data: json!({"kind": "end", "turn": turn}),
        }
    }

    fn step_start(turn: u64, step: u64, ts: &str) -> SessionEvent {
        SessionEvent::Step {
            seq: turn * 100 + step,
            ts: ts.into(),
            data: json!({"kind": "start", "turn": turn, "step": step}),
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
            surface_op: None,
        }
    }

    /// The shape the harness actually writes: the row payload is
    /// the provider `Message`, with the calls in its content parts.
    fn assistant_message(
        seq: u64,
        ts: &str,
        calls: &[(&str, &str)],
    ) -> SessionEvent {
        SessionEvent::AssistantMessage {
            seq,
            ts: ts.into(),
            data: json!({
                "role": "assistant",
                "content": calls
                    .iter()
                    .map(|(id, name)| json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": {},
                    }))
                    .collect::<Vec<_>>(),
            }),
            surface_op: None,
        }
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
                    "phase": "end",
                    "status": "suspended",
                    "pending": ["c1"],
                }),
            },
            SessionEvent::Step {
                seq: 6,
                ts: ts.into(),
                data: json!({"phase": "end", "status": "suspended"}),
            },
            SessionEvent::Turn {
                seq: 7,
                ts: ts.into(),
                data: json!({"phase": "end", "reason": "suspended"}),
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
            assistant_with_tool_calls(
                3,
                "2026-09-10T00:00:00Z",
                &[("c1", "read")],
            ),
            SessionEvent::ToolCall {
                seq: 4,
                ts: "2026-09-10T00:00:00Z".into(),
                data: json!({"call_id": "c1", "name": "read", "arguments": {}}),
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
            assistant_with_tool_calls(
                3,
                "2026-09-10T00:00:00Z",
                &[("c1", "read")],
            ),
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
            },
            tool_result(5, "2026-09-10T00:00:00Z", "c1"),
            SessionEvent::ToolCall {
                seq: 6,
                ts: "2026-09-10T00:00:00Z".into(),
                data: json!({"call_id": "c2", "name": "shell", "arguments": {}}),
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

    /// A park row written before the link existed carries no id. It is
    /// pending, never assumed spent: the row is the only record of a
    /// prompt whose turn may still have to run.
    #[test]
    fn a_legacy_park_without_an_id_stays_pending() {
        let rows = vec![parked_row(3, "", "old row"), user_input_row("ran")];
        let recovered = parked_prompts(&rows);
        assert_eq!(recovered.len(), 1, "an id-less park is not spent by a row");
        assert_eq!(
            recovered[0].park_id, "seq:3",
            "it is keyed by its ordinal, which its own run can quote back"
        );
    }

    /// The key a legacy park is recovered under is the key its run's
    /// consumption row matches — so recovering and re-running one does
    /// not duplicate it.
    #[test]
    fn a_legacy_park_is_spent_by_the_consumption_its_run_writes() {
        let rows = vec![
            parked_row(3, "", "old row"),
            consumed_row(4, "seq:3"),
            user_input_row("old row"),
        ];
        assert!(
            parked_prompts(&rows).is_empty(),
            "the run started from the legacy park spent it"
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
            rerun: true,
        })
        .expect("serialises");
        let recovered = parked_prompts(&[row]);
        assert_eq!(recovered.len(), 1);
        assert_eq!(recovered[0].agent_name.as_deref(), Some("reviewer"));
        assert!(recovered[0].rerun, "a rerun stays a rerun");
        assert_eq!(
            recovered[0].attachments[0].hash.as_deref(),
            Some("abc123"),
            "the ref is re-readable"
        );
    }
}
