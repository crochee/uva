//! The config-driven `model-adapter` plugin: provider catalogue plus the
//! tier-1 wrapping chain (semantic cache → resilience → budget), built
//! from a profile entry's `config` instead of boot-time static
//! construction.

use std::{collections::HashMap, sync::Arc};

use serde::{Deserialize, Serialize};
use uva_provider::{
    ProviderResolver,
    config::{ProviderEntry, WorkspaceConfig},
    plugin::{KEY_LLM, KEY_LLM_DEFAULT},
    profile::{
        AnthropicProfile,
        ModelCapabilities,
        OpenAIProfile,
        ProviderProfile,
        ProviderRegistry,
    },
    traits::ModelProvider,
};

/// The model id advertised for a `[providers.<name>]` entry that
/// declares no `default_model`.
///
/// The models endpoint publishes the same placeholder, so a client
/// that selects what it was shown resolves here.
pub const UNDECLARED_MODEL: &str = "unknown";

/// The deployment's configured providers — the map a turn's `model`
/// selection resolves against.
#[derive(Clone, Default)]
pub struct ProviderCatalogue {
    workspace: WorkspaceConfig,
    profiles: ProviderRegistry,
    handles: HashMap<String, Arc<dyn ModelProvider>>,
    failures: HashMap<String, String>,
    default_name: String,
}

impl ProviderResolver for ProviderCatalogue {
    fn resolve(
        &self,
        provider: &str,
        model: &str,
    ) -> Option<Arc<dyn ModelProvider>> {
        // The catalogue's spelling for a pair: `provider/model`.
        self.resolve(Some(&format!("{provider}/{model}"))).ok()
    }
}

impl ProviderCatalogue {
    /// Build the catalogue for a deployment: one profile per
    /// `[providers.<name>]` entry, each built through
    /// [`ProviderProfile::build_provider`] (which resolves its API key
    /// from the environment).
    ///
    /// An entry the build cannot serve is recorded with its reason
    /// instead of aborting the boot: a deployment whose *secondary*
    /// provider has no key must still start, and selecting that
    /// provider is then a request-time error the caller can name.
    pub fn from_workspace_config(config: &WorkspaceConfig) -> Self {
        let mut profiles = ProviderRegistry::new();
        let mut failures: HashMap<String, String> = HashMap::new();
        for (name, entry) in &config.providers {
            match profile_of(name, entry) {
                Ok(profile) => profiles = profiles.insert(profile),
                Err(reason) => {
                    failures.insert(name.clone(), reason);
                }
            }
        }

        let mut handles: HashMap<String, Arc<dyn ModelProvider>> =
            HashMap::new();
        for name in profiles.names() {
            match profiles.build(&name) {
                Ok(provider) => {
                    handles.insert(name, Arc::from(provider));
                }
                Err(error) => {
                    failures.insert(name, error.to_string());
                }
            }
        }
        for (name, reason) in &failures {
            tracing::warn!(
                provider = %name,
                reason = %reason,
                "configured provider is not available; selecting it fails"
            );
        }

        Self {
            workspace: config.clone(),
            profiles,
            handles,
            failures,
            default_name: config.default_provider.clone(),
        }
    }

    /// The workspace config the catalogue was built from.
    ///
    /// A consumer that needs the provider map itself (the models
    /// endpoint, the health surface) reads it here instead of parsing
    /// the profile row again.
    #[must_use]
    pub fn workspace(&self) -> &WorkspaceConfig {
        &self.workspace
    }

    /// The provider a run that names no model samples with.
    ///
    /// `Err` is the same [`ModelSelectionError`] any other selection
    /// produces, so the boot's fatal path and a request-time selection
    /// report the same thing about the same name.
    pub fn default_handle(
        &self,
    ) -> Result<Arc<dyn ModelProvider>, ModelSelectionError> {
        self.handle(&self.default_name)
    }

    /// Resolve a turn's `model` selection.
    ///
    /// `None` and a blank selection are the deployment default; every
    /// other value must name a configured provider and a model that
    /// provider advertises, or the result is an error naming the value
    /// the caller sent.
    pub fn resolve(
        &self,
        selection: Option<&str>,
    ) -> Result<Arc<dyn ModelProvider>, ModelSelectionError> {
        let selection = selection.map(str::trim).filter(|s| !s.is_empty());
        let Some(selection) = selection else {
            return self.default_handle();
        };
        // Split at the *first* slash: a model id may contain slashes
        // of its own (`meta-llama/llama-3.3-70b-instruct`), a
        // provider entry name cannot.
        match selection.split_once('/') {
            Some((provider, model)) => {
                let Some(profile) = self.profiles.get(provider) else {
                    // An entry that is in the config but whose profile
                    // could not be built (an unrecognised `type`) is
                    // reported by its reason: the operator did
                    // configure it, and the models endpoint advertises
                    // it, so "no such provider" would be a lie.
                    return Err(match self.failures.get(provider) {
                        Some(reason) => ModelSelectionError::Unavailable {
                            provider: provider.to_string(),
                            reason: reason.clone(),
                        },
                        None => ModelSelectionError::UnknownProvider {
                            value: selection.to_string(),
                        },
                    });
                };
                if profile.default_model() != model {
                    return Err(ModelSelectionError::UnknownModel {
                        value: selection.to_string(),
                        provider: Some(provider.to_string()),
                    });
                }
                self.handle(provider)
            }
            None => self.resolve_bare_model(selection),
        }
    }

    /// The configured provider id behind a model selection, in the
    /// same spellings [`Self::resolve`] accepts.
    pub fn provider_id(&self, selection: Option<&str>) -> Option<String> {
        let selection = selection.map(str::trim).filter(|s| !s.is_empty());
        let Some(selection) = selection else {
            return Some(self.default_name.clone());
        };
        match selection.split_once('/') {
            Some((provider, _)) => {
                self.profiles.get(provider).map(|_| provider.to_string())
            }
            None => self.profiles.names().into_iter().find(|name| {
                self.profiles
                    .get(name)
                    .is_some_and(|profile| profile.default_model() == selection)
            }),
        }
    }

    /// The deployment's default provider id.
    pub fn default_provider_name(&self) -> &str {
        &self.default_name
    }

    /// Register an already-built handle under `name`, skipping the
    /// environment-key resolution [`Self::from_workspace_config`]
    /// does.
    ///
    /// The prebuilt-handle seam: a host (or a test fixture) whose
    /// provider is not buildable from an entry — a demo echo handle, a
    /// fake — injects it here. The profile stored carries only the
    /// name and the advertised model id, which is all resolution
    /// reads. The first registration also becomes the default.
    #[must_use]
    pub fn with_handle(
        mut self,
        name: &str,
        model: &str,
        handle: Arc<dyn ModelProvider>,
    ) -> Self {
        use uva_provider::profile::StubProfile;
        self.profiles = std::mem::take(&mut self.profiles).insert(
            ProviderProfile::Stub(StubProfile {
                name: name.to_string(),
                default_model: model.to_string(),
            }),
        );
        if self.default_name.is_empty() {
            self.default_name = name.to_string();
        }
        self.handles.insert(name.to_string(), handle);
        self
    }

    /// A bare model name: resolve it against the providers that
    /// advertise it.
    ///
    /// Anthropic-protocol clients — the Anthropic SDK, Cursor, Claude
    /// Code — send a bare model id; they have no way to name a
    /// provider, so the deployment has to disambiguate for them. When
    /// exactly one provider offers the model, that wins. When several
    /// do, the deployment's `default_provider` (if it is among them)
    /// wins, otherwise the call is reported as ambiguous so the
    /// caller can fix it.
    fn resolve_bare_model(
        &self,
        model: &str,
    ) -> Result<Arc<dyn ModelProvider>, ModelSelectionError> {
        let matches: Vec<String> = self
            .profiles
            .names()
            .into_iter()
            .filter(|name| {
                self.profiles
                    .get(name)
                    .is_some_and(|profile| profile.default_model() == model)
            })
            .collect();
        match matches.as_slice() {
            [] => Err(ModelSelectionError::UnknownModel {
                value: model.to_string(),
                provider: None,
            }),
            [only] => self.handle(only),
            multiple => {
                if multiple.contains(&self.default_name) {
                    self.handle(&self.default_name)
                } else {
                    Err(ModelSelectionError::AmbiguousModel {
                        value: model.to_string(),
                        providers: multiple.to_vec(),
                    })
                }
            }
        }
    }

    fn handle(
        &self,
        name: &str,
    ) -> Result<Arc<dyn ModelProvider>, ModelSelectionError> {
        if let Some(provider) = self.handles.get(name) {
            return Ok(Arc::clone(provider));
        }
        match self.failures.get(name) {
            Some(reason) => Err(ModelSelectionError::Unavailable {
                provider: name.to_string(),
                reason: reason.clone(),
            }),
            None => Err(ModelSelectionError::UnknownProvider {
                value: name.to_string(),
            }),
        }
    }
}

/// Project a `[providers.<name>]` entry onto the typed profile the
/// registry keys, so every provider is built by exactly one path —
/// [`ProviderProfile::build_provider`].
///
/// The capability flags mirror the on-disk adaptation the provider
/// config layer performs, so a deployment's provider behaves the same
/// whichever entry point built it. An unrecognised `type` is the same
/// error text the config path produced.
fn profile_of(
    name: &str,
    entry: &ProviderEntry,
) -> Result<ProviderProfile, String> {
    let default_model = entry
        .default_model
        .clone()
        .unwrap_or_else(|| UNDECLARED_MODEL.to_string());
    let capabilities = ModelCapabilities {
        supports_tools: entry.supports_tools.unwrap_or(true),
        supports_streaming: entry.supports_streaming.unwrap_or(true),
        supports_reasoning: entry.supports_reasoning.unwrap_or(false),
        max_output_tokens: entry.max_output_tokens.unwrap_or(4096),
        context_window: entry.context_window.unwrap_or(128_000),
    };
    match entry.r#type.as_str() {
        "openai" => Ok(ProviderProfile::OpenAI(OpenAIProfile {
            name: name.to_string(),
            base_url: entry
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.openai.com/v1".to_string()),
            api_key_env: entry.api_key_env.clone(),
            default_model,
            capabilities: Some(capabilities),
        })),
        "anthropic" => Ok(ProviderProfile::Anthropic(AnthropicProfile {
            name: name.to_string(),
            base_url: entry.base_url.clone(),
            api_key_env: entry.api_key_env.clone(),
            default_model,
            capabilities: Some(capabilities),
        })),
        other => Err(format!("Unsupported provider type: {other}")),
    }
}

/// Why a `model` selection did not resolve.
///
/// Every variant names the value the caller sent, so the message that
/// crosses the wire tells them which string to fix instead of leaving
/// them to guess which provider ran. The transport layer maps the
/// variants onto its own status codes; the enum itself carries no
/// HTTP vocabulary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ModelSelectionError {
    /// The selection named a provider this deployment does not
    /// configure.
    UnknownProvider { value: String },
    /// The provider is configured but does not offer this model. The
    /// provider is `None` for a bare model name no provider matches.
    UnknownModel {
        value: String,
        provider: Option<String>,
    },
    /// A bare model name several providers offer — the selection has
    /// to name the provider.
    AmbiguousModel {
        value: String,
        providers: Vec<String>,
    },
    /// The provider is configured but the boot could not build it
    /// (missing API key, adapter not compiled in, unknown `type`).
    Unavailable { provider: String, reason: String },
}

impl std::fmt::Display for ModelSelectionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownProvider { value } => write!(
                f,
                "unknown provider in `model` value `{value}`: no \
                 configured provider by that name"
            ),
            Self::UnknownModel {
                value,
                provider: Some(provider),
            } => {
                write!(f, "unknown model `{value}` for provider `{provider}`")
            }
            Self::UnknownModel {
                value,
                provider: None,
            } => write!(
                f,
                "unknown model `{value}`: no configured provider \
                 offers it"
            ),
            Self::AmbiguousModel { value, providers } => write!(
                f,
                "ambiguous model `{value}`: offered by providers {}",
                providers.join(", ")
            ),
            Self::Unavailable { provider, reason } => {
                write!(f, "provider `{provider}` is not available: {reason}")
            }
        }
    }
}

impl std::error::Error for ModelSelectionError {}

/// The `model-adapter` plugin's configuration: the provider catalogue
/// plus the tier-1 wrapping toggles, all read from a profile row's
/// `config`.
///
/// The catalogue is flattened: `providers` / `default_provider` /
/// `default_model` sit at the **top level** of the row's config, next
/// to `cache` / `resilience` / `budget` — the document reads one
/// object, not a nested `workspace` table (dsh entry config shape).
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct ModelAdapterConfig {
    /// The provider map, in the same shape
    /// `<workspace>/.agents/config.toml` uses (`default_provider` /
    /// `default_model` / `providers`).
    #[serde(flatten)]
    pub catalogue: WorkspaceConfig,
    /// Semantic-cache wrap. Absent = disabled.
    #[serde(default)]
    pub cache: Option<CacheSection>,
    /// Resilience wrap (breaker + adaptive backoff). Absent = disabled.
    #[serde(default)]
    pub resilience: Option<ResilienceSection>,
    /// Budget enforcement. Absent = disabled.
    #[serde(default)]
    pub budget: Option<BudgetSection>,
}

/// Semantic-cache wrap config.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CacheSection {
    #[serde(default)]
    pub enabled: bool,
    /// Cosine similarity cutoff (0.0–1.0). Default 0.92.
    #[serde(default = "default_semantic_threshold")]
    pub threshold: f32,
    /// TTL in seconds. Default 3600.
    #[serde(default = "default_semantic_ttl")]
    pub ttl_secs: u64,
}

impl Default for CacheSection {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: default_semantic_threshold(),
            ttl_secs: default_semantic_ttl(),
        }
    }
}

/// Resilience wrap config (breaker + adaptive backoff).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResilienceSection {
    #[serde(default)]
    pub enabled: bool,
    /// Maximum attempts (1 = no retries). Default 3.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,
    /// Consecutive failures before the breaker opens. Default 5.
    #[serde(default = "default_failure_threshold")]
    pub failure_threshold: u32,
    /// Wall-clock seconds the breaker stays open before half-open
    /// probe. Default 15.
    #[serde(default = "default_half_open_after")]
    pub half_open_after_secs: u64,
}

impl Default for ResilienceSection {
    fn default() -> Self {
        Self {
            enabled: false,
            max_attempts: default_max_attempts(),
            failure_threshold: default_failure_threshold(),
            half_open_after_secs: default_half_open_after(),
        }
    }
}

/// Budget enforcement config.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BudgetSection {
    #[serde(default)]
    pub enabled: bool,
    /// Token limit. Default 200,000.
    #[serde(default = "default_tokens_limit")]
    pub tokens_limit: u64,
    /// Latency cap in seconds. Default 120.
    #[serde(default = "default_latency_secs")]
    pub latency_secs: u64,
    /// USD cost limit in cents. Default 5000 (= $50).
    #[serde(default = "default_cost_cents")]
    pub cost_cents_limit: u64,
}

impl Default for BudgetSection {
    fn default() -> Self {
        Self {
            enabled: false,
            tokens_limit: default_tokens_limit(),
            latency_secs: default_latency_secs(),
            cost_cents_limit: default_cost_cents(),
        }
    }
}

fn default_semantic_threshold() -> f32 {
    0.92
}
fn default_semantic_ttl() -> u64 {
    3600
}
fn default_max_attempts() -> u32 {
    3
}
fn default_failure_threshold() -> u32 {
    5
}
fn default_half_open_after() -> u64 {
    15
}
fn default_tokens_limit() -> u64 {
    200_000
}
fn default_latency_secs() -> u64 {
    120
}
fn default_cost_cents() -> u64 {
    5000
}

/// Build the catalogue a `model-adapter` profile entry configures.
#[must_use]
pub fn catalogue_from_config(config: &ModelAdapterConfig) -> ProviderCatalogue {
    ProviderCatalogue::from_workspace_config(&config.catalogue)
}

/// The manifest every [`ModelAdapterServicePlugin`] carries: the
/// native `model-adapter` plugin providing `llm` and `llm.default`.
///
/// A factory reads this without building the plugin, so the profile
/// loader and the mounted manifest cannot disagree about the service
/// keys.
#[must_use]
pub fn model_adapter_manifest() -> uva_kernel::PluginManifest {
    let mut manifest = uva_kernel::PluginManifest::new(
        "model-adapter",
        uva_kernel::PLUGIN_PROTOCOL_VERSION,
    );
    manifest.runtime = uva_kernel::RuntimeKind::Native;
    manifest.provides.push(KEY_LLM.to_string());
    manifest.provides.push(KEY_LLM_DEFAULT.to_string());
    manifest
}

/// The config-driven `model-adapter` service plugin.
///
/// Publishes two services:
///
/// - `llm` ([`KEY_LLM`]) — the [`ProviderCatalogue`] itself; a
///   per-turn `model` selection resolves against it.
/// - `llm.default` ([`KEY_LLM_DEFAULT`]) — the default provider
///   handle wrapped in the config's tier-1 chain
///   ([`wrap_tier1_providers`](crate::tier1::wrap_tier1_providers));
///   a run that names no model samples with this handle.
///
/// Unmounting the plugin withdraws both; the kernel parks consumers
/// that injected either key.
pub struct ModelAdapterServicePlugin {
    manifest: uva_kernel::PluginManifest,
    catalogue: Arc<ProviderCatalogue>,
    default_provider: Arc<dyn ModelProvider>,
}

impl ModelAdapterServicePlugin {
    /// Build the plugin over an already-built `catalogue`.
    ///
    /// The default handle must build — the same boot-fatal failure
    /// mode a static construction has, reported as
    /// [`ModelSelectionError`]. `config`'s tier-1 sections wrap the
    /// handle; all-off sections leave it untouched.
    ///
    /// # Errors
    ///
    /// [`ModelSelectionError`] when the catalogue's default provider
    /// cannot be built.
    pub fn new(
        catalogue: Arc<ProviderCatalogue>,
        config: &ModelAdapterConfig,
        clock: uva_core::SharedClock,
        sleeper: Arc<dyn uva_core::sleeper::Sleeper>,
    ) -> Result<Self, ModelSelectionError> {
        let default = catalogue.default_handle()?;
        let default_provider =
            crate::tier1::wrap_tier1_providers(default, config, clock, sleeper);
        Ok(Self {
            manifest: model_adapter_manifest(),
            catalogue,
            default_provider,
        })
    }
}

/// Build the `model-adapter` service plugin from `config`.
///
/// The catalogue is built from `config.workspace`; the tier-1 chain
/// from the cache / resilience / budget sections. The default
/// provider must build or the result is an error the boot reports.
///
/// # Errors
///
/// [`ModelSelectionError`] when the configured default provider
/// cannot be built.
pub fn model_adapter_plugin(
    config: &ModelAdapterConfig,
    clock: uva_core::SharedClock,
    sleeper: Arc<dyn uva_core::sleeper::Sleeper>,
) -> Result<ModelAdapterServicePlugin, ModelSelectionError> {
    ModelAdapterServicePlugin::new(
        Arc::new(catalogue_from_config(config)),
        config,
        clock,
        sleeper,
    )
}

impl uva_kernel::Plugin for ModelAdapterServicePlugin {
    fn manifest(&self) -> &uva_kernel::PluginManifest {
        &self.manifest
    }

    fn apply(
        &self,
        ctx: &uva_kernel::Context,
    ) -> Result<Vec<uva_kernel::EffectHandle>, uva_kernel::PluginError> {
        let llm = ctx.provide_arc(KEY_LLM, Arc::clone(&self.catalogue))?;
        let default =
            ctx.provide(KEY_LLM_DEFAULT, Arc::clone(&self.default_provider))?;
        Ok(vec![llm, default])
    }
}

#[cfg(test)]
mod tests;
