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
/// `ELYSPER_HOST=0.0.0.0` and `ELYSPER_PORT=8080`—so a containerized
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
    ///    `--host` / `--port` with `ELYSPER_HOST` / `ELYSPER_PORT`;
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
    #[serde(default)]
    pub auth: AuthConfig,
    /// Name of the default agent. When `None` the server falls
    /// back to the first agent registered in
    /// [`crate::state::AppState::agent_registry`].
    #[serde(default)]
    pub default_agent: Option<String>,
    #[serde(default)]
    pub cors: CorsConfig,
    /// R21: MCP servers to spawn at boot. Each server's tools
    /// join the local tool registry, so remote tools flow
    /// through the same guard pipeline, dispatch, and metrics
    /// as builtins. A server that fails to start is logged and
    /// skipped—it never blocks boot.
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
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
    /// Task 15: the deployment's plugin composition. Plugins are
    /// optional capability: an entry that fails to install is
    /// reported and skipped, never a boot failure. See
    /// [`PluginsConfig`].
    #[serde(default)]
    pub plugins: PluginsConfig,
    /// : optional semantic-cache wrap on the default
    /// provider. See [`SemanticCacheConfig`].
    #[serde(default)]
    pub cache: SemanticCacheConfig,
    /// : optional resilience wrap (breaker + adaptive
    /// backoff). See [`ResilienceConfig`].
    #[serde(default)]
    pub resilience: ResilienceConfig,
    /// : optional budget enforcement (token / cost /
    /// latency). See [`BudgetConfig`].
    #[serde(default)]
    pub budget: BudgetConfig,
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
    /// `"local"` (default) or `"redis"`.
    #[serde(default)]
    pub mode: ClusterMode,
    /// Redis URL for `mode = "redis"` (e.g. `redis://127.0.0.1:6379`).
    #[serde(default)]
    pub redis_url: Option<String>,
    /// Key/channel prefix. Default `"elysper"`.
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
            redis_url: None,
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
    /// Redis pub/sub + `SET NX EX` leases.
    Redis,
}

fn default_cluster_namespace() -> String {
    "elysper".to_string()
}

fn default_cluster_lease_ttl() -> u64 {
    15
}

/// : semantic-cache wrap config.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SemanticCacheConfig {
    #[serde(default)]
    pub enabled: bool,
    /// Cosine similarity cutoff (0.0–1.0). Default 0.92.
    #[serde(default = "default_semantic_threshold")]
    pub threshold: f32,
    /// TTL in seconds. Default 3600.
    #[serde(default = "default_semantic_ttl")]
    pub ttl_secs: u64,
}

impl Default for SemanticCacheConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold: default_semantic_threshold(),
            ttl_secs: default_semantic_ttl(),
        }
    }
}

fn default_semantic_threshold() -> f32 {
    0.92
}
fn default_semantic_ttl() -> u64 {
    3600
}

/// : resilience wrap config (breaker + adaptive backoff).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ResilienceConfig {
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

impl Default for ResilienceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_attempts: default_max_attempts(),
            failure_threshold: default_failure_threshold(),
            half_open_after_secs: default_half_open_after(),
        }
    }
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

/// : budget enforcement config.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BudgetConfig {
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

impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            tokens_limit: default_tokens_limit(),
            latency_secs: default_latency_secs(),
            cost_cents_limit: default_cost_cents(),
        }
    }
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
    /// call ([`elysper::tool::ToolExposure::Deferred`]).
    #[serde(default)]
    pub deferred: Vec<String>,
    /// Tools hidden from every listing *and* refused on dispatch
    /// ([`elysper::tool::ToolRegistry::set_hidden`]).
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

/// The deployment's plugins (Task 15).
///
/// This is the runtime half of "dependencies are compile-time,
/// composition is runtime": which plugins a deployment runs is read
/// from this section, not selected by a cargo feature.
///
/// The section **is** the list — `plugins:` holds the entries
/// directly, matching what `DEPLOYMENT.md` shows:
///
/// ```yaml
/// plugins:
///   - name: redactor
///     kind: remote
///     command: ["/usr/local/bin/redactor"]
/// ```
///
/// (`#[serde(transparent)]`: the struct exists so the section has a
/// named home, not to add a nesting level. The doubled
/// `plugins: { plugins: [...] }` shape is not accepted.)
///
/// Plugins are optional capability. An entry that cannot be
/// installed — a process that will not start, a handshake that
/// fails, a promotion the plugin cannot serve, a static name the
/// binary does not link — is reported and skipped; it never fails
/// the boot. The resolved and skipped sets are exposed through
/// [`crate::composition::Composition`].
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(transparent)]
pub struct PluginsConfig {
    pub plugins: Vec<PluginEntry>,
}

/// One plugin entry.
///
/// `kind` selects the channel: `static` names a factory compiled
/// into this binary, `remote` describes a process the host spawns
/// and speaks the plugin protocol to.
///
/// The free-form string fields are validated while parsing, so an
/// operator typo is a clear error naming the field and the accepted
/// values instead of a silent fallback to the default.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PluginEntry {
    /// A plugin compiled into this binary. The name is looked up in
    /// the server's [`StaticPlugins`](crate::composition::StaticPlugins)
    /// table.
    Static {
        /// The factory name in the binary's static plugin table.
        name: String,
    },
    /// A plugin process reached over the plugin protocol.
    Remote {
        /// The plugin's name: the prefix of its diagnostics and the
        /// owner of the tool names it claims.
        name: String,
        /// The argv the host spawns. An argv vector, not a shell
        /// string.
        command: Vec<String>,
        /// `"stdio"` (default) or `"http"`.
        #[serde(
            default = "default_plugin_transport",
            deserialize_with = "deserialize_plugin_transport"
        )]
        transport: String,
        /// The roles this deployment enables: `tools`, `observe`,
        /// `transform`, `gate`.
        #[serde(default, deserialize_with = "deserialize_plugin_capabilities")]
        capabilities: Vec<String>,
        /// `"reduced"` (default) or `"full"`.
        #[serde(
            default = "default_plugin_authority",
            deserialize_with = "deserialize_plugin_authority"
        )]
        authority: String,
        /// `"low"`, `"high"` or `"unknown"` (default).
        #[serde(
            default = "default_plugin_risk",
            deserialize_with = "deserialize_plugin_risk"
        )]
        risk: String,
        /// How long one call to this plugin may take, in
        /// milliseconds.
        ///
        /// The bound is **per plugin**: a deployment that knows one
        /// plugin answers faster (or slower) names the bound for
        /// that plugin, and one slow plugin cannot stretch the
        /// deadline of another. Omitted means the host's own default.
        #[serde(default)]
        call_timeout_ms: Option<u64>,
    },
}

const PLUGIN_TRANSPORTS: &[&str] = &["stdio", "http"];
const PLUGIN_CAPABILITIES: &[&str] = &["tools", "observe", "transform", "gate"];
const PLUGIN_AUTHORITIES: &[&str] = &["reduced", "full"];
const PLUGIN_RISKS: &[&str] = &["low", "high", "unknown"];

fn default_plugin_transport() -> String {
    "stdio".to_string()
}

fn default_plugin_authority() -> String {
    "reduced".to_string()
}

fn default_plugin_risk() -> String {
    "unknown".to_string()
}

/// The parse error for a value outside a field's accepted set. It
/// names the field and every value it accepts, so an operator can
/// fix the file without reading the code.
fn rejected_plugin_value(
    field: &str,
    value: &str,
    accepted: &[&str],
) -> String {
    let accepted: Vec<String> = accepted
        .iter()
        .map(|value| format!("\"{value}\""))
        .collect();
    format!(
        "invalid value `{value}` for field `{field}`: accepted values are {}",
        accepted.join(", ")
    )
}

fn check_plugin_value(
    field: &str,
    value: &str,
    accepted: &[&str],
) -> Result<(), String> {
    if accepted.contains(&value) {
        Ok(())
    } else {
        Err(rejected_plugin_value(field, value, accepted))
    }
}

fn deserialize_plugin_transport<'de, D>(
    deserializer: D,
) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    check_plugin_value("transport", &value, PLUGIN_TRANSPORTS)
        .map_err(serde::de::Error::custom)?;
    Ok(value)
}

fn deserialize_plugin_authority<'de, D>(
    deserializer: D,
) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    check_plugin_value("authority", &value, PLUGIN_AUTHORITIES)
        .map_err(serde::de::Error::custom)?;
    Ok(value)
}

fn deserialize_plugin_risk<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    check_plugin_value("risk", &value, PLUGIN_RISKS)
        .map_err(serde::de::Error::custom)?;
    Ok(value)
}

fn deserialize_plugin_capabilities<'de, D>(
    deserializer: D,
) -> Result<Vec<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let values = Vec::<String>::deserialize(deserializer)?;
    for value in &values {
        check_plugin_value("capabilities", value, PLUGIN_CAPABILITIES)
            .map_err(serde::de::Error::custom)?;
    }
    Ok(values)
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
/// One MCP server to launch over stdio (R21).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct McpServerConfig {
    /// Display name (used in logs and the tool's presentation
    /// hint).
    pub name: String,
    /// Executable to spawn.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Extra environment variables for the child process.
    #[serde(default)]
    pub env: std::collections::HashMap<String, String>,
    /// Set `false` to keep the entry in config without spawning.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl McpServerConfig {
    /// Convert to the transport spawn config.
    #[must_use]
    pub fn to_stdio_config(&self) -> elysper::mcp::StdioConfig {
        let mut config = elysper::mcp::StdioConfig::new(&self.command)
            .args(self.args.clone())
            .label(self.name.clone());
        for (key, value) in &self.env {
            config = config.env(key.clone(), value.clone());
        }
        config
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, Default)]
pub struct AuthConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub api_keys: Vec<String>,
    /// Optional per-API-key user_id mapping.
    ///
    /// Resolution order (see also
    /// `elysper_server::middleware::auth::resolve_user_id_from_key`):
    /// 1. If the request's API key is in `key_to_user`, use that
    ///    `user_id` verbatim.
    /// 2. Otherwise, if the key is in `api_keys` but unmapped, derive
    ///    `user_id = hex(sha256(key))[..16]` (deterministic, key-bound).
    /// 3. Otherwise, reject the request.
    ///
    /// An explicit map wins over derivation so that operators can pin
    /// stable namespaces regardless of key rotation.
    #[serde(default)]
    pub key_to_user: std::collections::HashMap<String, String>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct CorsConfig {
    #[serde(default = "default_allowed_origins")]
    pub allowed_origins: Vec<String>,
    #[serde(default = "default_allowed_methods")]
    pub allowed_methods: Vec<String>,
    #[serde(default = "default_allowed_headers")]
    pub allowed_headers: Vec<String>,
}

fn default_allowed_origins() -> Vec<String> {
    // Empty list → CORS layer falls back to `Any` (permissive by default).
    // Operators can override via `cors.allowed_origins` in config to lock
    // down to a specific set of origins.
    Vec::new()
}

fn default_allowed_methods() -> Vec<String> {
    // Empty list → CORS layer falls back to `Any` (permissive by default).
    Vec::new()
}

fn default_allowed_headers() -> Vec<String> {
    // Empty list → CORS layer falls back to `Any` (permissive by default).
    Vec::new()
}

impl Default for CorsConfig {
    fn default() -> Self {
        Self {
            allowed_origins: default_allowed_origins(),
            allowed_methods: default_allowed_methods(),
            allowed_headers: default_allowed_headers(),
        }
    }
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
            auth: AuthConfig::default(),
            cors: CorsConfig::default(),
            default_agent: None,
            mcp_servers: Vec::new(),
            operations: OperationEndpointConfig::default(),
            tools: ToolsConfig::default(),
            plugins: PluginsConfig::default(),
            cache: SemanticCacheConfig::default(),
            resilience: ResilienceConfig::default(),
            budget: BudgetConfig::default(),
            cluster: ClusterConfig::default(),
            scheduler: SchedulerConfig::default(),
        }
    }
}

#[cfg(test)]
mod tests;
