//! The boot-time helpers the server wires together:
//! readers for the agent table (`load_default_*`), the MCP
//! server registration sweep, the `[tools]` surface application,
//! and the prompt manifest builder. Each is a single-purpose
//! step `AppState::with_server_config` orchestrates; the file
//! is intentionally a flat list of free functions (no
//! shared state, no cross-calls except the private
//! helpers `apply_tools_config`
//! and `register_configured_mcp_servers` use).

use std::sync::Arc;

use elysper::{
    core::registry::{Registry as _, RegistryItem},
    harness::{AgentRegistry, PromptContext},
    tool::ToolRegistry,
};

// ---------------------------------------------------------------------------
// Agent table readers (R16 / R50 / R55 / R58)
// ---------------------------------------------------------------------------

/// Resolve the configured default iteration cap.
///
/// Reads `agents.<name>.max_steps` from the server config (the
/// same `[agents.<name>]` block that sets `description`,
/// `model`, etc.) and clamps it into `[1, 4096]`. Returns
/// `None` when the field is unset so callers can fall back to
/// [`elysper::harness::DEFAULT_MAX_ITERATIONS`].
pub(crate) fn load_default_max_iterations(
    cfg: Option<&crate::config::ServerConfig>,
    default_agent_name: Option<&str>,
) -> Option<usize> {
    let name = default_agent_name?;
    let raw = cfg?.agents.get(name)?.max_steps?;
    // enforces, so the operator can express "no cap" (u32::MAX)
    // without enabling an unbounded session.
    Some(raw.clamp(1, 4096) as usize)
}

/// R16: reads `agents.<name>.compaction` from the server config
/// and returns it for the run factory. `None` when unset — the
/// loop then keeps its default context manager.
pub(crate) fn load_default_compaction(
    cfg: Option<&crate::config::ServerConfig>,
    default_agent_name: Option<&str>,
) -> Option<elysper::session::CompactionSettings> {
    let name = default_agent_name?;
    cfg?.agents.get(name)?.compaction
}

/// `agents.<name>` keys the boot parses but does not apply.
///
/// R55: `[agents]` grew a display/feature shape (description, colour,
/// per-agent tool lists) whose consumers were never built — the listing
/// serves registry descriptors, the model comes from the descriptor's
/// `model_hint`, and what a run may *see and call* comes from the
/// `[tools]` surface plus the registry's privacy flag. A key that
/// parses and does nothing is worse than an absent one: an operator who
/// wrote `denied_tools = ["shell"]` has restricted nothing and has no
/// way to find out.
///
/// R124: `sandbox` (R62) is parsed and exposed via
/// [`crate::config::AgentConfig::sandbox_policy_name`], yet the run
/// factory's boot-time `ShellTool::new()` (registered at
/// `state/app_state/tools.rs`) is what every run actually uses; the
/// per-agent override is therefore silently inert today. Classify it
/// as unapplied so the boot log names the gap — the actual consumer
/// (`with_policy` re-bind in the run factory) is tracked in
/// `docs/design/2026-10-26-harness-v4-plan.md` §2 as a follow-up.
///
/// Returned rather than logged so the classification is testable; the
/// caller logs one line per configured agent.
pub(crate) fn unapplied_agent_keys(
    agent: &crate::config::AgentConfig,
) -> Vec<&'static str> {
    let mut keys = Vec::new();
    if agent.description.is_some() {
        keys.push("description");
    }
    if agent.model.is_some() {
        keys.push("model");
    }
    if agent.hidden {
        keys.push("hidden");
    }
    if agent.color.is_some() {
        keys.push("color");
    }
    if agent.sandbox_policy_name().is_some() {
        keys.push("sandbox");
    }
    keys
}

/// Log every configured agent that carries keys the boot ignores.
pub(crate) fn warn_unapplied_agent_keys(config: &crate::config::ServerConfig) {
    for (name, agent) in &config.agents {
        let unapplied = unapplied_agent_keys(agent);
        if unapplied.is_empty() {
            continue;
        }
        tracing::warn!(
            target: "elysper.agent",
            agent = %name,
            keys = ?unapplied,
            "agent config keys are parsed but not applied; what a run may \
             see and call comes from [tools] (groups / hidden / deferred) \
             and the registry's privacy flag — see DEPLOYMENT.md"
        );
    }
}

/// R58: reads `agents.<name>.{allowed_tools,denied_tools}` and turns
/// them into the [`elysper::tool::ToolRestriction`] the run factory
/// installs.
///
/// `allowed_tools` becomes the allow-list (empty means "no allow-list",
/// not "nothing is allowed"), `denied_tools` the deny-list; both empty
/// means no restriction at all, which is the common case and stays
/// allocation-free.
pub(crate) fn load_default_tool_restriction(
    cfg: Option<&crate::config::ServerConfig>,
    default_agent_name: Option<&str>,
) -> Option<Arc<elysper::tool::ToolRestriction>> {
    let name = default_agent_name?;
    let agent = cfg?.agents.get(name)?;
    if agent.allowed_tools.is_empty() && agent.denied_tools.is_empty() {
        return None;
    }
    let allow =
        (!agent.allowed_tools.is_empty()).then(|| agent.allowed_tools.clone());
    tracing::info!(
        agent = %name,
        allow = ?allow,
        deny = ?agent.denied_tools,
        "agent tool restriction configured"
    );
    Some(Arc::new(elysper::tool::ToolRestriction {
        allow,
        deny: agent.denied_tools.clone(),
    }))
}

/// Resolve a strategy name through one [`StrategyRegistry`]: the plugin
/// strategies this server's facade features enabled (`strategy-cot`,
/// `strategy-best-of-n`) are registered under their config aliases, and the
/// harness's built-in `react` resolves through the same call without a
/// registration.
pub(crate) fn resolve_strategy(
    configured: &str,
) -> Result<
    Arc<dyn elysper::harness::agent::ReasoningStrategy>,
    elysper::core::Error,
> {
    let registry = elysper::harness::agent::StrategyRegistry::new();
    let cot = || {
        Arc::new(elysper::strategy_cot::ChainOfThoughtStrategy::default())
            as Arc<dyn elysper::harness::agent::ReasoningStrategy>
    };
    let bon = || {
        Arc::new(elysper::strategy_best_of_n::BestOfNStrategy::default())
            as Arc<dyn elysper::harness::agent::ReasoningStrategy>
    };
    for name in ["chain-of-thought", "cot", "chain_of_thought"] {
        registry.register(name, cot());
    }
    for name in ["best-of-n", "bon", "best_of_n"] {
        registry.register(name, bon());
    }
    registry.resolve(configured)
}

/// R50: the raw strategy **alias** `agents.<name>.strategy` names.
///
/// The `agent-loop` row's config carries the alias, not a strategy
/// object: the row resolves it through [`resolve_strategy`] when it
/// builds its loop, so the configuration keeps its own vocabulary
/// (`chain-of-thought`, `best-of-n`, `react`, …) all the way to the
/// row. `None` when unset.
pub(crate) fn load_default_strategy_alias(
    cfg: Option<&crate::config::ServerConfig>,
    default_agent_name: Option<&str>,
) -> Option<String> {
    let name = default_agent_name?;
    cfg?.agents.get(name)?.strategy.clone()
}

// ---------------------------------------------------------------------------
// MCP server registration (R21/R30)
// ---------------------------------------------------------------------------

/// R21/R30: connect every enabled MCP server from the server
/// config through a [`elysper::mcp::McpSupervisor`] and publish
/// each server's tools into `registry` under namespaced names
/// (`mcp__<server>__<tool>`), so two servers may advertise the
/// same raw tool name.
///
/// One `tick` performs the boot connection sweep; the returned
/// health list covers every configured server, healthy or not.
/// Returns the live clients of the healthy servers — the caller
/// MUST keep them alive for the process lifetime, because
/// dropping a `StdioTransport` kills its child process
/// (`kill_on_drop`). A server that fails to spawn / handshake /
/// list is logged at `warn` and skipped, so one broken
/// integration cannot block boot.
pub(crate) async fn register_configured_mcp_servers(
    supervisor: Arc<elysper::mcp::McpSupervisor>,
    registry: &ToolRegistry,
    now: chrono::DateTime<chrono::Utc>,
    workspace_root: &std::path::Path,
) -> (
    Vec<(String, Arc<elysper::mcp::McpClient>)>,
    Vec<elysper::mcp::ServerHealth>,
) {
    for (name, entry) in load_mcp_declarations(workspace_root) {
        if !entry.is_enabled() {
            tracing::info!(
                server = %name,
                "the MCP server declaration is disabled; not launching it"
            );
            continue;
        }
        let factory = Arc::new(elysper::mcp::StdioTransportFactory::new(
            entry.to_stdio_config(&name),
        ));
        supervisor.supervise(name, factory).await;
    }
    let tick = supervisor.tick(registry, now).await;
    log_tick_health(&tick.health);
    let clients = supervisor.healthy_clients().await;
    (clients, tick.health)
}

/// Every MCP server the deployment declares, in precedence order.
///
/// The declarations come from the `mcp.json` files of the two-layer
/// `~/.agents` layout. A file inside a plugin bundle namespaces its
/// servers under the bundle's name (`<plugin>/<server>`), so two
/// bundles may declare a server of the same name without shadowing
/// each other.
///
/// A file that does not parse is skipped with a warning: a broken
/// declaration must never keep the other servers from launching.
/// The code components the deployment's plugin bundles declare.
///
/// A bundle's `plugin.json` may name a subprocess the host runs and
/// talks to over the plugin protocol (see
/// [`elysper::core::layout::PluginManifest`]). Each becomes a
/// [`crate::config::PluginEntry::Remote`] and installs through the
/// same channel a config-declared remote plugin uses — one install
/// path, one set of claims and refusals.
///
/// Order is precedence: the `[plugins]` config first, then project
/// bundles, then global bundles; a name declared earlier wins, and
/// [`elysper::core::layout::merge_by_name`] keeps that order.
///
/// A bundle without a manifest, or with one that fails to parse,
/// contributes nothing here — its skills and agents still load (they
/// are walked per-domain); only its code component needs this path.
/// Register the agents the layout's files define.
///
/// `agents/<name>/agent.md` (+ `config.json`) is the storage an
/// agent definition is; the boot materializes each one into the
/// registry via the same builder the CRUD surface uses. Precedence
/// is the layout's (project before global, direct before bundled),
/// already applied by [`elysper::agents::discover`]; a name the
/// registry already holds — the built-in `agent`, or an earlier
/// layer's definition — is left alone: first writer wins, which is
/// also what `merge_by_name` gives every other domain.
pub(crate) async fn register_file_agents(state: &crate::state::AppState) {
    for discovered in elysper::agents::discover(&state.workspace_root) {
        let descriptor = discovered.definition.descriptor();
        let name = descriptor.name.clone();
        if state.agent_registry.resolve_sync(&name).is_some() {
            tracing::debug!(
                agent = %name,
                "a file-backed agent is shadowed by an earlier definition; \
                 skipping"
            );
            continue;
        }
        // The boot runs before the tokio runtime owns the stack a
        // registry put would need to await on, so the builder's
        // synchronous contract is the one used here too.
        let agent = match crate::routes::agents::build_react_agent(
            state, descriptor, None,
        )
        .await
        {
            Ok(agent) => agent,
            Err(error) => {
                tracing::warn!(
                    agent = %name,
                    %error,
                    "the file-backed agent could not be built"
                );
                continue;
            }
        };
        let entry = elysper::harness::agent::AgentEntry::new(agent);
        if let Err(error) =
            futures::executor::block_on(state.agent_registry.put(entry))
        {
            tracing::warn!(
                agent = %name,
                %error,
                "the file-backed agent could not be registered"
            );
        }
    }
}

pub(crate) fn bundle_plugin_entries(
    workspace_root: &std::path::Path,
) -> Vec<crate::config::PluginEntry> {
    let mut roots =
        vec![workspace_root.join(elysper::core::layout::AGENTS_DIR)];
    if let Some(global) = elysper::core::layout::home_root() {
        roots.push(global);
    }
    let mut out = Vec::new();
    for root in roots {
        for (name, bundle) in elysper::core::layout::plugin_bundles(&root) {
            let Some(manifest) =
                elysper::core::layout::plugin_manifest(&bundle)
            else {
                tracing::debug!(
                    bundle = %name,
                    "the plugin bundle has no manifest; only its                      definitions load"
                );
                continue;
            };
            let Some(code) = manifest.code else {
                continue;
            };
            let mut command = vec![code.command];
            command.extend(code.args.unwrap_or_default());
            // The ceiling is the operator's default for a third-party
            // subprocess: tools and observation, never gate; reduced
            // authority; unclassified risk. A deployment that wants
            // more names the plugin in its `[plugins]` config, which
            // this list sits behind in precedence.
            out.push(crate::config::PluginEntry::Remote {
                name: manifest.name,
                command,
                transport: "stdio".to_string(),
                capabilities: vec!["tools".to_string(), "observe".to_string()],
                authority: "reduced".to_string(),
                risk: "unknown".to_string(),
                call_timeout_ms: None,
                env: code.env,
            });
        }
    }
    elysper::core::layout::merge_by_name(out, |entry| match entry {
        crate::config::PluginEntry::Remote { name, .. }
        | crate::config::PluginEntry::Static { name } => name,
    })
}

pub(crate) fn load_mcp_declarations(
    workspace_root: &std::path::Path,
) -> Vec<(String, elysper::mcp::config_file::McpServerEntry)> {
    let mut out = Vec::new();
    for (namespace, path) in
        elysper::core::layout::mcp_config_paths(workspace_root)
    {
        if !path.is_file() {
            continue;
        }
        let file = match elysper::mcp::config_file::McpConfigFile::read(&path) {
            Ok(file) => file,
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "skipping malformed MCP declaration file"
                );
                continue;
            }
        };
        for (name, entry) in file.servers {
            let qualified = if namespace.is_empty() {
                name
            } else {
                format!("{namespace}/{name}")
            };
            out.push((qualified, entry));
        }
    }
    out
}

/// Emit the boot-sweep log lines for every configured MCP server.
///
/// Walks the health slice in order. A healthy server is logged at
/// `info` with the count of registered tools; an unhealthy one is
/// logged at `warn` with its last state and consecutive-failure
/// count so an operator can decide whether to retry, restart the
/// child process, or drop the entry. The two log shapes keep the
/// orchestrator's loop flat.
fn log_tick_health(health: &[elysper::mcp::ServerHealth]) {
    for entry in health {
        if entry.healthy() {
            tracing::info!(
                target: "elysper.mcp",
                server = %entry.name,
                tools = entry.registered_tools,
                "registered MCP server tools"
            );
        } else {
            tracing::warn!(
                target: "elysper.mcp",
                server = %entry.name,
                state = ?entry.state,
                failures = entry.consecutive_failures,
                "MCP server unavailable; its tools are not registered"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// `[tools]` boot application (R34)
// ---------------------------------------------------------------------------

/// Apply the `[tools]` section to `registry` at boot.
///
/// The contract mirrors the MCP path: an optional integration that is
/// misconfigured must never block startup. A name the registry does
/// not have is dropped with a warning, as are an unknown
/// `active_groups` entry and a tool claimed by two groups — an MCP
/// server that failed to register must not take the server down. The
/// writes follow the documented order `groups` → `deferred` →
/// `hidden`, so a tool named by several keys ends with the later
/// treatment (`hidden` wins).
pub(crate) fn apply_tools_config(
    registry: &ToolRegistry,
    config: &crate::config::ToolsConfig,
) -> super::AppliedToolSurface {
    let mut skipped = Vec::new();
    let mut applied = super::AppliedToolSurface {
        max_visible: config.max_visible,
        groups: resolve_group_declarations(
            registry,
            &config.groups,
            &mut skipped,
        ),
        ..super::AppliedToolSurface::default()
    };

    activate_groups(&config.active_groups, &mut applied, &mut skipped);
    applied.skipped = skipped;
    if let Some(policy) = applied.policy()
        && let Err(error) = policy.write_to_registry(registry)
    {
        // Unreachable after the filtering above; kept visible so a
        // future divergence degrades loudly instead of silently
        // dropping the group state.
        tracing::warn!(
            target: "elysper.tools",
            %error,
            "tool surface: group policy was not applied"
        );
    }

    // Deferred, then hidden — a later key wins.
    applied.deferred = apply_surface_list(
        &config.deferred,
        &mut applied.skipped,
        "deferred",
        |name| {
            registry.set_exposure(name, elysper::tool::ToolExposure::Deferred)
        },
    );
    applied.hidden = apply_surface_list(
        &config.hidden,
        &mut applied.skipped,
        "hidden",
        |name| registry.set_hidden(name, true),
    );

    tracing::info!(
        target: "elysper.tools",
        deferred = applied.deferred.len(),
        hidden = applied.hidden.len(),
        groups = applied.groups.len(),
        active_groups = ?applied.active_groups,
        max_visible = ?applied.max_visible,
        skipped = ?applied.skipped,
        "tool surface applied from [tools] config"
    );
    applied
}

/// Activate the declared `active_groups` against `applied.groups`.
///
/// Walks `active_groups` in order and pushes the recognised names
/// onto `applied.active_groups`; an entry that was never declared
/// (or was filtered out by [`resolve_group_declarations`]) is
/// logged at `warn` and appended to `skipped` so the caller can
/// surface the boot-time drop. The verdict is later applied
/// through [`ToolSurfacePolicy`] when [`AppliedToolSurface::policy`]
/// is installed.
fn activate_groups(
    active_groups: &[String],
    applied: &mut super::AppliedToolSurface,
    skipped: &mut Vec<String>,
) {
    for group in active_groups {
        if applied.groups.contains_key(group) {
            applied.active_groups.push(group.clone());
        } else {
            tracing::warn!(
                target: "elysper.tools",
                group = %group,
                "tool surface: active group was never declared; skipped"
            );
            skipped.push(group.clone());
        }
    }
}

/// Resolve the `groups` declarations against `registry`: an unknown
/// member and a member already claimed by an earlier (name-order)
/// group are skipped with a warning and recorded in `skipped`. Never
/// fails — the boot-must-survive contract.
fn resolve_group_declarations(
    registry: &ToolRegistry,
    declared: &std::collections::BTreeMap<String, Vec<String>>,
    skipped: &mut Vec<String>,
) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut resolved = std::collections::BTreeMap::new();
    let mut owner: std::collections::BTreeMap<&str, &str> =
        std::collections::BTreeMap::new();
    for (group, members) in declared {
        let mut kept = Vec::with_capacity(members.len());
        for member in members {
            if !registry.contains(member) {
                tracing::warn!(
                    target: "elysper.tools",
                    group = %group,
                    tool = %member,
                    "tool surface: group member is not registered; skipped"
                );
                skipped.push(member.clone());
                continue;
            }
            if let Some(existing) = owner.get(member.as_str()) {
                tracing::warn!(
                    target: "elysper.tools",
                    group = %group,
                    tool = %member,
                    existing = %existing,
                    "tool surface: tool already claimed by another group; \
                     claim skipped"
                );
                skipped.push(member.clone());
                continue;
            }
            owner.insert(member.as_str(), group.as_str());
            kept.push(member.clone());
        }
        resolved.insert(group.clone(), kept);
    }
    resolved
}

/// Apply `settle` to every name, returning the ones the registry
/// took. An unknown name is warned about and recorded in `skipped` —
/// the boot-must-survive contract shared by the `deferred` and
/// `hidden` lists.
fn apply_surface_list(
    names: &[String],
    skipped: &mut Vec<String>,
    list: &str,
    settle: impl Fn(&str) -> bool,
) -> Vec<String> {
    let mut applied = Vec::with_capacity(names.len());
    for name in names {
        if settle(name) {
            applied.push(name.clone());
        } else {
            tracing::warn!(
                target: "elysper.tools",
                tool = %name,
                list = %list,
                "tool surface: tool is not registered; skipped"
            );
            skipped.push(name.clone());
        }
    }
    applied
}

// ---------------------------------------------------------------------------
// Prompt-context builder
// ---------------------------------------------------------------------------

/// Build the prompt-context manifest the assembler consumes.
///
/// Skills come from the `skills` service the kernel mounted
/// ([`SkillPlugin`](elysper::skill::SkillPlugin)), never from a direct
/// directory walk: the plugin publishes the deployment's one
/// [`SkillRegistry`](elysper::skill::SkillRegistry), the `skill` tool
/// registers into the same registry, and unloading the plugin removes
/// both the service and the `<available_skills>` block. An absent or
/// unloaded service contributes no skills — the block is then omitted,
/// exactly as it is for a deployment with no skill files.
///
/// Peer agents are snapshotted from the live `AgentRegistry`
/// inline; the caller-supplied `self_name` is excluded so the
/// running agent never sees itself as a handoff target.
pub(crate) async fn build_prompt_context(
    kernel: &elysper::kernel::Context,
    agent_registry: &Arc<AgentRegistry>,
    self_name: &str,
) -> PromptContext {
    let mut ctx = PromptContext::default();

    // Skills: the service is the source of truth. A read failure is
    // treated as "no service" — the prompt keeps its text and simply
    // advertises no skills rather than failing the agent build.
    if let Some(registry) = kernel
        .get::<elysper::skill::SkillRegistry>(elysper::skill::KEY_SKILLS)
        .ok()
        .flatten()
    {
        for skill in registry.collect().await.skills {
            ctx = ctx
                .with_skill(skill.name.clone(), skill.effective_description());
        }
    }

    // Peer agents: snapshot the registry, exclude `self_name`,
    // feed each remaining descriptor into the builder. A
    // registry error or empty list is non-fatal: the manifest
    // simply carries no peer agents.
    if let Ok(entries) = agent_registry.list(None).await {
        for entry in entries.into_iter().filter(|e| e.name() != self_name) {
            ctx = ctx.with_agent(entry.descriptor());
        }
    }

    ctx
}
