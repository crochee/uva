//! Resume: token validation, coverage, drift, and the
//! dispatched-but-unresulted crash window.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use synthia_context::AgentState;
use synthia_steering::{
    Action,
    Guard,
    GuardResult,
    HookDecision,
    HookEvent,
    Steering,
};
use synthia_tool::ToolAnnotations;
use tokio_util::sync::CancellationToken;

use super::{
    super::{support::*, *},
    approval::*,
};
use crate::{
    ReActAgent,
    agent::{
        approval::{ResumeDecision, ResumeStatus, ResumeToken},
        host::{Gate, Host},
    },
};

// A gate that asks for every call: the only way a Low-risk call can
// be suspended (the policy itself never asks about Low).
struct AskGate;

#[async_trait]
impl Gate for AskGate {
    fn name(&self) -> &str {
        "ask"
    }

    async fn check(&self, _event: &HookEvent) -> Result<HookDecision, String> {
        Ok(HookDecision::Ask {
            reason: "needs approval".to_string(),
        })
    }
}

// Rewrites every tool call's arguments, so the pre-guard and
// effective calls are observably different.
struct RewriteArgs;

impl Guard for RewriteArgs {
    fn name(&self) -> &str {
        "rewrite-args"
    }

    fn check(&self, action: &Action, _state: &AgentState) -> GuardResult {
        let mut action = action.clone();
        if let Action::ToolCall { arguments, .. } = &mut action {
            *arguments = json!({"rewritten": true});
        }
        GuardResult::Sanitize {
            action,
            warning: "rewritten".to_string(),
        }
    }
}

// The resume window's crash shape: both the decision and the intent
// row are durable, then the process dies before the result. The call
// is no longer pending, so the token's pending set is empty.
async fn simulate_approved_dispatch(
    writer: &synthia_session::LogWriter,
    token: &mut ResumeToken,
    call_id: &str,
    tool: &str,
    arguments_hash: &str,
) {
    let decided = synthia_session::approval_decided(
        &format!("approval-{call_id}"),
        call_id,
        "approved",
        "resume",
        None,
    );
    writer
        .append(serde_json::to_value(&decided).expect("decision serializes"))
        .await
        .expect("append the decision");
    let dispatch =
        synthia_session::tool_dispatch(call_id, tool, arguments_hash);
    let seq = writer
        .append(serde_json::to_value(&dispatch).expect("dispatch serializes"))
        .await
        .expect("append the dispatch");
    writer.flush().await.expect("flush the resume rows");
    token.last_seq = seq;
    token.pending.clear();
}

pub(super) async fn resume_dispatches_the_approved_call_and_continues() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("risky", false, &runs)]);
    let writer = logged_writer().await;
    let first = scripted(vec![call("c1", "risky")]);
    let events = run_agent(
        first,
        Arc::clone(&registry),
        Arc::clone(&writer),
        AgentInput::text("go"),
    )
    .await;
    let (_, token) = suspension_of(&events).expect("suspended");
    assert_eq!(runs.load(Ordering::SeqCst), 0);

    // The suspended run opened its turn and closed it as suspended;
    // the suspended boundary writes no ordinary end.
    let suspended_rows = rows(&writer).await;
    assert_eq!(
        count_turn(&suspended_rows, "start"),
        1,
        "the suspended run writes exactly one turn start"
    );
    assert_eq!(
        count_turn_end(&suspended_rows, "suspended"),
        1,
        "the suspended run writes exactly one suspended turn end"
    );
    assert_eq!(
        suspended_rows
            .iter()
            .filter(|row| row["type"] == "turn" && row["data"]["kind"] == "end")
            .count(),
        0,
        "a suspension writes no ordinary turn end"
    );
    let resumed_from = suspended_rows.len();

    let second =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("done")]));
    let resumed = run_agent(
        second,
        registry,
        Arc::clone(&writer),
        AgentInput::text("continue").resume_approving_all(token),
    )
    .await;

    assert_eq!(
        runs.load(Ordering::SeqCst),
        1,
        "the approved call runs exactly once on resume"
    );
    assert_eq!(end_reason(&resumed), Some(SessionEndReason::Completed));
    assert!(suspension_of(&resumed).is_none());

    let rows = rows(&writer).await;
    let index_of = |predicate: &dyn Fn(&serde_json::Value) -> bool| {
        rows.iter().position(predicate)
    };
    let decided = index_of(&|row| row["type"] == "approval_decided")
        .expect("a decision row");
    let dispatch =
        index_of(&|row| row["type"] == "tool_dispatch").expect("dispatch");
    let result = index_of(&|row| row["type"] == "tool_result").expect("result");
    assert!(decided < dispatch, "the decision precedes the dispatch");
    assert!(dispatch < result, "the dispatch precedes the result");
    assert_eq!(
        rows[dispatch]["call_id"], "c1",
        "the dispatch names the call"
    );
    let resumed_rows = &rows[resumed_from..];
    assert_eq!(
        count_turn(resumed_rows, "start"),
        1,
        "the resumed run writes exactly one turn start"
    );
    // The reopened turn precedes the dispatch intent, so a crash in
    // the window between them (or before the result) still leaves an
    // open turn for repair.
    let reopened = resumed_rows
        .iter()
        .position(|row| row["type"] == "turn" && row["data"]["kind"] == "start")
        .expect("the reopened turn");
    let re_decided = resumed_rows
        .iter()
        .position(|row| row["type"] == "approval_decided")
        .expect("a decision row");
    let re_dispatch = resumed_rows
        .iter()
        .position(|row| row["type"] == "tool_dispatch")
        .expect("dispatch");
    let re_result = resumed_rows
        .iter()
        .position(|row| row["type"] == "tool_result")
        .expect("result");
    assert!(
        reopened < re_decided,
        "the turn reopens before the decision: {resumed_rows:#?}"
    );
    assert!(re_decided < re_dispatch, "the decision precedes dispatch");
    assert!(re_dispatch < re_result, "the dispatch precedes the result");
    assert_eq!(
        count_turn_end(resumed_rows, "completed"),
        1,
        "the resumed run closes the turn it reopened"
    );
}

fn count_turn(rows: &[serde_json::Value], kind: &str) -> usize {
    rows.iter()
        .filter(|row| row["type"] == "turn" && row["data"]["kind"] == kind)
        .count()
}

fn count_turn_end(rows: &[serde_json::Value], reason: &str) -> usize {
    rows.iter()
        .filter(|row| {
            // The ordinary closer carries `kind: end`; the suspended
            // one carries `phase: end`. Both name their reason.
            row["type"] == "turn"
                && (row["data"]["kind"] == "end"
                    || row["data"]["phase"] == "end")
                && row["data"]["reason"] == reason
        })
        .count()
}

pub(super) async fn resume_must_cover_every_open_interrupt() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("risky", false, &runs)]);
    let writer = logged_writer().await;
    let first = scripted(vec![call("c1", "risky"), call("c2", "risky")]);
    let events = run_agent(
        first,
        Arc::clone(&registry),
        Arc::clone(&writer),
        AgentInput::text("go"),
    )
    .await;
    let (interrupts, mut token) = suspension_of(&events).expect("suspended");
    assert_eq!(interrupts.len(), 2);
    assert_eq!(token.pending, vec!["c1".to_string(), "c2".to_string()]);
    token.pending = vec!["c1".to_string()];

    let second =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("done")]));
    let resumed = run_agent(
        second,
        registry,
        writer,
        AgentInput::text("continue").resume_approving_all(token),
    )
    .await;
    match end_reason(&resumed) {
        Some(SessionEndReason::Error(message)) => assert!(
            message.contains("stale resume"),
            "a partial resume is refused: {message}"
        ),
        other => panic!("expected a stale-resume error, got {other:?}"),
    }
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "a refused resume runs nothing"
    );
}

pub(super) async fn a_changed_tool_schema_blocks_resume_with_config_drift() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("risky", false, &runs)]);
    let writer = logged_writer().await;
    let first = scripted(vec![call("c1", "risky")]);
    let events = run_agent(
        first,
        Arc::clone(&registry),
        Arc::clone(&writer),
        AgentInput::text("go"),
    )
    .await;
    let (_, token) = suspension_of(&events).expect("suspended");

    // The name set is identical; only the schema changed.
    let changed = build_registry(vec![tool_with_schema(
        "risky",
        false,
        &runs,
        json!({
            "type": "object",
            "properties": {"new": {"type": "string"}},
        }),
    )]);
    let drift_provider =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("done")]));
    let drifted = run_agent(
        drift_provider,
        Arc::clone(&changed),
        Arc::clone(&writer),
        AgentInput::text("continue").resume_approving_all(token.clone()),
    )
    .await;
    assert_eq!(
        end_reason(&drifted),
        Some(SessionEndReason::ConfigDrift),
        "a changed tool schema blocks the resume"
    );
    assert_eq!(runs.load(Ordering::SeqCst), 0);

    // The explicit flag is the caller taking responsibility.
    let accepted_provider =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("done")]));
    let accepted = run_agent(
        accepted_provider,
        changed,
        Arc::clone(&writer),
        AgentInput::text("continue")
            .resume_approving_all(token)
            .allow_drift(),
    )
    .await;
    assert_eq!(end_reason(&accepted), Some(SessionEndReason::Completed));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let logged = rows(&writer).await;
    assert!(
        logged.iter().any(|row| {
            row["type"] == "warning" && row["data"]["kind"] == "config_drift"
        }),
        "an accepted drift must leave a durable warning row: {logged:#?}"
    );
}

pub(super) async fn resume_refuses_a_dispatched_high_risk_call() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("risky", false, &runs)]);
    let writer = logged_writer().await;
    let first = scripted(vec![call("c1", "risky")]);
    let events = run_agent(
        first,
        Arc::clone(&registry),
        Arc::clone(&writer),
        AgentInput::text("go"),
    )
    .await;
    let (interrupts, mut token) = suspension_of(&events).expect("suspended");
    simulate_approved_dispatch(
        &writer,
        &mut token,
        "c1",
        "risky",
        &interrupts[0].arguments_hash,
    )
    .await;

    let second =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("done")]));
    let resumed = run_agent(
        second,
        registry,
        writer,
        AgentInput::text("continue").resume_approving_all(token),
    )
    .await;
    match end_reason(&resumed) {
        Some(SessionEndReason::Error(message)) => assert!(
            message.contains("outcome is unknown"),
            "the lost outcome is named: {message}"
        ),
        other => panic!("expected an unknown-outcome error, got {other:?}"),
    }
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "a High call is never re-run"
    );
}

pub(super) async fn resume_retries_a_low_idempotent_dispatched_call() {
    let runs = Arc::new(AtomicUsize::new(0));
    let read = Arc::new(ProbeTool {
        name: "read",
        runs: Arc::clone(&runs),
        annotations: Some(ToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
        }),
        schema: json!({"type": "object"}),
    });
    let registry = build_registry(vec![read]);
    let writer = logged_writer().await;
    let first = scripted(vec![call("c1", "read")]);
    let mut host = Host::new();
    let _gate = host.gate(Arc::new(AskGate));
    let agent = ReActAgent::new(first, Arc::clone(&registry))
        .with_log_writer(Arc::clone(&writer))
        .with_host(host);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    let (interrupts, mut token) =
        suspension_of(&events).expect("the gate suspends");
    simulate_approved_dispatch(
        &writer,
        &mut token,
        "c1",
        "read",
        &interrupts[0].arguments_hash,
    )
    .await;

    let second =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("done")]));
    // Resume without the gate: the approved call runs directly.
    let resumed = run_agent(
        second,
        registry,
        writer,
        AgentInput::text("continue").resume_approving_all(token),
    )
    .await;
    assert_eq!(runs.load(Ordering::SeqCst), 1, "Low + idempotent retries");
    assert_eq!(end_reason(&resumed), Some(SessionEndReason::Completed));
}

pub(super) async fn a_host_ask_approves_the_guarded_arguments() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("echo", true, &runs)]);
    let writer = logged_writer().await;
    let steering = Arc::new(Steering::noop().add_guard(Arc::new(RewriteArgs)));
    let mut host = Host::new();
    let _gate = host.gate(Arc::new(AskGate));
    let first = scripted(vec![call("c1", "echo")]);
    let agent = ReActAgent::new(first, Arc::clone(&registry))
        .with_log_writer(Arc::clone(&writer))
        .with_host(host)
        .with_steering(Arc::clone(&steering));
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    let mut events = Vec::new();
    while let Some(event) = stream.next().await {
        events.push(event);
    }
    let (interrupts, token) = suspension_of(&events).expect("the ask holds");
    let approved_hash = interrupts[0].arguments_hash.clone();
    // The raw call has `{}` as its input; the request must carry the
    // rewritten call's hash instead.
    assert_ne!(
        approved_hash,
        "sha256:44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a",
        "the approval must not hash the pre-guard arguments"
    );

    let second =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("done")]));
    let resumed_agent = ReActAgent::new(second, registry)
        .with_log_writer(Arc::clone(&writer))
        .with_steering(steering);
    let mut stream = resumed_agent
        .run(
            AgentInput::text("continue").resume_approving_all(token),
            Arc::new(CancellationToken::new()),
        )
        .await;
    let mut resumed = Vec::new();
    while let Some(event) = stream.next().await {
        resumed.push(event);
    }
    assert_eq!(end_reason(&resumed), Some(SessionEndReason::Completed));
    assert_eq!(runs.load(Ordering::SeqCst), 1);

    let rows = rows(&writer).await;
    let dispatch = rows
        .iter()
        .rev()
        .find(|row| row["type"] == "tool_dispatch")
        .expect("the resume dispatched the approved call");
    assert_eq!(
        dispatch["arguments_hash"], approved_hash,
        "the dispatch records the same effective hash the approval did"
    );
}

pub(super) async fn a_fresh_run_over_open_approvals_is_refused() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("risky", false, &runs)]);
    let writer = logged_writer().await;
    let first = scripted(vec![call("c1", "risky")]);
    let events = run_agent(
        first,
        Arc::clone(&registry),
        Arc::clone(&writer),
        AgentInput::text("go"),
    )
    .await;
    assert!(suspension_of(&events).is_some());

    let second =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("done")]));
    let fresh = run_agent(
        second,
        registry,
        writer,
        AgentInput::text("a different prompt"),
    )
    .await;
    match end_reason(&fresh) {
        Some(SessionEndReason::Error(message)) => assert!(
            message.contains("stale approval"),
            "the open approval is not silently skipped: {message}"
        ),
        other => panic!("expected a stale-approval error, got {other:?}"),
    }
    assert_eq!(runs.load(Ordering::SeqCst), 0);
}
// Task 9: the client supplies one decision per interrupt. A rejected
// call writes the model-visible failure instead of executing; a
// resolved one executes exactly once.
pub(super) async fn a_rejected_interrupt_writes_an_is_error_result() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("risky", false, &runs)]);
    let writer = logged_writer().await;
    let first = scripted(vec![call("c1", "risky")]);
    let events = run_agent(
        first,
        Arc::clone(&registry),
        Arc::clone(&writer),
        AgentInput::text("go"),
    )
    .await;
    let (interrupts, token) = suspension_of(&events).expect("suspended");

    let second =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("done")]));
    let resumed = run_agent(
        second,
        registry,
        Arc::clone(&writer),
        AgentInput::text("continue").resume_decisions(
            token,
            vec![ResumeDecision {
                id: interrupts[0].id.clone(),
                status: ResumeStatus::Rejected,
                payload: Some(json!({"reason": "not on my watch"})),
            }],
        ),
    )
    .await;

    assert_eq!(runs.load(Ordering::SeqCst), 0, "a rejected call never runs");
    assert_eq!(end_reason(&resumed), Some(SessionEndReason::Completed));
    let rows = rows(&writer).await;
    assert!(
        !rows.iter().any(|row| row["type"] == "tool_dispatch"),
        "a rejected call writes no dispatch intent"
    );
    let decided = rows
        .iter()
        .find(|row| row["type"] == "approval_decided")
        .expect("the decision is durable");
    assert_eq!(decided["status"], "denied");
    let result = rows
        .iter()
        .find(|row| row["type"] == "tool_result")
        .expect("the model sees a result");
    assert_eq!(result["is_error"], json!(true));
    assert!(
        result["data"]["content"]
            .to_string()
            .contains("not on my watch"),
        "the rejection reason reaches the model: {}",
        result["data"]
    );
}

pub(super) async fn a_resolved_decision_executes_exactly_once() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("risky", false, &runs)]);
    let writer = logged_writer().await;
    let first = scripted(vec![call("c1", "risky")]);
    let events = run_agent(
        first,
        Arc::clone(&registry),
        Arc::clone(&writer),
        AgentInput::text("go"),
    )
    .await;
    let (interrupts, token) = suspension_of(&events).expect("suspended");

    let second =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("done")]));
    let resumed = run_agent(
        second,
        registry,
        Arc::clone(&writer),
        AgentInput::text("continue").resume_decisions(
            token,
            vec![ResumeDecision {
                id: interrupts[0].id.clone(),
                status: ResumeStatus::Resolved,
                payload: None,
            }],
        ),
    )
    .await;

    assert_eq!(runs.load(Ordering::SeqCst), 1, "exactly one execution");
    assert_eq!(end_reason(&resumed), Some(SessionEndReason::Completed));
    let rows = rows(&writer).await;
    assert_eq!(
        rows.iter()
            .filter(|row| row["type"] == "tool_dispatch")
            .count(),
        1,
        "one dispatch intent per resolved call"
    );
    assert_eq!(
        rows.iter()
            .filter(|row| row["type"] == "tool_result")
            .count(),
        1
    );
}

// Partial coverage is a protocol error, not a silent default: the
// undecided call must not inherit the decided one's status.
pub(super) async fn resume_decisions_must_cover_every_open_interrupt() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("risky", false, &runs)]);
    let writer = logged_writer().await;
    let first = scripted(vec![call("c1", "risky"), call("c2", "risky")]);
    let events = run_agent(
        first,
        Arc::clone(&registry),
        Arc::clone(&writer),
        AgentInput::text("go"),
    )
    .await;
    let (interrupts, token) = suspension_of(&events).expect("suspended");

    let second =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("done")]));
    let resumed = run_agent(
        second,
        registry,
        writer,
        AgentInput::text("continue").resume_decisions(
            token,
            vec![ResumeDecision {
                id: interrupts[0].id.clone(),
                status: ResumeStatus::Resolved,
                payload: None,
            }],
        ),
    )
    .await;
    match end_reason(&resumed) {
        Some(SessionEndReason::Error(message)) => assert!(
            message.contains("does not answer") || message.contains("missing"),
            "the partial coverage is named: {message}"
        ),
        other => panic!("expected a coverage error, got {other:?}"),
    }
    assert_eq!(runs.load(Ordering::SeqCst), 0);
}

// An empty decision list is not a wildcard: an open interrupt with no
// answer is refused, and nothing runs.
pub(super) async fn resume_decisions_empty_list_is_refused() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("risky", false, &runs)]);
    let writer = logged_writer().await;
    let first = scripted(vec![call("c1", "risky")]);
    let events = run_agent(
        first,
        Arc::clone(&registry),
        Arc::clone(&writer),
        AgentInput::text("go"),
    )
    .await;
    let (_, token) = suspension_of(&events).expect("suspended");

    let second =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("done")]));
    let resumed = run_agent(
        second,
        registry,
        writer,
        AgentInput::text("continue").resume_decisions(token, Vec::new()),
    )
    .await;
    match end_reason(&resumed) {
        Some(SessionEndReason::Error(message)) => assert!(
            message.contains("does not answer"),
            "the missing answer is named: {message}"
        ),
        other => panic!("expected a coverage error, got {other:?}"),
    }
    assert_eq!(
        runs.load(Ordering::SeqCst),
        0,
        "an empty decision list runs nothing"
    );
}

pub(super) async fn resume_decisions_reject_an_unknown_interrupt() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("risky", false, &runs)]);
    let writer = logged_writer().await;
    let first = scripted(vec![call("c1", "risky")]);
    let events = run_agent(
        first,
        Arc::clone(&registry),
        Arc::clone(&writer),
        AgentInput::text("go"),
    )
    .await;
    let (_, token) = suspension_of(&events).expect("suspended");

    let second =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("done")]));
    let resumed = run_agent(
        second,
        registry,
        writer,
        AgentInput::text("continue").resume_decisions(
            token,
            vec![ResumeDecision {
                id: "not-an-interrupt".to_string(),
                status: ResumeStatus::Resolved,
                payload: None,
            }],
        ),
    )
    .await;
    match end_reason(&resumed) {
        Some(SessionEndReason::Error(message)) => assert!(
            message.contains("not-an-interrupt"),
            "the unknown id is named: {message}"
        ),
        other => panic!("expected an unknown-interrupt error, got {other:?}"),
    }
}

// The wire lists decisions in client order. Applying them by position
// would let an answer for one call authorize another — the exact
// failure this test pins: reject c2, resolve c1, submitted in that
// order.
pub(super) async fn resume_decisions_apply_by_id_not_by_position() {
    let first_runs = Arc::new(AtomicUsize::new(0));
    let second_runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![
        tool("first", false, &first_runs),
        tool("second", false, &second_runs),
    ]);
    let writer = logged_writer().await;
    let first = scripted(vec![call("c1", "first"), call("c2", "second")]);
    let events = run_agent(
        first,
        Arc::clone(&registry),
        Arc::clone(&writer),
        AgentInput::text("go"),
    )
    .await;
    let (interrupts, token) = suspension_of(&events).expect("suspended");
    assert_eq!(interrupts.len(), 2);

    let second =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("done")]));
    let resumed = run_agent(
        second,
        registry,
        Arc::clone(&writer),
        AgentInput::text("continue").resume_decisions(
            token,
            vec![
                ResumeDecision {
                    id: interrupts[1].id.clone(),
                    status: ResumeStatus::Rejected,
                    payload: Some(json!({"reason": "not this one"})),
                },
                ResumeDecision {
                    id: interrupts[0].id.clone(),
                    status: ResumeStatus::Resolved,
                    payload: None,
                },
            ],
        ),
    )
    .await;

    assert_eq!(
        first_runs.load(Ordering::SeqCst),
        1,
        "the resolved call (c1) ran"
    );
    assert_eq!(
        second_runs.load(Ordering::SeqCst),
        0,
        "the rejected call (c2) did not run"
    );
    assert_eq!(end_reason(&resumed), Some(SessionEndReason::Completed));
    let rows = rows(&writer).await;
    let decided = |call_id: &str| {
        rows.iter()
            .find(|row| {
                row["type"] == "approval_decided" && row["call_id"] == call_id
            })
            .unwrap_or_else(|| panic!("a decision for {call_id}: {rows:?}"))
    };
    assert_eq!(decided("c1")["status"], "approved");
    assert_eq!(decided("c2")["status"], "denied");
    assert_eq!(
        decided("c2")["payload"]["reason"],
        "not this one",
        "the payload follows its own decision, not its position"
    );
    let dispatched: Vec<&str> = rows
        .iter()
        .filter(|row| row["type"] == "tool_dispatch")
        .filter_map(|row| row["call_id"].as_str())
        .collect();
    assert_eq!(dispatched, vec!["c1"], "only the resolved call dispatched");
    let denied_result = rows
        .iter()
        .find(|row| {
            row["type"] == "tool_result" && row["data"]["tool_call_id"] == "c2"
        })
        .expect("the rejected call's result");
    assert_eq!(denied_result["is_error"], json!(true));
}

#[tokio::test]
async fn a_session_scope_decision_writes_a_replayable_grant_row() {
    let runs = Arc::new(AtomicUsize::new(0));
    let registry = build_registry(vec![tool("risky", false, &runs)]);
    let writer = logged_writer().await;
    let first = scripted(vec![call("c1", "risky")]);
    let events = run_agent(
        first,
        Arc::clone(&registry),
        Arc::clone(&writer),
        AgentInput::text("go"),
    )
    .await;
    let (interrupts, token) = suspension_of(&events).expect("suspended");
    let request = interrupts.first().expect("one interrupt");

    let second =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("done")]));
    let decision = ResumeDecision {
        id: request.id.clone(),
        status: ResumeStatus::Resolved,
        payload: Some(json!({ "scope": "session" })),
    };
    let resumed = run_agent(
        second,
        Arc::clone(&registry),
        Arc::clone(&writer),
        AgentInput::text("continue").resume_decisions(token, vec![decision]),
    )
    .await;

    assert_eq!(end_reason(&resumed), Some(SessionEndReason::Completed));
    assert_eq!(runs.load(Ordering::SeqCst), 1);
    let rows = rows(&writer).await;
    let grant = rows
        .iter()
        .find(|row| row["type"] == "approval_granted")
        .expect("a session-scope decision writes a grant row");
    assert_eq!(grant["tool"], "risky");
    assert_eq!(grant["scope"], "session");
    assert!(
        grant["workspace_hash"]
            .as_str()
            .is_some_and(|hash| !hash.is_empty()),
        "the grant is bound to a workspace"
    );
    assert_eq!(grant["id"], format!("grant-{}", request.id));

    // A fresh run over the same log folds the grant in setup: the same
    // high-risk call must run without asking again.
    let third = scripted(vec![call("c2", "risky")]);
    let events = run_agent(
        third,
        Arc::clone(&registry),
        Arc::clone(&writer),
        AgentInput::text("again"),
    )
    .await;
    assert!(
        suspension_of(&events).is_none(),
        "the replayed session grant covers the next run"
    );
    assert_eq!(
        runs.load(Ordering::SeqCst),
        2,
        "the second risky call runs without a new approval"
    );
}
