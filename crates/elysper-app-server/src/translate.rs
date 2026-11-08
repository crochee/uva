//! The `translate` command: migrate a legacy `config.yaml` into a
//! profile TOML's `model-adapter` row.
//!
//! This is the **one** migration entry point (the design's "profile
//! 唯一真相"): the yaml parsing that used to live in the boot-time
//! bridge survives here and only here. What the translator maps:
//!
//! - `providers` (+ `default_provider` / `default_model`) — both the
//!   legacy model-oriented shape (`api_key`, `base_url`, `models[]`)
//!   and the TOML `ProviderEntry` shape — into
//!   `model-adapter.config.workspace`;
//! - `cache` / `resilience` / `budget` into the tier-1 sections of the
//!   same row.
//!
//! Everything else the yaml may carry is reported as unmapped: those
//! sections keep being read by [`ServerConfig::load`](crate::config)
//! until their plugins solidify (stage 3), and the translator names
//! them so the operator knows what did not move.
//!
//! Inline `api_key`s are not written into the profile: the translated
//! row points `api_key_env` at a deterministic variable
//! (`ELYSPER_PROVIDER_API_KEY_<NAME>`), and the report names each one
//! the operator must export.

use std::{collections::HashMap, fs, path::PathBuf};

use anyhow::{Context, Result, bail};
use elysper::{
    provider::config::{ProviderEntry, WorkspaceConfig},
    runtime::{
        BudgetSection,
        CacheSection,
        ModelAdapterConfig,
        ResilienceSection,
    },
};

/// The command's arguments (all optional; defaults below).
#[derive(clap::Args, Debug)]
pub struct TranslateArgs {
    /// The legacy config file to migrate.
    ///
    /// Defaults to `<workspace>/config.yaml`.
    #[arg(long, value_name = "PATH")]
    pub config: Option<PathBuf>,
    /// Where to write the profile TOML.
    ///
    /// Defaults to stdout.
    #[arg(long, value_name = "PATH")]
    pub out: Option<PathBuf>,
}

/// The yaml sections the translator maps, in report order.
const MAPPED: &str = "providers / default_provider / default_model / \
                      cache / resilience / budget";

/// The yaml sections that stay on `ServerConfig::load` until their
/// plugins solidify.
const UNMAPPED_SECTIONS: &[&str] = &[
    "agents",
    "default_agent",
    "operations",
    "tools",
    "plugins",
    "cluster",
    "scheduler",
    "server",
];

/// One legacy provider entry, in either accepted shape.
///
/// The legacy YAML shape (`api_key` / `base_url` / `models[]`) and the
/// TOML `ProviderEntry` shape (`type` / `api_key_env` /
/// `default_model` / capability flags) both deserialize into this
/// struct; the bridge prefers the explicit typed fields and falls back
/// to inference for the legacy ones.
#[derive(Default, serde::Deserialize)]
struct SourceProvider {
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    models: Vec<SourceModel>,
    #[serde(default, rename = "type")]
    r#type: Option<String>,
    #[serde(default)]
    api_key_env: Option<String>,
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    context_window: Option<usize>,
    #[serde(default)]
    max_output_tokens: Option<usize>,
    #[serde(default)]
    supports_tools: Option<bool>,
    #[serde(default)]
    supports_streaming: Option<bool>,
    #[serde(default)]
    supports_reasoning: Option<bool>,
}

/// One `models[]` entry of the legacy shape.
#[derive(Default, serde::Deserialize)]
struct SourceModel {
    #[serde(default)]
    name: String,
    #[serde(default)]
    context_window: Option<usize>,
    #[serde(default)]
    max_tokens: Option<usize>,
}

/// The legacy file's provider-facing sections.
#[derive(serde::Deserialize)]
struct SourceFile {
    #[serde(default)]
    default_provider: Option<String>,
    #[serde(default)]
    default_model: Option<String>,
    #[serde(default)]
    providers: HashMap<String, SourceProvider>,
}

/// Run the migration: read the legacy file, write the profile TOML,
/// print the report to stderr.
pub fn run(args: &TranslateArgs, directory: &std::path::Path) -> Result<()> {
    let path = args
        .config
        .clone()
        .unwrap_or_else(|| directory.join("config.yaml"));
    let content = fs::read_to_string(&path)
        .with_context(|| format!("read {}", path.display()))?;
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !matches!(ext.as_str(), "yaml" | "yml" | "toml") {
        bail!(
            "`{}` is not a yaml/yml/toml file; nothing to translate",
            path.display()
        );
    }

    // The whole document, for the unmapped-section report.
    let document: serde_yaml::Value = if ext == "toml" {
        let value: toml::Value =
            toml::from_str(&content).context("parse toml")?;
        serde_yaml::to_value(&value).context("convert toml document")?
    } else {
        serde_yaml::from_str(&content).context("parse yaml")?
    };

    let source: SourceFile = serde_yaml::from_value(document.clone())
        .context("read the provider sections")?;
    let mut inline_keys = Vec::new();
    let workspace = bridge(source, &mut inline_keys)?;

    let config = ModelAdapterConfig {
        workspace,
        cache: section::<CacheSection>(&document, "cache")?,
        resilience: section::<ResilienceSection>(&document, "resilience")?,
        budget: section::<BudgetSection>(&document, "budget")?,
    };
    let text = profile_toml(&config)?;
    match &args.out {
        Some(out) => {
            fs::write(out, &text)
                .with_context(|| format!("write {}", out.display()))?;
        }
        None => {
            println!("{text}"); // stdout-ok: translate's default output
        }
    }
    report(&path, &config, &document, &inline_keys);
    Ok(())
}

/// Bridge the legacy provider sections into the workspace shape.
///
/// Unlike the old boot-time bridge this **never** mutates the process
/// environment: a translated deployment exports the named variables
/// itself, and `inline_keys` reports which ones it must export.
fn bridge(
    source: SourceFile,
    inline_keys: &mut Vec<String>,
) -> Result<WorkspaceConfig> {
    if source.providers.is_empty() {
        bail!("config has no `providers` entries");
    }
    let mut providers = HashMap::with_capacity(source.providers.len());
    for (name, entry) in source.providers {
        let provider_type = entry
            .r#type
            .clone()
            .unwrap_or_else(|| infer_provider_type(&name, &entry));
        let api_key_env =
            entry.api_key_env.clone().unwrap_or_else(|| env_for(&name));
        if entry
            .api_key
            .as_deref()
            .is_some_and(|k| !k.trim().is_empty())
        {
            inline_keys.push(api_key_env.clone());
        }

        let (model_default, model_context, model_output) =
            pick_model_settings(&entry.models);
        providers.insert(
            name.clone(),
            ProviderEntry {
                r#type: provider_type,
                base_url: entry.base_url,
                api_key_env,
                default_model: entry.default_model.or(model_default),
                context_window: entry.context_window.or(model_context),
                max_output_tokens: entry.max_output_tokens.or(model_output),
                supports_tools: entry.supports_tools.or(Some(true)),
                supports_streaming: entry.supports_streaming.or(Some(true)),
                supports_reasoning: entry.supports_reasoning.or(Some(false)),
            },
        );
    }

    let default_provider = source
        .default_provider
        .clone()
        .or_else(|| {
            providers
                .contains_key("openai")
                .then(|| "openai".to_string())
        })
        .or_else(|| providers.keys().min().cloned())
        .unwrap_or_else(|| "openai".to_string());
    let default_model = source.default_model.clone().unwrap_or_else(|| {
        providers
            .get(&default_provider)
            .and_then(|p| p.default_model.clone())
            .unwrap_or_else(|| "unknown".to_string())
    });
    Ok(WorkspaceConfig {
        default_provider,
        default_model,
        providers,
    })
}

/// Choose the `type` field for a legacy entry.
fn infer_provider_type(name: &str, entry: &SourceProvider) -> String {
    if let Some(base_url) = entry.base_url.as_deref() {
        if base_url.contains("/v1") || base_url.contains("openai") {
            return "openai".to_string();
        }
        if base_url.contains("anthropic") {
            return "anthropic".to_string();
        }
    }
    let lower = name.to_ascii_lowercase();
    if lower.contains("openai") || lower == "azure" || lower == "custom" {
        "openai".to_string()
    } else if lower.contains("anthropic") || lower.contains("claude") {
        "anthropic".to_string()
    } else {
        "openai".to_string()
    }
}

/// Deterministic environment variable backing an inline `api_key`.
fn env_for(name: &str) -> String {
    let mut upper = String::with_capacity(name.len());
    for ch in name.chars() {
        if ch.is_ascii_alphanumeric() {
            upper.push(ch.to_ascii_uppercase());
        } else {
            upper.push('_');
        }
    }
    format!("ELYSPER_PROVIDER_API_KEY_{upper}")
}

/// First `models[]` entry becomes the `default_model`; its
/// `context_window` / `max_tokens` override the entry defaults.
fn pick_model_settings(
    models: &[SourceModel],
) -> (Option<String>, Option<usize>, Option<usize>) {
    let Some(first) = models.first() else {
        return (None, None, None);
    };
    (
        (!first.name.is_empty()).then(|| first.name.clone()),
        first.context_window,
        first.max_tokens,
    )
}

/// Deserialize one top-level section, when the document has it.
///
/// A present-but-malformed section is an error: silently dropping it
/// would translate a deployment the operator never wrote.
fn section<T: serde::de::DeserializeOwned>(
    document: &serde_yaml::Value,
    name: &str,
) -> Result<Option<T>> {
    let Some(value) = document.get(name) else {
        return Ok(None);
    };
    serde_yaml::from_value(value.clone())
        .map(Some)
        .with_context(|| format!("parse the `{name}` section"))
}

/// Serialize the migrated profile: one `model-adapter` row whose
/// config is the [`ModelAdapterConfig`], in the wire shape
/// [`Profile::from_toml_str`](crate::profile::Profile::from_toml_str)
/// parses.
fn profile_toml(config: &ModelAdapterConfig) -> Result<String> {
    let json = serde_json::to_value(config).context("serialize the row")?;
    let row = toml::Value::Table(toml_table(&json)?);
    let mut entries = toml::value::Table::new();
    entries.insert("id".to_string(), "model-adapter".into());
    entries.insert("runtime".to_string(), "native".into());
    entries.insert("config".to_string(), row);
    let mut core = toml::value::Table::new();
    core.insert(
        "entries".to_string(),
        toml::Value::Array(vec![toml::Value::Table(entries)]),
    );
    let mut root = toml::value::Table::new();
    root.insert("hmr".to_string(), true.into());
    root.insert("core".to_string(), toml::Value::Table(core));
    toml::to_string_pretty(&root).context("serialize the profile")
}

/// Convert a JSON value into a TOML table value, omitting null
/// members (an absent optional is what the wire structs expect).
fn toml_table(value: &serde_json::Value) -> Result<toml::value::Table> {
    let serde_json::Value::Object(map) = value else {
        bail!("the model-adapter config must serialize to an object");
    };
    let mut table = toml::value::Table::new();
    for (key, value) in map {
        if value.is_null() {
            continue;
        }
        table.insert(key.clone(), json_to_toml(value)?);
    }
    Ok(table)
}

/// Recursively convert JSON into TOML values.
fn json_to_toml(value: &serde_json::Value) -> Result<toml::Value> {
    Ok(match value {
        // `Option::None` fields serialize to `null`; TOML has no
        // such value, and the wire structs' absent optionals are
        // exactly what the deserializer expects (serde defaults).
        // Callers drop object members that convert to `None`.
        serde_json::Value::Null => toml::Value::String(String::new()),
        serde_json::Value::Bool(b) => (*b).into(),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into()
            } else {
                n.as_f64().unwrap_or_default().into()
            }
        }
        serde_json::Value::String(s) => s.clone().into(),
        serde_json::Value::Array(items) => {
            let converted =
                items.iter().map(json_to_toml).collect::<Result<Vec<_>>>()?;
            toml::Value::Array(converted)
        }
        serde_json::Value::Object(map) => {
            let mut table = toml::value::Table::new();
            for (key, value) in map {
                if value.is_null() {
                    continue;
                }
                table.insert(key.clone(), json_to_toml(value)?);
            }
            toml::Value::Table(table)
        }
    })
}

/// Print the migration report to stderr.
fn report(
    path: &std::path::Path,
    config: &ModelAdapterConfig,
    document: &serde_yaml::Value,
    inline_keys: &[String],
) {
    eprintln!("translated {} -> model-adapter row", path.display());
    eprintln!(
        "  mapped: {MAPPED} ({} providers, default `{}`)",
        config.workspace.providers.len(),
        config.workspace.default_provider,
    );
    let unmapped: Vec<&str> = UNMAPPED_SECTIONS
        .iter()
        .copied()
        .filter(|name| document.get(*name).is_some())
        .collect();
    if unmapped.is_empty() {
        eprintln!("  unmapped: none present");
    } else {
        eprintln!(
            "  unmapped: {} (still read by ServerConfig::load; their \
             plugins solidify in a later stage)",
            unmapped.join(", "),
        );
    }
    for key in inline_keys {
        eprintln!(
            "  export {key}=… : the yaml's inline api_key is not \
             written into the profile",
        );
    }
}
