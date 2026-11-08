//! The `llm` service as a native kernel plugin.
//!
//! The service's value is a catalogue, not a single provider: a
//! consumer resolves a `(provider, model)` pair through it, and
//! `require` must report it unavailable once the plugin unloads.

use std::sync::Arc;

use elysper_kernel::{
    Context,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    RuntimeKind,
    mount_all,
};
use elysper_provider::{
    ModelProvider,
    ProviderResolver,
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

/// A plugin over `OnePair`, plus the two handles it must answer with.
fn fixture() -> (
    ModelAdapterPlugin,
    Arc<dyn ProviderResolver>,
    Arc<dyn ModelProvider>,
) {
    let provider: Arc<dyn ModelProvider> =
        Arc::new(ModelProviderStub::text_only("ok"));
    let resolver: Arc<dyn ProviderResolver> = Arc::new(OnePair {
        handle: Arc::clone(&provider),
    });
    let plugin = ModelAdapterPlugin::new(Arc::clone(&resolver));
    (plugin, resolver, provider)
}

#[test]
fn provides_service_and_unload_removes_it() {
    let ctx = Context::new();
    let (plugin, resolver, provider) = fixture();

    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn KernelPlugin>])
        .expect("mount `model-adapter`");
    assert_eq!(mounted.len(), 1);

    let service = ctx
        .require::<Arc<dyn ProviderResolver>>(KEY_LLM)
        .expect("`llm` resolves");
    assert!(
        Arc::ptr_eq(service.as_ref(), &resolver),
        "the service must be the resolver the plugin was built with"
    );

    // The catalogue half: a known pair answers with the configured
    // handle, and an unknown pair (either half) is reported as no
    // longer offered.
    let resolved = service
        .resolve("openai", "gpt-4o")
        .expect("the configured pair resolves");
    assert!(
        Arc::ptr_eq(&resolved, &provider),
        "the resolver must answer with its configured handle"
    );
    assert!(
        service.resolve("openai", "unknown-model").is_none(),
        "an unconfigured model must not resolve"
    );
    assert!(
        service.resolve("unknown-provider", "gpt-4o").is_none(),
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
    let (plugin, _resolver, _provider) = fixture();
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
    assert!(manifest.abi.is_none());
    assert!(manifest.config_schema.is_none());
}
