//! The `mcp` service: the MCP supervisor exposed as a native kernel
//! plugin.
//!
//! # The service is the supervisor
//!
//! [`McpPlugin`] constructs the host's [`McpSupervisor`] inside
//! [`apply`](elysper_kernel::Plugin::apply) and publishes it under
//! [`KEY_MCP`]. It adds no protocol logic of its own: the supervisor
//! already owns each server's live [`McpClient`](crate::McpClient) and
//! its registered [`McpToolGeneration`](crate::McpToolGeneration), and
//! reconnects with bounded backoff.
//!
//! # Tick ownership stays with the app-server
//!
//! The supervisor never spawns a timer: [`McpSupervisor::tick`] is a
//! clock-injected call the host drives. The app-server boot loop
//! resolves `mcp`, supervises the configured servers, and ticks with
//! its [`ToolRegistry`](elysper_tool::ToolRegistry), arming the next
//! deadline from the returned
//! [`SupervisorTick`](crate::SupervisorTick). Unloading the plugin
//! withdraws the service, so that loop's next `require` fails and the
//! loop stops — the plugin itself never ticks.
//!
//! The tools are therefore owned by the supervisor's generations, not
//! by the plugin: a reconnect swaps them, retry-budget exhaustion
//! unregisters them, and the boot loop's teardown drops the
//! supervisor. The plugin's only reversible effect is the service
//! registration.
//!
//! # The service value's shape
//!
//! [`McpSupervisor`] is `Sized`, so `apply` calls
//! `ctx.provide_arc(KEY_MCP, Arc<McpSupervisor>)` and a consumer writes
//! `ctx.require::<McpSupervisor>(KEY_MCP)`, which yields an `Arc`
//! pointing at the supervisor the plugin constructed for this mount.
//!
//! # Lifecycle
//!
//! The plugin returns its provide handle from
//! [`apply`](elysper_kernel::Plugin::apply), so unloading it revokes
//! the service. The supervisor is built by `apply` rather than handed
//! to the plugin value; the manifest injects nothing.

use std::sync::Arc;

use elysper_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};

use crate::supervisor::McpSupervisor;

/// The service key the `mcp` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_MCP: &str = "mcp";

// The plugin id the `mcp` manifest declares.
const MCP_ID: &str = "mcp";

/// The `mcp` kernel plugin: provides [`KEY_MCP`].
///
/// Mount it with [`elysper_kernel::mount_all`] and drive the
/// supervisor from the host's boot loop. See the [module
/// docs](crate::plugin) for the tick ownership boundary and the
/// unload contract.
pub struct McpPlugin {
    manifest: PluginManifest,
}

impl McpPlugin {
    /// Build the plugin; `apply` constructs a default
    /// [`McpSupervisor`] for each mount.
    #[must_use]
    pub fn new() -> Self {
        let mut manifest = PluginManifest::new(MCP_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_MCP.to_string());
        Self { manifest }
    }
}

impl Default for McpPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl KernelPlugin for McpPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let supervisor = Arc::new(McpSupervisor::new());
        Ok(vec![ctx.provide_arc(KEY_MCP, supervisor)?])
    }
}
