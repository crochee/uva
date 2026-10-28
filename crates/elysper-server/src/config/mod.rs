//! Server configuration module
//!
//! Provides configuration types for the Elysper server.

mod agent;
pub mod provider;
pub mod server;
pub mod yaml_bridge;

pub use agent::AgentConfig;
pub use provider::{ModelConfig, ProviderConfig};
pub use server::{
    AuthConfig,
    CorsConfig,
    DEFAULT_HOST,
    DEFAULT_MAX_AGENTS,
    DEFAULT_PORT,
    DEFAULT_VERSION,
    McpServerConfig,
    OperationEndpointConfig,
    PluginEntry,
    PluginsConfig,
    SchedulerConfig,
    ServerConfig,
    ToolsConfig,
};

/// Environment variable holding the deployment configuration as a
/// string. Wins over every file candidate so Kubernetes ConfigMaps
/// and CI runners can ship config without mounting a file (OpenCode
/// `OPENCODE_CONFIG_CONTENT` parity).
pub const CONFIG_CONTENT_ENV: &str = "ELYSPER_CONFIG_CONTENT";

/// The deployment configuration, resolved **once** per boot.
///
/// Resolution order:
/// 1. `ELYSPER_CONFIG_CONTENT` env-content (R62);
/// 2. the `--config <path>` the operator named, when it exists (R53);
/// 3. `{workspace_root}/config.toml`;
/// 4. `{workspace_root}/.elysper/config.toml`.
///
/// R53: the named file used to configure only the provider bridge, while
/// the agent / tool / auth / CORS / operations / MCP sections
/// were read from `config.toml` regardless — so `make dev-server`, which
/// passes `--config config.yaml`, silently dropped half of the file it
/// was told to use. One resolution point fixes that and removes the
/// repeated parse (every section used to re-read and re-parse the file).
///
/// R62: the env-content candidate is detected by [`ServerConfig::load`]
/// (extension pick) by writing the content to a tempfile in the system
/// temp dir; the tempfile's lifetime is the function call. A malformed
/// env overlay is logged and the next layer wins, matching the
/// existing fail-soft contract for file candidates.
///
/// [`ServerConfig::load`] reads a `.yaml` file as YAML and every
/// other extension as JSON, so a deployment document should be named
/// `.yaml`; a `.toml` candidate is attempted as JSON and skipped. A
/// candidate that exists but does not parse is logged and skipped —
/// the next candidate is tried, and when none is left the callers
/// get `None` and apply their own defaults, so a config typo never
/// blocks boot (the same fail-soft contract `[tools]`,
/// `[mcp_servers]` and the provider bridge follow).
pub fn resolve_server_config(
    workspace_root: &std::path::Path,
    config_path: Option<&std::path::PathBuf>,
) -> Option<crate::config::ServerConfig> {
    use crate::config::ServerConfig;

    let mut candidates: Vec<std::path::PathBuf> = Vec::new();
    // Keeps the env-content tempfile alive until the function
    // returns so `ServerConfig::load` can read it. Dropping the
    // helper deletes the file.
    let mut _env_keeper: Option<tempfile::NamedTempFile> = None;
    if let Some(content) = std::env::var_os(CONFIG_CONTENT_ENV)
        && let Some(content) = content.to_str()
        && !content.trim().is_empty()
        && let Some((path, keeper)) = write_env_content_tempfile(content)
    {
        _env_keeper = Some(keeper);
        candidates.push(path);
    }
    if let Some(path) = config_path {
        candidates.push(path.clone());
    }
    candidates.push(workspace_root.join("config.toml"));
    candidates.push(workspace_root.join(".elysper").join("config.toml"));

    for candidate in candidates {
        if !candidate.exists() {
            continue;
        }
        match ServerConfig::load(&candidate) {
            Ok(cfg) => {
                tracing::info!(
                    path = %candidate.display(),
                    "loaded server config"
                );
                return Some(cfg);
            }
            Err(e) => {
                tracing::warn!(
                    path = %candidate.display(),
                    error = %e,
                    "failed to load server config; trying the next candidate"
                );
            }
        }
    }
    None
}

/// Write the `ELYSPER_CONFIG_CONTENT` value to a tempfile and return
/// its path. The file extension is `.yaml` so the YAML loader picks
/// it up; callers fall through to the next candidate on parse
/// failure, so a malformed env overlay is non-fatal.
///
/// The path's [`TempPath`] handle is leaked (via [`std::mem::forget`]
/// is **not** used — the helper retains ownership in the surrounding
/// scope through [`tempfile::NamedTempPath`]) so the file persists
/// until [`ServerConfig::load`] finishes reading it. The simpler
/// approach is to write the file at a known, well-known path under
/// the system temp dir and keep it for the lifetime of the process;
/// subsequent calls overwrite it.
fn write_env_content_tempfile(
    content: &str,
) -> Option<(std::path::PathBuf, tempfile::NamedTempFile)> {
    use std::io::Write;
    let Ok(mut tmp) = tempfile::Builder::new()
        .prefix("elysper-config-")
        .suffix(".yaml")
        .tempfile()
    else {
        tracing::warn!(
            env = CONFIG_CONTENT_ENV,
            "could not create tempfile for env-content config; skipping"
        );
        return None;
    };
    if let Err(e) = tmp.write_all(content.as_bytes()) {
        tracing::warn!(
            env = CONFIG_CONTENT_ENV,
            error = %e,
            "could not write env-content config to tempfile; skipping"
        );
        return None;
    }
    let path = tmp.path().to_path_buf();
    Some((path, tmp))
}

#[cfg(test)]
mod tests;
