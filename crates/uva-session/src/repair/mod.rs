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
use uva_provider::ContentPart;

use crate::events::{
    CompactionLifecycleOutcome,
    RowTag,
    SessionEvent,
    SurfaceOp,
    restore_row,
};

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
                SessionEvent::Turn { .. } => match ev.boundary_kind() {
                    Some("start") => {
                        if let Some(turn) = ev.boundary_turn() {
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
                SessionEvent::Step { .. } => match ev.boundary_kind() {
                    Some("start") => {
                        if let (Some(turn), Some(step)) =
                            (ev.boundary_turn(), ev.boundary_step())
                        {
                            s.open_step = Some((turn, step));
                        }
                    }
                    Some("end") => {
                        s.open_step = None;
                    }
                    _ => {}
                },
                SessionEvent::ToolCall { .. } => {
                    if let Some(call_id) = ev.tool_call_id() {
                        let name =
                            ev.tool_call_name().unwrap_or("").to_string();
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
                    // The calls come from the row's **message**,
                    // read the way the vocabulary reads it — the
                    // provider's own encoding, not a hand-written
                    // guess at the payload's shape. (The flat
                    // `tool_calls` array is the legacy shape older
                    // logs carry.)
                    if s.open_turn.is_some() {
                        let mut calls: Vec<(String, String)> = Vec::new();
                        if let Some(message) = ev.message() {
                            for part in message.content.iter() {
                                if let ContentPart::ToolUse(call) = part {
                                    calls.push((
                                        call.id.clone(),
                                        call.name.clone(),
                                    ));
                                }
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
                                calls.push((id.to_string(), name.to_string()));
                            }
                        }
                        for (call_id, name) in calls {
                            if call_id.is_empty() {
                                continue;
                            }
                            s.pending_calls.entry(call_id).or_insert(
                                PendingCall {
                                    call_started: false,
                                    name,
                                },
                            );
                        }
                    }
                }
                SessionEvent::ToolResult { .. } => {
                    // The row answers one call. The typed field
                    // carries it; a synthetic closer or an older log
                    // spells it `call_id` in the payload, and the
                    // parser joins that spelling into the field.
                    if let Some(call_id) = ev.tool_call_id() {
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
            tool_call_id: Some(call_id.clone()),
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
            kind: Some("end".to_string()),
            turn: Some(turn),
            step: Some(step),
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
        kind: Some("end".to_string()),
        turn: Some(turn),
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

/// A prompt a node parked and never ran: the durable half of the
/// controller's queued turns.
///
/// The parked row is the queue entry, so a node that takes the
/// session over after a crash can put the turn back in the queue
/// instead of losing it. The attachment bytes are not here — the
/// refs resolve through the deployment's attachment store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParkedPromptRecovery {
    /// `seq` of the `parked_prompt` row.
    pub seq: u64,
    /// The id the consumer of this park quotes back in a
    /// `park_consumed` row: the row's own `park_id`. A keyless row
    /// (written before the link existed) never reaches this struct —
    /// the fold warns once and skips it.
    pub park_id: String,
    /// ISO-8601 timestamp of the row.
    pub ts: String,
    /// The prompt's text parts, concatenated.
    pub text: String,
    /// Re-readable refs to the turn's attachments.
    pub attachments: Vec<crate::events::AttachmentRefRow>,
    /// The agent the turn named, when the client selected one.
    pub agent_name: Option<String>,
    /// The provider the turn was submitted under.
    pub provider: Option<String>,
    /// The model the turn was submitted under.
    pub model: Option<String>,
    /// `true` when the turn was a rerun.
    pub rerun: bool,
}

/// The prompts a crash left parked, in log order.
///
/// A `parked_prompt` row is one queue entry, spent by whoever takes
/// it off the queue — the run started from it, or a cancel that
/// drops it — each appending a `park_consumed` row naming the park's
/// `park_id`. No other row spends a park:
///
/// - A `UserInput` row is not a consumption. Logs written before
///   every turn parked hold `UserInput` rows no park preceded, and a
///   run writes one only for a turn that carries text or
///   attachments — so pairing by position paired unrelated turns
///   with parks, and a crash before a park's own run rolled the
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
///   `park_id`, and this version's logs need one: the reader warns
///   once and skips the row. Recovering it by its ordinal would name
///   the wrong park as soon as any other row lands between it and
///   its run, and treating it as consumed because *some* row followed
///   would be the silent loss this fold exists to prevent — so the
///   keyless row is deprecated and left where it lies.
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
        let current = restore_row(row);
        match current.tag() {
            Some(RowTag::ParkedPrompt) => {
                let Some(SessionEvent::ParkedPrompt {
                    seq,
                    ts,
                    park_id,
                    text,
                    attachments,
                    agent_name,
                    provider,
                    model,
                    rerun,
                }) = current.event()
                else {
                    continue;
                };
                if park_id.is_empty() {
                    tracing::warn!(
                        seq,
                        "parked_prompt row without a park_id is skipped: \
                         this version of the logs needs a park_id"
                    );
                    continue;
                }
                parked.push(ParkedPromptRecovery {
                    seq,
                    park_id,
                    ts,
                    text,
                    attachments,
                    agent_name,
                    provider,
                    model,
                    rerun,
                });
            }
            Some(RowTag::ParkConsumed) => {
                if let Some(SessionEvent::ParkConsumed { park_id, .. }) =
                    current.event()
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
        | SessionEvent::ToolOutput { ts, .. }
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
        | SessionEvent::Feedback { ts, .. }
        | SessionEvent::Plugin { ts, .. }
        | SessionEvent::SessionShutdown { ts, .. } => Some(ts.as_str()),
    }
}

#[cfg(test)]
mod tests;
