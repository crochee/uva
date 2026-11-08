//! The `mcp` service as a native kernel plugin.
//!
//! The plugin publishes the supervisor and nothing else. The remote
//! tools belong to the supervisor's per-server generations, and the
//! app-server boot loop owns the tick that drives registration and
//! reconnect: these tests drive the same `tick` by hand, through the
//! supervisor the service hands out.
//!
//! The unload boundary is the service withdrawal. Dropping the mount
//! withdraws `mcp`; it does not walk the supervisor's generations,
//! because a generation is unregistered by the supervisor
//! (`report_failure` / retry-budget exhaustion, see
//! `tests/supervisor.rs::budget_exhaustion_unregisters_the_tools`) and
//! the boot loop stops ticking when the service it resolves disappears.

use std::sync::Arc;

use chrono::Utc;
use elysper_kernel::{
    Context,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};
use elysper_mcp::{
    HealthState,
    InMemoryTransport,
    InMemoryTransportFactory,
    McpSupervisor,
    plugin::{KEY_MCP, McpPlugin},
};
use elysper_tool::{
    ToolRegistry,
    plugin::{KEY_TOOLS, ToolRegistryPlugin},
};

#[tokio::test]
async fn provides_the_supervisor_and_unload_removes_it() {
    let ctx = Context::new();
    let supervisor = Arc::new(McpSupervisor::new());
    let plugin = McpPlugin::new(Arc::clone(&supervisor));

    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("mount `mcp`");
    assert_eq!(mounted.len(), 1, "the provider effect is tracked");

    let service = ctx
        .require::<McpSupervisor>(KEY_MCP)
        .expect("`mcp` resolves");
    assert!(
        Arc::ptr_eq(&service, &supervisor),
        "the service must be the supervisor the plugin was built with"
    );
    assert!(
        service.health().await.is_empty(),
        "a fresh supervisor supervises no servers"
    );

    drop(mounted);
    assert!(
        ctx.require::<McpSupervisor>(KEY_MCP).is_err(),
        "unload must withdraw `mcp`"
    );
}

#[tokio::test]
async fn tick_registers_remote_tools_through_the_service() {
    let ctx = Context::new();
    let supervisor = Arc::new(McpSupervisor::new());
    let registry = Arc::new(ToolRegistry::new());
    let wire = Arc::new(InMemoryTransport::new().with_echo_server());
    supervisor
        .supervise(
            "srv",
            Arc::new(InMemoryTransportFactory::new(Arc::clone(&wire))) as _,
        )
        .await;

    let mounted = mount_all(
        &ctx,
        &[Arc::new(McpPlugin::new(Arc::clone(&supervisor)))
            as Arc<dyn KernelPlugin>],
    )
    .expect("mount `mcp`");
    let service = ctx
        .require::<McpSupervisor>(KEY_MCP)
        .expect("`mcp` resolves");

    let tick = service.tick(&registry, Utc::now()).await;
    assert_eq!(tick.health.len(), 1);
    assert_eq!(tick.health[0].state, HealthState::Connected);
    assert_eq!(tick.health[0].registered_tools, 1);
    assert!(
        registry.contains("mcp__srv__echo"),
        "the tick through the service registers the remote tool"
    );
    assert!(
        registry
            .descriptors()
            .iter()
            .any(|tool| tool.name == "mcp__srv__echo"),
        "the remote tool reaches the model-facing list"
    );

    drop(mounted);
    assert!(
        ctx.require::<McpSupervisor>(KEY_MCP).is_err(),
        "unload withdraws the service even though the supervisor value lives on"
    );
}

#[tokio::test]
async fn unmounted_mcp_leaves_the_rest_of_the_system_intact() {
    let ctx = Context::new();
    let tools = Arc::new(ToolRegistry::new());
    let supervisor = Arc::new(McpSupervisor::new());
    let wire = Arc::new(InMemoryTransport::new().with_echo_server());
    supervisor
        .supervise("srv", Arc::new(InMemoryTransportFactory::new(wire)) as _)
        .await;

    let mounted = mount_all(
        &ctx,
        &[Arc::new(ToolRegistryPlugin::new(Arc::clone(&tools)))
            as Arc<dyn KernelPlugin>],
    )
    .expect("mount `tool-registry`");

    assert!(
        ctx.require::<McpSupervisor>(KEY_MCP).is_err(),
        "without the `mcp` plugin the service does not exist"
    );
    let service = ctx
        .require::<ToolRegistry>(KEY_TOOLS)
        .expect("the rest of the system runs without the optional capability");
    assert!(Arc::ptr_eq(&service, &tools));
    assert!(
        !tools.contains("mcp__srv__echo"),
        "nothing ticks, so no remote tool is registered"
    );

    drop(mounted);
}

#[test]
fn manifest_matches_provided_key() {
    let plugin = McpPlugin::new(Arc::new(McpSupervisor::new()));
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "mcp");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_MCP.to_string()]);
    assert_eq!(KEY_MCP, "mcp");
    assert!(
        manifest.inject.is_empty(),
        "the supervisor arrives with the plugin value; the plugin injects nothing"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}
