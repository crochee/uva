//! The `tools` service as a native kernel plugin.
//!
//! The plugin holds the registry and nothing else: a tool is visible
//! through the service because it was registered on the registry, not
//! because the plugin installed it. The tool bricks are their own
//! plugins.

use std::sync::Arc;

use elysper_kernel::{
    Context,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};
use elysper_test_support::FakeTool;
use elysper_tool::{
    ToolEntry,
    ToolRegistry,
    plugin::{KEY_TOOLS, ToolRegistryPlugin},
};

#[test]
fn provides_service_and_unload_removes_it() {
    let ctx = Context::new();
    let registry = Arc::new(ToolRegistry::new());
    registry
        .register_entry(ToolEntry::new(Arc::new(FakeTool::new("probe", "ok"))));
    let plugin = ToolRegistryPlugin::new(Arc::clone(&registry));

    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("mount `tool-registry`");
    assert_eq!(mounted.len(), 1);

    let service = ctx
        .require::<ToolRegistry>(KEY_TOOLS)
        .expect("`tools` resolves");
    assert!(
        Arc::ptr_eq(&service, &registry),
        "the service must be the registry the plugin was built with"
    );

    // The registry half: a tool registered before the mount is both
    // dispatchable and advertised through the service.
    assert!(service.contains("probe"), "the registered tool is visible");
    assert_eq!(service.tool_count(), 1);
    assert!(
        service
            .descriptors()
            .iter()
            .any(|tool| tool.name == "probe"),
        "the registered tool is advertised to the model"
    );

    drop(mounted);
    assert!(
        ctx.require::<ToolRegistry>(KEY_TOOLS).is_err(),
        "unload must withdraw `tools`"
    );
}

#[test]
fn manifest_matches_provided_key() {
    let registry = Arc::new(ToolRegistry::new());
    let plugin = ToolRegistryPlugin::new(registry);
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "tool-registry");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_TOOLS.to_string()]);
    assert_eq!(KEY_TOOLS, "tools");
    assert!(
        manifest.inject.is_empty(),
        "the plugin owns the registry; it injects nothing"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}
