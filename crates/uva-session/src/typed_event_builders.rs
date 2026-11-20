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
use uva_provider::TokenUsage;

use crate::{
    events::{
        AttachmentRefRow,
        CompactionLifecycleOutcome,
        SessionEvent,
        SurfaceOp,
        bound_plugin_kind,
        bound_plugin_payload,
    },
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
///
/// `action` names how the step ended: `"completed"`, `"failed"`,
/// `"cancelled"`, `"suspended"`, or the label of the work the step
/// did (`"tools_complete"`, `"tools_truncated"`). A step a crash cut
/// short has no outcome of its own, so the repair closes it with
/// `"interrupted"` — the same word the turn's own reason carries.
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

/// Build a `Turn { end }` event. `reason` is the name the caller
/// gives the outcome: the harness passes the run's own end reason
/// (`uva_events::SessionEndReason::as_str` — `"completed"`,
/// `"cancelled"`, `"error"`, `"max_iterations"`, `"suspended"`,
/// `"config_drift"`), and the crash repair passes
/// [`crate::TURN_INTERRUPTED`] (`"interrupted"`).
///
/// `message` is the failure detail the turn stopped with — why the
/// run gave up. `None` is the ordinary path, and the wire omits the
/// key then.
///
/// The message rides in the payload bag because it has no typed
/// field of its own. The constructor writes it together with the
/// rest of the row, so the row that reaches the log says why the
/// turn stopped and nobody has to patch the payload afterwards.
#[must_use]
pub fn turn_end(
    turn_idx: u32,
    reason: &str,
    message: Option<&str>,
) -> SessionEvent {
    let mut data = json!({
        "kind": "end",
        "turn": turn_idx,
        "reason": reason,
    });
    if let Some(message) = message {
        data["message"] = json!(message);
    }
    SessionEvent::Turn {
        seq: 0,
        ts: String::new(),
        data,
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

#[must_use]
/// Build a `Warning` row — the shared shape every panic-isolated
/// seam (hooks, guards, inbox, tracker, compaction) reports
/// through: `data` is always `{"kind": .., "message": ..}`.
pub fn warning(kind: &str, message: &str) -> SessionEvent {
    SessionEvent::Warning {
        seq: 0,
        ts: String::new(),
        data: json!({ "kind": kind, "message": message }),
    }
}

#[must_use]
/// Build a `ToolResult` row from the tool call's output envelope.
///
/// `data` is the [`uva_provider::Message`] payload (the tool's
/// output, not the failure); `is_error` rides beside it so the row
/// says what the live event says. `tool_call_id` is `None` only for
/// a result that answers no call.
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

/// Build the `tool_result` row a crash-repair writes for a call
/// that never finished.
///
/// The row is not a tool's output — there is none — so its payload
/// says what is known instead: the call it answers, the tool that
/// call named, the recovery code that says how far the call got, and
/// the `interrupted` mark that tells a reader the row was
/// synthesised rather than produced by a run.
///
/// `error_code` is the repair's recovery code: the call never
/// started, or it started and its outcome was never recorded.
///
/// The row joins the surface as an append: it is the answer the
/// model sees for the dangling call, so the transcript the next
/// request is built from is balanced.
#[must_use]
pub(crate) fn tool_result_interrupted(
    call_id: &str,
    tool_name: &str,
    error_code: &str,
) -> SessionEvent {
    SessionEvent::ToolResult {
        seq: 0,
        ts: String::new(),
        data: json!({
            "call_id": call_id,
            "tool_name": tool_name,
            "error": error_code,
            "interrupted": true,
        }),
        // The call never finished, so the row says the tool
        // failed: a reader that trusts the flag must not read
        // this as a call that returned.
        is_error: true,
        tool_call_id: Some(call_id.to_string()),
        surface_op: Some(SurfaceOp::append()),
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

#[must_use]
/// Build the `UserMessage` row a turn's prompt lands as — with its
/// re-readable attachment refs and the [`SurfaceOp`] the fold
/// computed for it: `None` appends, a rerun's `Replace` shadows the
/// turn it replaces.
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

#[must_use]
/// Build a `HookBlock` row — the veto's durable half: which hook,
/// gate or guard refused the call, why, and the call it refused.
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

#[must_use]
/// Build a `ToolResultTransformed` row — one row per rewritten
/// result, carrying the transformers that ran (in registration
/// order) and the digests of the result before and after.
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

#[must_use]
/// Build a `ParkedPrompt` row — a steering prompt parked while a
/// run held the session, with everything its recovered turn needs
/// to run under the selection it was submitted under.
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

#[must_use]
/// Build a `ParkConsumed` row — the durable half of the
/// park/consumer link: the park named by `park_id` left the queue.
pub fn park_consumed(park_id: String) -> SessionEvent {
    SessionEvent::ParkConsumed {
        seq: 0,
        ts: String::new(),
        park_id,
    }
}

#[must_use]
/// Build a `Feedback` row — a user's rating of one assistant
/// message, naming it by the writer-stamped ordinal it carries in
/// decimal.
pub fn feedback(message_id: String, thumbs_up: bool) -> SessionEvent {
    SessionEvent::Feedback {
        seq: 0,
        ts: String::new(),
        message_id,
        thumbs_up,
    }
}

#[must_use]
/// Build the session's `SessionShutdown` marker. The writer stamps
/// `ts` at append time like every row — the caller does not read
/// the clock for it.
pub fn session_shutdown(reason: SessionEndReason) -> SessionEvent {
    SessionEvent::SessionShutdown {
        seq: 0,
        ts: String::new(),
        data: json!({ "reason": reason }),
    }
}

#[must_use]
/// Build a plugin's own domain event — the durable half of the
/// plugin escape hatch.
///
/// The payload is the plugin's own shape: ADR-0009 leaves it free,
/// so the constructor does not type it and does not give it a key
/// set — it passes the plugin's value through and bounds it. A
/// payload over [`crate::events::MAX_PLUGIN_PAYLOAD_BYTES`]
/// (encoded) becomes a JSON string of its leading bytes, and the size
/// it had lands in `original_bytes`; the writer enforces the same
/// bound on every append path, so this is the ergonomic entry point
/// rather than the enforcement point.
///
/// `kind` is a filter key rather than a body: an empty one is
/// recorded as `"unnamed"`, and an over-long one is cut.
///
/// `seq` and `ts` are the caller's to stamp — the writer re-stamps
/// them at append time like every other row.
pub fn plugin_row(
    seq: u64,
    ts: impl Into<String>,
    kind: impl Into<String>,
    payload: Value,
) -> SessionEvent {
    let (payload, original_bytes) = bound_plugin_payload(payload);
    SessionEvent::Plugin {
        seq,
        ts: ts.into(),
        kind: bound_plugin_kind(kind.into()),
        payload,
        original_bytes,
    }
}

#[must_use]
/// Build the `compaction_start` row — the pass's opening audit row.
///
/// `token` correlates the rows of one pass: the start, the summary
/// and the end row carry the same one, so a loader can tell a
/// finished pass from an orphaned one.
///
/// The payload names the span the pass is about to replace: the
/// surface range it shadows (`start` / `end`) and what that span
/// cost (`shadowed_token_count`).
pub fn compaction_start(
    token: &str,
    reason: &str,
    start: usize,
    end: usize,
    shadowed_token_count: usize,
) -> SessionEvent {
    SessionEvent::CompactionStart {
        seq: 0,
        ts: String::new(),
        token: token.to_string(),
        data: json!({
            "reason": reason,
            "start": start,
            "end": end,
            "shadowed_token_count": shadowed_token_count,
        }),
    }
}

#[must_use]
/// Build the `compaction_summary` row — the text the pass produced.
///
/// The row carries the summary alone: the replacement itself is the
/// `compaction` row that follows it.
pub fn compaction_summary(token: &str, summary: &str) -> SessionEvent {
    SessionEvent::CompactionSummary {
        seq: 0,
        ts: String::new(),
        token: token.to_string(),
        data: json!({ "summary": summary }),
    }
}

#[must_use]
/// Build the `compaction` row — the replacement the pass commits.
///
/// The payload has one owner: this constructor writes the six keys
/// the row carries — the summary text, the surface indices it
/// replaces, the token price of the shadowed span, and the provider
/// / model / usage report of the call that wrote the summary — so
/// no caller has to spell them again.
///
/// `surface_op` is the row's typed field and it is not optional: a
/// compaction row *is* a replacement, and the fold refuses one that
/// does not say what it replaces. The constructor does not check
/// that the op and `source_indices` agree — the replace range the
/// op carries must cover exactly the indices named, and the caller
/// is the one who knows both, so the caller keeps them consistent.
/// They agree at every call site today, and `source_indices` has no
/// production reader yet; deriving one from the other is a bigger
/// change than this row needs.
pub fn compaction_row(
    summary: &str,
    source_indices: Vec<usize>,
    shadowed_token_count: usize,
    provider: Option<&str>,
    model: Option<&str>,
    usage: Option<&TokenUsage>,
    surface_op: SurfaceOp,
) -> SessionEvent {
    SessionEvent::Compaction {
        seq: 0,
        ts: String::new(),
        data: json!({
            "summary": summary,
            "source_indices": source_indices,
            "shadowed_token_count": shadowed_token_count,
            "provider": provider,
            "model": model,
            "usage": usage,
        }),
        surface_op,
    }
}

#[must_use]
/// Build the `compaction_end` row of a pass that committed.
///
/// The payload names what the pass shadowed — how many messages
/// (`shadowed_messages`) and what they cost (`shadowed_tokens`) —
/// and why the pass ran (`reason`).
pub fn compaction_end_committed(
    token: &str,
    shadowed_messages: usize,
    shadowed_tokens: usize,
    reason: &str,
) -> SessionEvent {
    SessionEvent::CompactionEnd {
        seq: 0,
        ts: String::new(),
        token: token.to_string(),
        outcome: CompactionLifecycleOutcome::Committed.as_str().to_string(),
        data: json!({
            "shadowed_messages": shadowed_messages,
            "shadowed_tokens": shadowed_tokens,
            "reason": reason,
        }),
    }
}

#[must_use]
/// Build the `compaction_end` row of a pass that did not commit.
///
/// The payload names the failure (`error`) and why the pass ran
/// (`reason`). `outcome` is the lifecycle state the end row
/// carries: `failed` for a summariser that errored, `interrupted`
/// for one that was cancelled.
pub fn compaction_end_failed(
    token: &str,
    outcome: CompactionLifecycleOutcome,
    error: &str,
    reason: &str,
) -> SessionEvent {
    SessionEvent::CompactionEnd {
        seq: 0,
        ts: String::new(),
        token: token.to_string(),
        outcome: outcome.as_str().to_string(),
        data: json!({ "error": error, "reason": reason }),
    }
}

// --- The surface projection's rows ------------------------------------
//
// The raw-log surface projection (`crate::log_surface`) rebuilds a
// row the fold can read: the logged row's payload becomes the
// *surface* payload, and the typed fields the fold reads are the
// projection's own. It is a producer of rows, so it asks here for
// their shape instead of spelling the variants itself — the same
// discipline every other row producer follows.
//
// These four are crate-private: they build a row the fold reads, not
// a row a writer appends, so they are no part of the crate's
// published surface.

/// Build the `user_message` row the surface projection reads back.
///
/// The payload is the surface payload — what the model saw — and not
/// the logged row's envelope. `source` is a row annotation the model
/// surface does not carry, and the attachment refs ride the logged
/// row for a transcript reader, so the projected row carries neither.
#[must_use]
pub(crate) fn projected_user_message(
    seq: u64,
    ts: String,
    data: Value,
    op: SurfaceOp,
) -> SessionEvent {
    SessionEvent::UserMessage {
        seq,
        ts,
        data,
        source: None,
        surface_op: Some(op),
        attachments: Vec::new(),
    }
}

/// Build the `assistant_message` row the surface projection reads
/// back.
///
/// The payload is the surface payload — what the model saw — and not
/// the logged row's envelope.
#[must_use]
pub(crate) fn projected_assistant_message(
    seq: u64,
    ts: String,
    data: Value,
    op: SurfaceOp,
) -> SessionEvent {
    SessionEvent::AssistantMessage {
        seq,
        ts,
        data,
        surface_op: Some(op),
    }
}

/// Build the `tool_result` row the surface projection reads back.
///
/// The payload is the surface payload — the tool's message, not the
/// failure — and the row carries no failure flag: the fold projects
/// the message, and the message is the transcript, not the logged
/// row's metadata.
#[must_use]
pub(crate) fn projected_tool_result(
    seq: u64,
    ts: String,
    data: Value,
    op: SurfaceOp,
) -> SessionEvent {
    SessionEvent::ToolResult {
        seq,
        ts,
        data,
        is_error: false,
        tool_call_id: None,
        surface_op: Some(op),
    }
}

/// Build the `compaction` row the surface projection reads back.
///
/// The payload is the surface payload — the summary that replaces the
/// shadowed span — and the op is the row's own `Replace`.
#[must_use]
pub(crate) fn projected_compaction(
    seq: u64,
    ts: String,
    data: Value,
    op: SurfaceOp,
) -> SessionEvent {
    SessionEvent::Compaction {
        seq,
        ts,
        data,
        surface_op: op,
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
