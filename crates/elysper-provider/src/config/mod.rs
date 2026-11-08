// Allow `result_large_err` for the whole file: P1b added 4 hidden
// fields to every struct-form variant (frames, backtrace, source,
// and the synthetic source chain), so every `Result<_, Error>` is
// at least 128 bytes. Boxing the error would force every call site
// to `.map_err(|e| *e)` (or accept the allocation), and the existing
// API has no `Box<Error>` in the public surface. Accept the size
// cost; revisit if profiling shows it matters.
#![allow(clippy::result_large_err)]

use std::{collections::HashMap, path::Path};

use elysper_core::{Error, Sensitive, SensitiveData};
use serde::{Deserialize, Serialize};

#[cfg(feature = "anthropic")]
use crate::anthropic::AnthropicProvider;
#[cfg(feature = "openai")]
use crate::openai::OpenAICompatibleProvider;
#[cfg(any(feature = "anthropic", feature = "openai"))]
use crate::types::ModelConfig;
use crate::{credential::CredentialError, traits::ModelProvider};

/// The error a build without the named adapter gets when the config
/// asks for it.
///
/// A config file is *data*: `[providers.x] type = "openai"` cannot fail
/// at compile time, so a build that turned the adapter off has to say
/// which feature to enable instead.
#[cfg(not(all(feature = "anthropic", feature = "openai")))]
pub(crate) fn adapter_not_compiled(kind: &str) -> Error {
    Error::config(format!(
        "provider type `{kind}` is not compiled in: enable the \
         `{kind}` feature of elysper-provider"
    ))
}

/// The `ModelConfig` a configured entry describes.
///
/// Adapter-independent in its body, but only the adapter construction
/// paths call it, so it is compiled with them.
#[cfg(any(feature = "anthropic", feature = "openai"))]
pub(crate) fn model_config_of(entry: &ProviderEntry) -> ModelConfig {
    ModelConfig {
        name: entry
            .default_model
            .clone()
            .unwrap_or_else(|| "unknown".to_string()),
        provider: entry.r#type.clone(),
        context_window: entry.context_window.unwrap_or(128_000),
        max_output_tokens: entry.max_output_tokens.unwrap_or(4096),
        supports_tools: entry.supports_tools.unwrap_or(true),
        supports_streaming: entry.supports_streaming.unwrap_or(true),
        supports_reasoning: entry.supports_reasoning.unwrap_or(false),
    }
}

#[derive(Clone, Debug)]
pub struct ProviderConfig {
    pub name: String,
    pub api_key: Sensitive<String>,
    pub api_endpoint: String,
    pub organization: Option<Sensitive<String>>,
    pub headers: HashMap<String, Sensitive<String>>,
}

impl SensitiveData for ProviderConfig {
    fn sensitive_fields() -> Vec<&'static str> {
        vec!["api_key", "organization", "headers"]
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ProviderEntry {
    pub r#type: String,
    #[serde(default)]
    pub base_url: Option<String>,
    pub api_key_env: String,
    #[serde(default)]
    pub default_model: Option<String>,
    #[serde(default)]
    pub context_window: Option<usize>,
    #[serde(default)]
    pub max_output_tokens: Option<usize>,
    #[serde(default)]
    pub supports_tools: Option<bool>,
    #[serde(default)]
    pub supports_streaming: Option<bool>,
    #[serde(default)]
    pub supports_reasoning: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    #[serde(default = "default_provider")]
    pub default_provider: String,
    #[serde(default = "default_model")]
    pub default_model: String,
    pub providers: HashMap<String, ProviderEntry>,
}

fn default_provider() -> String {
    "openai".to_string()
}

fn default_model() -> String {
    "gpt-4o".to_string()
}

/// Read an environment variable, returning the fallback if it is unset
/// or empty. Used so that empty `OPENAI_MODEL=""` does not silently
/// override the built-in default.
fn env_or(key: &str, fallback: &str) -> String {
    std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback.to_string())
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        Self {
            default_provider: default_provider(),
            default_model: default_model(),
            providers: HashMap::new(),
        }
    }
}

impl WorkspaceConfig {
    /// Load configuration from `<workspace_root>/.agents/config.toml`,
    /// falling back to environment variables when the file is missing.
    ///
    /// Panics if neither a config file nor any provider environment
    /// variables are present — running without a model provider is a
    /// hard configuration error and silently degrading to the
    /// built-in defaults would mask operator mistakes.
    pub fn load_from_dir(workspace_root: &Path) -> Result<Self, Error> {
        let config_path = workspace_root.join(".agents").join("config.toml");
        if config_path.exists() {
            let content = std::fs::read_to_string(&config_path)
                .map_err(|e| Error::config(e.to_string()))?;
            let config: WorkspaceConfig = toml::from_str(&content)
                .map_err(|e| Error::parse(e.to_string()))?;
            Ok(config)
        } else {
            let config = Self::from_env();
            if config.providers.is_empty() {
                return Err(Error::config(
                    "Elysper provider configuration missing: no .agents/config.toml found and \
                     no OPENAI_API_KEY/OPENAI_BASE_URL/ANTHROPIC_API_KEY environment variables \
                     are set. Provide one of these to start the server."
                        .to_string(),
                ));
            }
            Ok(config)
        }
    }

    pub fn from_env() -> Self {
        let mut providers = HashMap::new();

        if std::env::var("OPENAI_API_KEY").is_ok()
            || std::env::var("OPENAI_BASE_URL").is_ok()
        {
            let default_model = env_or("OPENAI_MODEL", "gpt-4o");
            providers.insert(
                "openai".to_string(),
                ProviderEntry {
                    r#type: "openai".to_string(),
                    base_url: std::env::var("OPENAI_BASE_URL").ok(),
                    api_key_env: "OPENAI_API_KEY".to_string(),
                    default_model: Some(default_model),
                    context_window: Some(128_000),
                    max_output_tokens: Some(4096),
                    supports_tools: Some(true),
                    supports_streaming: Some(true),
                    supports_reasoning: Some(true),
                },
            );
        }

        if std::env::var("ANTHROPIC_API_KEY").is_ok() {
            let default_model =
                env_or("ANTHROPIC_MODEL", "claude-sonnet-4-20250514");
            providers.insert(
                "anthropic".to_string(),
                ProviderEntry {
                    r#type: "anthropic".to_string(),
                    base_url: std::env::var("ANTHROPIC_BASE_URL").ok(),
                    api_key_env: "ANTHROPIC_API_KEY".to_string(),
                    default_model: Some(default_model),
                    context_window: Some(200_000),
                    max_output_tokens: Some(8192),
                    supports_tools: Some(true),
                    supports_streaming: Some(true),
                    supports_reasoning: Some(true),
                },
            );
        }

        let default_provider = if providers.contains_key("openai") {
            "openai".to_string()
        } else if providers.contains_key("anthropic") {
            "anthropic".to_string()
        } else {
            "openai".to_string()
        };

        let default_model = providers
            .get(&default_provider)
            .and_then(|p| p.default_model.clone())
            .unwrap_or_else(|| "gpt-4o".to_string());

        Self {
            default_provider,
            default_model,
            providers,
        }
    }

    pub fn resolve_api_key(
        &self,
        entry: &ProviderEntry,
    ) -> Result<String, Error> {
        let raw = std::env::var(&entry.api_key_env).map_err(|_| {
            Error::config(format!(
                "Missing API key: environment variable {} not set for provider {}",
                entry.api_key_env, entry.r#type
            ))
        })?;
        match crate::credential::normalize_api_key(&raw) {
            Ok(trimmed) => Ok(trimmed),
            Err(CredentialError::Empty) => Err(Error::config(format!(
                "Invalid API key in {}: empty after trim",
                entry.api_key_env
            ))),
            Err(CredentialError::IllegalCharacters { byte_index }) => {
                Err(Error::config(format!(
                    "Invalid API key in {}: illegal character at byte {byte_index}",
                    entry.api_key_env
                )))
            }
            Err(CredentialError::UnknownSource { source_name }) => {
                Err(Error::config(format!(
                    "Invalid API key in {}: unknown source {source_name}",
                    entry.api_key_env
                )))
            }
        }
    }

    pub fn create_provider(
        &self,
        name: &str,
    ) -> Result<Box<dyn ModelProvider>, Error> {
        let entry = self
            .providers
            .get(name)
            .ok_or_else(|| Error::not_found(name))?;

        match entry.r#type.as_str() {
            #[cfg(feature = "openai")]
            "openai" => {
                let base_url = entry
                    .base_url
                    .clone()
                    .unwrap_or_else(|| "https://api.openai.com/v1".to_string());
                let api_key = self.resolve_api_key(entry)?;
                let provider = OpenAICompatibleProvider::new(
                    base_url,
                    model_config_of(entry),
                )
                .with_api_key(&api_key);
                Ok(Box::new(provider))
            }
            #[cfg(not(feature = "openai"))]
            "openai" => Err(adapter_not_compiled("openai")),
            #[cfg(feature = "anthropic")]
            "anthropic" => {
                let api_key = self.resolve_api_key(entry)?;
                let mut provider =
                    AnthropicProvider::new(model_config_of(entry))
                        .with_api_key(&api_key);
                if let Some(base_url) = &entry.base_url {
                    provider = provider.with_base_url(base_url);
                }
                Ok(Box::new(provider))
            }
            #[cfg(not(feature = "anthropic"))]
            "anthropic" => Err(adapter_not_compiled("anthropic")),
            other => Err(Error::validation(format!(
                "Unsupported provider type: {}",
                other
            ))),
        }
    }

    pub fn available_providers(&self) -> Vec<String> {
        self.providers.keys().cloned().collect()
    }
}

#[cfg(test)]
mod tests;
