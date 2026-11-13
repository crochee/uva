//! Server configuration types

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::{AgentConfig, ProviderConfig};

pub const DEFAULT_HOST: &str = "127.0.0.1";
pub const DEFAULT_PORT: u16 = 8080;
pub const DEFAULT_VERSION: &str = "1.0";
pub const DEFAULT_MAX_AGENTS: usize = 5;

/// Where the server binds, after CLI / env / config / defaults.
///
/// R54: this existed only as two `clap` defaults (`127.0.0.1:8080`),
/// while the Dockerfile and both compose files set
/// `UVA_HOST=0.0.0.0` and `UVA_PORT=8080`—so a containerized
/// server bound loopback and its published port was unreachable, and
/// `host` / `port` in the deployment config were read by nobody.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindAddress {
    pub host: String,
    pub port: u16,
}

impl BindAddress {
    /// Resolve the address, most specific source first:
    ///
    /// 1. `cli`—the value `clap` produced, which already merged
    ///    `--host` / `--port` with `UVA_HOST` / `UVA_PORT`;
    /// 2. the deployment config's `host` / `port`;
    /// 3. [`DEFAULT_HOST`] / [`DEFAULT_PORT`].
    ///
    /// Each part resolves independently, so `--port 9000` with
    /// `host = "0.0.0.0"` in the config means exactly that.
    #[must_use]
    pub fn resolve(cli: BindOverrides, config: Option<&ServerConfig>) -> Self {
        let host = cli
            .host
            .or_else(|| config.map(|c| c.host.clone()))
            .unwrap_or_else(|| DEFAULT_HOST.to_string());
        let port = cli
            .port
            .or_else(|| config.map(|c| c.port))
            .unwrap_or(DEFAULT_PORT);
        Self { host, port }
    }
}

/// The command line's contribution to [`BindAddress::resolve`].
///
/// `Option` rather than a default-filled value on purpose: a default
/// would be indistinguishable from an explicit `--host 127.0.0.1`, and
/// the config file could then never win.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BindOverrides {
    pub host: Option<String>,
    pub port: Option<u16>,
}

impl std::fmt::Display for BindAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.host, self.port)
    }
}

fn default_version() -> String {
    DEFAULT_VERSION.to_string()
}

fn default_host() -> String {
    DEFAULT_HOST.to_string()
}

fn default_port() -> u16 {
    DEFAULT_PORT
}

fn default_max_agents() -> usize {
    DEFAULT_MAX_AGENTS
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ServerConfig {
    #[serde(default = "default_version")]
    pub version: String,
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    /// Cap on registered agents. `POST /api/v1/agents` refuses a
    /// registration once the registry holds this many, so a public
    /// deployment cannot grow its agent set without bound. The default
    /// member counts, so `max_agents = 5` leaves room for four
    /// registered ones.
    #[serde(default = "default_max_agents")]
    pub max_agents: usize,
    #[serde(default)]
    pub providers: std::collections::HashMap<String, ProviderConfig>,
    #[serde(default)]
    pub agents: std::collections::HashMap<String, AgentConfig>,
    /// Name of the default agent. When `None` the server falls
    /// back to the first agent registered in
    /// [`crate::state::AppState::agent_registry`].
    #[serde(default)]
    pub default_agent: Option<String>,
    /// R29: the optional `POST.../operation` discriminated-union
    /// endpoint. Off by default—the route is not registered at
    /// all when disabled, so the path falls through to the
    /// standard 404 envelope.
    #[serde(default)]
    pub operations: OperationEndpointConfig,
    /// R34: the deployment's model-facing tool surface. Applied to
    /// the live tool registry at boot; a name the registry does not
    /// have is logged and skipped, never a startup failure. See
    /// [`ToolsConfig`].
    #[serde(default)]
    pub tools: ToolsConfig,
    /// `[cluster]`—the multi-node control-plane bus. Default
    /// `mode = "local"` (single process, zero external deps).
    #[serde(default)]
    pub cluster: ClusterConfig,
    /// `[scheduler]` — the background tick that fires due jobs.
    #[serde(default)]
    pub scheduler: SchedulerConfig,
}

/// `[scheduler]` — the deployment's background tick.
///
/// One node holds the scheduler lease and ticks; every other node
/// waits. The tick interval bounds how late a due job can fire.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SchedulerConfig {
    /// Seconds between two scheduler passes. Default 30.
    #[serde(default = "default_scheduler_tick_secs")]
    pub tick_secs: u64,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            tick_secs: default_scheduler_tick_secs(),
        }
    }
}

fn default_scheduler_tick_secs() -> u64 {
    30
}

/// `[cluster]`—multi-node control-plane configuration.
///
/// Multi-node deployment = stateless compute nodes + a shared
/// workspace filesystem (sessions / workflows / evals / schedules are
/// the durable truth) + a shared bus (Redis) for leases, session-frame
/// fan-out, and control-plane mutations. `mode = "local"` is the
/// single-process default and needs nothing.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ClusterConfig {
    /// `"local"` — the one bus this build carries. A config naming
    /// the retired `"redis"` fails to parse, loudly, rather than
    /// silently falling back to single-node.
    #[serde(default)]
    pub mode: ClusterMode,
    /// Key/channel prefix. Default `"uva"`.
    #[serde(default = "default_cluster_namespace")]
    pub namespace: String,
    /// Session-lease TTL in seconds. Default 15; a crashed owner's
    /// sessions become takeover-able after this long.
    #[serde(default = "default_cluster_lease_ttl")]
    pub lease_ttl_secs: u64,
    /// Stable node id. `None` mints a ULID at boot.
    #[serde(default)]
    pub node_id: Option<String>,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            mode: ClusterMode::default(),
            namespace: default_cluster_namespace(),
            lease_ttl_secs: default_cluster_lease_ttl(),
            node_id: None,
        }
    }
}

/// Which bus backs the cluster control plane.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize,
)]
#[serde(rename_all = "lowercase")]
pub enum ClusterMode {
    /// In-process bus: single-node behaviour, zero external deps.
    #[default]
    Local,
}

fn default_cluster_namespace() -> String {
    "uva".to_string()
}

fn default_cluster_lease_ttl() -> u64 {
    15
}

/// The `[tools]` section (R34): which registered tools the model is
/// told about, and in what form.
///
/// The section is applied to the live registry at boot—after the
/// builtins, the `skill` tool, and any configured MCP server's tools
/// have registered—so it can name remote tools too. Every name is
/// resolved against that registry; an unknown name is **skipped with
/// a warning** rather than failing startup, because an MCP server
/// that fails to connect must not take the server down. The effective
/// surface is logged at boot and exposed as
/// [`crate::state::AppState::tool_surface`].
///
/// Write order is `groups` → `deferred` → `hidden`, so when one tool
/// appears in several places the later entry wins. The knobs:
///
/// | key | effect |
/// |---|---|
/// | `groups` + `active_groups` | active-group member → advertised; inactive-group member → not advertised but still callable |
/// | `deferred` | advertised by name and description only until its first call in the transcript |
/// | `hidden` | not advertised **and** refused by the registry's dispatch (the privacy flag) |
/// | `max_visible` | caps the agent's model-facing list in registry order; not a registry write, so the HTTP catalog endpoint shows the uncapped list |
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct ToolsConfig {
    /// Tools advertised by name only until the transcript shows a
    /// call ([`uva::tool::ToolExposure::Deferred`]).
    #[serde(default)]
    pub deferred: Vec<String>,
    /// Tools hidden from every listing *and* refused on dispatch
    /// ([`uva::tool::ToolRegistry::set_hidden`]).
    #[serde(default)]
    pub hidden: Vec<String>,
    /// Named groups: group name → member tool names. A member of an
    /// inactive group keeps its registration but is not advertised.
    #[serde(default)]
    pub groups: std::collections::BTreeMap<String, Vec<String>>,
    /// Groups whose members are advertised.
    #[serde(default)]
    pub active_groups: Vec<String>,
    /// Cap on how many tools one agent request advertises, applied in
    /// registry (name) order. `None` advertises the whole visible
    /// catalog.
    #[serde(default)]
    pub max_visible: Option<usize>,
}

/// The `POST.../operation` endpoint gate (R29).
///
/// One flag, one endpoint. Kept as a struct (rather than a bare
/// `bool` on `ServerConfig`) so future operation-level knobs
/// land here without another top-level field—the same shape
/// `cors` already uses.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct OperationEndpointConfig {
    /// Register `POST /api/v1/chat/sessions/{id}/operation`.
    /// Default `false`.
    #[serde(default)]
    pub enabled: bool,
}
impl ServerConfig {
    pub fn load(path: &PathBuf) -> Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }

        let content = std::fs::read_to_string(path)?;
        if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
            serde_yaml::from_str(&content).map_err(Into::into)
        } else {
            serde_json::from_str(&content).map_err(Into::into)
        }
    }
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            version: default_version(),
            host: default_host(),
            port: default_port(),
            max_agents: default_max_agents(),
            providers: std::collections::HashMap::new(),
            agents: std::collections::HashMap::new(),
            default_agent: None,
            operations: OperationEndpointConfig::default(),
            tools: ToolsConfig::default(),
            cluster: ClusterConfig::default(),
            scheduler: SchedulerConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests;
