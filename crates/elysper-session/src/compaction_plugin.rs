//! The `compaction` service: the node's compactor, exposed as a native
//! kernel plugin.
//!
//! # A wrapper, not a policy
//!
//! [`CompactionPlugin`] publishes the host-built [`Compactor`] under
//! [`KEY_COMPACTION`]. Selection (which span to shadow, what the
//! summary says) and the pairing rule that keeps every tool call with
//! its result
//! ([`balanced_cut_at_or_before`])
//! stay in the compactor and the harness; publishing the trait object
//! here changes neither.
//!
//! # The service value's shape
//!
//! [`Compactor`] is a trait object, and
//! the kernel's `Context::provide` / `Context::require` are `Sized`, so
//! the object travels **inside** the stored `Arc`: `apply` calls
//! `ctx.provide(KEY_COMPACTION, Arc<dyn Compactor>)` and a consumer
//! writes `ctx.require::<Arc<dyn Compactor>>(KEY_COMPACTION)`. The
//! resolved value is `Arc<Arc<dyn Compactor>>`; one deref is the
//! compactor.
//!
//! # Lifecycle
//!
//! The plugin returns its provide handle from
//! [`apply`](elysper_kernel::Plugin::apply), so unloading it revokes
//! the service. A loop that injected `compaction` then has no
//! summariser and falls back to its existing truncation path instead of
//! inventing a second selection rule. The compactor arrives with the
//! plugin value, so the manifest injects nothing.

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

use crate::compaction::Compactor;

/// The service key the `compaction` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_COMPACTION: &str = "compaction";

/// The plugin id the `context-compactor` manifest declares.
const CONTEXT_COMPACTOR_ID: &str = "context-compactor";

/// The `context-compactor` kernel plugin: provides [`KEY_COMPACTION`].
///
/// Mount it with [`elysper_kernel::mount_all`]. See the [module
/// docs](crate::compaction_plugin) for the service's shape and the
/// unload contract.
pub struct CompactionPlugin {
    manifest: PluginManifest,
    compactor: Arc<dyn Compactor>,
}

impl CompactionPlugin {
    /// Publish `compactor` as the `compaction` service.
    ///
    /// The host builds the compactor (the default
    /// [`SummaryCompactor`](crate::compaction::SummaryCompactor) or a
    /// deployment's own); the plugin only publishes it.
    pub fn new(compactor: Arc<dyn Compactor>) -> Self {
        let mut manifest =
            PluginManifest::new(CONTEXT_COMPACTOR_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_COMPACTION.to_string());
        Self {
            manifest,
            compactor,
        }
    }
}

impl KernelPlugin for CompactionPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        Ok(vec![
            ctx.provide(KEY_COMPACTION, Arc::clone(&self.compactor))?,
        ])
    }
}
