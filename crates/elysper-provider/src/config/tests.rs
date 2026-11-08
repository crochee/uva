use super::*;

#[test]
fn test_default_config() {
    let config = WorkspaceConfig::default();
    assert_eq!(config.default_provider, "openai");
    assert_eq!(config.default_model, "gpt-4o");
}

#[test]
fn test_config_from_toml() {
    let toml_str = r#"
default_provider = "anthropic"
default_model = "claude-sonnet-4-20250514"

[providers.openai]
type = "openai"
base_url = "https://api.openai.com/v1"
api_key_env = "OPENAI_API_KEY"
default_model = "gpt-4o"

[providers.anthropic]
type = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
default_model = "claude-sonnet-4-20250514"
"#;
    let config: WorkspaceConfig = toml::from_str(toml_str).unwrap();
    assert_eq!(config.default_provider, "anthropic");
    assert_eq!(config.providers.len(), 2);
    assert!(config.providers.contains_key("openai"));
    assert!(config.providers.contains_key("anthropic"));
}

#[test]
fn test_resolve_api_key_missing() {
    let config = WorkspaceConfig::default();
    let entry = ProviderEntry {
        r#type: "openai".to_string(),
        base_url: None,
        api_key_env: "NONEXISTENT_KEY_FOR_TEST_12345".to_string(),
        default_model: None,
        context_window: None,
        max_output_tokens: None,
        supports_tools: None,
        supports_streaming: None,
        supports_reasoning: None,
    };
    let result = config.resolve_api_key(&entry);
    assert!(result.is_err());
}

#[test]
fn test_provider_not_found() {
    let config = WorkspaceConfig::default();
    let result = config.create_provider("nonexistent");
    assert!(result.is_err());
}

#[test]
#[serial_test::serial]
fn test_unsupported_provider_type() {
    let mut config = WorkspaceConfig::default();
    config.providers.insert(
        "test".to_string(),
        ProviderEntry {
            r#type: "gemini".to_string(),
            base_url: None,
            api_key_env: "GEMINI_API_KEY".to_string(),
            default_model: None,
            context_window: None,
            max_output_tokens: None,
            supports_tools: None,
            supports_streaming: None,
            supports_reasoning: None,
        },
    );
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("GEMINI_API_KEY", "test-key")
    };
    let result = config.create_provider("test");
    #[allow(unsafe_code)]
    unsafe {
        std::env::remove_var("GEMINI_API_KEY")
    };
    assert!(result.is_err());
}

/// A config entry naming an adapter this build does not have must
/// fail with a message that names the feature, not with the generic
/// "Unsupported provider type" (the config *is* supported; the
/// binary is not).
#[cfg(not(feature = "openai"))]
#[test]
fn openai_entry_without_the_adapter_names_the_feature() {
    let mut config = WorkspaceConfig::default();
    config.providers.insert(
        "o".to_string(),
        ProviderEntry {
            r#type: "openai".to_string(),
            base_url: None,
            api_key_env: "OPENAI_API_KEY".to_string(),
            default_model: None,
            context_window: None,
            max_output_tokens: None,
            supports_tools: None,
            supports_streaming: None,
            supports_reasoning: None,
        },
    );
    let Err(err) = config.create_provider("o") else {
        panic!("a build without the `openai` adapter must not build one");
    };
    let formatted = format!("{err:?}");
    assert!(
        formatted.contains("enable the `openai` feature"),
        "error must name the feature, got: {formatted}"
    );
}

/// `model_config_of` is the one piece of the config → provider
/// bridge the adapters share; its defaulting rules are pinned here
/// so both adapters cannot drift.
#[cfg(any(feature = "anthropic", feature = "openai"))]
#[test]
fn model_config_of_projects_entry_defaults() {
    let entry = ProviderEntry {
        r#type: "openai".to_string(),
        base_url: None,
        api_key_env: "OPENAI_API_KEY".to_string(),
        default_model: Some("gpt-4o-mini".to_string()),
        context_window: Some(64_000),
        max_output_tokens: Some(2048),
        supports_tools: Some(false),
        supports_streaming: Some(true),
        supports_reasoning: Some(true),
    };
    let cfg = model_config_of(&entry);
    assert_eq!(cfg.name, "gpt-4o-mini");
    assert_eq!(cfg.provider, "openai");
    assert_eq!(cfg.context_window, 64_000);
    assert_eq!(cfg.max_output_tokens, 2048);
    assert!(!cfg.supports_tools);
    assert!(cfg.supports_streaming);
    assert!(cfg.supports_reasoning);

    let empty = ProviderEntry {
        r#type: "custom".to_string(),
        base_url: None,
        api_key_env: "K".to_string(),
        default_model: None,
        context_window: None,
        max_output_tokens: None,
        supports_tools: None,
        supports_streaming: None,
        supports_reasoning: None,
    };
    let cfg = model_config_of(&empty);
    assert_eq!(cfg.name, "unknown");
    assert_eq!(cfg.context_window, 128_000);
    assert_eq!(cfg.max_output_tokens, 4096);
    assert!(cfg.supports_tools);
    assert!(cfg.supports_streaming);
    assert!(!cfg.supports_reasoning);
}

#[test]
fn test_available_providers() {
    let mut config = WorkspaceConfig::default();
    config.providers.insert(
        "openai".to_string(),
        ProviderEntry {
            r#type: "openai".to_string(),
            base_url: None,
            api_key_env: "OPENAI_API_KEY".to_string(),
            default_model: None,
            context_window: None,
            max_output_tokens: None,
            supports_tools: None,
            supports_streaming: None,
            supports_reasoning: None,
        },
    );
    let providers = config.available_providers();
    assert!(providers.contains(&"openai".to_string()));
}

#[test]
#[serial_test::serial]
fn test_env_or_returns_fallback_when_unset() {
    #[allow(unsafe_code)]
    unsafe {
        std::env::remove_var("ELYSPER_TEST_ENV_OR_MISSING")
    };
    assert_eq!(
        env_or("ELYSPER_TEST_ENV_OR_MISSING", "fallback"),
        "fallback"
    );
}

#[test]
#[serial_test::serial]
fn test_env_or_returns_value_when_set() {
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("ELYSPER_TEST_ENV_OR_PRESENT", "gpt-test")
    };
    assert_eq!(
        env_or("ELYSPER_TEST_ENV_OR_PRESENT", "fallback"),
        "gpt-test"
    );
    #[allow(unsafe_code)]
    unsafe {
        std::env::remove_var("ELYSPER_TEST_ENV_OR_PRESENT")
    };
}

#[test]
#[serial_test::serial]
fn test_env_or_ignores_empty_value() {
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("ELYSPER_TEST_ENV_OR_EMPTY", "   ")
    };
    assert_eq!(env_or("ELYSPER_TEST_ENV_OR_EMPTY", "fallback"), "fallback");
    #[allow(unsafe_code)]
    unsafe {
        std::env::remove_var("ELYSPER_TEST_ENV_OR_EMPTY")
    };
}

#[test]
#[serial_test::serial]
fn test_from_env_uses_openai_model_override() {
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("OPENAI_API_KEY", "test-key")
    };
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("OPENAI_MODEL", "custom-model")
    };
    let config = WorkspaceConfig::from_env();
    let openai = config.providers.get("openai").unwrap();
    assert_eq!(openai.default_model.as_deref(), Some("custom-model"));
    assert_eq!(config.default_model, "custom-model");
    #[allow(unsafe_code)]
    unsafe {
        std::env::remove_var("OPENAI_MODEL")
    };
    #[allow(unsafe_code)]
    unsafe {
        std::env::remove_var("OPENAI_API_KEY")
    };
}

#[test]
#[serial_test::serial]
fn test_from_env_uses_anthropic_model_override() {
    // Save and restore env vars to avoid polluting parallel tests.
    let saved: Vec<(&'static str, Option<String>)> =
        ["ANTHROPIC_API_KEY", "ANTHROPIC_MODEL"]
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();

    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("ANTHROPIC_API_KEY", "test-key")
    };
    #[allow(unsafe_code)]
    unsafe {
        std::env::set_var("ANTHROPIC_MODEL", "custom-claude")
    };
    let config = WorkspaceConfig::from_env();
    let anthropic = config.providers.get("anthropic").unwrap();
    assert_eq!(anthropic.default_model.as_deref(), Some("custom-claude"));

    for (k, v) in saved {
        #[allow(unsafe_code)]
        match v {
            Some(value) => unsafe { std::env::set_var(k, value) },
            None => unsafe { std::env::remove_var(k) },
        }
    }
}

#[test]
#[serial_test::serial]
fn test_from_env_returns_empty_providers_when_unset() {
    // Save and clear every provider env var so the test is hermetic.
    let saved = save_provider_env();
    clear_provider_env(&saved);
    let config = WorkspaceConfig::from_env();
    assert!(
        config.providers.is_empty(),
        "expected no providers when no env vars are set"
    );
    restore_provider_env(saved);
}

#[test]
#[serial_test::serial]
fn test_load_from_dir_returns_config_error_without_config_or_env() {
    let saved = save_provider_env();
    clear_provider_env(&saved);

    // Use a workspace root that definitely has no .agents/config.toml.
    let dir = tempfile_env_root();
    let result = WorkspaceConfig::load_from_dir(&dir);
    assert!(
        matches!(result, Err(Error::Config { .. })),
        "expected Error::Config on missing config, got {result:?}"
    );

    restore_provider_env(saved);
}

fn tempfile_env_root() -> std::path::PathBuf {
    // Cargo's target dir is always present and never contains a
    // .agents/config.toml, so we can use it as a "no config here" workspace.
    std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
}

/// Snapshot every env var that `WorkspaceConfig::from_env` (or any
/// future bridge like `ELYSPER_PROVIDER_API_KEY_*` injection) reads.
/// Tests must call this *before* mutating the environment so the
/// snapshot can be restored on the way out — without it, parallel
/// cargo-test workers can leak env state into each other and the
/// next test sees a polluted view.
fn save_provider_env() -> Vec<(String, Option<String>)> {
    const STATIC_VARS: &[&str] = &[
        "OPENAI_API_KEY",
        "OPENAI_BASE_URL",
        "OPENAI_MODEL",
        "ANTHROPIC_API_KEY",
        "ANTHROPIC_BASE_URL",
        "ANTHROPIC_MODEL",
    ];
    let mut out: Vec<(String, Option<String>)> = STATIC_VARS
        .iter()
        .map(|k| ((*k).to_string(), std::env::var(k).ok()))
        .collect();

    // Catch any injected API key (e.g. from an operator's env)
    // so a hermetic test doesn't accidentally observe one left behind
    // by a previous test in the same process.
    for (k, v) in std::env::vars() {
        if k.starts_with("ELYSPER_PROVIDER_API_KEY_") {
            out.push((k, Some(v)));
        }
    }
    out
}

#[allow(unsafe_code)]
fn clear_provider_env(saved: &[(String, Option<String>)]) {
    for (k, _) in saved {
        unsafe {
            std::env::remove_var(k);
        }
    }
}

#[allow(unsafe_code)]
fn restore_provider_env(saved: Vec<(String, Option<String>)>) {
    for (k, v) in saved {
        match v {
            Some(value) => unsafe {
                std::env::set_var(&k, value);
            },
            None => unsafe {
                std::env::remove_var(&k);
            },
        }
    }
}
