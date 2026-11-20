//! `uva-eval` HTTP surface — in-memory `EvalSuite` registry
//! plus a keyword-only runner.
//!
//! Surface:
//! - `GET    /api/v1/evals`         — list suites
//! - `GET    /api/v1/evals/{name}`  — single suite
//! - `POST   /api/v1/evals`         — register a suite
//! - `DELETE /api/v1/evals/{name}`  — drop a suite
//! - `POST   /api/v1/evals/{name}/run` — run the suite, return report

use std::sync::Arc;

use uva::eval::{EvalReport, EvalSuite};

use crate::{api::List, rpc::RpcError, state::AppState};

/// `GET /api/v1/evals` — list suites.
///
/// Not paginated: the suite set is small and bounded (one row per
/// stored suite), so the response is the whole set with `total`
/// filled and `next_cursor` absent. The method therefore takes no
/// page parameters — accepting `limit` / `cursor` / `sort` and
/// silently ignoring them would be an interface that promises what
/// it does not keep.
pub async fn list_evals(
    state: Arc<AppState>,
) -> Result<List<EvalSuite>, RpcError> {
    let suites = state.evals.list().await;
    let total = suites.len() as u64;
    // No page: the constructors carry no cursor, which is the fact
    // this route reports.
    Ok(List::new(suites).with_total(total))
}

/// `GET /api/v1/evals/{name}` — single suite detail.
pub async fn get_eval(
    state: Arc<AppState>,
    name: String,
) -> Result<EvalSuite, RpcError> {
    let suite = state.evals.get(&name).await.ok_or_else(|| {
        RpcError::from(uva::core::Error::not_found(format!("eval '{name}'")))
    })?;
    Ok(suite)
}

/// `POST /api/v1/evals` — register a suite.
pub async fn create_eval(
    state: Arc<AppState>,
    req: EvalSuite,
) -> Result<EvalSuite, RpcError> {
    let name = req.name().to_string();
    state
        .evals
        .create(req)
        .await
        .map_err(|e| RpcError::from(uva::core::Error::already_exists(e)))?;
    let suite = state.evals.get(&name).await.ok_or_else(|| {
        RpcError::from(uva::core::Error::internal(
            "created eval suite disappeared",
        ))
    })?;
    Ok(suite)
}

/// `DELETE /api/v1/evals/{name}` — drop a suite.
pub async fn delete_eval(
    state: Arc<AppState>,
    name: String,
) -> Result<(), RpcError> {
    if state
        .evals
        .remove(&name)
        .await
        .map_err(|e| RpcError::from(uva::core::Error::internal(e)))?
    {
        Ok(())
    } else {
        Err(RpcError::from(uva::core::Error::not_found(format!(
            "eval '{name}'"
        ))))
    }
}

/// `POST /api/v1/evals/{name}/run` — run the suite.
pub async fn run_eval(
    state: Arc<AppState>,
    name: String,
) -> Result<EvalReport, RpcError> {
    let factory = state.agent_loop().ok_or_else(|| {
        RpcError::from(uva::core::Error::internal(
            "the `agentLoop` service is not mounted; eval runs are unavailable",
        ))
    })?;
    let agent = crate::state::HarnessEvalAgent::new(
        factory,
        state.default_provider.read().clone(),
        Arc::clone(&state.tool_registry),
        state.plugin_host.clone(),
        state.system_prompt.clone(),
        state.workspace_root.clone(),
    );
    let report = state
        .evals
        .run(&name, &agent)
        .await
        .map_err(|e| RpcError::from(uva::core::Error::not_found(e)))?;
    Ok(report)
}
