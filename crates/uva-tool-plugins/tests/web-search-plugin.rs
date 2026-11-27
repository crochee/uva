//! The `web_search` tool brick as a native kernel plugin.
//!
//! The brick injects `tools` and files its tool under its own
//! registration token, so mounting it with the real `tool-registry`
//! makes `web_search` dispatchable and dropping the mount withdraws
//! exactly that entry. Mounting it without the registry is refused
//! before it starts, and a Core entry under the same name makes the
//! mount fail loudly instead of mounting a brick whose tool never
//! appeared.

use std::sync::Arc;

use uva_kernel::{
    Context,
    MountError,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginState,
    RuntimeKind,
    mount_all,
};
use uva_tool::{
    Tool,
    ToolEntry,
    ToolProvenance,
    ToolRegistry,
    plugin::{KEY_TOOLS, ToolRegistryPlugin},
};
use uva_tool_plugins::web::{
    NoSearch,
    WebSearchTool,
    search_plugin::WebSearchToolPlugin,
};

#[test]
fn registers_the_tool_and_unload_removes_it() {
    let ctx = Context::new();
    let registry = Arc::new(ToolRegistry::new());
    let tool = WebSearchTool::new(Arc::new(NoSearch));
    let name = tool.name().to_string();

    let mounted = mount_all(
        &ctx,
        &[
            Arc::new(ToolRegistryPlugin::new(Arc::clone(&registry)))
                as Arc<dyn KernelPlugin>,
            Arc::new(WebSearchToolPlugin::new()) as Arc<dyn KernelPlugin>,
        ],
    )
    .expect("mount `tool-registry` and `tool-web-search`");
    assert_eq!(mounted.len(), 2);

    let service = ctx
        .require::<ToolRegistry>(KEY_TOOLS)
        .expect("`tools` resolves");
    assert!(
        Arc::ptr_eq(&service, &registry),
        "the brick registers through the service's registry"
    );
    assert!(service.contains(&name), "the brick's tool is dispatchable");
    assert_eq!(service.tool_count(), 1);
    // A shipped tool is `Core`: once it is registered, no later
    // registration may take the name.
    let record = service
        .snapshot_with_provenance()
        .into_iter()
        .find(|record| record.metadata.name == name)
        .expect("the brick's tool is registered");
    assert_eq!(
        record.provenance,
        ToolProvenance::Core,
        "the `web_search` tool owns its name"
    );

    assert!(
        service.descriptors().iter().any(|tool| tool.name == name),
        "the brick's tool is advertised to the model"
    );

    drop(mounted);
    assert!(
        !registry.contains(&name),
        "unload must withdraw the brick's registration"
    );
    assert_eq!(registry.tool_count(), 0);
}

#[test]
fn missing_tools_parks_the_brick() {
    let ctx = Context::new();

    let mounted = mount_all(
        &ctx,
        &[Arc::new(WebSearchToolPlugin::new()) as Arc<dyn KernelPlugin>],
    )
    .expect("a missing dependency parks the brick, it does not fail the group");

    assert_eq!(
        mounted.plugin_state("tool-web-search"),
        Some(PluginState::Pending),
        "the brick waits for `tools` (dsh PENDING)"
    );
}

#[test]
fn mount_fails_loudly_when_a_core_tool_owns_the_name() {
    let ctx = Context::new();
    let registry = Arc::new(ToolRegistry::new());
    let tool = WebSearchTool::new(Arc::new(NoSearch));
    let name = tool.name().to_string();
    assert!(
        registry.register_core_entry(ToolEntry::new(Arc::new(tool))),
        "the fixture core-registers `{name}`"
    );

    let error = mount_all(
        &ctx,
        &[
            Arc::new(ToolRegistryPlugin::new(Arc::clone(&registry)))
                as Arc<dyn KernelPlugin>,
            Arc::new(WebSearchToolPlugin::new()) as Arc<dyn KernelPlugin>,
        ],
    )
    .expect_err("the mount must fail, not silently skip `web_search`");

    let MountError::ApplyFailed { plugin_id, .. } = error else {
        panic!("expected ApplyFailed, got {error:?}");
    };
    assert_eq!(plugin_id, "tool-web-search");
    assert_eq!(
        registry.tool_count(),
        1,
        "only the fixture's core entry remains"
    );
}

#[test]
fn manifest_injects_only_the_registry() {
    let plugin = WebSearchToolPlugin::new();
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "tool-web-search");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert!(
        manifest.provides.is_empty(),
        "the brick consumes `tools`; it provides nothing"
    );
    assert_eq!(manifest.inject, vec![KEY_TOOLS.to_string()]);
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.config_schema.is_none());
}
