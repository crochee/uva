//! The runtime plugin composition layer: the deployment's plugins
//! come from config, not from cargo features.
//!
//! ## What is composed
//!
//! Every entry in the deployment's `[plugins]` section is installed
//! into the deployment's [`Host`] at boot:
//!
//! - a `remote` entry becomes a [`RemotePlugin`]: the host spawns
//!   the process and handshakes with it;
//! - a `static` entry is looked up in [`StaticPlugins`], the
//!   binary's name → factory table, and installed in-process.
//!
//! ## Failure policy
//!
//! A plugin is optional capability, not a boot precondition. An
//! entry that cannot be installed — a process that will not start, a
//! handshake that fails, a promotion the plugin cannot serve, a
//! static name this binary does not link, a static install that
//! registers nothing, a programmatically built entry whose value the
//! config surface would have rejected — is recorded in
//! [`Composition::skipped`] with its reason, logged, and skipped;
//! boot continues. This is deliberately unlike the harness's gate
//! semantics: a gate that cannot answer fails closed, because it is
//! a security decision, while a plugin that does not start is not.
//!
//! A registration the registry refuses — a tool whose name a core
//! tool already owns, or a name another entry already claimed — is
//! folded into the same skipped set, and the plugin's *whole* install
//! is withdrawn: a plugin that half-works is a plugin an operator
//! cannot reason about, so [`Composition::resolved`] never claims a
//! success the registry refused and [`Composition::skipped`] never
//! leaves a live registration unaccounted for. The check runs for
//! statics and remotes alike, and covers both channels: a duplicate
//! name between two static entries is as much a substitution as one
//! between two remotes.
//!
//! The rule the checks enforce is one sentence: **a plugin cannot
//! shadow anything the deployment already ships.** The shipped names
//! are core-owned — the built-in bricks, the `skill` tool, the
//! MCP-published tools — so the registry refuses a plugin that claims
//! one. The live registry is consulted as well, so a name another
//! component registered is refused too, whatever its provenance.
//!
//! The bind runs **before** any install. A host already bound to a
//! different registry is refused, and installing first would leave
//! every plugin in the foreign registry while this composition
//! claimed them.
//!
//! ## Handle lifetime
//!
//! A [`PluginHandle`]'s drop withdraws every registration it owns,
//! so the handles the composition keeps *are* the plugin's lifetime:
//! the server stores it in `AppState`, and dropping it silently
//! uninstalls the plugin. `composition`'s tests pin both halves — a
//! handle that outlives boot keeps its tool registered, and dropping
//! the last handle takes the tool away.
//!
//! The default boot then hands those registrations to the kernel's
//! `process-plugins` entry ([`Composition::take_registrations`]):
//! once the entry mounts, its effect is the lifetime, so unloading
//! the row withdraws the plugin tools and drops a remote plugin's
//! last connection (which kills its process). A profile that omits
//! the row leaves the composition owning them, and the host-based
//! agent path keeps working.
//!
//! ## One host per deployment
//!
//! [`Composition::build`] takes the host it installs into rather
//! than creating a global one. The boot owns exactly one: it
//! composes into it, stores it on `AppState`, and every agent the
//! server builds — the boot agent, each per-run session agent, an
//! eval agent, a dynamically registered agent — receives a clone.
//! A `Host` clone shares its registrations, so one installed plugin
//! reaches every agent, and each agent's `with_host` binds the same
//! registry (the harness makes that bind a no-op). The composition,
//! not the host, is the plugin's lifetime: `AppState` keeps it.

use std::{collections::HashMap, sync::Arc};

use elysper::{
    harness::{
        Diagnostic,
        DiagnosticKind,
        Host,
        Plugin,
        PluginHandle,
        Registration,
    },
    plugin_host::{
        Authority,
        PluginRegistry,
        RemotePlugin,
        RemotePluginConfig,
        TransportKind,
    },
    plugin_protocol::PluginCapabilities,
    tool::{Risk, ToolRegistry},
};

pub(crate) mod kernel;

use crate::config::{PluginEntry, PluginsConfig};

/// The factory a statically linked plugin is registered under.
///
/// A static plugin is a Rust crate inside the binary, so the binary
/// must hold the name → constructor mapping: there is nothing to
/// discover at runtime.
pub type StaticPluginFactory = fn() -> Arc<dyn Plugin>;

/// The binary's static plugin table.
///
/// It starts **empty**: no in-tree crate implements
/// [`elysper::harness::Plugin`] today. A fork that links a plugin
/// crate registers its factories in the server's boot path, one line
/// per plugin:
///
/// ```ignore
/// let plugins = StaticPlugins::new()
///     .register("eraser", || Arc::new(my_eraser::Eraser::new()));
/// ```
///
/// Registering a name the config never names is harmless; naming a
/// name the table does not have is a diagnostic and a skip, never a
/// boot failure.
#[derive(Clone, Default)]
pub struct StaticPlugins {
    factories: Vec<(String, StaticPluginFactory)>,
}

impl StaticPlugins {
    /// An empty table: this binary links no static plugin.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Register `factory` under `name`. Builder style so a fork's
    /// boot lists its plugins in one expression.
    ///
    /// # Panics
    ///
    /// Panics when `name` is already registered. A duplicate in a
    /// fork's table is a programming error, not a preference: the
    /// lookup is by name and returns the first match, so the second
    /// factory would never run and the mistake would stay invisible.
    #[must_use]
    pub fn register(
        mut self,
        name: &str,
        factory: StaticPluginFactory,
    ) -> Self {
        assert!(
            !self
                .factories
                .iter()
                .any(|(registered, _)| registered == name),
            "duplicate static plugin factory `{name}`: the table is \
             looked up by name, so the second factory would never run"
        );
        self.factories.push((name.to_string(), factory));
        self
    }

    /// The factory registered under `name`.
    pub(crate) fn get(&self, name: &str) -> Option<StaticPluginFactory> {
        self.factories
            .iter()
            .find(|(registered, _)| registered == name)
            .map(|(_, factory)| *factory)
    }
}

/// How a plugin reached this binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginSource {
    /// Compiled into this binary, installed in-process.
    Static,
    /// A process reached over the plugin protocol.
    Remote,
}

/// One plugin the composition installed.
///
/// This is the operator-facing record, so it carries the facts an
/// operator needs to reason about the capability the deployment now
/// runs: which channel it came through, how much authority it holds,
/// the worst risk among its tools, and the names it claims. The
/// composition stores it; `GET /api/v1/plugins` is its reader.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedPlugin {
    /// The configured name.
    pub name: String,
    /// The channel it came through.
    pub source: PluginSource,
    /// How much authority it holds.
    ///
    /// A remote entry reports the value its config entry granted. A
    /// static plugin reports [`Authority::Full`]: it is code inside
    /// this binary, so the escalation that enum names is already
    /// inherent — pretending a reduced ceiling it cannot enforce
    /// would be a worse answer than naming the fact.
    pub authority: Authority,
    /// The risk bound this plugin was granted.
    ///
    /// A remote entry reports the value its config entry granted — the
    /// host's decision for every one of its tools.
    ///
    /// A static entry has no configured risk, so it reports the worst
    /// **annotation bound** among the tools it claimed
    /// ([`Risk::from_annotations`]), or [`Risk::Unknown`] when it
    /// claimed no tool at all (an observer- or gate-only plugin):
    /// there is nothing to judge, and "unknown" is the value that asks
    /// for attention rather than the one that says the plugin is safe.
    ///
    /// For a static plugin this is a bound, not the verdict a call
    /// will receive: `Tool::risk` takes the arguments and a tool may
    /// override it, so a tool annotated `read_only_hint: true` whose
    /// `risk` returns `High` is published here as whatever its
    /// annotations say while every call it serves is judged `High`.
    /// The authoritative per-call judgement is the one the agent makes
    /// at dispatch; this field exists so an operator can see what a
    /// plugin declared, not to predict what it will be allowed to do.
    pub risk: Risk,
    /// The tool names it claimed, in registration order.
    pub tools: Vec<String>,
}

/// One entry that could not be installed, with the reason.
///
/// The reason is the install's own error text (a spawn failure, a
/// handshake failure, a refused promotion, an empty install), the
/// table's missing entry for an unknown static name, a tool the
/// registry would not take (a core-owned name, or a name another
/// entry already claimed), or the bind's own refusal when the host
/// lives in a different registry.
///
/// A refused tool name is attributed back to the plugin that claimed
/// it, and that plugin's whole install is withdrawn with it. A
/// refusal that names the host's binding rather than one tool — or a
/// tool of the host's that no configured entry claimed — is named by
/// the diagnostic's source instead of being dropped, so `skipped()`
/// is a complete record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedPlugin {
    /// The configured name, or the registration a diagnostic named.
    pub name: String,
    /// Why the entry was skipped.
    pub reason: String,
}

/// The deployment's installed plugins and the handles that own their
/// registrations.
///
/// The composition is the lifetime of every plugin it installed until
/// [`Composition::take_registrations`] hands them to the kernel's
/// `process-plugins` entry: keep it alive as long as the agent whose
/// host it populated.
pub struct Composition {
    /// Installed remote plugins: their handles and claimed tool
    /// names.
    remote: PluginRegistry,
    /// The statically linked plugins' handles, with the tool names
    /// each install added (its claim, for the duplicate and
    /// core-name checks and for attributing a refusal).
    statics: Vec<StaticInstall>,
    /// Installed entries, in config order.
    installed: Vec<ResolvedPlugin>,
    /// Entries that could not be installed, or whose registration
    /// the registry refused.
    skipped: Vec<SkippedPlugin>,
}

/// One static plugin's name, handle, and the tools it registered.
struct StaticInstall {
    name: String,
    handle: PluginHandle,
    tools: Vec<String>,
}

impl Default for Composition {
    fn default() -> Self {
        Self {
            remote: PluginRegistry::new(),
            statics: Vec::new(),
            installed: Vec::new(),
            skipped: Vec::new(),
        }
    }
}

impl Composition {
    /// No plugins: the default deployment, and what a fixture that
    /// does not compose uses.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Install every configured plugin into `host`, bind it to
    /// `registry`, and record the outcome.
    ///
    /// This never fails as a whole: a plugin that cannot be
    /// installed is a diagnostic plus a skip, so there is no error
    /// for the caller to propagate and boot cannot be blocked by an
    /// optional integration (see the module docs). Entries are
    /// attempted in config order; a skipped entry does not stop the
    /// ones after it.
    ///
    /// The bind happens **first**. A host that already lives in a
    /// different registry cannot be moved, and installing first would
    /// leave every plugin registered in the foreign one while
    /// `resolved()` claimed them here; so a refused bind installs
    /// nothing, and every configured entry is skipped with the bind's
    /// own reason.
    ///
    /// A bind that lands returns the refusals of *that call*, which
    /// are folded here instead of the host's whole diagnostic
    /// history: a history read would re-report a previous
    /// composition's refusals, and its 64-entry cap would drop
    /// refusals while `resolved()` kept claiming them.
    #[must_use = "the composition owns the plugins' handles; dropping it uninstalls every plugin"]
    pub async fn build(
        config: &PluginsConfig,
        statics: &StaticPlugins,
        registry: &Arc<ToolRegistry>,
        host: &mut Host,
    ) -> Self {
        let mut composition = Self::empty();
        let refusals = host.bind(Arc::clone(registry));
        if let Some(refused) = refusals.iter().find(|diagnostic| {
            diagnostic.kind == DiagnosticKind::RegistryAlreadyBound
        }) {
            let reason =
                format!("the host refused the bind: {}", describe(refused));
            for entry in &config.plugins {
                composition.skip(entry_name(entry), reason.clone());
            }
            return composition;
        }
        composition.fold_refusals(&refusals);
        for entry in &config.plugins {
            match entry {
                PluginEntry::Static { .. } => {
                    composition.install_static(entry, statics, host, registry);
                }
                PluginEntry::Remote { .. } => {
                    composition.install_remote(entry, host, registry).await;
                }
            }
        }
        composition
    }

    /// The plugins that installed, in config order.
    ///
    /// A plugin the registry refused — at its claim check or at the
    /// bind — is not here; it is in [`Self::skipped`] with the reason,
    /// and none of its registrations are live.
    #[must_use]
    pub fn resolved(&self) -> &[ResolvedPlugin] {
        &self.installed
    }

    /// The entries that were skipped, with the reason.
    ///
    /// See [`SkippedPlugin`] for how a bind-time diagnostic is
    /// named when it cannot be attributed to one plugin.
    #[must_use]
    pub fn skipped(&self) -> &[SkippedPlugin] {
        &self.skipped
    }

    /// Which remote plugin claimed `tool`.
    ///
    /// Only remote claims are recorded here (a `Host` has no notion
    /// of plugin identity); the registry refuses a second plugin
    /// claiming a name, so this is also the record of who owns what.
    #[must_use]
    pub fn tool_owner(&self, tool: &str) -> Option<&str> {
        self.remote.owner(tool)
    }

    /// The number of registrations the composition owns.
    ///
    /// Every one of them is withdrawn when the composition is
    /// dropped, which is why this is the measured form of "the
    /// handles are alive". After [`Self::take_registrations`] the
    /// count is zero: the kernel entry, not the composition, is then
    /// the lifetime.
    #[must_use]
    pub fn registration_count(&self) -> usize {
        let remote = self
            .remote
            .handles()
            .iter()
            .map(|handle| handle.registrations().len())
            .sum::<usize>();
        let statics = self
            .statics
            .iter()
            .map(|install| install.handle.registrations().len())
            .sum::<usize>();
        remote + statics
    }

    /// Move every installed registration out of the composition.
    ///
    /// The boot's default profile mounts the `process-plugins` entry,
    /// which adopts this set: its effect binds the host to `tools` and
    /// drops the registrations on unload. A `Registration` withdraws
    /// only when its last clone drops, so a lending copy here would
    /// keep every plugin tool alive past the kernel's unload.
    ///
    /// The `resolved` / `skipped` records stay: an operator listing
    /// still reports what was installed, but this composition no
    /// longer uninstalls it. The caller hands the returned set to the
    /// kernel entry; dropping it withdraws exactly those
    /// registrations.
    pub fn take_registrations(&mut self) -> Vec<Registration> {
        let mut registrations = self.remote.take_registrations();
        for install in &mut self.statics {
            registrations
                .extend(install.handle.registrations().iter().cloned());
            install.handle = PluginHandle::default();
        }
        registrations
    }

    /// Install one `static` entry from the binary's table.
    fn install_static(
        &mut self,
        entry: &PluginEntry,
        statics: &StaticPlugins,
        host: &mut Host,
        registry: &ToolRegistry,
    ) {
        let PluginEntry::Static { name } = entry else {
            return;
        };
        let Some(factory) = statics.get(name) else {
            self.skip(
                name,
                "no static plugin factory is registered under this name",
            );
            return;
        };
        let before = tool_names(host);
        let plugin = factory();
        let handle = host.install(plugin.as_ref());
        // The remote path refuses a plugin that reaches the host
        // with nothing this host can use; a static plugin gets the
        // same answer. Reporting it resolved would claim a plugin
        // that contributes nothing.
        if handle.registrations().is_empty() {
            drop(handle);
            self.skip(
                name,
                "the plugin's install made no registration this host \
                 can use",
            );
            return;
        }
        let tools = added_tools(&before, &tool_names(host));
        // All or nothing: the plugin's tools are checked as one claim,
        // and one refusal withdraws the whole install. A plugin that
        // half-works is a plugin the operator cannot reason about, and
        // `resolved()` plus `skipped()` would no longer partition what
        // is live. Both channels are checked here, statics and remotes
        // alike: only remote claims used to go through
        // `PluginRegistry`, so two static entries could register one
        // name and the later would silently shadow the earlier.
        if let Some(reason) = self.claim_refusal(name, &tools, registry) {
            drop(handle);
            self.skip(name, reason);
            return;
        }
        let risk = tools_risk(host, &tools);
        self.statics.push(StaticInstall {
            name: name.clone(),
            handle,
            tools: tools.clone(),
        });
        self.installed.push(ResolvedPlugin {
            name: name.clone(),
            source: PluginSource::Static,
            // In-process code: the escalation `Full` names is already
            // inherent (see `ResolvedPlugin::authority`).
            authority: Authority::Full,
            risk,
            tools,
        });
    }

    /// Install one `remote` entry: spawn the process, handshake, and
    /// register what the two sides agreed on.
    async fn install_remote(
        &mut self,
        entry: &PluginEntry,
        host: &mut Host,
        registry: &ToolRegistry,
    ) {
        let PluginEntry::Remote {
            name,
            command,
            transport,
            capabilities,
            authority,
            risk,
            call_timeout_ms,
            env,
        } = entry
        else {
            return;
        };
        // The mappings are fallible so an entry built in code cannot
        // bypass the validation the config surface applies: an
        // unknown value is a diagnostic, never a silent default.
        let config = match remote_config(RemoteEntry {
            name,
            command,
            transport,
            capabilities,
            authority,
            risk,
            call_timeout_ms: *call_timeout_ms,
            env: env.as_ref(),
        }) {
            Ok(config) => config,
            Err(reason) => {
                self.skip(name, reason);
                return;
            }
        };
        let granted = config.authority;
        let granted_risk = config.risk;
        let plugin = RemotePlugin::new(config)
            // The server runs on tokio: the channel's blocking IO
            // goes to the blocking pool instead of one OS thread per
            // call.
            .with_blocking(crate::spawner::server_blocking());
        match self.remote.install(&plugin, host).await {
            Ok(tools) => {
                if let Some(reason) = self.claim_refusal(name, &tools, registry)
                {
                    // The host took some of the plugin's registrations
                    // before the registry refused a tool: withdraw them
                    // all, and free the claims so the next entry may
                    // use the names.
                    self.remote.withdraw(name);
                    self.skip(name, reason);
                    return;
                }
                // The handshake window's flooding diagnostic, logged
                // once here: the plugin kept the first frames and
                // lost the rest, and an operator should see that it
                // happened — not wonder which events went missing.
                if let Some(dropped) = self.remote.dropped_early_publishes(name)
                    && dropped > 0
                {
                    tracing::warn!(
                        target: "elysper.plugins",
                        plugin = name,
                        dropped,
                        "the plugin flooded `elysper/publish` during its \
                         handshake; the first frames were kept, the rest \
                         were dropped"
                    );
                }
                self.installed.push(ResolvedPlugin {
                    name: name.clone(),
                    source: PluginSource::Remote,
                    authority: granted,
                    risk: granted_risk,
                    tools,
                });
            }
            // A dead process, a failed handshake and a refused
            // promotion all land here; the plugin is optional
            // capability, so the reason is recorded and boot goes on.
            Err(error) => self.skip(name, error.to_string()),
        }
    }

    /// The first reason one of `tools` cannot stand as `plugin`'s
    /// claim, if any.
    ///
    /// Three rules, all of which the registry enforces somewhere else
    /// and none of which an install can see from its own return
    /// value: a name another entry already claimed (a second plugin
    /// on one name is capability substitution, not a stack), a name
    /// a core tool owns, which the registry refuses to file at all,
    /// and a name the **live registry** already holds — the shipped
    /// `skill` tool, an MCP-published tool, a host tool. A plugin
    /// that took such a name would shadow it: the registry resolves
    /// a name LIFO, so the later entry wins the dispatch.
    ///
    /// The plugin's own claims do not count against it — the remote
    /// path files them before this check runs — so the live-registry
    /// rule asks how many entries the name holds: one is the
    /// plugin's own, two or more means somebody else is under it too.
    fn claim_refusal(
        &self,
        plugin: &str,
        tools: &[String],
        registry: &ToolRegistry,
    ) -> Option<String> {
        for tool in tools {
            if let Some(owner) = self.owner_of(tool)
                && owner != plugin
            {
                return Some(format!(
                    "tool `{tool}` is already claimed by `{owner}`; the \
                     install is withdrawn"
                ));
            }
            if registry.core_owns(tool) {
                return Some(format!(
                    "tool `{tool}` is refused: a core tool already owns \
                     the name; the install is withdrawn"
                ));
            }
            if registry.name_occupants(tool) > 1 {
                return Some(format!(
                    "tool `{tool}` is refused: the name is already taken \
                     by another registration; the install is withdrawn"
                ));
            }
        }
        None
    }

    /// Withdraw one installed entry: its registrations and its claims.
    fn withdraw(&mut self, name: &str) {
        self.remote.withdraw(name);
        if let Some(position) =
            self.statics.iter().position(|install| install.name == name)
        {
            // Dropping the handle withdraws its registrations.
            self.statics.remove(position);
        }
        self.installed.retain(|plugin| plugin.name != name);
    }

    /// Fold one bind call's refusals into the skipped set.
    ///
    /// A registration the host refuses at the bind it just ran — a
    /// tool of the host's whose name a core tool owns — is a
    /// registration `resolved()` must not claim. The list is the
    /// bind's own, not the host's history: a history read re-reports
    /// the previous composition's refusals and drops beyond its cap.
    fn fold_refusals(&mut self, refusals: &[Diagnostic]) {
        for diagnostic in refusals {
            let attributed =
                self.owner_of(&diagnostic.source).map(str::to_string);
            let name = attributed
                .clone()
                .unwrap_or_else(|| diagnostic.source.clone());
            if attributed.is_some() {
                self.withdraw(&name);
            }
            self.skip(&name, describe(diagnostic));
        }
    }

    /// Which installed entry claimed `tool`, remote or static.
    fn owner_of(&self, tool: &str) -> Option<&str> {
        self.tool_owner(tool)
            .or_else(|| self.static_tool_owner(tool))
    }

    /// The static plugin whose install registered `tool`.
    fn static_tool_owner(&self, tool: &str) -> Option<&str> {
        self.statics
            .iter()
            .find(|install| install.tools.iter().any(|name| name == tool))
            .map(|install| install.name.as_str())
    }

    /// Record a skipped entry and log it once.
    fn skip(&mut self, name: &str, reason: impl Into<String>) {
        let reason = reason.into();
        tracing::warn!(
            plugin = name,
            reason = %reason,
            "plugin skipped; boot continues"
        );
        self.skipped.push(SkippedPlugin {
            name: name.to_string(),
            reason,
        });
    }
}

/// The configured name of one entry.
fn entry_name(entry: &PluginEntry) -> &str {
    match entry {
        PluginEntry::Static { name } | PluginEntry::Remote { name, .. } => name,
    }
}

/// A refusal as one line: what kind, and why.
fn describe(diagnostic: &Diagnostic) -> String {
    format!("{:?}: {}", diagnostic.kind, diagnostic.detail)
}

/// Every tool name the host holds, including one the registry will
/// refuse at bind (the row exists before the registry does).
fn tool_names(host: &Host) -> Vec<String> {
    host.tools()
        .iter()
        .map(|tool| tool.name().to_string())
        .collect()
}

/// The worst declared [`Risk`] bound among the host registrations named
/// in `tools`.
///
/// Risk is a per-call question (`Tool::risk` takes the arguments), and
/// the composition has no call, so a static plugin — which has no
/// configured risk to report — is shown at the argument-independent
/// bound its tools declare ([`Risk::from_annotations`], the same value
/// the default `Tool::risk` uses). `High` dominates, `Unknown` (a tool
/// that declared nothing) outranks `Low`, and a plugin that claimed no
/// tool at all reports `Unknown` — see [`ResolvedPlugin::risk`].
fn tools_risk(host: &Host, tools: &[String]) -> Risk {
    host.tools()
        .iter()
        .filter(|tool| tools.iter().any(|name| name == tool.name()))
        .map(|tool| Risk::from_annotations(tool.annotations()))
        .reduce(worse)
        .unwrap_or(Risk::Unknown)
}

/// The more demanding of two risks, for folding a plugin's tools into
/// one figure.
fn worse(left: Risk, right: Risk) -> Risk {
    match (left, right) {
        (Risk::High, _) | (_, Risk::High) => Risk::High,
        (Risk::Unknown, _) | (_, Risk::Unknown) => Risk::Unknown,
        (Risk::Low, Risk::Low) => Risk::Low,
    }
}

/// The names in `after` that `before` does not already account for,
/// counted rather than compared by membership: a second install of a
/// name the host already holds adds a row, and a membership test
/// would hide it — which is exactly the duplicate claim the check
/// exists to catch.
fn added_tools(before: &[String], after: &[String]) -> Vec<String> {
    let mut budget: HashMap<&str, usize> = HashMap::new();
    for name in before {
        *budget.entry(name.as_str()).or_default() += 1;
    }
    let mut added = Vec::new();
    for name in after {
        match budget.get_mut(name.as_str()) {
            Some(count) if *count > 0 => *count -= 1,
            _ => added.push(name.clone()),
        }
    }
    added
}

/// The fields a `remote` entry contributes to the host's config.
struct RemoteEntry<'a> {
    name: &'a str,
    command: &'a [String],
    transport: &'a str,
    capabilities: &'a [String],
    authority: &'a str,
    risk: &'a str,
    call_timeout_ms: Option<u64>,
    env: Option<&'a std::collections::HashMap<String, String>>,
}

/// The host's config for one `remote` entry.
fn remote_config(entry: RemoteEntry<'_>) -> Result<RemotePluginConfig, String> {
    Ok(RemotePluginConfig {
        name: entry.name.to_string(),
        command: entry.command.to_vec(),
        transport: transport_kind(entry.transport)?,
        capabilities: capabilities_of(entry.capabilities)?,
        authority: authority_kind(entry.authority)?,
        risk: risk_kind(entry.risk)?,
        call_timeout_ms: entry.call_timeout_ms.filter(|ms| *ms > 0),
        env: entry.env.cloned(),
    })
}

/// `http` has no endpoint in the config surface because this host
/// does not implement the transport: the install reports
/// [`elysper::plugin_host::HostError::UnsupportedTransport`] and the
/// composition skips the entry. The empty URL exists only to build
/// the value.
fn transport_kind(value: &str) -> Result<TransportKind, String> {
    match value {
        "stdio" => Ok(TransportKind::Stdio),
        "http" => Ok(TransportKind::Http { url: String::new() }),
        other => Err(format!(
            "unknown transport `{other}`: the config accepts `stdio` \
             and `http`"
        )),
    }
}

fn authority_kind(value: &str) -> Result<Authority, String> {
    match value {
        "reduced" => Ok(Authority::Reduced),
        "full" => Ok(Authority::Full),
        other => Err(format!(
            "unknown authority `{other}`: the config accepts `reduced` \
             and `full`"
        )),
    }
}

fn risk_kind(value: &str) -> Result<Risk, String> {
    match value {
        "low" => Ok(Risk::Low),
        "high" => Ok(Risk::High),
        "unknown" => Ok(Risk::Unknown),
        other => Err(format!(
            "unknown risk `{other}`: the config accepts `low`, `high` \
             and `unknown`"
        )),
    }
}

fn capabilities_of(values: &[String]) -> Result<PluginCapabilities, String> {
    let mut capabilities = PluginCapabilities {
        tools: false,
        observe: false,
        transform: false,
        gate: false,
        // Deliberately not config-settable: no deployment wiring
        // routes a host-level plugin event to a session yet, so a
        // config that named `publish` would enable a role nothing
        // consumes. The seam is programmatic — a sink attached with
        // `RemotePlugin::with_event_sink` at the session that should
        // receive the events — until a routing decision lands.
        publish: false,
    };
    for value in values {
        match value.as_str() {
            "tools" => capabilities.tools = true,
            "observe" => capabilities.observe = true,
            "transform" => capabilities.transform = true,
            "gate" => capabilities.gate = true,
            // A real protocol role, but not one a deployment file can
            // grant yet — see the comment on the struct literal above.
            "publish" => {
                return Err(
                    "the `publish` capability is not configurable yet: \
                     nothing routes a host-level plugin event to a \
                     session, so enabling it here would grant a role \
                     nothing consumes. Attach a sink in code with \
                     `RemotePlugin::with_event_sink` instead"
                        .to_string(),
                );
            }
            other => {
                return Err(format!(
                    "unknown capability `{other}`: the config accepts \
                     `tools`, `observe`, `transform` and `gate`"
                ));
            }
        }
    }
    Ok(capabilities)
}

#[cfg(test)]
mod tests;
