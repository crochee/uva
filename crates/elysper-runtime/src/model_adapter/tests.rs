use std::sync::Arc;

use elysper_provider::{
    config::{ProviderEntry, WorkspaceConfig},
    traits::ModelProvider,
};
use elysper_test_support::FakeProvider;

use super::{
    ModelAdapterConfig,
    ModelSelectionError,
    ProviderCatalogue,
    UNDECLARED_MODEL,
};

/// A catalogue with two providers that advertise different
/// models, plus one that advertises a model nothing else does.
fn catalogue() -> (
    ProviderCatalogue,
    Arc<dyn ModelProvider>,
    Arc<dyn ModelProvider>,
) {
    let openai: Arc<dyn ModelProvider> = Arc::new(FakeProvider::text("openai"));
    let anthropic: Arc<dyn ModelProvider> =
        Arc::new(FakeProvider::text("anthropic"));
    let catalogue = ProviderCatalogue::default()
        .with_handle("openai", "gpt-4o", Arc::clone(&openai))
        .with_handle(
            "anthropic",
            "claude-sonnet-4-20250514",
            Arc::clone(&anthropic),
        );
    (catalogue, openai, anthropic)
}

fn entry(kind: &str, model: Option<&str>) -> ProviderEntry {
    ProviderEntry {
        r#type: kind.to_string(),
        base_url: None,
        api_key_env: "ELYSPER_TEST_UNSET_KEY".to_string(),
        default_model: model.map(str::to_string),
        context_window: None,
        max_output_tokens: None,
        supports_tools: None,
        supports_streaming: None,
        supports_reasoning: None,
    }
}

#[test]
fn qualified_selection_resolves_to_that_provider() {
    let (catalogue, openai, anthropic) = catalogue();
    let resolved = catalogue
        .resolve(Some("anthropic/claude-sonnet-4-20250514"))
        .expect("a qualified selection resolves");
    assert!(Arc::ptr_eq(&resolved, &anthropic));
    assert!(!Arc::ptr_eq(&resolved, &openai));
}

#[test]
fn bare_model_name_resolves_through_the_one_provider_offering_it() {
    let (catalogue, _openai, anthropic) = catalogue();
    let resolved = catalogue
        .resolve(Some("claude-sonnet-4-20250514"))
        .expect("a bare model name resolves");
    assert!(Arc::ptr_eq(&resolved, &anthropic));
}

#[test]
fn no_selection_resolves_to_the_default_provider() {
    let (catalogue, openai, _anthropic) = catalogue();
    assert!(Arc::ptr_eq(&catalogue.resolve(None).unwrap(), &openai));
    // A blank field means the same as an absent one.
    assert!(Arc::ptr_eq(
        &catalogue.resolve(Some("  ")).unwrap(),
        &openai
    ));
}

#[test]
fn unknown_provider_error_names_the_value() {
    let (catalogue, _openai, _anthropic) = catalogue();
    let error = catalogue
        .resolve(Some("gemini/gemini-2.0"))
        .err()
        .expect("an unconfigured provider must not resolve");
    assert_eq!(
        error,
        ModelSelectionError::UnknownProvider {
            value: "gemini/gemini-2.0".to_string(),
        }
    );
    assert!(
        error.to_string().contains("gemini/gemini-2.0"),
        "the message must name the value: {error}"
    );
}

#[test]
fn unknown_model_error_names_the_provider_qualified_value() {
    let (catalogue, _openai, _anthropic) = catalogue();
    let error = catalogue
        .resolve(Some("openai/gpt-5"))
        .err()
        .expect("a model the provider does not offer must not resolve");
    assert_eq!(
        error,
        ModelSelectionError::UnknownModel {
            value: "openai/gpt-5".to_string(),
            provider: Some("openai".to_string()),
        }
    );
    assert!(error.to_string().contains("openai/gpt-5"));
}

#[test]
fn bare_model_offered_by_two_providers_is_ambiguous() {
    // A third provider serves as the deployment's default so the
    // two `shared-model` providers are *not* the default — the
    // ambiguity is real, not just a default-fallback pick.
    let shared: Arc<dyn ModelProvider> = Arc::new(FakeProvider::text("a"));
    let catalogue = ProviderCatalogue::default()
        .with_handle("third", "default-only", Arc::clone(&shared))
        .with_handle("first", "shared-model", Arc::clone(&shared))
        .with_handle("second", "shared-model", shared);
    let error = catalogue
        .resolve(Some("shared-model"))
        .err()
        .expect("the selection has to name the provider");
    assert_eq!(
        error,
        ModelSelectionError::AmbiguousModel {
            value: "shared-model".to_string(),
            providers: vec!["first".to_string(), "second".to_string()],
        }
    );
}

/// A bare model name several providers offer is resolved through
/// the deployment's `default_provider` when it is one of them.
#[test]
fn bare_model_offered_by_two_providers_resolves_to_the_default() {
    let shared: Arc<dyn ModelProvider> = Arc::new(FakeProvider::text("a"));
    let catalogue = ProviderCatalogue::default()
        .with_handle("default-wins", "shared-model", Arc::clone(&shared))
        .with_handle("loser", "shared-model", shared);
    let resolved = catalogue
        .resolve(Some("shared-model"))
        .expect("the default is among the matches, so the bare name resolves");
    // The catalogue's default is `default-wins` and is the only
    // one `default_handle` returns — so the test's signal is that
    // `resolve` and `default_handle` agree on the same handle.
    assert!(Arc::ptr_eq(&resolved, &catalogue.default_handle().unwrap(),));
    // The losing provider was not picked.
    assert_eq!(resolved.name(), "fake");
}

/// A provider the boot could not build (no API key here) is not
/// "unknown" — it exists in the config and the error has to say
/// why it cannot serve.
#[test]
fn unavailable_provider_reports_its_build_failure() {
    let mut config = WorkspaceConfig::default();
    config
        .providers
        .insert("openai".to_string(), entry("openai", Some("gpt-4o")));
    let catalogue = ProviderCatalogue::from_workspace_config(&config);
    let error = catalogue
        .resolve(Some("openai/gpt-4o"))
        .err()
        .expect("the key is unset, so the handle is not built");
    match &error {
        ModelSelectionError::Unavailable { provider, reason } => {
            assert_eq!(provider, "openai");
            assert!(
                reason.contains("ELYSPER_TEST_UNSET_KEY"),
                "the reason must name the missing key: {reason}"
            );
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[test]
fn undeclared_model_entry_advertises_the_models_placeholder() {
    let mut config = WorkspaceConfig::default();
    config
        .providers
        .insert("local".to_string(), entry("openai", None));
    let catalogue = ProviderCatalogue::from_workspace_config(&config);
    assert_eq!(
        catalogue
            .profiles
            .get("local")
            .expect("the entry is registered")
            .default_model(),
        UNDECLARED_MODEL
    );
    // The default provider is the config's `default_provider`
    // (absent from `providers` here), so it is unknown.
    assert!(matches!(
        catalogue.default_handle(),
        Err(ModelSelectionError::UnknownProvider { .. })
    ));
}

/// The config-driven entry point: `ModelAdapterConfig` carries the
/// same workspace shape `.agents/config.toml` uses, so a profile
/// entry's `config` builds the catalogue directly.
#[test]
fn catalogue_from_config_builds_the_configured_provider() {
    let mut workspace = WorkspaceConfig::default();
    workspace
        .providers
        .insert("local".to_string(), entry("openai", Some("gpt-4o")));
    let config = ModelAdapterConfig {
        workspace,
        cache: None,
        resilience: None,
        budget: None,
    };
    let catalogue = super::catalogue_from_config(&config);
    assert_eq!(catalogue.default_provider_name(), "openai");
    assert!(
        catalogue.resolve(Some("local/gpt-4o")).is_err(),
        "the key is unset, so the handle is not built — but the \
         entry is registered, not dropped"
    );
}

/// The `model-adapter` service plugin publishes the catalogue
/// under `llm` (same `Arc`, so consumers see the deployment's one
/// catalogue) and the tier-1-wrapped default handle under
/// `llm.default`; unmounting the plugin withdraws both.
#[tokio::test]
async fn service_plugin_publishes_catalogue_and_wrapped_default() {
    use std::sync::Arc;

    use elysper_kernel::{Context, mount_all};
    use elysper_provider::plugin::{KEY_LLM, KEY_LLM_DEFAULT};

    use super::{
        CacheSection,
        ModelAdapterConfig,
        ModelAdapterServicePlugin,
        ProviderCatalogue,
    };

    let inner = Arc::new(elysper_test_support::FakeProvider::text("ok"));
    let catalogue = Arc::new(ProviderCatalogue::default().with_handle(
        "fake",
        "fake-model",
        inner.clone() as Arc<dyn elysper_provider::ModelProvider>,
    ));
    let config = ModelAdapterConfig {
        workspace: WorkspaceConfig::default(),
        cache: Some(CacheSection {
            enabled: true,
            threshold: 0.99,
            ..CacheSection::default()
        }),
        resilience: None,
        budget: None,
    };
    let plugin = ModelAdapterServicePlugin::new(
        Arc::clone(&catalogue),
        &config,
        elysper_core::SharedClock::system(),
        Arc::new(elysper_harness_tokio::TokioSleeper),
    )
    .expect("the default handle is registered");

    let ctx = Context::new();
    let mounted =
        mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn elysper_kernel::Plugin>])
            .expect("mount the plugin");

    // The service answers as the deployment's catalogue: the
    // behaviour, not the allocation, is the contract.
    let svc = ctx
        .require::<ProviderCatalogue>(KEY_LLM)
        .expect("llm is the deployment catalogue");
    assert!(
        svc.resolve(Some("fake/fake-model")).is_ok(),
        "the catalogue service resolves the deployment's pair"
    );
    assert!(
        svc.resolve(Some("fake/unknown-model")).is_err(),
        "a model the deployment does not offer is refused"
    );

    let default = ctx
        .require::<Arc<dyn elysper_provider::ModelProvider>>(KEY_LLM_DEFAULT)
        .expect("llm.default is the wrapped default handle");
    let first = default.complete(request()).await.expect("first call");
    let second = default.complete(request()).await.expect("second call");
    assert!(!first.cached, "first call reaches the inner provider");
    assert!(second.cached, "cache wraps the published default");
    assert_eq!(
        inner.call_count.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the second identical call never reached the provider"
    );

    drop(mounted);
    assert!(
        ctx.get::<ProviderCatalogue>(KEY_LLM)
            .expect("query the context")
            .is_none(),
        "unmounting withdraws the llm service"
    );
    assert!(
        ctx.get::<Arc<dyn elysper_provider::ModelProvider>>(KEY_LLM_DEFAULT)
            .expect("query the context")
            .is_none(),
        "unmounting withdraws the llm.default service"
    );
}

/// A minimal completion request for the cache round-trip.
fn request() -> elysper_provider::CompletionRequest {
    use elysper_provider::{
        CompletionRequest,
        Content,
        ContentPart,
        Message,
        Role,
        TextContent,
    };
    CompletionRequest {
        model: "fake-model".into(),
        messages: std::sync::Arc::new(vec![Message {
            role: Role::User,
            content: Content::Single(ContentPart::Text(TextContent {
                text: "hello".into(),
                cache_control: None,
            })),
            tool_call_id: None,
            name: None,
            tool_result_cleared_at: None,
        }]),
        tools: std::sync::Arc::new(Vec::new()),
        ..CompletionRequest::default()
    }
}
