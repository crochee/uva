//! The `hooks` kernel service: the plugin that publishes the
//! deployment's [`HookRegistry`].
//!
//! # The registry is the service
//!
//! [`HookPlugin`] provides the deployment's registry as the `hooks`
//! service. The plugin's value holds a clone of the same table, so a
//! caller installs registrations into a registry before mounting the
//! plugin:
//!
//! ```ignore
//! let registry = HookRegistry::new();
//! let registration = registry.register_gate(TOOL_PRE, gate)?;
//! mount_all(&ctx, &[Arc::new(HookPlugin::new(registry.clone()))])?;
//! // `agent-loop` injects `hooks`, resolves the service, and hands
//! // the registry to every run's `Seams`.
//! ```
//!
//! Dropping `registration` withdraws the row from every agent that
//! shares the registry; dropping the mount handle withdraws the
//! service, and the kernel parks every consumer that injected it.
//!
//! # Lifecycle
//!
//! `apply` returns its provide handle, so unloading the plugin revokes
//! the service. The manifest injects nothing: the registry arrives
//! with the plugin value.

use std::sync::Arc;

use uva_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};

use crate::registry::HookRegistry;

/// The service key the `hooks` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_HOOKS: &str = "hooks";

// The plugin id the `hooks` manifest declares.
const HOOKS_ID: &str = "hooks";

/// The `hooks` kernel plugin: provides [`KEY_HOOKS`].
///
/// Mount it with [`uva_kernel::mount_all`]. See the module docs for
/// the service's shape and the unload contract.
pub struct HookPlugin {
    manifest: PluginManifest,
    registry: HookRegistry,
}

impl HookPlugin {
    /// Publish `registry` as the `hooks` service.
    ///
    /// The caller builds and populates the registry; the plugin only
    /// publishes it.
    pub fn new(registry: HookRegistry) -> Self {
        let mut manifest =
            PluginManifest::new(HOOKS_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_HOOKS.to_string());
        Self { manifest, registry }
    }

    /// The registry this plugin publishes.
    #[must_use]
    pub fn registry(&self) -> &HookRegistry {
        &self.registry
    }
}

impl KernelPlugin for HookPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        Ok(vec![
            ctx.provide_arc(KEY_HOOKS, Arc::new(self.registry.clone()))?,
        ])
    }
}
