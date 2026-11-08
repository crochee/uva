//! The `scheduler` service as a native kernel plugin.
//!
//! The service holds one [`ScheduleStore`] and one [`Scheduler`]; these
//! tests prove the kernel hands out exactly that pair (identity), that a
//! due job is delivered by the service's own tick, and that the
//! declaration parser refuses an interval it cannot honour.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use elysper_kernel::{
    Context,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};
use elysper_scheduler::{
    Job,
    JobKind,
    ScheduleStore,
    SchedulerService,
    plugin::{KEY_SCHEDULER, SchedulerPlugin},
    task_file::{TASK_FILE, TaskDeclaration, TaskFileError},
};
use serde_json::json;
use tempfile::TempDir;

/// Pinned "now" so the firing is deterministic.
const T0: &str = "2026-09-19T09:00:00Z";

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(T0)
        .expect("valid T0")
        .with_timezone(&Utc)
}

fn mount_scheduler(
    ctx: &Context,
    service: &Arc<SchedulerService>,
) -> elysper_kernel::MountHandle {
    mount_all(
        ctx,
        &[Arc::new(SchedulerPlugin::new(Arc::clone(service)))
            as Arc<dyn KernelPlugin>],
    )
    .expect("mount `scheduler`")
}

#[test]
fn provides_the_service_and_unload_removes_it() {
    let ctx = Context::new();
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(ScheduleStore::new(dir.path()));
    let service = Arc::new(SchedulerService::new(Arc::clone(&store)));

    let mounted = mount_scheduler(&ctx, &service);

    let resolved = ctx
        .require::<SchedulerService>(KEY_SCHEDULER)
        .expect("`scheduler` resolves");
    assert!(
        Arc::ptr_eq(&resolved, &service),
        "the service must be the pair the plugin was built with"
    );
    assert!(
        Arc::ptr_eq(resolved.store(), &store),
        "the service exposes the store it was built over"
    );
    assert!(Arc::ptr_eq(resolved.store(), service.store()));

    drop(mounted);
    assert!(
        ctx.require::<SchedulerService>(KEY_SCHEDULER).is_err(),
        "unload must withdraw `scheduler`"
    );
}

#[test]
fn scheduler_tick_fires_due_job() {
    let ctx = Context::new();
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(ScheduleStore::new(dir.path()));
    let service = Arc::new(SchedulerService::new(Arc::clone(&store)));
    let mounted = mount_scheduler(&ctx, &service);

    let resolved = ctx
        .require::<SchedulerService>(KEY_SCHEDULER)
        .expect("`scheduler` resolves");
    resolved
        .store()
        .add(Job::new(
            "recheck-ci",
            JobKind::Once,
            json!({"agent": "maintainer", "prompt": "re-check CI"}),
            now(),
        ))
        .expect("add the due job");

    let tick = resolved.tick(now());
    assert_eq!(tick.fired.len(), 1, "the due job must fire");
    assert_eq!(tick.fired[0].name, "recheck-ci");
    assert_eq!(
        tick.fired[0].payload,
        json!({"agent": "maintainer", "prompt": "re-check CI"})
    );
    assert!(
        resolved.next_deadline(now()).is_none(),
        "a one-shot job leaves nothing to wait for"
    );

    drop(mounted);
}

#[test]
fn invalid_schedule_is_rejected() {
    let dir = TempDir::new().expect("temp dir");
    let task_dir = dir.path().join("broken");
    std::fs::create_dir_all(&task_dir).expect("create the task directory");
    std::fs::write(
        task_dir.join(TASK_FILE),
        "---\nname: broken\nkind: interval\ninterval_secs: 0\n\
         agent: maintainer\n---\n\nRe-check the failing job.",
    )
    .expect("write the declaration");

    let declaration =
        TaskDeclaration::read(&task_dir).expect("the frontmatter parses");
    let error = declaration
        .job_kind()
        .expect_err("a zero interval is not a schedule");

    match error {
        TaskFileError::MissingField {
            name,
            kind,
            missing,
        } => {
            assert_eq!(name, "broken");
            assert_eq!(kind, "interval");
            assert_eq!(missing, "interval_secs");
        }
        other => panic!("expected a MissingField refusal, got {other:?}"),
    }
}

#[test]
fn manifest_matches_provided_key() {
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(ScheduleStore::new(dir.path()));
    let plugin = SchedulerPlugin::new(Arc::new(SchedulerService::new(store)));
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "scheduler");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_SCHEDULER.to_string()]);
    assert_eq!(KEY_SCHEDULER, "scheduler");
    assert!(
        manifest.inject.is_empty(),
        "the store and scheduler arrive with the plugin value"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}
