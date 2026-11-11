//! The `subagents` service as a native kernel plugin.
//!
//! The plugin publishes the host-built delegator under `subagents`;
//! callers resolve the delegator, advertise its definitions, and lose
//! the service when the plugin unloads. Registering the `task` tool
//! into the `tools` registry is the tool-brick follow-up (Task 4.11).

use std::sync::Arc;

use elysper_harness::{DelegatingTool, DelegationCall};
use elysper_kernel::{
    Context,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};
use elysper_provider::ToolDefinition;
use elysper_tool::ToolOutput;
use elysper_tool_plugins::task::{
    TASK_TOOL_NAME,
    plugin::{KEY_SUBAGENTS, SubagentPlugin},
    task_tool_definition,
};
use futures::future::BoxFuture;

/// The smallest delegator the service test needs: it advertises the
/// real `task` definition and never runs a child.
struct StubDelegator;

impl DelegatingTool for StubDelegator {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![task_tool_definition()]
    }

    fn execute(
        &self,
        _call: DelegationCall<'_>,
    ) -> BoxFuture<'static, ToolOutput> {
        Box::pin(async { ToolOutput::text("stub child result") })
    }
}

#[test]
fn subagent_unload_removes_service() {
    let ctx = Context::new();
    let backend: Arc<dyn DelegatingTool> = Arc::new(StubDelegator);
    let plugin = SubagentPlugin::new(Arc::clone(&backend));

    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("mount `subagent-backend`");
    assert_eq!(mounted.len(), 1);

    let service = ctx
        .require::<Arc<dyn DelegatingTool>>(KEY_SUBAGENTS)
        .expect("`subagents` resolves");
    assert!(
        Arc::ptr_eq(service.as_ref(), &backend),
        "the service must be the delegator the plugin was built with"
    );
    let definitions = service.definitions();
    assert_eq!(definitions.len(), 1);
    assert_eq!(definitions[0].name, TASK_TOOL_NAME);

    drop(mounted);
    assert!(
        ctx.require::<Arc<dyn DelegatingTool>>(KEY_SUBAGENTS)
            .is_err(),
        "unload must withdraw `subagents`"
    );
}

#[test]
fn manifest_matches_provided_key() {
    let plugin = SubagentPlugin::new(Arc::new(StubDelegator));
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "subagent-backend");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_SUBAGENTS.to_string()]);
    assert_eq!(KEY_SUBAGENTS, "subagents");
    assert!(
        manifest.inject.is_empty(),
        "the delegator arrives with the plugin value; it injects nothing"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}
