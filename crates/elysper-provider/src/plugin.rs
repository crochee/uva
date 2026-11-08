//! The `llm` service: the deployment's provider catalogue, exposed as a
//! native kernel plugin.
//!
//! # The service is a catalogue, not a provider
//!
//! A turn names the provider and the model it wants, and a resumed turn
//! names the pair its request snapshot recorded. One handle cannot
//! answer either question, so [`ModelAdapterPlugin`] provides an
//! [`Arc<dyn ProviderResolver>`](ProviderResolver) under
//! [`KEY_LLM`]: a consumer resolves `(provider, model)` to the handle
//! that pair runs on, and two requests of one session resolve to
//! different providers without sharing state.
//!
//! The resolver's implementation lives with whoever knows the
//! deployment's configured entries (the app server's
//! `ProviderCatalogue`), so this crate stays unaware of configuration
//! files, credentials, and per-provider adapters.
//!
//! # The service value's shape
//!
//! The kernel's `Context::provide_arc` / `Context::require` are
//! `Sized`, so a trait object travels **inside** the stored `Arc`
//! rather than as the type parameter. `apply` calls
//! `ctx.provide(KEY_LLM, Arc<dyn ProviderResolver>)`, and a consumer
//! writes
//!
//! ```ignore
//! let llm = ctx.require::<Arc<dyn ProviderResolver>>(KEY_LLM)?;
//! let handle = llm.resolve("openai", "gpt-4o");
//! ```
//!
//! The resolved value is `Arc<Arc<dyn ProviderResolver>>`; one deref is
//! the resolver. This is the same carrier shape
//! `elysper_harness::plugin::AgentLoopFactory` uses, and
//! `Arc::ptr_eq` on the inner `Arc` proves the service is the exact
//! resolver the plugin was built with.
//!
//! # The deployed plugin's carrier
//!
//! The app server registers `elysper-runtime`'s
//! `ModelAdapterServicePlugin` under the same `llm` key, and that
//! plugin stores the concrete `Arc<ProviderCatalogue>` (consumers
//! `require::<ProviderCatalogue>(KEY_LLM)` and coerce to
//! [`ProviderResolver`] themselves). This crate's [`ModelAdapterPlugin`]
//! stays the resolver-carrier shape for embedders that only have a
//! resolver; the two shapes are not interchangeable within one mount.
//!
//! # Lifecycle
//!
//! The plugin returns its provide handle from
//! [`apply`](elysper_kernel::Plugin::apply), so unloading it revokes
//! the service. Consumers that injected `llm` (the `agentLoop` plugin
//! among them) are parked in `Pending` by the kernel, and a request
//! that needs the service is refused until a provider is mounted again.
//!
//! The manifest declares no `inject` entries: the resolver arrives with
//! the plugin value (`ModelAdapterPlugin::new`), because the host built
//! it from the profile at boot. It declares no capabilities, no ABI,
//! and no configuration schema either — a native plugin takes its
//! configuration through its constructor.

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

use crate::traits::ModelProvider;

/// The service key the `llm` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_LLM: &str = "llm";

/// The service key the tier-1-wrapped default provider handle is
/// registered under.
///
/// `llm` is the catalogue a per-turn selection resolves against;
/// `llm.default` is the handle a run that names no model samples
/// with, already wrapped in the deployment's tier-1 chain
/// (semantic-cache → resilience → budget). A consumer holds
/// `Arc<dyn ModelProvider>`, the same carrier shape [`KEY_LLM`]'s
/// resolver uses.
pub const KEY_LLM_DEFAULT: &str = "llm.default";

/// The plugin id the `model-adapter` manifest declares.
const MODEL_ADAPTER_ID: &str = "model-adapter";

/// Resolves a recorded provider / model pair back to a live handle.
///
/// The `llm` service resolves to this catalogue rather than to a
/// single [`ModelProvider`], because one deployment can offer several
/// providers and a request picks among them per message. A resumed
/// turn resolves the pair its request-snapshot row recorded, so the
/// resume samples the same envelope the turn was suspended under
/// instead of silently falling back to the deployment default.
pub trait ProviderResolver: Send + Sync {
    /// The handle for `provider` / `model`, or `None` when the
    /// deployment no longer offers that pair.
    fn resolve(
        &self,
        provider: &str,
        model: &str,
    ) -> Option<Arc<dyn ModelProvider>>;
}

/// The `model-adapter` kernel plugin: provides [`KEY_LLM`].
///
/// Mount it with [`elysper_kernel::mount_all`]. See the
/// [module docs](crate::plugin) for the service's resolution shape and
/// the unload contract.
pub struct ModelAdapterPlugin {
    manifest: PluginManifest,
    resolver: Arc<dyn ProviderResolver>,
}

impl ModelAdapterPlugin {
    /// Wrap `resolver` as the `llm` service.
    ///
    /// The resolver is supplied by the host, which owns the profile's
    /// provider entries; the plugin only publishes it.
    pub fn new(resolver: Arc<dyn ProviderResolver>) -> Self {
        let mut manifest =
            PluginManifest::new(MODEL_ADAPTER_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_LLM.to_string());
        Self { manifest, resolver }
    }
}

impl KernelPlugin for ModelAdapterPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        Ok(vec![ctx.provide(KEY_LLM, Arc::clone(&self.resolver))?])
    }
}
