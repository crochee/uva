//! The `schedule` tool brick as a native kernel plugin.
//!
//! The brick is the consumer that turns the `scheduler` service into a
//! model-facing tool: it injects both `scheduler` and `tools`, files the
//! tool under its own registration token, and unloads exactly that
//! entry. Mounting it without the service must leave nothing behind.

use std::sync::Arc;

use serde_json::json;
use tempfile::TempDir;
use uva_kernel::{
    Context,
    MountError,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};
use uva_scheduler::{
    ScheduleStore,
    SchedulerService,
    plugin::{KEY_SCHEDULER, SchedulerPlugin},
};
use uva_tool::{
    Context as ToolContext,
    ToolEntry,
    ToolRegistry,
    plugin::{KEY_TOOLS, ToolRegistryPlugin},
};
use uva_tool_plugins::scheduler::{
    SCHEDULE_TOOL_NAME,
    SchedulerTool,
    plugin::ScheduleToolPlugin,
};

#[tokio::test]
async fn schedule_tool_is_registered_and_unload_removes_only_its_entry() {
    let ctx = Context::new();
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(ScheduleStore::new(dir.path()));
    let _service = Arc::new(SchedulerService::new(Arc::clone(&store)));
    let tools = Arc::new(ToolRegistry::new());

    // A foreign `schedule` entry: unloading the brick must not take it.
    assert!(
        tools.register_entry(ToolEntry::new(Arc::new(SchedulerTool::new(
            Arc::clone(&store)
        )))),
        "the fixture files a foreign `schedule` entry"
    );
    assert_eq!(tools.name_occupants(SCHEDULE_TOOL_NAME), 1);

    let tools_mount = mount_all(
        &ctx,
        &[Arc::new(ToolRegistryPlugin::new(Arc::clone(&tools)))
            as Arc<dyn KernelPlugin>],
    )
    .expect("mount `tools`");

    let mounted = mount_all(
        &ctx,
        &[
            Arc::new(SchedulerPlugin::new(
                uva_scheduler::SchedulerPluginConfig {
                    schedules_dir: dir.path().to_path_buf(),
                },
            )) as Arc<dyn KernelPlugin>,
            Arc::new(ScheduleToolPlugin::new()) as Arc<dyn KernelPlugin>,
        ],
    )
    .expect("mount `scheduler` and `schedule-tool`");
    assert_eq!(mounted.len(), 2);
    assert_eq!(
        tools.name_occupants(SCHEDULE_TOOL_NAME),
        2,
        "the brick files a second entry under `schedule`"
    );

    // Calling the resolved tool proves the brick built it over the
    // service's store: the job it creates lands in that store.
    let tool = tools
        .get(SCHEDULE_TOOL_NAME)
        .expect("the brick's entry resolves")
        .tool_instance();
    let output = tool
        .call(
            json!({"action": "create", "name": "nightly", "kind": "once"}),
            &ToolContext::default(),
        )
        .await;
    assert_eq!(output.is_error, None, "`create` must succeed");
    // The brick injects the mounted `scheduler` service, so the job
    // lands in the service's store (the one the row built).
    let scheduler = ctx
        .require::<SchedulerService>(KEY_SCHEDULER)
        .expect("scheduler service");
    let jobs = scheduler.store().list();
    assert_eq!(jobs.len(), 1);
    assert_eq!(jobs[0].name, "nightly");

    drop(mounted);
    assert!(
        tools.contains(SCHEDULE_TOOL_NAME),
        "the foreign entry stays registered"
    );
    assert_eq!(
        tools.name_occupants(SCHEDULE_TOOL_NAME),
        1,
        "unregister_by_token removes exactly the brick's entry"
    );
    assert!(
        ctx.require::<SchedulerService>(KEY_SCHEDULER).is_err(),
        "the sibling `scheduler` mount withdrew its own service"
    );
    assert!(
        ctx.require::<ToolRegistry>(KEY_TOOLS).is_ok(),
        "the separately mounted `tools` service is untouched by the cascade"
    );

    drop(tools_mount);
    assert!(
        ctx.require::<ToolRegistry>(KEY_TOOLS).is_err(),
        "the `tools` mount still withdraws its own service"
    );
}

#[test]
fn missing_scheduler_leaves_the_tool_unmounted() {
    let ctx = Context::new();
    let tools = Arc::new(ToolRegistry::new());
    let tools_mount = mount_all(
        &ctx,
        &[Arc::new(ToolRegistryPlugin::new(Arc::clone(&tools)))
            as Arc<dyn KernelPlugin>],
    )
    .expect("mount `tools`");

    let error = mount_all(
        &ctx,
        &[Arc::new(ScheduleToolPlugin::new()) as Arc<dyn KernelPlugin>],
    )
    .expect_err("the brick cannot activate without `scheduler`");

    match error {
        MountError::MissingDependency { plugin_id, keys } => {
            assert_eq!(plugin_id, "schedule-tool");
            assert!(
                keys.iter().any(|key| key == KEY_SCHEDULER),
                "the refusal names `scheduler`: {keys:?}"
            );
        }
        other => panic!("expected MissingDependency, got {other:?}"),
    }
    assert!(
        !tools.contains(SCHEDULE_TOOL_NAME),
        "an unmounted brick registers no tool"
    );
    assert!(
        ctx.require::<ToolRegistry>(KEY_TOOLS).is_ok(),
        "the rest of the system runs without the optional capability"
    );

    drop(tools_mount);
}

#[test]
fn mount_fails_loudly_when_a_core_tool_owns_the_name() {
    let ctx = Context::new();
    let dir = TempDir::new().expect("temp dir");
    let store = Arc::new(ScheduleStore::new(dir.path()));
    let _service = Arc::new(SchedulerService::new(Arc::clone(&store)));
    let tools = Arc::new(ToolRegistry::new());
    assert!(
        tools.register_core_entry(ToolEntry::new(Arc::new(
            SchedulerTool::new(Arc::clone(&store))
        ))),
        "the fixture core-registers `schedule`"
    );

    let tools_mount = mount_all(
        &ctx,
        &[Arc::new(ToolRegistryPlugin::new(Arc::clone(&tools)))
            as Arc<dyn KernelPlugin>],
    )
    .expect("mount `tools`");
    let scheduler_mount =
        mount_all(
            &ctx,
            &[Arc::new(SchedulerPlugin::new(
                uva_scheduler::SchedulerPluginConfig {
                    schedules_dir: dir.path().to_path_buf(),
                },
            )) as Arc<dyn KernelPlugin>],
        )
        .expect("mount `scheduler`");

    let error = mount_all(
        &ctx,
        &[Arc::new(ScheduleToolPlugin::new()) as Arc<dyn KernelPlugin>],
    )
    .expect_err("the mount must fail, not silently skip `schedule`");
    let MountError::ApplyFailed { plugin_id, source } = error else {
        panic!("expected ApplyFailed, got {error:?}");
    };
    assert_eq!(plugin_id, "schedule-tool");
    let message = source.to_string();
    assert!(
        message.contains(SCHEDULE_TOOL_NAME) && message.contains("core"),
        "the error must name the tool and the core owner: {message}"
    );

    assert_eq!(
        tools.name_occupants(SCHEDULE_TOOL_NAME),
        1,
        "the core entry is untouched and the brick's registration was refused"
    );
    assert!(
        ctx.require::<ToolRegistry>(KEY_TOOLS).is_ok(),
        "a failed brick mount does not touch the already-mounted `tools`"
    );
    assert!(
        ctx.require::<SchedulerService>(KEY_SCHEDULER).is_ok(),
        "a failed brick mount does not touch the already-mounted `scheduler`"
    );

    drop(scheduler_mount);
    drop(tools_mount);
}

#[test]
fn manifest_injects_the_service_and_registry() {
    let plugin = ScheduleToolPlugin::new();
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "schedule-tool");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert!(
        manifest.provides.is_empty(),
        "the brick contributes a tool, not a service"
    );
    assert_eq!(
        manifest.inject,
        vec![KEY_SCHEDULER.to_string(), KEY_TOOLS.to_string()]
    );
    assert_eq!(KEY_SCHEDULER, "scheduler");
    assert_eq!(KEY_TOOLS, "tools");
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.config_schema.is_none());
}
