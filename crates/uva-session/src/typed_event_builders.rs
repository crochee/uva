//! Typed event builders for the ReAct loop's structural
//! boundaries.
//!
//! R6-2 (dsh `turn/start|end` + `step/start|end` parity). The
//! [`SessionEvent`] enum already carries `Step` / `Turn` /
//! `Iteration` / `SubagentEnter` / `SubagentExit` variants, but
//! the loop has had no producer for them. This module pins the
//! wire shape each event takes so the loop and the controller
//! agree on the JSON contract before any of them have to wire
//! up the actual call site.
//!
//! ## Why builders, not direct enum construction?
//!
//! Five reasons:
//!
//! 1. **Centralised `seq` / `ts` stamping** — the loop fills
//!    the structural payload (iteration / step / turn) and the
//!    builder stamps `seq: 0` + `ts: ""`. The controller
//!    re-stamps at append time.
//! 2. **Shape stability** — if the loop constructs a
//!    `SessionEvent::Step` inline, any future schema change
//!    touches every site. The builder is the single
//!    canonicalisation point.
//! 3. **Frontend codegen** — the app-server contract catalog turns
//!    the vocabulary's own tag type (`RowTag`) into the
//!    `SessionEventType` union `uva-web` generates; these builders pin
//!    the *payload* shape each tag carries on the wire.
//! 4. **Replay-friendliness** — `fold_surface` /
//!    `interrupted_turn_closers` already understand the
//!    `data`-as-`Value` envelope; builders produce the exact
//!    shape the repair helpers expect.
//! 5. **No engine-level wiring required** — the builders are
//!    pure functions. The agent loop can adopt them
//!    incrementally without touching its hot path; future R7
//!    work wires them into the actual emission points.

use serde_json::{Value, json};

use crate::{
    events::{AttachmentRefRow, SessionEvent, SurfaceOp},
    sink::SessionEndReason,
    token_meter::UsageBuckets,
};

/// Build a `Step { start }` event. `step_idx` is the 0-based
/// step ordinal inside the current turn; `turn_idx` is the
/// 0-based turn ordinal inside the run.
#[must_use]
pub fn step_start(turn_idx: u32, step_idx: u32) -> SessionEvent {
    SessionEvent::Step {
        seq: 0,
        ts: String::new(),
        data: json!({
            "kind": "start",
            "turn": turn_idx,
            "step": step_idx,
        }),
        kind: Some("start".to_string()),
        turn: Some(u64::from(turn_idx)),
        step: Some(u64::from(step_idx)),
    }
}

/// Build a `Step { end }` event. Mirrors [`step_start`].
#[must_use]
pub fn step_end(turn_idx: u32, step_idx: u32, action: &str) -> SessionEvent {
    SessionEvent::Step {
        seq: 0,
        ts: String::new(),
        data: json!({
            "kind": "end",
            "turn": turn_idx,
            "step": step_idx,
            "action": action,
        }),
        kind: Some("end".to_string()),
        turn: Some(u64::from(turn_idx)),
        step: Some(u64::from(step_idx)),
    }
}

/// Build a `Turn { start }` event. The 0-based turn ordinal
/// increments every time the model emits a non-final answer.
#[must_use]
pub fn turn_start(turn_idx: u32) -> SessionEvent {
    SessionEvent::Turn {
        seq: 0,
        ts: String::new(),
        data: json!({
            "kind": "start",
            "turn": turn_idx,
        }),
        kind: Some("start".to_string()),
        turn: Some(u64::from(turn_idx)),
    }
}

/// Build a `Turn { end }` event. `reason` is one of:
/// `"completed"`, `"max_iterations"`, `"cancelled"`, `"error"`.
#[must_use]
pub fn turn_end(turn_idx: u32, reason: &str) -> SessionEvent {
    SessionEvent::Turn {
        seq: 0,
        ts: String::new(),
        data: json!({
            "kind": "end",
            "turn": turn_idx,
            "reason": reason,
        }),
        kind: Some("end".to_string()),
        turn: Some(u64::from(turn_idx)),
    }
}

/// Build an `ApprovalAsked` row for one held call.
#[must_use]
pub fn approval_asked(
    id: &str,
    call_id: &str,
    tool: &str,
    arguments_hash: &str,
    risk: &str,
    agent: &str,
) -> SessionEvent {
    SessionEvent::ApprovalAsked {
        seq: 0,
        ts: String::new(),
        id: id.to_string(),
        call_id: call_id.to_string(),
        tool: tool.to_string(),
        arguments_hash: arguments_hash.to_string(),
        risk: risk.to_string(),
        agent: agent.to_string(),
    }
}

/// Build an `ApprovalDecided` row. `payload` carries the decision's
/// optional detail (a denial reason, a grant scope, …).
#[must_use]
pub fn approval_decided(
    id: &str,
    call_id: &str,
    status: &str,
    by: &str,
    payload: Option<Value>,
) -> SessionEvent {
    SessionEvent::ApprovalDecided {
        seq: 0,
        ts: String::new(),
        id: id.to_string(),
        call_id: call_id.to_string(),
        status: status.to_string(),
        by: by.to_string(),
        payload,
    }
}

/// Build an `ApprovalGranted` row for a durable session grant.
#[must_use]
pub fn approval_granted(
    id: &str,
    tool: &str,
    scope: &str,
    workspace_hash: &str,
) -> SessionEvent {
    SessionEvent::ApprovalGranted {
        seq: 0,
        ts: String::new(),
        id: id.to_string(),
        tool: tool.to_string(),
        scope: scope.to_string(),
        workspace_hash: workspace_hash.to_string(),
    }
}

/// Build the `ToolDispatch` intent row written before a call runs.
#[must_use]
pub fn tool_dispatch(
    call_id: &str,
    tool: &str,
    arguments_hash: &str,
) -> SessionEvent {
    SessionEvent::ToolDispatch {
        seq: 0,
        ts: String::new(),
        call_id: call_id.to_string(),
        tool: tool.to_string(),
        arguments_hash: arguments_hash.to_string(),
    }
}

/// Build the `ToolOutput` row written for one streamed chunk.
///
/// `chunk` is the text the tool produced in this slice — one row per
/// chunk, in the order the chunks arrived. The row is **log-only**:
/// it is the durable half of the live `ToolProgress` event, so a
/// reload replays the progress a client already saw; the settled
/// `tool_result` row stays the model's copy.
#[must_use]
pub fn tool_output(call_id: &str, tool: &str, chunk: &str) -> SessionEvent {
    SessionEvent::ToolOutput {
        seq: 0,
        ts: String::new(),
        data: serde_json::json!({ "output": chunk }),
        call_id: Some(call_id.to_string()),
        tool: Some(tool.to_string()),
    }
}

/// Build the `RequestSnapshot` row written before a model request.
#[must_use]
pub fn request_snapshot(
    provider: &str,
    model: &str,
    params_hash: &str,
    tools_hash: &str,
    system_hash: &str,
    sections: Vec<String>,
    policy_hash: &str,
) -> SessionEvent {
    SessionEvent::RequestSnapshot {
        seq: 0,
        ts: String::new(),
        provider: provider.to_string(),
        model: model.to_string(),
        params_hash: params_hash.to_string(),
        tools_hash: tools_hash.to_string(),
        system_hash: system_hash.to_string(),
        sections,
        policy_hash: policy_hash.to_string(),
    }
}

/// Build the `Turn { suspended }` marker: the turn stopped with
/// calls still pending, and lists them.
#[must_use]
pub fn turn_suspended(pending: Vec<String>) -> SessionEvent {
    SessionEvent::Turn {
        seq: 0,
        ts: String::new(),
        data: json!({
            "kind": "suspended",
            "status": "suspended",
            "pending": pending,
        }),
        kind: Some("suspended".to_string()),
        turn: None,
    }
}

/// Build the `Step { end, status: suspended }` row.
#[must_use]
pub fn step_end_suspended() -> SessionEvent {
    SessionEvent::Step {
        seq: 0,
        ts: String::new(),
        data: json!({
            "kind": "end",
            "status": "suspended",
        }),
        kind: Some("end".to_string()),
        turn: None,
        step: None,
    }
}

/// Build the `Turn { end, reason: suspended }` row.
#[must_use]
pub fn turn_end_suspended() -> SessionEvent {
    SessionEvent::Turn {
        seq: 0,
        ts: String::new(),
        data: json!({
            "kind": "end",
            "reason": "suspended",
        }),
        kind: Some("end".to_string()),
        turn: None,
    }
}

/// Build an `Iteration { start }` event. R6-2 keeps this
/// distinct from `step` to mirror dsh's
/// `iteration/start|end` semantics: an iteration wraps one
/// pass of the LLM, a step wraps one tool dispatch within an
/// iteration.
#[must_use]
pub fn iteration_start(iteration_idx: u32) -> SessionEvent {
    SessionEvent::Iteration {
        seq: 0,
        ts: String::new(),
        data: json!({
            "kind": "start",
            "iteration": iteration_idx,
        }),
    }
}

/// Build an `Iteration { end }` event.
#[must_use]
pub fn iteration_end(iteration_idx: u32, action: &str) -> SessionEvent {
    SessionEvent::Iteration {
        seq: 0,
        ts: String::new(),
        data: json!({
            "kind": "end",
            "iteration": iteration_idx,
            "action": action,
        }),
    }
}

/// Build a `SubagentEnter` event. `child_session_id` is the
/// session id the delegating sub-agent will use; `depth` is the
/// sub-agent depth (1 = direct child, 2 = grandchild, ...).
#[must_use]
pub fn subagent_enter(
    child_session_id: &str,
    parent_session_id: &str,
    depth: u32,
    agent_name: &str,
) -> SessionEvent {
    SessionEvent::SubagentEnter {
        seq: 0,
        ts: String::new(),
        data: json!({
            "child_session_id": child_session_id,
            "parent_session_id": parent_session_id,
            "depth": depth,
            "agent_name": agent_name,
        }),
        child_session_id: Some(child_session_id.to_string()),
        parent_session_id: Some(parent_session_id.to_string()),
        agent_name: Some(agent_name.to_string()),
        depth: Some(u64::from(depth)),
    }
}

/// Build a `SubagentExit` event. `status` is one of:
/// `"completed"`, `"cancelled"`, `"error"`.
#[must_use]
pub fn subagent_exit(
    child_session_id: &str,
    depth: u32,
    status: &str,
) -> SessionEvent {
    SessionEvent::SubagentExit {
        seq: 0,
        ts: String::new(),
        data: json!({
            "child_session_id": child_session_id,
            "depth": depth,
            "status": status,
        }),
        child_session_id: Some(child_session_id.to_string()),
        depth: Some(u64::from(depth)),
        status: Some(status.to_string()),
    }
}

/// Build a `RequestHeader` event with the resolved provider +
/// model + tools_hash triple. `reason` is `"initial"` on the
/// first run, `"change"` when the configuration drifted.
#[must_use]
pub fn request_header(
    provider: &str,
    model: &str,
    tools_hash: &str,
    reason: &str,
) -> SessionEvent {
    SessionEvent::RequestHeader {
        seq: 0,
        ts: String::new(),
        data: json!({
            "reason": reason,
            "provider": provider,
            "model": model,
            "tools_hash": tools_hash,
        }),
        provider: Some(provider.to_string()),
        model: Some(model.to_string()),
    }
}

/// Build a `Usage` event for one LLM call. `prompt_tokens` /
/// `completion_tokens` are the required fields;
/// `reasoning_tokens` / `cache_read_tokens` /
/// `cache_write_tokens` are optional and serialised only
/// when present.
///
/// R30: the event carries the provider report twice — the typed
/// `usage` field (the [`crate::token_meter`] fold and new
/// readers) and the legacy `data` payload (older readers) — so
/// a log written by either generation replays on both.
#[must_use]
pub fn usage(
    prompt_tokens: usize,
    completion_tokens: usize,
    total_tokens: usize,
    reasoning_tokens: Option<usize>,
    cache_read_tokens: Option<usize>,
    cache_write_tokens: Option<usize>,
) -> SessionEvent {
    let mut data = json!({
        "prompt_tokens": prompt_tokens,
        "completion_tokens": completion_tokens,
        "total_tokens": total_tokens,
    });
    if let Some(r) = reasoning_tokens {
        data["reasoning_tokens"] = json!(r);
    }
    if let Some(r) = cache_read_tokens {
        data["cache_read_tokens"] = json!(r);
    }
    if let Some(w) = cache_write_tokens {
        data["cache_write_tokens"] = json!(w);
    }
    let usage = UsageBuckets {
        input_tokens: prompt_tokens as u64,
        output_tokens: completion_tokens as u64,
        cache_read_tokens: cache_read_tokens.map(|n| n as u64),
        cache_write_tokens: cache_write_tokens.map(|n| n as u64),
    };
    SessionEvent::Usage {
        seq: 0,
        ts: String::new(),
        data,
        usage: Some(usage),
    }
}

/// Build a `SandboxMode { mode }` event — the durable
/// sandbox-policy override for a session (R31).
///
/// `mode` is the execution-policy wire name (`"read-only"` /
/// `"workspace-write"` / `"danger-full-access"`). It is preserved
/// verbatim rather than validated: this crate is the wire layer
/// and does not know the policy enum, a log written by a newer
/// build must round-trip here, and the policy fold ignores a mode
/// it does not know. The seq is stamped `0` like every builder's —
/// the controller re-stamps at append time.
#[must_use]
pub fn sandbox_mode(mode: &str) -> SessionEvent {
    SessionEvent::SandboxMode {
        seq: 0,
        ts: String::new(),
        mode: mode.to_string(),
    }
}

/// Build a `UserMessage { data, source }` event.
///
/// `data` is a [`uva_provider::Message`] serialised with the
/// crate's wire (`{"role": "user", "content": …}`); `source` names
/// where the turn came from when it is not the ordinary turn
/// boundary. Today the one non-ordinary source is `"steer"`: a
/// message the controller appends through the session's writer
/// while a run is live, so the loop's next step boundary — not a
/// second queue — carries it into the model's next request.
///
/// `None` is the ordinary path, and the wire omits the field.
#[must_use]
pub fn user_message(data: Value, source: Option<&str>) -> SessionEvent {
    SessionEvent::UserMessage {
        seq: 0,
        ts: String::new(),
        data,
        source: source.map(str::to_string),
        surface_op: None,
        attachments: Vec::new(),
    }
}

#[must_use]
/// Build an `AssistantMessage { data }` event.
///
/// `data` is a [`uva_provider::Message`] serialised with the
/// crate's wire (`{"role": "assistant", "content": …}`) — the turn
/// the model produced. The row carries no source: an assistant turn
/// always comes from the run, never from a steering injection.
///
/// The seq is stamped `0` like every builder's — the writer
/// re-stamps at append time.
pub fn assistant_message(data: Value) -> SessionEvent {
    SessionEvent::AssistantMessage {
        seq: 0,
        ts: String::new(),
        data,
        surface_op: None,
    }
}

/// Build a `Warning` row — the shared shape every panic-isolated
/// seam (hooks, guards, inbox, tracker, compaction) reports
/// through: `data` is always `{"kind": .., "message": ..}`.
#[must_use]
pub fn warning(kind: &str, message: &str) -> SessionEvent {
    SessionEvent::Warning {
        seq: 0,
        ts: String::new(),
        data: json!({ "kind": kind, "message": message }),
    }
}

/// Build a `ToolResult` row from the tool call's output envelope.
///
/// `data` is the [`uva_provider::Message`] payload (the tool's
/// output, not the failure); `is_error` rides beside it so the row
/// says what the live event says. `tool_call_id` is `None` only for
/// a result that answers no call.
#[must_use]
pub fn tool_result(
    data: Value,
    tool_call_id: Option<String>,
    is_error: bool,
) -> SessionEvent {
    SessionEvent::ToolResult {
        seq: 0,
        ts: String::new(),
        data,
        is_error,
        tool_call_id,
        surface_op: None,
    }
}

/// Build the row a role-bearing provider message lands as — the
/// seeding path's one entry for a client-supplied history.
///
/// `User` and `System` both land as `user_message` (that is the
/// role the model and the fold see for an inline system prompt),
/// `Assistant` as `assistant_message`, `Tool` as `tool_result` with
/// the message's own call id and no error flag. Serialisation is
/// the one fallible step, so the row comes back as `Result`.
pub fn message_row(
    message: &uva_provider::Message,
) -> Result<SessionEvent, serde_json::Error> {
    let data = serde_json::to_value(message)?;
    Ok(match message.role {
        uva_provider::Role::User | uva_provider::Role::System => {
            user_message(data, None)
        }
        uva_provider::Role::Assistant => assistant_message(data),
        uva_provider::Role::Tool => {
            tool_result(data, message.tool_call_id.clone(), false)
        }
    })
}

/// Build the `UserMessage` row a turn's prompt lands as — with its
/// re-readable attachment refs and the [`SurfaceOp`] the fold
/// computed for it: `None` appends, a rerun's `Replace` shadows the
/// turn it replaces.
#[must_use]
pub fn turn_prompt_row(
    data: Value,
    attachments: Vec<AttachmentRefRow>,
    op: Option<SurfaceOp>,
) -> SessionEvent {
    SessionEvent::UserMessage {
        seq: 0,
        ts: String::new(),
        data,
        source: None,
        surface_op: op,
        attachments,
    }
}

/// Build a `HookBlock` row — the veto's durable half: which hook,
/// gate or guard refused the call, why, and the call it refused.
#[must_use]
pub fn hook_block(
    hook: &str,
    reason: &str,
    tool: &str,
    call_id: &str,
) -> SessionEvent {
    SessionEvent::HookBlock {
        seq: 0,
        ts: String::new(),
        data: json!({
            "hook": hook,
            "reason": reason,
            "tool": tool,
            "call_id": call_id,
        }),
    }
}

/// Build a `ToolResultTransformed` row — one row per rewritten
/// result, carrying the transformers that ran (in registration
/// order) and the digests of the result before and after.
#[must_use]
pub fn tool_result_transformed(
    transformers: Vec<String>,
    tool_name: &str,
    call_id: &str,
    before: String,
    after: String,
) -> SessionEvent {
    SessionEvent::ToolResultTransformed {
        seq: 0,
        ts: String::new(),
        transformers,
        tool_name: tool_name.to_string(),
        call_id: call_id.to_string(),
        before,
        after,
    }
}

/// Build a `ParkedPrompt` row — a steering prompt parked while a
/// run held the session, with everything its recovered turn needs
/// to run under the selection it was submitted under.
#[must_use]
pub fn parked_prompt(
    park_id: String,
    text: String,
    attachments: Vec<AttachmentRefRow>,
    agent_name: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    rerun: bool,
) -> SessionEvent {
    SessionEvent::ParkedPrompt {
        seq: 0,
        ts: String::new(),
        park_id,
        text,
        attachments,
        agent_name,
        provider,
        model,
        rerun,
    }
}

/// Build a `ParkConsumed` row — the durable half of the
/// park/consumer link: the park named by `park_id` left the queue.
#[must_use]
pub fn park_consumed(park_id: String) -> SessionEvent {
    SessionEvent::ParkConsumed {
        seq: 0,
        ts: String::new(),
        park_id,
    }
}

/// Build a `Feedback` row — a user's rating of one assistant
/// message, naming it by the writer-stamped ordinal it carries in
/// decimal.
#[must_use]
pub fn feedback(message_id: String, thumbs_up: bool) -> SessionEvent {
    SessionEvent::Feedback {
        seq: 0,
        ts: String::new(),
        message_id,
        thumbs_up,
    }
}

/// Build the session's `SessionShutdown` marker. The writer stamps
/// `ts` at append time like every row — the caller does not read
/// the clock for it.
#[must_use]
pub fn session_shutdown(reason: SessionEndReason) -> SessionEvent {
    SessionEvent::SessionShutdown {
        seq: 0,
        ts: String::new(),
        data: json!({ "reason": reason }),
    }
}

/// Round-trip a typed event through `serde_json::Value` and
/// back. Builders must produce JSON that survives
/// `from_value` — that is the durability contract.
#[must_use]
pub fn round_trip(event: &SessionEvent) -> Option<SessionEvent> {
    let value = serde_json::to_value(event).ok()?;
    SessionEvent::from_value(&value)
}

/// Extract the `kind` tag from a `Step` / `Turn` / `Iteration`
/// event's data payload, if any. Returns `None` for events
/// that are not structural boundaries.
#[must_use]
pub fn structural_kind(event: &SessionEvent) -> Option<&str> {
    event.boundary_kind()
}

#[cfg(test)]
mod tests;
