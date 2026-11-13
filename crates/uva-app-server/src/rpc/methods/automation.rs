//! The automation surfaces: schedules, tasks and evals.
use serde::Deserialize;
use uva::eval::EvalSuite;

use super::{parse, parse_raw};
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
    router.register("schedules/list", |state, raw| async move {
        let page: PageQuery = parse(raw)?;
        encode(schedules::list_schedules(state, page).await?)
    });
    router.register("schedules/create", |state, raw| async move {
        let request: CreateScheduleRequest = parse(raw)?;
        encode(schedules::create_schedule(state, request).await?)
    });
    router.register("schedules/get", |state, raw| async move {
        let params: IdParams = parse(raw)?;
        encode(schedules::get_schedule(state, params.id).await?)
    });
    router.register("schedules/patch", |state, raw| async move {
        let params: PatchParams = parse(raw)?;
        encode(
            schedules::patch_schedule(state, params.id, params.request).await?,
        )
    });
    router.register("schedules/delete", |state, raw| async move {
        let params: IdParams = parse(raw)?;
        encode(schedules::delete_schedule(state, params.id).await?)
    });
    router.register("schedules/tick", |state, raw| async move {
        let params: IdParams = parse(raw)?;
        encode(schedules::tick_schedule(state, params.id).await?)
    });
    router.register("tasks/list", |state, raw| async move {
        let page: PageQuery = parse(raw)?;
        encode(tasks::list_tasks(state, page).await?)
    });
    router.register("tasks/get", |state, raw| async move {
        let params: IdParams = parse(raw)?;
        encode(tasks::get_task(state, params.id).await?)
    });
    router.register("evals/list", |state, raw| async move {
        let page: PageQuery = parse(raw)?;
        encode(evals::list_evals(state, page).await?)
    });
    router.register("evals/create", |state, raw| async move {
        let request: EvalSuite = parse_raw(raw)?;
        encode(evals::create_eval(state, request).await?)
    });
    router.register("evals/get", |state, raw| async move {
        let params: NameParams = parse(raw)?;
        encode(evals::get_eval(state, params.name).await?)
    });
    router.register("evals/delete", |state, raw| async move {
        let params: NameParams = parse(raw)?;
        encode(evals::delete_eval(state, params.name).await?)
    });
    router.register("evals/run", |state, raw| async move {
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
