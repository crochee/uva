//! `uva-scheduler` HTTP surface — wires the existing
//! `ScheduleStore` (disk-backed, PID-locked atomic JSON) to the
//! `/api/v1/schedules/*` routes the frontend commits to.
//!
//! Surface:
//! - `GET    /api/v1/schedules`         — list (cursor-paginated)
//! - `GET    /api/v1/schedules/{id}`    — single job detail
//! - `POST   /api/v1/schedules`         — register a new job
//! - `PATCH  /api/v1/schedules/{id}`    — pause / resume / update payload
//! - `DELETE /api/v1/schedules/{id}`    — drop a job
//! - `POST   /api/v1/schedules/{id}/tick` — drive a single scheduler tick
//!
//! The store's wire types are `Job` / `JobKind` / `JobStatus`
//! (`crates/uva-scheduler/src/job.rs`), serialized with
//! default snake_case. The frontend types in
//! `uva-web/src/api/types.ts` mirror them.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use uva::{
    core::Clock as _,
    scheduler::{Job, JobId, JobKind, JobStatus},
};

use super::helpers::paginate;
use crate::{
    api::{
        List,
        PageQuery,
        ResolvedPage,
        resolve_page,
        validate_resource_name,
        validate_sort,
    },
    rpc::RpcError,
    state::AppState,
};

/// Sortable fields for the schedules list endpoint.
const SCHEDULE_SORT_WHITELIST: &[&str] = &["id", "name", "next_fire_at"];

/// `GET /api/v1/schedules` — list row.
#[derive(Serialize)]
pub struct ScheduleInfo {
    pub id: String,
    pub name: String,
    pub kind: JobKind,
    pub status: JobStatus,
    pub next_fire_at: chrono::DateTime<chrono::Utc>,
}

/// `GET /api/v1/schedules/{id}` — full row.
#[derive(Serialize)]
pub struct ScheduleDetail {
    #[serde(flatten)]
    pub job: Job,
}

/// `POST /api/v1/schedules` body. Mirrors `CreateScheduleRequest`
/// in `uva-web/src/api/types.ts`.
#[derive(Deserialize, validator::Validate)]
pub struct CreateScheduleRequest {
    /// Operator-facing name. The store's `has_name` enforces
    /// uniqueness; the route returns 409 on a duplicate.
    #[validate(length(min = 1, max = 200))]
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub kind: JobKind,
    #[serde(default)]
    pub payload: serde_json::Value,
    /// Optional override for the first fire; if absent the
    /// server computes it from the kind.
    #[serde(default)]
    pub next_fire_at: Option<chrono::DateTime<chrono::Utc>>,
}

/// `PATCH /api/v1/schedules/{id}` body — pause / resume / mutate payload.
#[derive(Deserialize)]
pub struct UpdateScheduleRequest {
    #[serde(default)]
    pub status: Option<JobStatus>,
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
}

/// Response from `POST /api/v1/schedules/{id}/tick`.
#[derive(Serialize)]
pub struct ScheduleTickResponse {
    pub fired: Vec<JobInfo>,
}

/// Re-export of `FiredJob` under a JSON-friendly shape.
#[derive(Serialize)]
pub struct JobInfo {
    pub id: String,
    pub name: String,
    pub fired_at: chrono::DateTime<chrono::Utc>,
    pub payload: serde_json::Value,
}

/// `GET /api/v1/schedules` — list jobs with cursor pagination.
pub async fn list_schedules(
    state: Arc<AppState>,
    page: PageQuery,
) -> Result<List<ScheduleInfo>, RpcError> {
    validate_sort(
        page.sort.as_deref().unwrap_or("name"),
        SCHEDULE_SORT_WHITELIST,
    )?;
    let resolved: ResolvedPage = resolve_page(&page)?;

    let jobs = state.schedules.list();
    let mut infos: Vec<ScheduleInfo> = jobs
        .iter()
        .map(|j| ScheduleInfo {
            id: j.id.to_string(),
            name: j.name.clone(),
            kind: j.kind.clone(),
            status: j.status,
            next_fire_at: j.next_fire_at,
        })
        .collect();
    match resolved.sort_field.as_deref() {
        Some("id") => infos.sort_by(|a, b| a.id.cmp(&b.id)),
        Some("next_fire_at") => infos.sort_by_key(|a| a.next_fire_at),
        _ => infos.sort_by(|a, b| a.name.cmp(&b.name)),
    }
    let total = infos.len() as u64;
    let page_out = paginate(infos, &resolved, |s| s.id.as_str());
    Ok(List {
        data: page_out.data,
        next_cursor: page_out.next_cursor,
        total: Some(total),
    })
}

/// `GET /api/v1/schedules/{id}` — single job detail.
pub async fn get_schedule(
    state: Arc<AppState>,
    id: String,
) -> Result<ScheduleDetail, RpcError> {
    let job_id = JobId::from_string(id);
    let job = state.schedules.get(&job_id).ok_or_else(|| {
        RpcError::from(uva::core::Error::not_found("schedule"))
    })?;
    Ok(ScheduleDetail { job })
}

/// `POST /api/v1/schedules` — register a new job.
///
/// Validates the name, computes the first `next_fire_at` from
/// the kind when absent (one-shot = now; interval = now; cron =
/// first occurrence after now), and forwards to the store.
pub async fn create_schedule(
    state: Arc<AppState>,
    req: CreateScheduleRequest,
) -> Result<Job, RpcError> {
    use validator::Validate;
    req.validate()?;
    validate_resource_name(&req.name).map_err(|e| {
        RpcError::from(uva::core::Error::validation(e.to_string()))
    })?;

    let now = state.schedules.clock().now();
    let next_fire_at = req.next_fire_at.unwrap_or(match &req.kind {
        JobKind::Once => now,
        JobKind::Interval { .. } => now,
        JobKind::Cron { expr } => {
            uva::scheduler::trigger::CronTrigger::first_after(expr, now)
                .map_err(|e| {
                    RpcError::from(uva::core::Error::validation(format!(
                        "invalid cron expression: {e}"
                    )))
                })?
        }
    });

    let job = Job::new(req.name.clone(), req.kind, req.payload, next_fire_at)
        .with_description(req.description.clone());

    state
        .schedules
        .add(job.clone())
        .map_err(|e| RpcError::from(uva::core::Error::already_exists(e)))?;
    Ok(job)
}

/// `PATCH /api/v1/schedules/{id}` — pause / resume / mutate payload.
pub async fn patch_schedule(
    state: Arc<AppState>,
    id: String,
    req: UpdateScheduleRequest,
) -> Result<Job, RpcError> {
    let job_id = JobId::from_string(id.clone());
    let updated = state
        .schedules
        .update(&job_id, |j| {
            if let Some(s) = req.status {
                j.status = s;
            }
            if let Some(p) = req.payload.clone() {
                j.payload = p;
            }
        })
        .map_err(|e| RpcError::from(uva::core::Error::internal(e)))?;
    let job = updated.ok_or_else(|| {
        RpcError::from(uva::core::Error::not_found(format!("schedule '{id}'")))
    })?;
    Ok(job)
}

/// `DELETE /api/v1/schedules/{id}` — remove a job. Returns 204
/// on success, 404 if no such id.
pub async fn delete_schedule(
    state: Arc<AppState>,
    id: String,
) -> Result<(), RpcError> {
    let job_id = JobId::from_string(id.clone());
    let removed = state
        .schedules
        .remove(&job_id)
        .map_err(|e| RpcError::from(uva::core::Error::internal(e)))?;
    if removed {
        Ok(())
    } else {
        Err(RpcError::from(uva::core::Error::not_found(format!(
            "schedule '{id}'"
        ))))
    }
}

/// `POST /api/v1/schedules/{id}/tick` — drive one scheduler tick.
pub async fn tick_schedule(
    state: Arc<AppState>,
    id: String,
) -> Result<ScheduleTickResponse, RpcError> {
    let job_id = JobId::from_string(id);
    let fired = state.schedules.tick();
    let now = state.schedules.clock().now();
    let job_infos: Vec<JobInfo> = fired
        .into_iter()
        .filter(|f| f.id == job_id)
        .map(|f| JobInfo {
            id: f.id.to_string(),
            name: f.name,
            fired_at: now,
            payload: f.payload,
        })
        .collect();
    Ok(ScheduleTickResponse { fired: job_infos })
}
