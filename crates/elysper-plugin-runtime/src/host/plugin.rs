//! The process plugin channel as a native kernel plugin.
//!
//! [`ProcessPluginsPlugin`] is the kernel entry (`id =
//! "process-plugins"`) for the registrations the deployment's
//! `[plugins]` section installed into a harness [`Host`]. It does not
//! spawn anything itself — [`RemotePlugin`](crate::host::RemotePlugin) owns
//! the process — it adopts the result of that install:
//!
//! - `apply` binds the adopted host to the `tools` service, so the
//!   tools the host holds are dispatchable through the same
//!   [`ToolRegistry`] every other plugin uses;
//! - the returned effect owns the registrations, and dropping it
//!   withdraws them. A [`Registration`] withdraws when its last clone
//!   drops, so the boot path hands the composition's registrations
//!   over rather than keeping a copy.
//!
//! `RuntimeKind` gains no `Process` variant: a process is a host
//! policy, not a way to load a plugin. The remote channel stays a
//! native entry whose tools are ordinary registry entries.
//!
//! # The registry is injected, not built
//!
//! The manifest injects `tools`, which the `tool-registry` plugin
//! provides ([`ToolRegistryPlugin`](elysper_tool::ToolRegistryPlugin)).
//! Mounting this entry without that service is a missing dependency,
//! not a silently unbound host whose tools nobody can call.
//!
//! # Unload
//!
//! The effect takes the registrations out of the plugin, so a second
//! `apply` on the same value (a framework re-activation, not a remount)
//! returns an effect that owns nothing. Withdrawal of a remote plugin's
//! last registration drops the [`RemoteTool`](crate::host::RemotePlugin)
//! handles, and the last handle to the connection kills the plugin
//! process — see `Connection`'s drop.

use std::sync::{Arc, Mutex, PoisonError};

use elysper_harness::{Host, Registration};
use elysper_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};
use elysper_tool::{KEY_TOOLS, ToolRegistry};

/// The plugin id the `process-plugins` manifest declares.
const PROCESS_PLUGINS_ID: &str = "process-plugins";

/// Label of the effect that owns the adopted registrations.
const PROCESS_PLUGINS_EFFECT: &str = "process-plugins";

/// The `process-plugins` kernel plugin: adopts a host's registrations.
///
/// Mount it with [`elysper_kernel::mount_all`] in the same group as
/// (or after) the `tool-registry` plugin. The boot builds one from the
/// host the `[plugins]` composition populated. Unloading the entry
/// drops the registrations it adopted, which withdraws the tools and
/// kills the plugin processes.
pub struct ProcessPluginsPlugin {
    manifest: PluginManifest,
    host: Host,
    // `None` once the effect took them: the registration set has
    // exactly one owner, which is what makes unload withdrawal work.
    registrations: Mutex<Option<Vec<Registration>>>,
}

impl ProcessPluginsPlugin {
    /// Build the entry over `host` and the registrations it produced.
    ///
    /// `host` is a clone of the deployment's one host: the clone
    /// shares its registrations, so binding it here is binding the
    /// host every agent already receives. `registrations` are the
    /// install's handles; hand over the only live set, or the effect's
    /// drop cannot withdraw them.
    #[must_use]
    pub fn new(host: Host, registrations: Vec<Registration>) -> Self {
        let mut manifest =
            PluginManifest::new(PROCESS_PLUGINS_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.inject.push(KEY_TOOLS.to_string());
        Self {
            manifest,
            host,
            registrations: Mutex::new(Some(registrations)),
        }
    }
}

impl KernelPlugin for ProcessPluginsPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let tools = ctx.require::<ToolRegistry>(KEY_TOOLS)?;
        // A host already bound to this registry is a no-op: the
        // composition in the boot path binds before it installs, and
        // the kernel then provides that same registry. A bind some
        // other registry owns is recorded on the host (and logged
        // here); the adopted registrations are still withdrawn on
        // unload.
        for diagnostic in self.host.bind(Arc::clone(&tools)) {
            tracing::warn!(
                kind = ?diagnostic.kind,
                source = %diagnostic.source,
                detail = %diagnostic.detail,
                "host registration refused while mounting `process-plugins`"
            );
        }
        let registrations = self
            .registrations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .unwrap_or_default();
        // The closure owns the registrations: a failed `effect` call
        // drops it, which withdraws them; a successful one drops them
        // when the kernel unloads the plugin.
        match ctx.effect(PROCESS_PLUGINS_EFFECT, move || {
            drop(registrations);
            Ok(())
        }) {
            Ok(effect) => Ok(vec![effect]),
            Err(error) => Err(error),
        }
    }
}
