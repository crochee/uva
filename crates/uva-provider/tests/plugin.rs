//! The `llm` service as a native kernel plugin.
//!
//! The service's value is a catalogue, not a single provider: a
//! consumer resolves a `(provider, model)` pair through it, and
//! `require` must report it unavailable once the plugin unloads.

use std::sync::Arc;

use uva_kernel::{
    Context,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};
use uva_provider::{
    CompletionRequest,
    ModelProvider,
    ProviderResolver,
    completion_to_sampling,
    plugin::{KEY_LLM, ModelAdapterPlugin},
    traits_stub::ModelProviderStub,
};

/// A catalogue with exactly one entry: `openai/gpt-4o`.
struct OnePair {
    handle: Arc<dyn ModelProvider>,
}

impl ProviderResolver for OnePair {
    fn resolve(
        &self,
        provider: &str,
        model: &str,
    ) -> Option<Arc<dyn ModelProvider>> {
        (provider == "openai" && model == "gpt-4o")
            .then(|| Arc::clone(&self.handle))
    }
}

/// A `model-adapter` plugin whose catalogue offers `openai/gpt-4o`,
/// answering with a provider that completes with `ok`.
fn plugin() -> ModelAdapterPlugin {
    let provider: Arc<dyn ModelProvider> =
        Arc::new(ModelProviderStub::text_only("ok"));
    ModelAdapterPlugin::new(Arc::new(OnePair { handle: provider }))
}

#[tokio::test]
async fn provides_service_and_unload_removes_it() {
    let ctx = Context::new();

    let mounted =
        mount_all(&ctx, &[Arc::new(plugin()) as Arc<dyn KernelPlugin>])
            .expect("mount `model-adapter`");
    assert_eq!(mounted.len(), 1);

    // Behaviour, not object identity: a consumer of the mounted
    // service resolves the configured pair and drives the provider it
    // gets back, so the published catalogue must answer with a live
    // provider.
    let llm = ctx
        .require::<Arc<dyn ProviderResolver>>(KEY_LLM)
        .expect("`llm` resolves");
    let provider = llm
        .resolve("openai", "gpt-4o")
        .expect("the configured pair resolves");
    let response = provider
        .complete(CompletionRequest::default())
        .await
        .expect("the resolved provider answers");
    assert_eq!(
        completion_to_sampling(&response).text,
        "ok",
        "the service must hand back the catalogue's entry"
    );

    // An unconfigured pair (either half) is reported as no longer
    // offered rather than falling back to another entry.
    assert!(
        llm.resolve("openai", "unknown-model").is_none(),
        "an unconfigured model must not resolve"
    );
    assert!(
        llm.resolve("unknown-provider", "gpt-4o").is_none(),
        "an unconfigured provider must not resolve"
    );

    drop(mounted);
    assert!(
        ctx.require::<Arc<dyn ProviderResolver>>(KEY_LLM).is_err(),
        "unload must withdraw `llm`"
    );
}

#[test]
fn manifest_matches_provided_key() {
    let plugin = plugin();
    let manifest = plugin.manifest();

    assert_eq!(manifest.id, "model-adapter");
    assert_eq!(manifest.version, PLUGIN_PROTOCOL_VERSION);
    assert_eq!(manifest.runtime, RuntimeKind::Native);
    assert_eq!(manifest.provides, vec![KEY_LLM.to_string()]);
    assert_eq!(KEY_LLM, "llm");
    assert!(
        manifest.inject.is_empty(),
        "the host supplies the resolver; the plugin injects nothing"
    );
    assert!(manifest.capabilities.is_empty());
    assert!(manifest.config_schema.is_none());
}
