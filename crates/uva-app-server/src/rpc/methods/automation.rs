//! The automation surfaces: schedules, tasks and evals.
use serde::Deserialize;
use uva::eval::EvalSuite;

use super::{names, parse, parse_raw};
use crate::{
    api::PageQuery,
    routes::{
        evals::{self},
        schedules::{self, CreateScheduleRequest, UpdateScheduleRequest},
        tasks::{self},
    },
    rpc::{RpcRouter, encode},
};

/// Register the automation methods.
pub fn register(router: &mut RpcRouter) {
    router.register(names::SCHEDULES_LIST, |state, raw| async move {
        let page: PageQuery = parse(raw)?;
        encode(schedules::list_schedules(state, page).await?)
    });
    router.register(names::SCHEDULES_CREATE, |state, raw| async move {
        let request: CreateScheduleRequest = parse(raw)?;
        encode(schedules::create_schedule(state, request).await?)
    });
    router.register(names::SCHEDULES_GET, |state, raw| async move {
        let params: IdParams = parse(raw)?;
        encode(schedules::get_schedule(state, params.id).await?)
    });
    router.register(names::SCHEDULES_PATCH, |state, raw| async move {
        let params: PatchParams = parse(raw)?;
        encode(
            schedules::patch_schedule(state, params.id, params.request).await?,
        )
    });
    router.register(names::SCHEDULES_DELETE, |state, raw| async move {
        let params: IdParams = parse(raw)?;
        encode(schedules::delete_schedule(state, params.id).await?)
    });
    router.register(names::SCHEDULES_TICK, |state, raw| async move {
        let params: IdParams = parse(raw)?;
        encode(schedules::tick_schedule(state, params.id).await?)
    });
    router.register(names::TASKS_LIST, |state, raw| async move {
        let page: PageQuery = parse(raw)?;
        encode(tasks::list_tasks(state, page).await?)
    });
    router.register(names::TASKS_GET, |state, raw| async move {
        let params: IdParams = parse(raw)?;
        encode(tasks::get_task(state, params.id).await?)
    });
    router.register(names::EVALS_LIST, |state, _raw| async move {
        encode(evals::list_evals(state).await?)
    });
    router.register(names::EVALS_CREATE, |state, raw| async move {
        let request: EvalSuite = parse_raw(raw)?;
        encode(evals::create_eval(state, request).await?)
    });
    router.register(names::EVALS_GET, |state, raw| async move {
        let params: NameParams = parse(raw)?;
        encode(evals::get_eval(state, params.name).await?)
    });
    router.register(names::EVALS_DELETE, |state, raw| async move {
        let params: NameParams = parse(raw)?;
        encode(evals::delete_eval(state, params.name).await?)
    });
    router.register(names::EVALS_RUN, |state, raw| async move {
        let params: NameParams = parse(raw)?;
        encode(evals::run_eval(state, params.name).await?)
    });
}
/// The one-parameter shape: `{"id": "…"}`.
#[derive(Deserialize, validator::Validate)]
struct IdParams {
    id: String,
}
/// The one-parameter shape: `{"name": "…"}`.
#[derive(Deserialize, validator::Validate)]
struct NameParams {
    name: String,
}
/// `schedules/patch` — the schedule id beside the patch body.
#[derive(Deserialize, validator::Validate)]
struct PatchParams {
    id: String,
    #[serde(flatten)]
    request: UpdateScheduleRequest,
}
