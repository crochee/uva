//! `elysper-workflow` HTTP surface—in-memory `WorkflowSpec`
//! registry + plan validation.
//!
//! Surface:
//! - `GET /api/v1/workflows`—list documents
//! - `GET /api/v1/workflows/{id}`—single document
//! - `POST /api/v1/workflows`—register a document
//! - `PUT /api/v1/workflows/{id}`—replace a document
//! - `DELETE /api/v1/workflows/{id}`—remove a document
//! - `POST /api/v1/workflows/{id}/run`—execute: every planned
//!   call dispatches through
//!   [`AppState::workflow_host`](crate::state::AppState::workflow_host),
//!   which resolves the step's agent by name from the deployment's
//!   registry and runs it to completion.
//!
//! The wire shapes match `elysper::workflow::WorkflowSpec` and
//! (for the run result) `elysper::workflow::result::WorkflowRun`.

use std::sync::Arc;

use axum::{Json, extract::State, http::StatusCode};
use serde::Serialize;
use elysper::workflow::{WorkflowRun as WorkflowRunResult, WorkflowSpec};

use crate::{
    api::{AppError, AppPath, AppQuery, List, PageQuery, resolve_page},
    state::AppState,
};

#[derive(Serialize)]
pub struct WorkflowSummary {
    pub id: String,
    pub step_count: usize,
    pub phase_count: usize,
}

/// `GET /api/v1/workflows`—list all registered documents.
pub async fn list_workflows(
    State(state): State<Arc<AppState>>,
    AppQuery(page): AppQuery<PageQuery>,
) -> Result<Json<List<WorkflowSummary>>, AppError> {
    let _ = resolve_page(&page)?;
    let specs = state.workflows.list().await;
    let total = specs.len() as u64;
    let data = specs
        .into_iter()
        .map(|s| WorkflowSummary {
            id: s.id.clone(),
            step_count: s.steps.len(),
            phase_count: s.phases.len(),
        })
        .collect();
    Ok(Json(List {
        data,
        next_cursor: None,
        total: Some(total),
    }))
}

/// `GET /api/v1/workflows/{id}`—single document detail.
pub async fn get_workflow(
    State(state): State<Arc<AppState>>,
    AppPath(id): AppPath<String>,
) -> Result<Json<WorkflowSpec>, AppError> {
    let spec = state.workflows.get(&id).await.ok_or_else(|| {
        AppError::from(elysper::core::Error::not_found(format!(
            "workflow '{id}'"
        )))
    })?;
    Ok(Json(spec))
}

/// `POST /api/v1/workflows`—register a new document.
pub async fn create_workflow(
    State(state): State<Arc<AppState>>,
    axum::Json(req): axum::Json<WorkflowSpec>,
) -> Result<(StatusCode, Json<WorkflowSpec>), AppError> {
    let id = req.id.clone();
    state
        .workflows
        .create(req)
        .await
        .map_err(|e| AppError::from(elysper::core::Error::already_exists(e)))?;
    let spec = state.workflows.get(&id).await.ok_or_else(|| {
        AppError::from(elysper::core::Error::internal(
            "created workflow disappeared",
        ))
    })?;
    Ok((StatusCode::CREATED, Json(spec)))
}

/// `PUT /api/v1/workflows/{id}`—replace a stored document.
pub async fn replace_workflow(
    State(state): State<Arc<AppState>>,
    AppPath(id): AppPath<String>,
    axum::Json(req): axum::Json<WorkflowSpec>,
) -> Result<Json<WorkflowSpec>, AppError> {
    state
        .workflows
        .replace(&id, req)
        .await
        .map_err(|e| AppError::from(elysper::core::Error::validation(e)))?;
    let spec = state.workflows.get(&id).await.ok_or_else(|| {
        AppError::from(elysper::core::Error::internal(
            "replaced workflow disappeared",
        ))
    })?;
    Ok(Json(spec))
}

/// `DELETE /api/v1/workflows/{id}`—drop a document.
pub async fn delete_workflow(
    State(state): State<Arc<AppState>>,
    AppPath(id): AppPath<String>,
) -> Result<StatusCode, AppError> {
    if state
        .workflows
        .remove(&id)
        .await
        .map_err(|e| AppError::from(elysper::core::Error::internal(e)))?
    {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(AppError::from(elysper::core::Error::not_found(format!(
            "workflow '{id}'"
        ))))
    }
}

/// `POST /api/v1/workflows/{id}/run`—execute.
///
/// Dispatches every planned call through
/// [`AppState::workflow_host`](crate::state::AppState::workflow_host)'s
/// `spawn_agent`, translating each `AgentOutcome` into a `CallRun`:
/// `Succeeded(text)` when the host reported success, `Failed(error)`
/// when it did not. The host is the real `ServerWorkflowHost` (see
/// the `state::app_state::workflow_host` module): it resolves the
/// step's agent name against the deployment's `AgentRegistry` and
/// runs the agent to completion on the step's prompt, so an
/// unregistered name, a session that ends before completing, and a
/// reply with no text are all per-step `Failed` outcomes — a
/// missing agent is one failure path, not the only one. Only a
/// host-level `Err` (never produced by the current host) aborts
/// the whole request.
pub async fn run_workflow(
    State(state): State<Arc<AppState>>,
    AppPath(id): AppPath<String>,
) -> Result<Json<WorkflowRunResult>, AppError> {
    let spec = state.workflows.get(&id).await.ok_or_else(|| {
        AppError::from(elysper::core::Error::not_found(format!(
            "workflow '{id}'"
        )))
    })?;
    let plan = state.workflows.plan(&spec).await.map_err(|e| {
        AppError::from(elysper::core::Error::validation(e.to_string()))
    })?;
    let run_id = format!("wf_{}", uuid::Uuid::new_v4());
    let mut call_runs = Vec::with_capacity(plan.calls().len());
    let mut spawned = 0usize;
    for call in plan.calls() {
        let request = elysper::workflow::AgentRequest {
            run_id: run_id.clone(),
            step_id: call.step_id.clone(),
            position: call.position,
            depth: 0,
            agent: call.agent.clone(),
            prompt: call.prompt.clone(),
            gate: call.gate.clone(),
            isolation: call.isolation,
            cwd: None,
        };
        let outcome = state.workflow_host.spawn_agent(&request).await;
        let run = match outcome {
            Ok(outcome) if outcome.succeeded() => {
                spawned += 1;
                elysper::workflow::CallRun {
                    position: call.position,
                    step_id: call.step_id.clone(),
                    phase: call.phase.clone(),
                    status: elysper::workflow::CallStatus::Succeeded,
                    text: outcome.text,
                    error: None,
                    gate: elysper::workflow::GateVerdict::Absent,
                    winner: false,
                    branch_score: None,
                }
            }
            Ok(outcome) => elysper::workflow::CallRun {
                position: call.position,
                step_id: call.step_id.clone(),
                phase: call.phase.clone(),
                status: elysper::workflow::CallStatus::Failed,
                text: None,
                error: outcome
                    .error
                    .or_else(|| Some("agent produced no outcome".into())),
                gate: elysper::workflow::GateVerdict::Absent,
                winner: false,
                branch_score: None,
            },
            // A host `Err` means the call could not be made at all—
            // the run cannot continue, so the whole request fails
            // with the host's reason rather than half-settling.
            Err(e) => {
                return Err(AppError::from(elysper::core::Error::internal(
                    e.to_string(),
                )));
            }
        };
        call_runs.push(run);
    }
    Ok(Json(WorkflowRunResult {
        run_id,
        output: None,
        calls: call_runs,
        replayed: 0,
        spawned,
    }))
}
