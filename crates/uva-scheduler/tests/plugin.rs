//! The `scheduler` service as a native kernel plugin.
//!
//! The plugin builds its own `ScheduleStore` and `Scheduler` from
//! configuration inside `apply`; these tests mount it over a fresh
//! context and prove the resolved service serves the configured store
//! (a job written through it lists back), that a due job is delivered by
//! the service's own tick, that unload withdraws the service and frees
//! it, and that the declaration parser refuses an interval it cannot
//! honour.

use std::sync::Arc;

use chrono::{DateTime, Utc};
use serde_json::json;
use tempfile::TempDir;
use uva_kernel::{
    Context,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};
use uva_scheduler::{
    Job,
    JobKind,
    SchedulerPluginConfig,
    SchedulerService,
    plugin::{KEY_SCHEDULER, SchedulerPlugin},
    task_file::{TASK_FILE, TaskDeclaration, TaskFileError},
};

/// Pinned "now" so the firing is deterministic.
const T0: &str = "2026-09-19T09:00:00Z";

fn now() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(T0)
        .expect("valid T0")
        .with_timezone(&Utc)
}

fn config(schedules_dir: &std::path::Path) -> SchedulerPluginConfig {
    SchedulerPluginConfig {
        schedules_dir: schedules_dir.to_path_buf(),
    }
}

fn mount_scheduler(
    ctx: &Context,
    schedules_dir: &std::path::Path,
) -> uva_kernel::MountHandle {
    mount_all(
        ctx,
        &[Arc::new(SchedulerPlugin::new(config(schedules_dir)))
            as Arc<dyn KernelPlugin>],
    )
    .expect("mount `scheduler`")
}

#[test]
fn provides_a_working_service_and_unload_removes_it() {
    let ctx = Context::new();
    let dir = TempDir::new().expect("temp dir");
    let mounted = mount_scheduler(&ctx, dir.path());

    let resolved = ctx
        .require::<SchedulerService>(KEY_SCHEDULER)
        .expect("`scheduler` resolves");
    assert_eq!(
        resolved.store().root(),
        dir.path(),
        "the service's store is rooted at the configured directory"
    );

    // The service is not a hollow shell: a job written through the
    // resolved store lists back and is ticked by the same service.
    resolved
        .store()
        .add(Job::new(
            "nightly-audit",
            JobKind::Once,
            json!({"agent": "maintainer"}),
            now(),
        ))
        .expect("add a job through the resolved service");
    let listed = resolved.store().list();
    assert_eq!(listed.len(), 1, "the job must reach the store");
    assert_eq!(listed[0].name, "nightly-audit");

    // Keep only a weak witness so unload can be checked by liveness,
    // not just by visibility in the context.
    let service = Arc::downgrade(&resolved);
    drop(resolved);
    drop(mounted);

    assert!(
        ctx.require::<SchedulerService>(KEY_SCHEDULER).is_err(),
        "unload must withdraw `scheduler`"
    );
    assert!(
        service.upgrade().is_none(),
        "unload must drop the service, not merely hide it"
    );
}

#[test]
fn scheduler_tick_fires_due_job() {
    let ctx = Context::new();
    let dir = TempDir::new().expect("temp dir");
    let mounted = mount_scheduler(&ctx, dir.path());

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
    let plugin = SchedulerPlugin::new(config(dir.path()));
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "scheduler");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_SCHEDULER.to_string()]);
    assert_eq!(KEY_SCHEDULER, "scheduler");
    assert!(
        manifest.inject.is_empty(),
        "the store is built from config inside apply, so nothing is injected"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.config_schema.is_none());
}
