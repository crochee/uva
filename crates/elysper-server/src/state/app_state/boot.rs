//! `AppState` construction: resolve the deployment config,
//! compose the tool surface, build the default agent, and
//! assemble every field. One concern—the boot.

use std::{path::PathBuf, sync::Arc};

use dashmap::DashMap;
use elysper::{
    core::{Clock, registry::Registry},
    harness::{AgentEntry, AgentRegistry},
    provider::{config::WorkspaceConfig, traits::ModelProvider},
    session::{
        JsonlSessionSearch,
        SharedSessionSearch,
        manager::SessionRegistry,
    },
    skill::{register_skill_tool, seed_default_skills},
    tool::ToolRegistry,
    tool_task::TaskDelegator,
};

use super::{
    AppState,
    EvalsState,
    ProviderCatalogue,
    SchedulesState,
    WorkflowsState,
    tools::plugin_tool_registry,
    usage::UsageMetrics,
};
use crate::{
    config::{AuthConfig, CorsConfig},
    state::boot::{
        apply_tools_config,
        build_prompt_context,
        load_default_compaction,
        load_default_max_iterations,
        load_default_strategy,
        load_default_tool_restriction,
        register_configured_mcp_servers,
        warn_unapplied_agent_keys,
    },
};

/// Resolve the runtime [`WorkspaceConfig`] from the optional `--config`
/// override first, falling back to the legacy
/// `<workspace>/.agents/config.toml` path when no override is given or
/// the override fails to load.
fn load_workspace_config(
    workspace_root: &std::path::Path,
    config_path: Option<&std::path::PathBuf>,
) -> Result<WorkspaceConfig, anyhow::Error> {
    let Some(path) = config_path else {
        return legacy_workspace_config(workspace_root);
    };
    match bridged_workspace_config(path) {
        Ok(cfg) => Ok(cfg),
        Err(reason) => {
            tracing::warn!(path = %path.display(), reason, "falling back");
            legacy_workspace_config(workspace_root)
        }
    }
}

/// One attempt at loading the `--config` YAML as a
/// [`WorkspaceConfig`]. `Err` carries the human-readable
/// reason (missing / unsupported / parse failure) so the
/// caller logs one line and falls back.
fn bridged_workspace_config(
    path: &std::path::Path,
) -> Result<WorkspaceConfig, String> {
    use crate::config::yaml_bridge;
    match yaml_bridge::load_yaml_config_as_workspace_config(path) {
        Ok(Some(cfg)) => {
            tracing::info!(
                path = %path.display(),
                providers = ?cfg.providers.keys().collect::<Vec<_>>(),
                "loaded provider configuration from --config"
            );
            Ok(cfg)
        }
        Ok(None) => Err("--config path missing or unsupported".to_string()),
        Err(e) => Err(format!("failed to bridge --config: {e}")),
    }
}

/// `load_workspace_config`'s legacy fallback: the
/// `<workspace>/.agents/config.toml` path used when the
/// `--config` override is absent, unsupported, or fails.
fn legacy_workspace_config(
    workspace_root: &std::path::Path,
) -> Result<WorkspaceConfig, anyhow::Error> {
    WorkspaceConfig::load_from_dir(workspace_root)
        .map_err(|e| anyhow::anyhow!(e.to_string()))
}

impl AppState {
    pub async fn new(
        workspace_root: PathBuf,
        config_path: Option<&std::path::PathBuf>,
    ) -> Result<Arc<Self>, String> {
        let server_config =
            crate::config::resolve_server_config(&workspace_root, config_path);
        Self::with_server_config(workspace_root, config_path, server_config)
            .await
    }

    /// [`AppState::new`] with the deployment configuration already
    /// resolved by the caller.
    ///
    /// The CLI needs the resolved config *before* any state exists—to
    /// compute the bind address from `--host`/`ELYSPER_HOST`/`host`
    /// precedence (R54)—so it resolves once and hands the value in
    /// rather than letting this function read the file a second time.
    pub async fn with_server_config(
        workspace_root: PathBuf,
        config_path: Option<&std::path::PathBuf>,
        server_config: Option<crate::config::ServerConfig>,
    ) -> Result<Arc<Self>, String> {
        let workspace_config = load_workspace_config(&workspace_root, config_path)
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "Failed to load workspace config, using env fallback");
                WorkspaceConfig::from_env()
            });

        // R55: `agents.<name>` accepts keys the boot does not apply
        // (`description`, `model`, `allowed_tools`, `denied_tools`,
        // `hidden`, `color`). Silence is the bug—an operator who wrote
        // `denied_tools = ["shell"]` believes the tool is gone. Name the
        // keys, once per agent, at boot.
        if let Some(config) = server_config.as_ref() {
            warn_unapplied_agent_keys(config);
        }

        let default_model = workspace_config.default_model.clone();

        // Build every configured provider once, here: a turn then
        // resolves a name to a handle without touching the
        // environment, and the boot's failure mode stays explicit—
        // the *default* provider must build, a secondary one may not.
        let provider_catalogue =
            ProviderCatalogue::from_workspace_config(&workspace_config);
        let default_provider: Arc<dyn ModelProvider> =
            provider_catalogue.default_handle().map_err(|e| {
                tracing::error!(error = %e, "Failed to create default provider");
                format!(
                    "No LLM provider available: {e}. Configure.agents/config.toml or set \
                     OPENAI_API_KEY/ANTHROPIC_API_KEY environment variables."
                )
            })?;

        // Tier-1 provider wraps (semantic-cache →
        // resilience → budget) compose onto the default provider
        // when their config keys are enabled. Each wrapper is
        // opt-in: the boot reads the relevant `[cache.semantic]`,
        // `[resilience]`, and `[budget]` sections and composes
        // them in inner-to-outer order so the cache decision sees
        // real (unwrapped) provider errors, the breaker sees real
        // cache misses, and the budget gate is the last gate the
        // call goes through.
        // One clock for the whole process: every timestamped
        // component (session log rows, operation snapshots, the
        // attachment store, the MCP supervisor's tick) reads it from
        // here rather than calling the wall clock itself.
        let clock = elysper::core::SharedClock::system();

        // Tier-1 provider wraps (semantic-cache →
        // resilience → budget) compose onto the default provider
        // when their config keys are enabled. Each wrapper is
        // opt-in: the boot reads the relevant `[cache.semantic]`,
        // `[resilience]`, and `[budget]` sections and composes
        // them in inner-to-outer order so the cache decision sees
        // real (unwrapped) provider errors, the breaker sees real
        // cache misses, and the budget gate is the last gate the
        // call goes through.
        let default_provider = wrap_tier1_providers(
            default_provider,
            server_config.as_ref(),
            clock.clone(),
        );

        // The store walk and every session's fs writes run on the
        // tokio blocking pool (the seams' defaults are runtime-free
        // but pay one OS thread per call).
        let session_search: SharedSessionSearch = Arc::new(
            JsonlSessionSearch::new(workspace_root.join("sessions"))
                .with_blocking(crate::spawner::server_blocking()),
        );
        let session_manager = Arc::new(
            SessionRegistry::new(workspace_root.join("sessions"))
                .with_blocking(crate::spawner::server_blocking()),
        );
        // The default tool surface, composed from the plugin
        // crates, then the agent-facing `skill` tool on top.
        // The skill implementation lives in `elysper-skill` so the
        // HTTP list route and the LLM-facing tool share one home;
        // registering before wrapping in `Arc` lets the synchronous
        // `register_skill_tool(&registry)` call take `&ToolRegistry`
        // directly.
        let tool_registry_built = plugin_tool_registry();
        register_skill_tool(&tool_registry_built);
        // R21/R30: connect the configured MCP servers through the
        // supervisor and publish their tools. A server that fails
        // to start is logged (with its health state) and skipped—
        // boot must never be blocked by an optional integration.
        let (mcp_clients, mcp_health) = match server_config.as_ref() {
            Some(config) => {
                register_configured_mcp_servers(
                    &tool_registry_built,
                    clock.now(),
                    config,
                )
                .await
            }
            None => (Vec::new(), Vec::new()),
        };
        // Smaller per-request load: with cross-domain search wired
        // below, MCP tools stop carrying their full JSON schemas on
        // every completion request—they advertise name +
        // description (`ToolExposure::Deferred`) and the schema
        // promotes on the first call. Only `Direct` flips, so an
        // operator's explicit `deferred` / `hidden` verdict stands.
        for d in tool_registry_built.descriptors().iter().filter(|d| {
            d.name.starts_with("mcp__")
                && d.exposure == elysper::tool::ToolExposure::Direct
        }) {
            tool_registry_built
                .set_exposure(&d.name, elysper::tool::ToolExposure::Deferred);
        }
        let tool_registry = Arc::new(tool_registry_built);
        // Per-section config: auth, CORS, operations, default
        // agent name, max iterations, compaction, restriction,
        // strategy. The eight fields group by what the boot does
        // with them, not by where they sit in the file.
        // Extracting the eight near-identical `cfg?` chains
        // keeps the orchestrator flat: the reader scans one
        // list and one shape at a time, instead of repeating
        // the chain eight times.
        let PerSectionConfig {
            auth_config,
            cors_config,
            operations_config,
            default_agent_name_value,
            default_max_iterations,
            default_compaction,
            default_tool_restriction,
            default_strategy,
        } = Self::load_per_section_config(&server_config);

        // Default system prompt. The ReAct loop builds its
        // own system prompt per-dispatch via
        // `elysper::harness::PromptContext::assemble`, so
        // this is only the legacy/default fallback for
        // callers that read `AppState::system_prompt`
        // directly without an explicit descriptor.
        let system_prompt = elysper::harness::DEFAULT_SYSTEM_PROMPT.to_string();

        // Seed the workflow skills on disk so the v1 HTTP
        // `GET /api/v1/skills` endpoint—which scans the workspace's
        // `.agents/skills/` directory—has visible data on a fresh
        // checkout. Existing user-installed skills are never
        // overwritten.
        if let Err(err) = seed_default_skills(&workspace_root) {
            tracing::warn!(error = %err, "failed to seed default skills");
        }

        // Build the multi-agent registry, the server-wide host
        // (policy + plugins), and register the default ReAct agent.
        // Extracted so the orchestrator stays flat—the
        // largest block in `with_server_config` (50+ lines
        // threading tool registry / provider / policy /
        // restriction / strategy through the
        // `ReActAgent::with_*` builder chain, plus a
        // `try_read()` snapshot and four `if let Some` chains)
        // lives in its own private fn.
        // Task 15: the deployment's plugins come from this config
        // section, not from cargo features. The static table starts
        // empty — no in-tree crate implements
        // `elysper_harness::Plugin` today — so a fork that links a
        // plugin crate registers its factory here, one line per
        // plugin. The composition is installed into the deployment's
        // one host; both the host and the composition's handles live
        // on `AppState` below, for the process lifetime. Every agent
        // this server builds — including each per-run session agent —
        // gets a clone of that host and binds the shared registry, so
        // a plugin installed here is visible to chat, not only to the
        // boot agent.
        let plugins_config = server_config
            .as_ref()
            .map(|config| config.plugins.clone())
            .unwrap_or_default();
        let static_plugins = crate::composition::StaticPlugins::new();
        // The deployment's one host: the production policy
        // (guards / hints / tracker / logging hook) plus every
        // configured plugin. Every agent this server builds gets a
        // clone, so the policy applies to chat runs too.
        let mut plugin_host =
            elysper::harness::Host::default_policy(workspace_root.clone());
        let plugins = crate::composition::Composition::build(
            &plugins_config,
            &static_plugins,
            &tool_registry,
            &mut plugin_host,
        )
        .await;
        // R34 (Task 16): apply the deployment's `[tools]` surface after
        // the plugin bind, not before it. The bind is what puts a
        // plugin's tools in the registry, so a `[tools]` entry naming
        // one was silently skipped when this ran first — the config was
        // read against a registry that did not have the tool yet.
        // Every registration source has now had its say: builtins, the
        // skill tool, MCP servers, the plugins. Unknown names are
        // warnings, never boot failures — see `apply_tools_config`.
        let tool_surface = apply_tools_config(
            tool_registry.as_ref(),
            &server_config
                .as_ref()
                .map(|c| c.tools.clone())
                .unwrap_or_default(),
        );
        let tool_surface_policy = tool_surface.policy();
        let agent_registry = Self::build_default_agent(
            &default_provider,
            &tool_registry,
            tool_surface_policy,
            default_max_iterations,
            default_strategy.clone(),
            default_tool_restriction.clone(),
            plugin_host.clone(),
        )
        .await;

        // Tool schemas travel on the completion request's
        // `tools` channel, not in the prompt—the runtime's
        // `ToolRegistry` is the source of truth and the live
        // snapshot is computed inside the ReAct loop on each
        // call. The prompt context here therefore only carries
        // skills + peer agents.

        // Build the prompt-context manifest from the workspace
        // and the live agent registry, excluding the canonical
        // ReAct agent itself (it must not appear in its own
        // handoff manifest). All skills are enabled.
        let prompt_context = Arc::new(
            build_prompt_context(&workspace_root, &agent_registry, "agent")
                .await,
        );

        // Multimodal attachment store (R11). Build before the
        // `Arc::new_cyclic` closure because `workspace_root`
        // moves into the struct initializer below.
        let attachments_dir = workspace_root.join(".attachments");
        let attachment_store = Arc::new(
            elysper::attachment::AttachmentStore::new_fs(&attachments_dir)
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        error = %e,
                        "failed to initialise attachments dir; using in-memory store"
                    );
                    elysper::attachment::AttachmentStore::default()
                })
                .with_clock(clock.clone()),
        );

        // Long-term memory: files under the workspace's nearest
        // scopes (`.elysper/memory-local` writes, `.elysper/memory`
        // as a checked-in overlay). A failed construction (path
        // guards) degrades to the in-memory tier rather than
        // blocking boot—same contract as every optional
        // integration above.
        let memory: Arc<dyn elysper::context::Memory> =
            match elysper::context::FileMemory::new(
                elysper::context::FileMemoryConfig::new(
                    &workspace_root,
                    "elysper",
                )
                .with_scopes(vec![
                    elysper::context::MemoryScope::Local,
                    elysper::context::MemoryScope::Project,
                ]),
            ) {
                Ok(m) => Arc::new(m),
                Err(e) => {
                    tracing::warn!(
                        error = %e,
                        "failed to open file memory; using in-memory tier"
                    );
                    Arc::new(elysper::context::InMemoryMemory::new())
                }
            };

        // The cross-domain search engine over six domains—the
        // same registry serves the model-facing `search` tool
        // (capability 1) and `GET /api/v1/search` (capability 2).
        let search =
            crate::search::SearchService::build(crate::search::SearchSources {
                tool_registry: Arc::clone(&tool_registry),
                agent_registry: Arc::clone(&agent_registry),
                workspace_root: workspace_root.clone(),
                memory: Arc::clone(&memory),
                session_search: Arc::clone(&session_search),
                clock: clock.clone(),
            });
        // Capability 1: the agent's discovery tool. Deferred
        // exposure—the cold-start tool list carries name +
        // description, and the real schema promotes on the first
        // call.
        if !elysper::tool_search::register_search_tool(
            &tool_registry,
            search.registry(),
        ) {
            tracing::warn!("failed to register the search tool: name shadowed");
        }

        // The four HTTP-backed management subsystems. Schedules,
        // workflows and evals all persist under the workspace
        // (schedules/ + workflows/ + evals/); boot loads them and
        // every mutation writes through.
        let schedules = SchedulesState::build(&workspace_root);
        let workflows = WorkflowsState::build(&workspace_root);
        let evals = EvalsState::build(&workspace_root);

        // Sub-agent session router over the same registry + cache
        // the HTTP surface uses: delegated children get sessions
        // exactly like user conversations (one code path for both).
        let active_sessions = Arc::new(crate::state::ActiveSessions::new());
        let subagent_router =
            Arc::new(crate::session::subagent_router::SubagentRouter::new(
                Arc::clone(&session_manager),
                Arc::clone(&active_sessions),
            ));
        // Workflow-runtime bridge over the same agent registry the
        // interactive routes resolve through—cloned here because
        // the struct literal below moves the original.
        let workflow_host = super::workflow_host::as_arc(
            super::workflow_host::ServerWorkflowHost::new(Arc::clone(
                &agent_registry,
            )),
        );
        // The cluster bus: `mode = "local"` (default) is the
        // in-process bus; `mode = "redis"` fails the boot when the
        // bus is unreachable, because a multi-node deployment that
        // cannot coordinate must not serve split-brain sessions.
        let cluster_config = server_config
            .as_ref()
            .map(|c| c.cluster.clone())
            .unwrap_or_default();
        let cluster =
            crate::cluster::ClusterRuntime::from_config(&cluster_config)
                .await
                .map_err(|e| format!("[cluster] {e}"))?;
        let cluster = Arc::new(cluster);
        // The state handle is bound before the self-management
        // registration below—its tools close over the Arc.
        let state = Arc::new_cyclic(|_weak| Self {
            clock,
            cluster,
            tool_registry,
            agent_registry,
            session_search,
            session_manager,
            workspace_root,
            default_model,
            workspace_config,
            default_provider,
            provider_catalogue: Arc::new(provider_catalogue),
            auth_config,
            cors_config,
            operations_config,
            system_prompt,
            prompt_context,
            server_config,
            default_max_iterations,
            default_compaction,
            default_strategy,
            default_tool_restriction,
            tool_surface,
            tool_defs_cache: parking_lot::RwLock::new(None),
            mcp_clients,
            mcp_health,
            plugin_host,
            plugins,
            active_sessions,
            spawn_gates: Arc::new(DashMap::new()),
            spawn_gate_timeout: super::sessions::COLD_OPEN_GATE_TIMEOUT,
            subagent_router,
            default_agent_name: parking_lot::RwLock::new(
                default_agent_name_value,
            ),
            usage_metrics: Arc::new(UsageMetrics::default()),
            attachment_store,
            memory,
            search,
            schedules,
            workflows,
            workflow_host,
            evals,
        });
        // The self-management MCP server needs the finished state
        // handle (its tools call back into it), so it registers
        // after construction—the shared registry makes the tools
        // visible to every later run.
        super::self_mcp::register_self_mcp(&state).await;
        // One process-wide control consumer: applies registry
        // mutations from peer nodes and forwards remote ops to the
        // local controller that owns the session.
        super::cluster_ops::spawn_control_dispatcher(Arc::clone(&state));
        // The dead-controller sweep behind the exit notification:
        // weak on the state, so it ends with it instead of
        // outliving it (the exit notification itself needs no
        // task — the controller's loop runs it).
        super::sessions::spawn_dead_session_reaper(Arc::downgrade(&state));
        // The background scheduler tick: one leader per deployment.
        super::schedules::spawn_scheduler_driver(Arc::clone(&state));
        Ok(state)
    }
}

/// Output of `AppState::load_per_section_config`: every
/// per-section config block the orchestrator hands to
/// `AppState::with_server_config`'s final `Arc::new_cyclic`
/// block. Private—the boot is the only caller.
struct PerSectionConfig {
    auth_config: Arc<AuthConfig>,
    cors_config: Arc<CorsConfig>,
    operations_config: Arc<crate::config::OperationEndpointConfig>,
    default_agent_name_value: Option<String>,
    default_max_iterations: usize,
    default_compaction: Option<elysper::session::CompactionSettings>,
    default_tool_restriction: Option<Arc<elysper::tool::ToolRestriction>>,
    default_strategy:
        Option<Arc<dyn elysper::harness::agent::ReasoningStrategy>>,
}

impl AppState {
    /// Read every per-section config block the agent table
    /// needs. The eight fields group by what the boot does
    /// with them: auth / CORS / operations go straight into
    /// `AppState`; `default_agent_name_value` and the four
    /// `load_default_*` readers wire into the default agent
    /// and every run the session factory builds.
    ///
    /// Each reader follows the same shape (`cfg?` →
    /// `agents.get(name)?` → field read → fallback). Grouping
    /// them keeps the orchestrator's cognitive complexity
    /// flat: the reader scans one list and one shape at a
    /// time, instead of repeating the `cfg?` chain eight
    /// times across the boot.
    fn load_per_section_config(
        server_config: &Option<crate::config::ServerConfig>,
    ) -> PerSectionConfig {
        let auth_config = Arc::new(
            server_config
                .as_ref()
                .map(|c| c.auth.clone())
                .unwrap_or_default(),
        );
        let cors_config = Arc::new(
            server_config
                .as_ref()
                .map(|c| c.cors.clone())
                .unwrap_or_default(),
        );
        let operations_config = Arc::new(
            server_config
                .as_ref()
                .map(|c| c.operations.clone())
                .unwrap_or_default(),
        );
        let default_agent_name_value =
            server_config.as_ref().and_then(|c| c.default_agent.clone());
        // Resolve the configured iteration cap from
        // `agents.<default>.max_steps` (clamped to `[1, 4096]`).
        // Falls back to the ReAct default (25) when no config
        // exists, no default agent is named, or the field is
        // unset on the chosen agent.
        let default_max_iterations = load_default_max_iterations(
            server_config.as_ref(),
            default_agent_name_value.as_deref(),
        )
        .unwrap_or(elysper::harness::DEFAULT_MAX_ITERATIONS);

        // R16: resolve the optional compaction policy from
        // `agents.<default>.compaction`. `None` keeps the loop's
        // default context manager.
        let default_compaction = load_default_compaction(
            server_config.as_ref(),
            default_agent_name_value.as_deref(),
        );

        // R58: resolve the agent's own allow/deny list from
        // `agents.<default>.{allowed_tools,denied_tools}`. An
        // empty pair means no restriction (every advertised
        // tool stays callable).
        let default_tool_restriction = load_default_tool_restriction(
            server_config.as_ref(),
            default_agent_name_value.as_deref(),
        );

        // R50: resolve the configured reasoning loop from
        // `agents.<default>.strategy`. An unknown name is
        // logged and treated as unset (ReAct)—a typo in one
        // agent's config must not stop the server from booting.
        let default_strategy = load_default_strategy(
            server_config.as_ref(),
            default_agent_name_value.as_deref(),
        );

        PerSectionConfig {
            auth_config,
            cors_config,
            operations_config,
            default_agent_name_value,
            default_max_iterations,
            default_compaction,
            default_tool_restriction,
            default_strategy,
        }
    }

    /// Build the multi-agent registry and register the default
    /// ReAct agent. Pulled out of `with_server_config` because
    /// the original was the largest single block in the boot—
    /// ~50 lines threading tool registry / provider / policy /
    /// restriction / strategy through the `ReActAgent::with_*`
    /// builder chain and four `if let Some` chains. In its own
    /// private fn the orchestrator stays at "wire → assemble".
    ///
    /// The deployment's host is passed in: the boot built its policy
    /// and composed the plugins into it, `AppState` keeps it, and this
    /// agent gets a clone — the same handle every run will get.
    #[allow(clippy::too_many_arguments)]
    async fn build_default_agent(
        default_provider: &Arc<dyn ModelProvider>,
        tool_registry: &Arc<ToolRegistry>,
        tool_surface_policy: Option<elysper::tool::ToolSurfacePolicy>,
        default_max_iterations: usize,
        default_strategy: Option<
            Arc<dyn elysper::harness::agent::ReasoningStrategy>,
        >,
        default_tool_restriction: Option<Arc<elysper::tool::ToolRestriction>>,
        plugin_host: elysper::harness::Host,
    ) -> Arc<AgentRegistry> {
        let agent_registry = Arc::new(AgentRegistry::new());
        let default_agent =
            crate::agent_assembly::AgentAssembly {
                provider: Arc::clone(default_provider),
                // The node's one registry — not a snapshot. A
                // plugin's tools are registered here at bind time,
                // and every per-run agent sees them because it binds
                // the same handle.
                tool_registry: Arc::clone(tool_registry),
                workspace_root: std::path::PathBuf::new(),
                descriptor: elysper::harness::default_descriptor(
                    elysper::harness::DEFAULT_SYSTEM_PROMPT.to_string(),
                ),
                prompt_context: Arc::new(
                    elysper::harness::PromptContext::default(),
                ),
                max_iterations: default_max_iterations,
                // The default member carries the delegation plugin so
                // `task` calls resolve the same catalog the session
                // factory builds. (Its prompt manifest is assembled
                // once `prompt_context` exists, after this call.)
                delegators: vec![Arc::new(TaskDelegator::new(Arc::clone(
                    &agent_registry,
                )))],
                tool_surface: tool_surface_policy,
                strategy: default_strategy,
                tool_restriction: default_tool_restriction,
                log_writer: None,
                subagent_writers: None,
                context_manager: None,
                compactor: None,
                host: Some(plugin_host),
            }
            .build();
        if let Err(e) = agent_registry
            .put(AgentEntry::new(Arc::new(default_agent)))
            .await
        {
            tracing::warn!(
                error = %e,
                "failed to register default agent; proceeding without it"
            );
        }
        agent_registry
    }
}

/// Compose the Tier-1 provider wraps in inner-to-outer
/// order on the default provider. Each wrap is opt-in and reads
/// its own config section; the function is a no-op when none of
/// the sections are enabled.
pub(crate) fn wrap_tier1_providers(
    inner: Arc<dyn ModelProvider>,
    config: Option<&crate::config::ServerConfig>,
    clock: elysper::core::SharedClock,
) -> Arc<dyn ModelProvider> {
    use elysper::{
        // The server is a tokio application: install the runtime's
        // sleeper so a retry backoff costs a timer entry, not the
        // OS thread the crate's `ThreadSleeper` default spends.
        harness_tokio::TokioSleeper,
        resilience::{
            BreakerConfig,
            Jitter,
            ResiliencePolicy,
            ResilientProvider,
        },
        search::Embedder,
        semantic_cache::{
            InMemorySemanticCache,
            SemanticCacheConfig,
            SemanticCacheProvider,
        },
    };
    let Some(cfg) = config else { return inner };

    // 1. Semantic cache (innermost, so the breaker sees real errors).
    let mut next: Arc<dyn ModelProvider> = if cfg.cache.enabled {
        let store: Arc<dyn elysper::semantic_cache::SemanticCacheStore> =
            Arc::new(InMemorySemanticCache::default());
        let embedder: Arc<dyn Embedder> =
            Arc::new(elysper::search::HashingEmbedder::new(
                64,
                Arc::new(elysper::search::CjkTokenizer),
            ));
        let clock_arc: Arc<dyn elysper::core::Clock> = Arc::new(clock.clone());
        Arc::new(SemanticCacheProvider::new(
            inner.clone(),
            store,
            embedder,
            SemanticCacheConfig::default()
                .threshold(cfg.cache.threshold)
                .ttl(std::time::Duration::from_secs(cfg.cache.ttl_secs)),
            clock_arc,
        ))
    } else {
        inner.clone()
    };

    // 2. Resilience (breaker + adaptive backoff).
    if cfg.resilience.enabled {
        let clock_arc: Arc<dyn elysper::core::Clock> = Arc::new(clock.clone());
        next = Arc::new(
            ResilientProvider::new(
                next,
                ResiliencePolicy::default()
                    .max_attempts(cfg.resilience.max_attempts)
                    .breaker(BreakerConfig {
                        failure_threshold: cfg.resilience.failure_threshold,
                        half_open_after: std::time::Duration::from_secs(
                            cfg.resilience.half_open_after_secs,
                        ),
                        success_threshold_in_half_open: 1,
                    })
                    .jitter(Jitter::Full),
                clock_arc,
            )
            .with_sleeper(Arc::new(TokioSleeper)),
        );
    }

    // 3. Budget (last gate; trips on token / cost / latency).
    if cfg.budget.enabled {
        use elysper::budget::{
            Budget,
            BudgetProvider,
            CostBudget,
            LatencyBudget,
            PricingTable,
            StaticPricingTable,
            TokenBudget,
        };
        let pricing_table = StaticPricingTable::anthropic_2026();
        let pricing: Arc<dyn PricingTable> = pricing_table;
        let clock_arc: Arc<dyn elysper::core::Clock> = Arc::new(clock.clone());
        let budget = Budget::builder()
            .tokens(TokenBudget::new(cfg.budget.tokens_limit))
            .latency(LatencyBudget::new(
                std::time::Duration::from_secs(cfg.budget.latency_secs),
                clock_arc,
            ))
            .cost(CostBudget::new(cfg.budget.cost_cents_limit, pricing))
            .build();
        next = Arc::new(BudgetProvider::new(next, budget));
    }

    next
}
