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

use std::{cmp::Ordering, sync::Arc};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use uva::{
    core::Clock as _,
    scheduler::{Job, JobId, JobKind, JobStatus},
};

use super::list::{Direction, SortKey};
use crate::{
    api::{List, PageQuery, validate_resource_name},
    rpc::RpcError,
    state::AppState,
};

/// `GET /api/v1/schedules` — list row.
#[derive(Serialize, JsonSchema)]
pub struct ScheduleInfo {
    pub id: String,
    pub name: String,
    pub kind: JobKind,
    pub status: JobStatus,
    pub next_fire_at: chrono::DateTime<chrono::Utc>,
}

/// `GET /api/v1/schedules/{id}` — full row.
#[derive(Serialize, JsonSchema)]
pub struct ScheduleDetail {
    #[serde(flatten)]
    pub job: Job,
}

/// `POST /api/v1/schedules` body. Mirrors `CreateScheduleRequest`
/// in `uva-web/src/api/types.ts`.
#[derive(Deserialize, JsonSchema, validator::Validate)]
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
#[derive(Deserialize, JsonSchema)]
pub struct UpdateScheduleRequest {
    #[serde(default)]
    pub status: Option<JobStatus>,
    #[serde(default)]
    pub payload: Option<serde_json::Value>,
}

/// Response from `POST /api/v1/schedules/{id}/tick`.
#[derive(Serialize, JsonSchema)]
pub struct ScheduleTickResponse {
    pub fired: Vec<JobInfo>,
}

/// Re-export of `FiredJob` under a JSON-friendly shape.
#[derive(Serialize, JsonSchema)]
pub struct JobInfo {
    pub id: String,
    pub name: String,
    pub fired_at: chrono::DateTime<chrono::Utc>,
    pub payload: serde_json::Value,
}

/// The schedule list's sort vocabulary: three keys, declared once.
///
/// The old handler sorted all three and then never read the resolved
/// direction, so `?sort=-name` (and `-id`, and `-next_fire_at`)
/// validated, resolved to descending, and came back ascending. The
/// direction is applied by the recipe now, once, for every key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScheduleSort {
    Id,
    Name,
    NextFireAt,
}

impl SortKey<ScheduleInfo> for ScheduleSort {
    const ALL: &'static [Self] = &[Self::Id, Self::Name, Self::NextFireAt];

    fn as_str(self) -> &'static str {
        match self {
            Self::Id => "id",
            Self::Name => "name",
            Self::NextFireAt => "next_fire_at",
        }
    }

    fn compare(self, a: &ScheduleInfo, b: &ScheduleInfo) -> Ordering {
        match self {
            Self::Id => a.id.cmp(&b.id),
            Self::Name => a.name.cmp(&b.name),
            Self::NextFireAt => a.next_fire_at.cmp(&b.next_fire_at),
        }
    }

    fn default_sort() -> (Self, Direction) {
        (Self::Name, Direction::Ascending)
    }
}

/// `GET /api/v1/schedules` — list (cursor-paginated).
pub async fn list_schedules(
    state: Arc<AppState>,
    page: PageQuery,
) -> Result<List<ScheduleInfo>, RpcError> {
    let jobs = state.schedules.list();
    let infos: Vec<ScheduleInfo> = jobs
        .iter()
        .map(|j| ScheduleInfo {
            id: j.id.to_string(),
            name: j.name.clone(),
            kind: j.kind.clone(),
            status: j.status,
            next_fire_at: j.next_fire_at,
        })
        .collect();
    Ok(ScheduleSort::sort_and_page(infos, &page, |s| {
        s.id.as_str()
    })?)
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

#[cfg(test)]
mod tests {
    use chrono::DateTime;

    use super::*;

    async fn test_state() -> (Arc<AppState>, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp workspace");
        let sessions = uva::session::manager::SessionRegistry::new(
            dir.path().join("sessions"),
        );
        let state =
            AppState::for_test(sessions, dir.path().to_path_buf()).await;
        (Arc::new(state), dir)
    }

    fn at(seconds: i64) -> DateTime<chrono::Utc> {
        DateTime::from_timestamp(seconds, 0).expect("a valid instant")
    }

    fn job(name: &str, next_fire_at: DateTime<chrono::Utc>) -> Job {
        Job::new(name, JobKind::Once, serde_json::Value::Null, next_fire_at)
    }

    fn names(list: &List<ScheduleInfo>) -> Vec<&str> {
        list.data.iter().map(|s| s.name.as_str()).collect()
    }

    /// Seed three jobs whose name order and fire order are different,
    /// so a key that silently sorts by the wrong thing is visible.
    async fn seeded() -> (Arc<AppState>, tempfile::TempDir) {
        let (state, dir) = test_state().await;
        state.schedules.add(job("charlie", at(10))).expect("adds");
        state.schedules.add(job("alpha", at(30))).expect("adds");
        state.schedules.add(job("bravo", at(20))).expect("adds");
        (state, dir)
    }

    /// Every declared key sorts, and a `-` prefix turns it.
    ///
    /// The direction used to be dropped entirely: all three keys
    /// validated a `-` prefix, resolved to descending, and answered
    /// ascending.
    #[tokio::test]
    async fn every_declared_key_sorts_and_a_minus_prefix_turns_it() {
        let (state, _dir) = seeded().await;

        let defaulted = list_schedules(Arc::clone(&state), PageQuery::new())
            .await
            .expect("the default list");
        assert_eq!(
            names(&defaulted),
            ["alpha", "bravo", "charlie"],
            "the default is name ascending"
        );

        let cases = [
            ("name", ["alpha", "bravo", "charlie"]),
            ("-name", ["charlie", "bravo", "alpha"]),
            ("next_fire_at", ["charlie", "bravo", "alpha"]),
            ("-next_fire_at", ["alpha", "bravo", "charlie"]),
        ];
        for (sort, expected) in cases {
            let listed = list_schedules(
                Arc::clone(&state),
                PageQuery::new().with_sort(sort),
            )
            .await
            .expect("the list");
            assert_eq!(names(&listed), expected, "`{sort}`");
        }
    }

    /// The id key orders by id and turns — the same declaration, not a
    /// separate arm the old `match` could forget.
    #[tokio::test]
    async fn the_id_key_sorts_and_turns() {
        let (state, _dir) = seeded().await;

        let up = list_schedules(
            Arc::clone(&state),
            PageQuery::new().with_sort("id"),
        )
        .await
        .expect("the list");
        let mut ids: Vec<String> =
            up.data.iter().map(|s| s.id.clone()).collect();
        let mut ascending = ids.clone();
        ascending.sort();
        assert_eq!(ids, ascending, "`id` orders by id ascending");

        let down = list_schedules(
            Arc::clone(&state),
            PageQuery::new().with_sort("-id"),
        )
        .await
        .expect("the list");
        ids = down.data.iter().map(|s| s.id.clone()).collect();
        ascending.reverse();
        assert_eq!(ids, ascending, "`-id` turns it");
    }

    #[tokio::test]
    async fn a_sort_the_route_does_not_declare_is_refused() {
        let (state, _dir) = seeded().await;
        for refused in ["kind", "-kind", ""] {
            let err = list_schedules(
                Arc::clone(&state),
                PageQuery::new().with_sort(refused),
            )
            .await
            .err()
            .unwrap_or_else(|| panic!("`{refused}` should be refused"));
            let expected = if refused.is_empty() {
                "validation"
            } else {
                "invalid_item"
            };
            assert_eq!(err.code, expected, "`{refused}` answered {err:?}");
        }
    }
}
