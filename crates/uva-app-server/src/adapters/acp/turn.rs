//! One turn, from the client's prompt to the stop reason.
//!
//! The turn is the same turn the core's own surfaces run: the
//! prompt is parked durably, the run publishes its deltas on the
//! session's live channel, and the approval gate holds a risky call
//! until the operator decides. This module is the projection of that
//! machinery onto the protocol: the deltas become session updates,
//! the held call becomes a permission question, and the run's end
//! becomes the stop reason.
//!
//! # The order that matters
//!
//! A held call ends the run *before* the operator is asked: the
//! harness writes the approval rows, suspends the turn and settles
//! the run. The permission question therefore follows the turn's
//! end, and the resume that follows the answer continues the turn.
//! Asking earlier would race the controller, which refuses a resume
//! while a run is still live — the answer would be dropped and the
//! turn would never continue.

use std::sync::Arc;

use agent_client_protocol::{
    Client,
    Error,
    v1::schema::{
        PermissionOption,
        PermissionOptionId,
        PermissionOptionKind,
        RequestPermissionOutcome,
        RequestPermissionRequest,
        RequestPermissionResponse,
        SessionNotification,
        StopReason,
        ToolCallId,
        ToolCallUpdate,
        ToolCallUpdateFields,
    },
};
use tracing::{debug, info, warn};
use uuid::Uuid;
use uva::{
    harness::{
        AgentEvent,
        ApprovalRequest,
        ResumeDecision,
        ResumeStatus,
        SessionEndReason,
        Suspension,
        SystemEvent,
    },
    provider::ContentPart,
};

use crate::{
    adapters::acp::{agent::AcpAgent, projection},
    event_stream::BroadcastEvent,
    routes::follow::{InterruptDecision, ResumeRequest},
    session::controller::SessionOp,
};

/// The permission options the agent offers.
///
/// The protocol's four kinds are the four decisions an operator can
/// take; this deployment offers the two that are one-shot, because a
/// remembered decision belongs to the deployment's own approval
/// policy, not to the protocol's answer.
fn permission_options() -> Vec<PermissionOption> {
    vec![
        PermissionOption::new(
            PermissionOptionId::new("allow_once"),
            "Allow once",
            PermissionOptionKind::AllowOnce,
        ),
        PermissionOption::new(
            PermissionOptionId::new("reject_once"),
            "Reject",
            PermissionOptionKind::RejectOnce,
        ),
    ]
}

/// Run one turn and answer with its stop reason.
///
/// # Errors
///
/// The turn's own failure is an error: a run that ended with an
/// error has no stop reason to report, so the request is answered
/// with the failure instead.
pub async fn run(
    agent: &Arc<AcpAgent>,
    cx: &agent_client_protocol::v1::ConnectionTo<Client>,
    request: &agent_client_protocol::v1::schema::PromptRequest,
) -> Result<StopReason, Error> {
    let state = Arc::clone(&agent.state);
    let session_id = request.session_id.to_string();
    let user_id = state.default_user_id().to_string();
    // The client's working directory is the session's, but the
    // deployment's workspace root is where the tools run: the
    // difference is worth a line in the log, because a client that
    // expects its own directory will otherwise be surprised.
    let record = agent.session(&session_id);
    debug!(
        session_id = %session_id,
        client_cwd = ?record.cwd,
        tool_root = %state.workspace_root.display(),
        "the ACP turn runs in the deployment's workspace root"
    );
    let parts = projection::prompt_parts(&request.prompt)
        .map_err(|message| Error::invalid_params().data(message))?;
    if parts.is_empty() {
        return Err(Error::invalid_params()
            .data("a prompt needs at least one content block"));
    }
    let controller = state
        .create_on_demand_session_controller(&user_id, &session_id)
        .await
        .map_err(|error| {
            Error::internal_error()
                .data(format!("the session could not be opened: {error}"))
        })?;
    // The turn's id is minted here and travels with the turn: it is
    // the parked prompt's id, and it tags every live event of the
    // turn, so the pump can tell this turn's events from the tail of
    // an earlier one.
    let turn_id = Uuid::new_v4().to_string();
    let mut events = controller.subscribe();
    let op = op_for(parts);
    controller
        .submit_durable_turn(op, None, Some(turn_id.clone()))
        .await
        .map_err(|error| {
            Error::internal_error()
                .data(format!("the turn could not be parked: {error}"))
        })?;
    let mut suspension: Option<Suspension> = None;
    let mut hit_budget = false;
    // The message the current chunks belong to. One model response is
    // one assistant message: the id is minted with its first chunk
    // and dropped when that response ends, so the next one starts a
    // new message.
    let mut message_id: Option<String> = None;
    loop {
        match events.recv().await {
            Ok(BroadcastEvent::Run {
                turn_id: tag,
                event,
            }) => {
                if !belongs_to(tag.as_deref(), &turn_id) {
                    continue;
                }
                if let AgentEvent::System(SystemEvent::Suspended(suspended)) =
                    event.as_ref()
                {
                    suspension = Some(suspended.clone());
                }
                match event.as_ref() {
                    AgentEvent::System(SystemEvent::SessionEnded {
                        reason: SessionEndReason::Error(message),
                    }) => {
                        return Err(Error::internal_error().data(format!(
                            "the run ended with an error: {message}"
                        )));
                    }
                    // The deployment's iteration budget ended the
                    // turn: the client hears that as its own stop
                    // reason, not as a turn that finished on its own.
                    AgentEvent::System(SystemEvent::SessionEnded {
                        reason: SessionEndReason::MaxIterations,
                    }) => hit_budget = true,
                    _ => {}
                }
                if let AgentEvent::ModelDone(_) = event.as_ref() {
                    message_id = None;
                }
                if starts_a_message(&event) && message_id.is_none() {
                    message_id = Some(Uuid::new_v4().to_string());
                }
                // A call names the files it will touch when the tool
                // can say so: the client follows along while the call
                // is in flight.
                let call_context = uva::tool::Context::new(
                    user_id.clone(),
                    state.workspace_root.clone(),
                );
                for update in projection::updates_for(
                    &event,
                    message_id.as_deref(),
                    &state.tool_registry,
                    &call_context,
                ) {
                    if let Err(error) = cx.send_notification(
                        SessionNotification::new(session_id.clone(), update),
                    ) {
                        warn!(
                            session_id = %session_id,
                            error = %error,
                            "the session update could not be sent"
                        );
                        return Err(
                            Error::internal_error().data("the client is gone")
                        );
                    }
                }
            }
            Ok(BroadcastEvent::TurnEnded {
                turn_id: tag,
                reason,
            }) => {
                if !belongs_to(tag.as_deref(), &turn_id) {
                    continue;
                }
                if reason == "suspended" {
                    let Some(suspended) = suspension.take() else {
                        // A suspension without its token: the turn
                        // cannot continue, and nothing may continue
                        // it without the operator's decision.
                        warn!(
                            session_id = %session_id,
                            "the turn suspended without a resume token"
                        );
                        return Ok(StopReason::Cancelled);
                    };
                    if continue_after_permission(
                        agent,
                        cx,
                        &session_id,
                        &suspended,
                    )
                    .await?
                    {
                        // The operator cancelled: the turn is over,
                        // and no further event of it will arrive —
                        // the run that held the call already ended.
                        return Ok(StopReason::Cancelled);
                    }
                    continue;
                }
                if hit_budget {
                    return Ok(StopReason::MaxTurnRequests);
                }
                return Ok(stop_reason(&reason));
            }
            // The receiver fell behind: the live channel dropped its
            // oldest unread items. The durable rows still hold the
            // turn, so the client that needs the whole answer
            // re-reads the transcript.
            Err(tokio::sync::broadcast::error::RecvError::Lagged(dropped)) => {
                debug!(
                    session_id = %session_id,
                    dropped,
                    "the ACP turn stream dropped live frames"
                );
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => {
                return Ok(StopReason::Cancelled);
            }
        }
    }
}

/// The stop reason a finished turn reports.
fn stop_reason(reason: &str) -> StopReason {
    match reason {
        "completed" => StopReason::EndTurn,
        "cancelled" => StopReason::Cancelled,
        _ => StopReason::EndTurn,
    }
}

/// Whether an event opens a new assistant message.
///
/// The answer and the reasoning are the message's text; a tool call
/// is not (the tool call carries its own id), so a turn that only
/// calls tools has no message to name.
fn starts_a_message(event: &AgentEvent) -> bool {
    matches!(
        event,
        AgentEvent::Model(ContentPart::Text(_))
            | AgentEvent::Model(ContentPart::Reasoning(_))
    )
}

/// The op one prompt's parts are parked as.
///
/// A prompt of one text part is the plain prompt; anything else is
/// the multimodal one, so the original bytes reach the model.
fn op_for(parts: Vec<ContentPart>) -> SessionOp {
    if let [ContentPart::Text(text)] = parts.as_slice() {
        return SessionOp::Prompt {
            content: text.text.clone(),
            priority: 1,
        };
    }
    SessionOp::PromptMulti {
        parts,
        agent_name: None,
        priority: 1,
    }
}

/// Whether a live event belongs to this turn.
///
/// A tagged event belongs to the turn that carries its tag; an
/// untagged one belongs to the turn that is running now — the
/// resumed continuation of a suspended turn has no park of its own,
/// so its events carry no tag.
fn belongs_to(tag: Option<&str>, turn_id: &str) -> bool {
    match tag {
        Some(tag) => tag == turn_id,
        None => true,
    }
}

/// Ask the operator about the held calls and continue the turn.
///
/// The decisions cover every open interrupt — the harness refuses a
/// resume that leaves one unanswered — and the resume carries the
/// token the suspension was issued with.
///
/// Returns whether the operator cancelled: a cancelled turn is over,
/// and the caller's answer says so instead of waiting for an event
/// that will not come.
async fn continue_after_permission(
    agent: &Arc<AcpAgent>,
    cx: &agent_client_protocol::v1::ConnectionTo<Client>,
    session_id: &str,
    suspension: &Suspension,
) -> Result<bool, Error> {
    let Suspension::AwaitingApproval { interrupts, resume } = suspension;
    let mut decisions = Vec::new();
    let mut cancelled = false;
    for request in interrupts {
        let outcome = ask(cx, session_id, request).await;
        if matches!(outcome, PermissionOutcome::Cancelled) {
            cancelled = true;
        }
        decisions.push(ResumeDecision {
            id: request.id.clone(),
            status: decision_status(&outcome),
            payload: None,
        });
    }
    if cancelled {
        // The operator cancelled the turn: the held calls are settled
        // with the cancellation and the turn ends there.
        info!(
            session_id = %session_id,
            "the operator cancelled the ACP turn"
        );
        cancel_session(agent, session_id).await;
        return Ok(true);
    }
    let request = ResumeRequest {
        resume_token: resume.clone(),
        interrupts: decisions
            .into_iter()
            .map(|decision| InterruptDecision {
                id: decision.id,
                status: decision.status,
                payload: decision.payload,
            })
            .collect(),
        allow_drift: false,
    };
    crate::routes::follow::resume(
        Arc::clone(&agent.state),
        session_id.to_string(),
        request,
    )
    .await
    .map_err(|error| {
        Error::internal_error()
            .data(format!("the turn could not continue: {}", error.message))
    })?;
    Ok(false)
}

/// The decision one held call's answer maps to.
enum PermissionOutcome {
    /// The operator allowed the call.
    Allowed,
    /// The operator refused the call.
    Rejected,
    /// The question was not answered — the client is gone, or the
    /// operator cancelled the turn.
    Cancelled,
}

/// The decision the operator's answer maps to.
fn decision_status(outcome: &PermissionOutcome) -> ResumeStatus {
    match outcome {
        PermissionOutcome::Allowed => ResumeStatus::Resolved,
        PermissionOutcome::Rejected => ResumeStatus::Rejected,
        PermissionOutcome::Cancelled => ResumeStatus::Cancelled,
    }
}

/// Ask the client about one held call.
///
/// The question is one-shot: the operator decides about this call,
/// and the next call asks again.
async fn ask(
    cx: &agent_client_protocol::v1::ConnectionTo<Client>,
    session_id: &str,
    request: &ApprovalRequest,
) -> PermissionOutcome {
    let tool_call = ToolCallUpdate::new(
        ToolCallId::new(request.call_id.clone()),
        ToolCallUpdateFields::new()
            .title(request.tool.clone())
            .kind(projection::tool_kind(&request.tool)),
    );
    let question = RequestPermissionRequest::new(
        session_id.to_string(),
        tool_call,
        permission_options(),
    );
    match cx.send_request(question).block_task().await {
        Ok(answer) => outcome_of(&answer),
        Err(error) => {
            warn!(
                session_id = %session_id,
                tool = %request.tool,
                error = %error,
                "the permission question could not be answered; the call is refused"
            );
            PermissionOutcome::Cancelled
        }
    }
}

/// The decision the client's answer carries.
fn outcome_of(answer: &RequestPermissionResponse) -> PermissionOutcome {
    match &answer.outcome {
        RequestPermissionOutcome::Cancelled => PermissionOutcome::Cancelled,
        RequestPermissionOutcome::Selected(selected) => {
            match selected.option_id.0.as_ref() {
                "allow_once" => PermissionOutcome::Allowed,
                _ => PermissionOutcome::Rejected,
            }
        }
        // An outcome this agent does not know is a refusal: the safe
        // direction when the protocol grows a variant.
        _ => PermissionOutcome::Rejected,
    }
}

/// Cancel the session's live turn.
async fn cancel_session(agent: &Arc<AcpAgent>, session_id: &str) {
    let state = Arc::clone(&agent.state);
    let user_id = state.default_user_id().to_string();
    if let Ok(controller) = state
        .must_exist_session_controller(&user_id, session_id)
        .await
        && let Err(error) = controller.cancel().await
    {
        warn!(
            session_id = %session_id,
            error = %error,
            "the ACP cancellation did not reach the session"
        );
    }
}
