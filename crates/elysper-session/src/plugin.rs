//! The `sessions` service: the node's session registry, exposed as a
//! native kernel plugin.
//!
//! # The registry, and nothing else
//!
//! [`SessionStorePlugin`] builds a
//! [`SessionRegistry`](crate::manager::SessionRegistry) from the
//! [`SessionStoreConfig`] it was constructed with and provides it
//! under [`KEY_SESSIONS`]. A run takes its own sink and writer from
//! the registry (`SessionRegistry::sink` / `SessionRegistry::writer`),
//! so concurrent sessions write through separate writers and their
//! rows never interleave. The registry owns the per-session maps, so
//! two callers that ask for the same session get the same sink — that
//! identity is what makes "one writer per session" enforceable.
//!
//! # The service value's shape
//!
//! [`SessionRegistry`](crate::manager::SessionRegistry) is `Sized`, so
//! `apply` calls
//! `ctx.provide_arc(KEY_SESSIONS, Arc<SessionRegistry>)` and a consumer
//! writes `ctx.require::<SessionRegistry>(KEY_SESSIONS)`, which yields
//! an `Arc` pointing at the registry the plugin built.
//!
//! # Lifecycle
//!
//! The plugin returns its provide handle from
//! [`apply`](elysper_kernel::Plugin::apply), so unloading it revokes
//! the service. Consumers that injected `sessions` (the `agentLoop`
//! plugin among them) are parked in `Pending`, and a run that needs a
//! writer is refused until a session store is mounted again.
//!
//! The manifest declares no `inject` entries (the registry is built
//! from the plugin's own configuration), no capabilities, no ABI, and
//! no configuration schema — a native plugin takes its configuration
//! through its constructor. Which backend a session gets (JSONL on
//! disk or the in-memory sink) is the sink factory's business, not the
//! plugin's.

use std::{path::PathBuf, sync::Arc};

use elysper_core::blocking::Blocking;
use elysper_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};

use crate::manager::SessionRegistry;

/// The service key the `sessions` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_SESSIONS: &str = "sessions";

// The plugin id the `session-store` manifest declares.
const SESSION_STORE_ID: &str = "session-store";

#[derive(Clone)]
/// The configuration [`SessionStorePlugin`] builds its registry from.
///
/// The plugin owns the construction, so the host hands over the root
/// the node's sessions live under and, when it runs on a real runtime,
/// the blocking seam its sinks append on.
pub struct SessionStoreConfig {
    /// The `<sessions_root>` every session directory hangs off.
    pub sessions_dir: PathBuf,
    /// The blocking seam every sink this registry creates runs its fs
    /// work on. `None` keeps the registry's runtime-free default
    /// ([`ThreadBlocking`](elysper_core::blocking::ThreadBlocking)).
    pub blocking: Option<Arc<dyn Blocking>>,
}

/// The `session-store` kernel plugin: provides [`KEY_SESSIONS`].
///
/// Mount it with [`elysper_kernel::mount_all`] before any run that
/// writes a session log. See the [module docs](crate::plugin) for the
/// service's shape and the unload contract.
pub struct SessionStorePlugin {
    manifest: PluginManifest,
    config: SessionStoreConfig,
}

impl SessionStorePlugin {
    #[must_use]
    /// Build the plugin over `config`; [`apply`](KernelPlugin::apply)
    /// constructs the registry from it.
    pub fn new(config: SessionStoreConfig) -> Self {
        let mut manifest =
            PluginManifest::new(SESSION_STORE_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_SESSIONS.to_string());
        Self { manifest, config }
    }
}

impl KernelPlugin for SessionStorePlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let mut registry =
            SessionRegistry::new(self.config.sessions_dir.clone());
        if let Some(blocking) = self.config.blocking.as_ref() {
            registry = registry.with_blocking(Arc::clone(blocking));
        }
        Ok(vec![ctx.provide_arc(KEY_SESSIONS, Arc::new(registry))?])
    }
}
