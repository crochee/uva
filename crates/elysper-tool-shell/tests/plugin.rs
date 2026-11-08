//! The shell crate's native kernel plugins: the `sandbox` service and
//! the `shell` tool brick.
//!
//! The `sandbox` plugin publishes the backend/policy pair under
//! `sandbox`; the one rule the mount enforces is fail-closed: a
//! confined policy whose backend cannot enforce it fails `apply`
//! instead of publishing a service that would silently run unconfined.
//!
//! The `shell` tool brick injects `tools` and files its tool under its
//! own registration token, so dropping the mount withdraws exactly that
//! entry. Mounting it without the registry is refused before it starts,
//! and a Core entry under the same name makes the mount fail loudly.

use std::{path::Path, sync::Arc};

use elysper_kernel::{
    Context,
    MountError,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};
use elysper_tool::{
    Tool,
    ToolEntry,
    ToolRegistry,
    plugin::{KEY_TOOLS, ToolRegistryPlugin},
};
use elysper_tool_shell::{
    BackendAvailability,
    ConfinedCommand,
    ExecutionPolicy,
    SandboxBackend,
    SandboxError,
    ShellTool,
    plugin::{KEY_SANDBOX, SandboxPlugin, SandboxService, ShellToolPlugin},
};

/// A backend whose availability the test decides.
///
/// It never builds an enforcing wrap: every test here exercises the
/// availability gate, not argv construction (that is
/// [`elysper_tool_shell::bwrap_profile_args`]'s suite).
#[derive(Debug)]
struct StubBackend {
    availability: BackendAvailability,
}

impl StubBackend {
    fn available() -> Self {
        Self {
            availability: BackendAvailability::Available,
        }
    }

    fn unavailable(reason: &str) -> Self {
        Self {
            availability: BackendAvailability::Unavailable {
                reason: reason.to_string(),
            },
        }
    }
}

impl SandboxBackend for StubBackend {
    fn name(&self) -> &str {
        "stub"
    }

    fn availability(&self) -> BackendAvailability {
        self.availability.clone()
    }

    fn confine(
        &self,
        policy: ExecutionPolicy,
        _workspace_root: &Path,
    ) -> Result<ConfinedCommand, SandboxError> {
        Err(SandboxError::unavailable(
            policy,
            "the stub backend never confines",
        ))
    }

    fn denial_signatures(&self) -> &'static [&'static str] {
        &[]
    }
}

#[test]
fn sandbox_without_backend_fails_closed() {
    let ctx = Context::new();
    let plugin = SandboxPlugin::new(
        Arc::new(StubBackend::unavailable("bwrap is not installed")),
        ExecutionPolicy::ReadOnly,
    );

    let error = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect_err("a confined policy without a usable backend must fail");

    match error {
        MountError::ApplyFailed { plugin_id, source } => {
            assert_eq!(plugin_id, "sandbox");
            let message = source.to_string();
            assert!(
                message.contains("bwrap is not installed"),
                "the refusal names the probe's reason: {message}"
            );
            assert!(
                message.contains("read-only"),
                "the refusal names the policy: {message}"
            );
        }
        other => panic!("unexpected mount error: {other}"),
    }
    assert!(
        ctx.require::<SandboxService>(KEY_SANDBOX).is_err(),
        "a failed mount must not leave a `sandbox` service behind"
    );
}

#[test]
fn sandbox_with_available_backend_provides_service() {
    let ctx = Context::new();
    let plugin = SandboxPlugin::new(
        Arc::new(StubBackend::available()),
        ExecutionPolicy::WorkspaceWrite,
    );

    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("mount `sandbox`");
    assert_eq!(mounted.len(), 1);

    let service = ctx
        .require::<SandboxService>(KEY_SANDBOX)
        .expect("`sandbox` resolves");
    assert_eq!(service.policy, ExecutionPolicy::WorkspaceWrite);
    assert_eq!(service.backend.name(), "stub");
    assert!(service.backend.availability().is_available());

    drop(mounted);
    assert!(
        ctx.require::<SandboxService>(KEY_SANDBOX).is_err(),
        "unload must withdraw `sandbox`"
    );
}

#[test]
fn unconfined_policy_does_not_need_a_backend() {
    let ctx = Context::new();
    let plugin = SandboxPlugin::new(
        Arc::new(StubBackend::unavailable("bwrap is not installed")),
        ExecutionPolicy::DangerFullAccess,
    );

    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("`DangerFullAccess` never needs a confining backend");
    let service = ctx
        .require::<SandboxService>(KEY_SANDBOX)
        .expect("`sandbox` resolves");
    assert_eq!(service.policy, ExecutionPolicy::DangerFullAccess);

    drop(mounted);
}

#[test]
fn manifest_matches_provided_key() {
    let plugin = SandboxPlugin::new(
        Arc::new(StubBackend::available()),
        ExecutionPolicy::ReadOnly,
    );
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "sandbox");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_SANDBOX.to_string()]);
    assert_eq!(KEY_SANDBOX, "sandbox");
    assert!(
        manifest.inject.is_empty(),
        "the backend and policy arrive with the plugin value; it injects nothing"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}

#[test]
fn registers_the_tool_and_unload_removes_it() {
    let ctx = Context::new();
    let registry = Arc::new(ToolRegistry::new());
    let tool = ShellTool::new();
    let name = tool.name().to_string();

    let mounted = mount_all(
        &ctx,
        &[
            Arc::new(ToolRegistryPlugin::new(Arc::clone(&registry)))
                as Arc<dyn KernelPlugin>,
            Arc::new(ShellToolPlugin::new()) as Arc<dyn KernelPlugin>,
        ],
    )
    .expect("mount `tool-registry` and `tool-shell`");
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
        &[Arc::new(ShellToolPlugin::new()) as Arc<dyn KernelPlugin>],
    )
    .expect_err("the brick cannot activate without `tools`");

    match error {
        MountError::MissingDependency { plugin_id, keys } => {
            assert_eq!(plugin_id, "tool-shell");
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
    let tool = ShellTool::new();
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
            Arc::new(ShellToolPlugin::new()) as Arc<dyn KernelPlugin>,
        ],
    )
    .expect_err("the mount must fail, not silently skip `shell`");

    let MountError::ApplyFailed { plugin_id, source } = error else {
        panic!("expected ApplyFailed, got {error:?}");
    };
    assert_eq!(plugin_id, "tool-shell");
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
fn shell_tool_manifest_injects_only_the_registry() {
    let plugin = ShellToolPlugin::new();
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "tool-shell");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert!(
        manifest.provides.is_empty(),
        "the brick consumes `tools`; it provides nothing"
    );
    assert_eq!(manifest.inject, vec![KEY_TOOLS.to_string()]);
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}
