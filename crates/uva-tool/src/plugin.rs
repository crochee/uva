//! The `tools` service: the node's tool registry, exposed as a native
//! kernel plugin.
//!
//! # The registry, and nothing else
//!
//! [`ToolRegistryPlugin`] provides the registry under [`KEY_TOOLS`]; it
//! does **not** install any tool. Each agent-facing tool (`read` /
//! `write` / `shell` / `todo` / `web`, …) is its own plugin that
//! registers its entries in `apply` and returns the registration as a
//! kernel effect, so unloading a brick removes exactly its tools while
//! the registry — and every other tool — stays available.
//!
//! The registry keeps the exposure distinction the loop relies on
//! ([`ToolExposure`] `::Direct` / `Deferred` / `Hidden`): the plugin
//! moves the registry, it does not flatten the surface.
//!
//! # The service value's shape
//!
//! [`ToolRegistry`] is `Sized`, so `apply` calls
//! `ctx.provide_arc(KEY_TOOLS, Arc<ToolRegistry>)` and a consumer
//! writes `ctx.require::<ToolRegistry>(KEY_TOOLS)`, which yields an
//! `Arc` pointing at the same registry the plugin holds.
//!
//! # Lifecycle
//!
//! The plugin returns its provide handle from
//! [`apply`](uva_kernel::Plugin::apply), so unloading it revokes
//! the service. Consumers that injected `tools` (the `agentLoop` plugin
//! among them) are parked in `Pending`, and a tool registration that
//! targets the withdrawn service fails rather than landing in a
//! detached registry.
//!
//! The manifest declares no `inject` entries (the registry arrives with
//! the plugin value), no capabilities, no ABI, and no configuration
//! schema — a native plugin takes its configuration through its
//! constructor.

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

use crate::registry::ToolRegistry;

/// The service key the `tools` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_TOOLS: &str = "tools";

/// The plugin id the `tool-registry` manifest declares.
const TOOL_REGISTRY_ID: &str = "tool-registry";

/// The `tool-registry` kernel plugin: provides [`KEY_TOOLS`].
///
/// Mount it with [`uva_kernel::mount_all`] before any tool brick
/// that registers through it. See the [module docs](crate::plugin) for
/// the service's shape and the unload contract.
pub struct ToolRegistryPlugin {
    manifest: PluginManifest,
    registry: Arc<ToolRegistry>,
}

impl ToolRegistryPlugin {
    /// Publish `registry` as the `tools` service.
    ///
    /// The registry is built by the host (one per node) and shared by
    /// every agent the host runs.
    pub fn new(registry: Arc<ToolRegistry>) -> Self {
        let mut manifest =
            PluginManifest::new(TOOL_REGISTRY_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_TOOLS.to_string());
        Self { manifest, registry }
    }
}

impl KernelPlugin for ToolRegistryPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        Ok(vec![
            ctx.provide_arc(KEY_TOOLS, Arc::clone(&self.registry))?,
        ])
    }
}
