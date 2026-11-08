//! The `subagents` service: the node's delegation backend, exposed as a
//! native kernel plugin.
//!
//! # The service is the delegator
//!
//! [`SubagentPlugin`] publishes an `Arc<dyn DelegatingTool>` under
//! [`KEY_SUBAGENTS`]. The delegator is built by the host (from its
//! profile: in-process or subprocess) and already knows how to advertise
//! itself (`DelegatingTool::definitions`) and how to run one child
//! (`DelegatingTool::execute`); the plugin adds no selection logic of
//! its own.
//!
//! # The `task` tool is a separate brick
//!
//! Registering `task` in the `tools` registry is the tool-brick
//! follow-up (Task 4.11), not this plugin's job: the brick's
//! registration effect is what makes the tool appear in the model's
//! visible list and disappear when the brick unloads. This plugin owns
//! the **backend**, so a consumer resolves the delegator here and the
//! registry remains the only place that decides which tools a model
//! sees.
//!
//! # The service value's shape
//!
//! The kernel's `Context::provide` / `Context::require` are `Sized`, so
//! the trait object travels **inside** the stored `Arc`: `apply` calls
//! `ctx.provide(KEY_SUBAGENTS, Arc<dyn DelegatingTool>)` and a consumer
//! writes `ctx.require::<Arc<dyn DelegatingTool>>(KEY_SUBAGENTS)`. The
//! resolved value is `Arc<Arc<dyn DelegatingTool>>`; one deref is the
//! delegator, and `Arc::ptr_eq` on the inner `Arc` proves the service
//! is the exact backend the plugin was built with.
//!
//! # Lifecycle
//!
//! The plugin returns its provide handle from
//! [`apply`](elysper_kernel::Plugin::apply), so unloading it revokes
//! the service. A consumer that needed delegation then has no backend
//! to dispatch to; the manifest injects nothing because the delegator
//! arrives with the plugin value.

use std::sync::Arc;

use elysper_harness::DelegatingTool;
use elysper_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};

/// The service key the `subagents` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_SUBAGENTS: &str = "subagents";

/// The plugin id the `subagent-backend` manifest declares.
const SUBAGENT_BACKEND_ID: &str = "subagent-backend";

/// The `subagent-backend` kernel plugin: provides [`KEY_SUBAGENTS`].
///
/// Mount it with [`elysper_kernel::mount_all`]. See the [module
/// docs](crate::plugin) for the service's shape, the `task`-brick
/// boundary, and the unload contract.
pub struct SubagentPlugin {
    manifest: PluginManifest,
    backend: Arc<dyn DelegatingTool>,
}

impl SubagentPlugin {
    /// Publish `backend` as the `subagents` service.
    ///
    /// The host builds the delegator from its profile; the plugin only
    /// publishes it.
    pub fn new(backend: Arc<dyn DelegatingTool>) -> Self {
        let mut manifest =
            PluginManifest::new(SUBAGENT_BACKEND_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_SUBAGENTS.to_string());
        Self { manifest, backend }
    }
}

impl KernelPlugin for SubagentPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        Ok(vec![ctx.provide(KEY_SUBAGENTS, Arc::clone(&self.backend))?])
    }
}
