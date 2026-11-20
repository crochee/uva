//! The shell crate's native kernel plugins: the `sandbox` service and
//! the `shell` tool brick.
//!
//! The `sandbox` plugin builds its backend from a
//! [`SandboxPluginConfig`] in `apply` and publishes the backend/policy
//! pair under `sandbox`; the one rule the mount enforces is
//! fail-closed: a confined policy the configured backend cannot enforce
//! fails `apply` instead of publishing a service that would silently
//! run unconfined.
//!
//! The `shell` tool brick injects `tools` and files its tool under its
//! own registration token, so dropping the mount withdraws exactly that
//! entry. Mounting it without the registry is refused before it starts,
//! and a Core entry under the same name makes the mount fail loudly.

use std::{path::Path, sync::Arc};

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
use uva_tool_plugins::shell::{
    ConfinedCommand,
    ExecutionPolicy,
    ShellTool,
    plugin::{
        KEY_SANDBOX,
        SandboxBackendKind,
        SandboxPlugin,
        SandboxPluginConfig,
        SandboxService,
        ShellToolPlugin,
    },
};

#[test]
fn confined_policy_with_the_noop_backend_fails_closed() {
    for policy in [ExecutionPolicy::ReadOnly, ExecutionPolicy::WorkspaceWrite] {
        let ctx = Context::new();
        let plugin = SandboxPlugin::new(SandboxPluginConfig {
            backend: SandboxBackendKind::Noop,
            policy,
        });

        let error =
            mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
                .expect_err("a confined policy cannot mount the no-op backend");

        let MountError::ApplyFailed { plugin_id, source } = error else {
            panic!("expected ApplyFailed, got {error:?}");
        };
        assert_eq!(plugin_id, "sandbox");
        let message = source.to_string();
        assert!(
            message.contains(policy.as_str()),
            "the refusal names the policy: {message}"
        );
        assert!(
            message.contains("does not confine"),
            "the refusal names why the backend cannot guard it: {message}"
        );
        assert!(
            ctx.require::<SandboxService>(KEY_SANDBOX).is_err(),
            "a failed mount must not leave a `sandbox` service behind"
        );
    }
}

#[test]
fn noop_backend_serves_danger_full_access() {
    let ctx = Context::new();
    let plugin = SandboxPlugin::new(SandboxPluginConfig::default());

    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("the default config needs no runner");
    assert_eq!(mounted.len(), 1);

    let service = ctx
        .require::<SandboxService>(KEY_SANDBOX)
        .expect("`sandbox` resolves");
    assert_eq!(service.policy, ExecutionPolicy::DangerFullAccess);
    assert_eq!(service.backend.name(), "none");
    assert!(service.is_enforceable());
    assert_eq!(
        service
            .backend
            .confine(ExecutionPolicy::DangerFullAccess, Path::new("/"))
            .expect("the no-op backend serves every unconfined run"),
        ConfinedCommand::unconfined(),
        "an unconfined run keeps today's `sh -c` argv"
    );

    drop(mounted);
    assert!(
        ctx.require::<SandboxService>(KEY_SANDBOX).is_err(),
        "unload must withdraw `sandbox`"
    );
}

#[test]
fn bwrap_kind_builds_the_bwrap_backend() {
    let ctx = Context::new();
    let plugin = SandboxPlugin::new(SandboxPluginConfig {
        backend: SandboxBackendKind::Bwrap,
        policy: ExecutionPolicy::DangerFullAccess,
    });

    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("an unconfined policy never probes the runner");
    let service = ctx
        .require::<SandboxService>(KEY_SANDBOX)
        .expect("`sandbox` resolves");
    assert_eq!(service.backend.name(), "bwrap");
    assert!(service.is_enforceable());
    // `DangerFullAccess` bypasses the runner, so this holds even where
    // bubblewrap is not installed.
    assert_eq!(
        service
            .backend
            .confine(ExecutionPolicy::DangerFullAccess, Path::new("/"))
            .expect("an unconfined run never needs the runner"),
        ConfinedCommand::unconfined()
    );

    drop(mounted);
    assert!(ctx.require::<SandboxService>(KEY_SANDBOX).is_err());
}

#[test]
fn default_config_is_the_noop_unconfined_pair() {
    let config = SandboxPluginConfig::default();
    assert_eq!(config.backend, SandboxBackendKind::Noop);
    assert_eq!(config.policy, ExecutionPolicy::DangerFullAccess);
}

#[test]
fn manifest_matches_provided_key() {
    let plugin = SandboxPlugin::new(SandboxPluginConfig::default());
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "sandbox");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_SANDBOX.to_string()]);
    assert_eq!(KEY_SANDBOX, "sandbox");
    assert!(
        manifest.inject.is_empty(),
        "the backend kind and policy arrive in the plugin's config; it injects nothing"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.config_schema.is_none());
}

#[test]
fn registers_the_tool_and_unload_removes_it() {
    let ctx = Context::new();
    let tool = ShellTool::new();
    let name = tool.name().to_string();

    let mounted = mount_all(
        &ctx,
        &[
            Arc::new(ToolRegistryPlugin::new(Arc::new(ToolRegistry::new())))
                as Arc<dyn KernelPlugin>,
            Arc::new(ShellToolPlugin::new()) as Arc<dyn KernelPlugin>,
        ],
    )
    .expect("mount `tool-registry` and `tool-shell`");
    assert_eq!(mounted.len(), 2);

    let service = ctx
        .require::<ToolRegistry>(KEY_TOOLS)
        .expect("`tools` resolves");
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
        "the `shell` tool owns its name"
    );

    assert!(
        service.descriptors().iter().any(|tool| tool.name == name),
        "the brick's tool is advertised to the model"
    );

    // Keep a handle on the registry to observe what unloading the group
    // does to the brick's registration.
    let observed = Arc::clone(&service);
    drop(mounted);
    assert!(
        !observed.contains(&name),
        "unload must withdraw the brick's registration"
    );
    assert_eq!(observed.tool_count(), 0);
    assert!(
        ctx.require::<ToolRegistry>(KEY_TOOLS).is_err(),
        "the same mount group owned the `tools` service"
    );
}

#[test]
fn missing_tools_parks_the_brick() {
    let ctx = Context::new();

    let mounted = mount_all(
        &ctx,
        &[Arc::new(ShellToolPlugin::new()) as Arc<dyn KernelPlugin>],
    )
    .expect("a missing dependency parks the brick, it does not fail the group");

    assert_eq!(
        mounted.plugin_state("tool-shell"),
        Some(PluginState::Pending),
        "the brick waits for `tools` (dsh PENDING)"
    );
}

#[test]
fn mount_fails_loudly_when_a_core_tool_owns_the_name() {
    let ctx = Context::new();
    let name = ShellTool::new().name().to_string();
    // The registry is constructed by its own mount, so the fixture
    // mounts it first and core-registers through the published service.
    let registry_mount = mount_all(
        &ctx,
        &[
            Arc::new(ToolRegistryPlugin::new(Arc::new(ToolRegistry::new())))
                as Arc<dyn KernelPlugin>,
        ],
    )
    .expect("mount `tool-registry`");
    let registry = ctx
        .require::<ToolRegistry>(KEY_TOOLS)
        .expect("`tools` resolves");
    assert!(
        registry
            .register_core_entry(ToolEntry::new(Arc::new(ShellTool::new()))),
        "the fixture core-registers `{name}`"
    );

    let error = mount_all(
        &ctx,
        &[Arc::new(ShellToolPlugin::new()) as Arc<dyn KernelPlugin>],
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

    drop(registry_mount);
    assert!(
        ctx.require::<ToolRegistry>(KEY_TOOLS).is_err(),
        "dropping the provider mount withdraws `tools`"
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
    assert!(manifest.config_schema.is_none());
}
