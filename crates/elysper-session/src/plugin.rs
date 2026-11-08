//! The `sessions` service: the node's session registry, exposed as a
//! native kernel plugin.
//!
//! # The registry, and nothing else
//!
//! [`SessionStorePlugin`] provides
//! [`SessionRegistry`](crate::manager::SessionRegistry) under
//! [`KEY_SESSIONS`]. A run takes its own sink and writer from the
//! registry (`SessionRegistry::sink` / `SessionRegistry::writer`), so
//! concurrent sessions write through separate writers and their rows
//! never interleave. The registry owns the per-session maps, so two
//! callers that ask for the same session get the same sink — that
//! identity is what makes "one writer per session" enforceable.
//!
//! # The service value's shape
//!
//! [`SessionRegistry`](crate::manager::SessionRegistry) is `Sized`, so
//! `apply` calls
//! `ctx.provide_arc(KEY_SESSIONS, Arc<SessionRegistry>)` and a consumer
//! writes `ctx.require::<SessionRegistry>(KEY_SESSIONS)`, which yields
//! an `Arc` pointing at the same registry the plugin holds.
//!
//! # Lifecycle
//!
//! The plugin returns its provide handle from
//! [`apply`](elysper_kernel::Plugin::apply), so unloading it revokes
//! the service. Consumers that injected `sessions` (the `agentLoop`
//! plugin among them) are parked in `Pending`, and a run that needs a
//! writer is refused until a session store is mounted again.
//!
//! The manifest declares no `inject` entries (the registry arrives with
//! the plugin value), no capabilities, no ABI, and no configuration
//! schema — a native plugin takes its configuration through its
//! constructor. Which backend a session gets (JSONL on disk or the
//! in-memory sink) is the sink factory's business, not the plugin's.

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

use crate::manager::SessionRegistry;

/// The service key the `sessions` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_SESSIONS: &str = "sessions";

/// The plugin id the `session-store` manifest declares.
const SESSION_STORE_ID: &str = "session-store";

/// The `session-store` kernel plugin: provides [`KEY_SESSIONS`].
///
/// Mount it with [`elysper_kernel::mount_all`] before any run that
/// writes a session log. See the [module docs](crate::plugin) for the
/// service's shape and the unload contract.
pub struct SessionStorePlugin {
    manifest: PluginManifest,
    registry: Arc<SessionRegistry>,
}

impl SessionStorePlugin {
    /// Publish `registry` as the `sessions` service.
    ///
    /// The registry is built by the host (one per node) and shared by
    /// every session the host runs.
    pub fn new(registry: Arc<SessionRegistry>) -> Self {
        let mut manifest =
            PluginManifest::new(SESSION_STORE_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_SESSIONS.to_string());
        Self { manifest, registry }
    }
}

impl KernelPlugin for SessionStorePlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        Ok(vec![
            ctx.provide_arc(KEY_SESSIONS, Arc::clone(&self.registry))?,
        ])
    }
}
