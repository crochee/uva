//! The `compaction` service: the node's compactor, exposed as a native
//! kernel plugin.
//!
//! # A wrapper, not a policy
//!
//! [`CompactionPlugin`] builds a
//! [`SummaryCompactor`] over the
//! provider it injects (`llm.default`) and the model and policy its
//! [`CompactionPluginConfig`] carries, then publishes it under
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
//! The provider arrives the same way: `llm.default` is stored as an
//! `Arc<dyn ModelProvider>`, so `apply` resolves
//! `ctx.require::<Arc<dyn ModelProvider>>(KEY_LLM_DEFAULT)` and keeps
//! the inner handle.
//!
//! # Lifecycle
//!
//! The manifest injects `llm.default`, so the kernel mounts the
//! compactor after the provider and parks it while the provider is
//! absent. The plugin returns its provide handle from
//! [`apply`](elysper_kernel::Plugin::apply), so unloading it revokes
//! the service. A loop that injected `compaction` then has no
//! summariser and falls back to its existing truncation path instead
//! of inventing a second selection rule.

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
use elysper_provider::{ModelProvider, plugin::KEY_LLM_DEFAULT};

use crate::compaction::{CompactionSettings, Compactor, SummaryCompactor};

/// The service key the `compaction` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_COMPACTION: &str = "compaction";

// The plugin id the `context-compactor` manifest declares.
const CONTEXT_COMPACTOR_ID: &str = "context-compactor";

#[derive(Clone, Debug, PartialEq, Eq)]
/// The configuration [`CompactionPlugin`] builds its compactor from.
///
/// The provider is not part of the config: it is the `llm.default`
/// service the plugin injects, so a provider swap parks and remounts
/// the compactor instead of leaving it summarising through a stale
/// handle.
pub struct CompactionPluginConfig {
    /// The model the summary request names.
    pub model: String,
    /// The host's compaction policy.
    pub settings: CompactionSettings,
}

/// The `context-compactor` kernel plugin: provides [`KEY_COMPACTION`].
///
/// Mount it with [`elysper_kernel::mount_all`]. See the [module
/// docs](crate::compaction_plugin) for the service's shape and the
/// unload contract.
pub struct CompactionPlugin {
    manifest: PluginManifest,
    config: CompactionPluginConfig,
}

impl CompactionPlugin {
    #[must_use]
    /// Build the plugin over `config`; [`apply`](KernelPlugin::apply)
    /// constructs the [`SummaryCompactor`] from it and the injected
    /// provider.
    pub fn new(config: CompactionPluginConfig) -> Self {
        let mut manifest =
            PluginManifest::new(CONTEXT_COMPACTOR_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_COMPACTION.to_string());
        manifest.inject.push(KEY_LLM_DEFAULT.to_string());
        Self { manifest, config }
    }
}

impl KernelPlugin for CompactionPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let provider =
            ctx.require::<Arc<dyn ModelProvider>>(KEY_LLM_DEFAULT)?;
        let compactor: Arc<dyn Compactor> = Arc::new(
            SummaryCompactor::new(
                Arc::clone(&*provider),
                self.config.model.clone(),
            )
            .with_settings(self.config.settings),
        );
        Ok(vec![ctx.provide(KEY_COMPACTION, compactor)?])
    }
}
