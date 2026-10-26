//! Resume of a suspended run: validate the token against the log,
//! dispatch the approved calls, and reopen the turn.
//!
//! The server does not run tools. It hands the token back; the loop
//! checks that the log still says what the token says, writes the
//! decision and dispatch rows, flushes them, and only then executes.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde_json::Value;
use synthia_provider::Message;
use synthia_tool::ToolOutput;
use tracing::warn;

use super::{
    ReActLoop,
    TURN_ORDINAL,
    guard,
    prepare,
    tools::commit_tool_result,
};
use crate::{
    agent::{
        approval::{
            ApprovalRequest,
            GrantKey,
            GrantScope,
            ResumeDecision,
            ResumeStatus,
            ResumeToken,
            align_decisions,
            may_retry,
            open_requests,
            suspended_segment,
            wants_session_grant,
        },
        re_act::state::RunState,
    },
    events::SessionEndReason,
};

pub(super) async fn resume_suspended(
    this: &mut ReActLoop,
    session: &mut RunState,
    token: &ResumeToken,
    decisions: &[ResumeDecision],
    allow_drift: bool,
    current_snapshot_hash: &str,
) -> Result<(), SessionEndReason> {
    let writer = this.caps.env.log_writer.clone().ok_or_else(|| {
        SessionEndReason::Error(
            "resume requires a session log writer".to_string(),
        )
    })?;
    let rows = writer.read().await.map_err(|error| {
        SessionEndReason::Error(format!("session log read failed: {error}"))
    })?;
    let validated = validate(&rows, token, allow_drift, current_snapshot_hash)?;
    // An accepted drift is a durable fact, not just a tracing line: the
    // later review of this session must see that the resume ran under a
    // different envelope.
    if let Some((logged, current)) = &validated.drift {
        this.emit_typed(synthia_session::SessionEvent::Warning {
            seq: 0,
            ts: String::new(),
            data: serde_json::json!({
                "kind": "config_drift",
                "message": format!(
                    "resume accepted a changed request envelope: log \
                     {logged}, current {current}"
                ),
            }),
        });
    }
    let requests = validated.requests;
    // Every interrupt needs its own answer: an empty or partial list is
    // refused by `align_decisions`, never widened to "approve all". A
    // caller that means approve-all sends one `Resolved` decision per
    // request (`AgentInput::resume_approving_all` does exactly that).
    // Bind each decision to its request BY ID. The wire is a list, and
    // its order is the client's: position must never pick the call an
    // answer authorizes.
    let decisions = align_decisions(&requests, decisions).map_err(|error| {
        SessionEndReason::Error(format!("invalid resume: {error}"))
    })?;
    let recorded = recorded_calls(&rows);
    let mut plan: Vec<(synthia_provider::ToolUse, Guarded)> =
        Vec::with_capacity(requests.len());
    apply_decisions(
        this, session, &recorded, &requests, &decisions, &mut plan,
    )?;
    // A crash *inside* the resume window leaves a decided+dispatched
    // call that is no longer pending. Scan the whole suspended
    // segment, not just the pending set, or an empty-pending token
    // would continue with an unanswered `tool_use`.
    let pending_ids: BTreeSet<&str> =
        requests.iter().map(|r| r.call_id.as_str()).collect();
    plan.extend(retry_unanswered(
        this,
        session,
        &rows,
        &recorded,
        &pending_ids,
    )?);
    // Reopen the turn *before* the dispatch intent. A crash between
    // the dispatch flush and its result then leaves an open turn,
    // exactly like the ordinary mid-call crash window, so repair can
    // close it instead of the next resume failing on an unbalanced
    // tail.
    this.emit_typed(synthia_session::turn_start(TURN_ORDINAL));
    this.turn_open = true;
    for (request, decision) in requests.iter().zip(&decisions) {
        writer.append_event_nowait(&synthia_session::approval_decided(
            &request.id,
            &request.call_id,
            decision.durable_status(),
            "resume",
            decision.payload.clone(),
        ));
        if decision.status == ResumeStatus::Resolved
            && wants_session_grant(decision.payload.as_ref())
        {
            writer.append_event_nowait(&synthia_session::approval_granted(
                &format!("grant-{}", request.id),
                &request.tool,
                "session",
                &this.workspace_hash,
            ));
            session.approval.grants.insert(GrantKey {
                tool: request.tool.clone(),
                workspace_hash: this.workspace_hash.clone(),
                scope: GrantScope::Session,
            });
        }
    }
    for (_, entry) in &plan {
        if let Guarded::Run(call) = entry {
            writer.append_event_nowait(&synthia_session::tool_dispatch(
                &call.id,
                &call.name,
                &guard::arguments_hash(&call.input),
            ));
        }
    }
    // The decision and the dispatch intent must be durable before
    // the side effect they authorize.
    this.flush_log().await?;
    execute_plan(this, session, &plan).await?;
    this.flush_log().await
}

// A call the guards may have rewritten (the approved call) or vetoed
// since the approval was recorded.
enum Guarded {
    Run(synthia_provider::ToolUse),
    Denied(ToolOutput),
}

// Turn each decision into the plan entry it authorizes. A resolved
// call runs through the guards again, so the call that executes is
// the one the approval was hashed against; a cancelled/rejected call
// writes the model-visible failure and no dispatch intent.
fn apply_decisions(
    this: &ReActLoop,
    session: &RunState,
    recorded: &HashMap<String, synthia_provider::ToolUse>,
    requests: &[ApprovalRequest],
    decisions: &[ResumeDecision],
    plan: &mut Vec<(synthia_provider::ToolUse, Guarded)>,
) -> Result<(), SessionEndReason> {
    for (request, decision) in requests.iter().zip(decisions) {
        let call =
            recorded.get(&request.call_id).cloned().ok_or_else(|| {
                stale(format!(
                    "stale resume: no recorded tool call for {}",
                    request.call_id
                ))
            })?;
        if !decision.executes() {
            plan.push((
                call,
                Guarded::Denied(ToolOutput::error(decision.denial_message())),
            ));
            continue;
        }
        match guard::apply_guard_pipeline(this, &call, &session.agent_state) {
            guard::Verdict::Allowed(rewritten) => {
                let effective = rewritten.into_owned();
                if effective.name != request.tool
                    || guard::arguments_hash(&effective.input)
                        != request.arguments_hash
                {
                    return Err(stale(format!(
                        "stale resume: the approval for {} does not match \
                         the guarded call",
                        request.call_id
                    )));
                }
                plan.push((call, Guarded::Run(effective)));
            }
            guard::Verdict::Denied(output) => {
                plan.push((call, Guarded::Denied(output)));
            }
        }
    }
    Ok(())
}

fn validate(
    rows: &[Value],
    token: &ResumeToken,
    allow_drift: bool,
    current_snapshot_hash: &str,
) -> Result<ValidatedResume, SessionEndReason> {
    let tail = rows
        .iter()
        .filter_map(|row| row.get("seq").and_then(Value::as_u64))
        .max()
        .unwrap_or(0);
    if tail != token.last_seq {
        return Err(stale(format!(
            "stale resume: token ends at {} but the log ends at {tail}",
            token.last_seq
        )));
    }
    let open = open_requests(rows);
    let mut logged_ids: Vec<String> =
        open.iter().map(|request| request.call_id.clone()).collect();
    logged_ids.sort();
    let mut expected = token.pending.clone();
    expected.sort();
    expected.dedup();
    if logged_ids != expected {
        return Err(stale(format!(
            "stale resume: token covers {expected:?} but the log holds \
             {logged_ids:?}"
        )));
    }
    let logged = rows
        .iter()
        .rev()
        .find_map(|row| {
            synthia_session::SessionEvent::from_value(row)
                .as_ref()
                .and_then(prepare::request_snapshot_hash)
        })
        .ok_or_else(|| {
            stale("stale resume: the log has no request snapshot")
        })?;
    if logged != token.request_snapshot_hash || logged != current_snapshot_hash
    {
        if !allow_drift {
            return Err(SessionEndReason::ConfigDrift);
        }
        warn!(
            logged = %logged,
            current = %current_snapshot_hash,
            "resume accepted a changed request envelope (allow_drift)"
        );
        return Ok(ValidatedResume {
            requests: open,
            drift: Some((logged, current_snapshot_hash.to_string())),
        });
    }
    Ok(ValidatedResume {
        requests: open,
        drift: None,
    })
}

// A validated resume: the interrupts to answer, and the envelope
// change `allow_drift` accepted (recorded as a durable row).
struct ValidatedResume {
    requests: Vec<ApprovalRequest>,
    // `(logged, current)` fingerprints when the caller accepted drift.
    drift: Option<(String, String)>,
}

// Open interrupts belong to the last suspended turn: an `asked` row
// from an abandoned suspension must not be re-approved by a later
// resume.
pub(super) fn has_open_requests(rows: &[Value]) -> bool {
    !open_requests(rows).is_empty()
}

fn recorded_calls(
    rows: &[Value],
) -> HashMap<String, synthia_provider::ToolUse> {
    let mut recorded: HashMap<String, synthia_provider::ToolUse> =
        HashMap::new();
    for row in rows {
        let Some(synthia_session::SessionEvent::AssistantMessage {
            data, ..
        }) = synthia_session::SessionEvent::from_value(row)
        else {
            continue;
        };
        let Ok(message) = serde_json::from_value::<Message>(data) else {
            continue;
        };
        for call in message.content.extract_tool_uses() {
            recorded.insert(call.id.clone(), call);
        }
    }
    recorded
}

// Every dispatch in the suspended segment with no result: the
// resume window's crash shape. High/Unknown is refused; Low +
// idempotent is re-run as a new attempt.
fn retry_unanswered(
    this: &ReActLoop,
    session: &RunState,
    rows: &[Value],
    recorded: &HashMap<String, synthia_provider::ToolUse>,
    pending_ids: &BTreeSet<&str>,
) -> Result<Vec<(synthia_provider::ToolUse, Guarded)>, SessionEndReason> {
    let Some(segment) = suspended_segment(rows) else {
        return Ok(Vec::new());
    };
    let dispatched = dispatched_calls(segment);
    let completed = completed_calls(segment);
    let hashes = dispatch_hashes(segment);
    let mut retries = Vec::new();
    for call_id in dispatched.difference(&completed) {
        if pending_ids.contains(call_id.as_str()) {
            continue;
        }
        let call = recorded.get(call_id).cloned().ok_or_else(|| {
            stale(format!("stale resume: no recorded tool call for {call_id}"))
        })?;
        if !may_retry(
            guard::risk_of(this, &call),
            guard::idempotent_hint_of(this, &call),
        ) {
            return Err(stale(format!(
                "call {call_id} was dispatched and its outcome is \
                 unknown; refusing to re-run it"
            )));
        }
        match guard::apply_guard_pipeline(this, &call, &session.agent_state) {
            guard::Verdict::Allowed(rewritten) => {
                let effective = rewritten.into_owned();
                let expected = hashes.get(call_id).ok_or_else(|| {
                    stale(format!(
                        "stale resume: no dispatch hash for {call_id}"
                    ))
                })?;
                if guard::arguments_hash(&effective.input) != *expected {
                    return Err(stale(format!(
                        "stale resume: the dispatch for {call_id} does not \
                         match the guarded call"
                    )));
                }
                retries.push((call, Guarded::Run(effective)));
            }
            guard::Verdict::Denied(output) => {
                retries.push((call, Guarded::Denied(output)));
            }
        }
    }
    Ok(retries)
}

fn dispatch_hashes(rows: &[Value]) -> BTreeMap<String, String> {
    let mut hashes = BTreeMap::new();
    for row in rows {
        let Some(synthia_session::SessionEvent::ToolDispatch {
            call_id,
            arguments_hash,
            ..
        }) = synthia_session::SessionEvent::from_value(row)
        else {
            continue;
        };
        hashes.insert(call_id, arguments_hash);
    }
    hashes
}

fn dispatched_calls(rows: &[Value]) -> BTreeSet<String> {
    rows.iter()
        .filter_map(|row| {
            let event = synthia_session::SessionEvent::from_value(row)?;
            match event {
                synthia_session::SessionEvent::ToolDispatch {
                    call_id, ..
                } => Some(call_id),
                _ => None,
            }
        })
        .collect()
}

fn completed_calls(rows: &[Value]) -> BTreeSet<String> {
    rows.iter()
        .filter_map(|row| {
            let event = synthia_session::SessionEvent::from_value(row)?;
            let synthia_session::SessionEvent::ToolResult { data, .. } = event
            else {
                return None;
            };
            data.get("tool_call_id")
                .or_else(|| data.get("call_id"))
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .collect()
}

async fn execute_plan(
    this: &ReActLoop,
    session: &mut RunState,
    plan: &[(synthia_provider::ToolUse, Guarded)],
) -> Result<(), SessionEndReason> {
    for (raw, call) in plan {
        let (output, touched, terminate) = match call {
            Guarded::Run(call) => {
                let outcome = {
                    let state = &session.agent_state;
                    guard::execute_gated(this, call, state).await
                };
                (outcome.output, outcome.touched, outcome.terminate)
            }
            // A guard vetoed a call the operator approved: the veto
            // is the result, and the call does not run.
            Guarded::Denied(output) => (output.clone(), None, None),
        };
        if let Some(pair) = touched {
            if let Some(path) = pair.0 {
                session.touched.read_files.push(path);
            }
            if let Some(path) = pair.1 {
                session.touched.modified_files.push(path);
            }
        }
        if session.terminate.is_none() {
            session.terminate = terminate;
        }
        commit_tool_result(this, session, raw, output).await;
    }
    this.flush_log().await
}

fn stale(message: impl Into<String>) -> SessionEndReason {
    SessionEndReason::Error(message.into())
}
