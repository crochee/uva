//! The `web_fetch` tool brick as a native kernel plugin.
//!
//! The brick injects `tools` and files its tool under its own
//! registration token, so mounting it with the real `tool-registry`
//! makes `web_fetch` dispatchable and dropping the mount withdraws
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
    RuntimeKind,
    mount_all,
};
use uva_tool::{
    Tool,
    ToolEntry,
    ToolRegistry,
    plugin::{KEY_TOOLS, ToolRegistryPlugin},
};
use uva_tool_plugins::web::{WebFetchTool, plugin::WebFetchToolPlugin};

#[test]
fn registers_the_tool_and_unload_removes_it() {
    let ctx = Context::new();
    let registry = Arc::new(ToolRegistry::new());
    let tool = WebFetchTool::new();
    let name = tool.name().to_string();

    let mounted = mount_all(
        &ctx,
        &[
            Arc::new(ToolRegistryPlugin::new(Arc::clone(&registry)))
                as Arc<dyn KernelPlugin>,
            Arc::new(WebFetchToolPlugin::new()) as Arc<dyn KernelPlugin>,
        ],
    )
    .expect("mount `tool-registry` and `tool-web`");
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
    assert!(
        ctx.require::<ToolRegistry>(KEY_TOOLS).is_err(),
        "the same mount group owned the `tools` service"
    );
}

#[test]
fn missing_tools_leaves_the_brick_unmounted() {
    let ctx = Context::new();

    let error = mount_all(
        &ctx,
        &[Arc::new(WebFetchToolPlugin::new()) as Arc<dyn KernelPlugin>],
    )
    .expect_err("the brick cannot activate without `tools`");

    match error {
        MountError::MissingDependency { plugin_id, keys } => {
            assert_eq!(plugin_id, "tool-web");
            assert!(
                keys.iter().any(|key| key == KEY_TOOLS),
                "the refusal names `tools`: {keys:?}"
            );
        }
        other => panic!("expected MissingDependency, got {other:?}"),
    }
}

#[test]
fn mount_fails_loudly_when_a_core_tool_owns_the_name() {
    let ctx = Context::new();
    let registry = Arc::new(ToolRegistry::new());
    let tool = WebFetchTool::new();
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
            Arc::new(WebFetchToolPlugin::new()) as Arc<dyn KernelPlugin>,
        ],
    )
    .expect_err("the mount must fail, not silently skip `web_fetch`");

    let MountError::ApplyFailed { plugin_id, source } = error else {
        panic!("expected ApplyFailed, got {error:?}");
    };
    assert_eq!(plugin_id, "tool-web");
    let message = source.to_string();
    assert!(
        message.contains(&name) && message.contains("core"),
        "the error must name the tool and the core owner: {message}"
    );
    assert_eq!(
        registry.tool_count(),
        1,
        "only the fixture's core entry remains"
    );
}

#[test]
fn manifest_injects_only_the_registry() {
    let plugin = WebFetchToolPlugin::new();
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "tool-web");
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
