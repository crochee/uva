//! `AppState` construction: resolve the deployment config,
//! compose the tool surface, build the default agent, and
//! assemble every field. One concern—the boot.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

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
    skill::{SkillRegistry, seed_default_skills},
    tool::ToolRegistry,
    tool_task::TaskDelegator,
};

use super::{
    AppState,
    EvalsState,
    McpRuntime,
    ProviderCatalogue,
    SchedulesState,
    WorkflowsState,
    tools::plugin_tool_registry,
    usage::UsageMetrics,
};
use crate::{
    composition::kernel::{
        KernelHost,
        mount_profile,
        native_profile,
        scheduler_service,
    },
    profile::Profile,
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

/// Resolve the runtime [`WorkspaceConfig`] from the workspace's
/// `<workspace>/.agents/config.toml` — the transitional fallback input
/// for a deployment that has not moved its providers into a profile
/// row yet (the `model-adapter` plugin's config is the authoritative
/// source once it has one).
fn load_workspace_config(
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
        Self::with_server_config(workspace_root, server_config).await
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
        server_config: Option<crate::config::ServerConfig>,
    ) -> Result<Arc<Self>, String> {
        Self::with_server_config_and_profile(
            workspace_root,
            server_config,
            None,
        )
        .await
    }

    /// [`AppState::with_server_config`] with the CLI's resolved profile.
    ///
    /// `None` mounts the built-in native set. `Some` selects the
    /// profile's explicit `[core]` rows (a legacy-only document falls
    /// back to the built-in set; see
    /// rows) and projects its `[ui]` rows for `ui/schema`.
    pub async fn with_server_config_and_profile(
        workspace_root: PathBuf,
        server_config: Option<crate::config::ServerConfig>,
        resolved_profile: Option<Profile>,
    ) -> Result<Arc<Self>, String> {
        let workspace_config = load_workspace_config(&workspace_root)
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
            // The config.yaml provider section is no longer consulted:
            // providers come from the profile's `model-adapter` row or
            // `.agents/config.toml`. Silence here would leave an
            // operator's `[providers]` silently unread.
            if !config.providers.is_empty() {
                tracing::warn!(
                    providers = config.providers.len(),
                    "the server config's providers section is no longer \
                     read; run `elysper-app-server translate` to move it \
                     into a profile's model-adapter row"
                );
            }
        }

        // One clock for the whole process: every timestamped
        // component (session log rows, operation snapshots, the
        // attachment store, the MCP supervisor's tick) reads it from
        // here rather than calling the wall clock itself.
        let clock = elysper::core::SharedClock::system();

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
        // The built-in tool bricks. The agent-facing `skill` tool is
        // **not** registered here: the `skills` kernel plugin owns
        // that registration (token-scoped), so the tool and the
        // service are withdrawn together. Registering it twice would
        // make the kernel refuse the plugin's own `apply` — by
        // design.
        let tool_registry_built = plugin_tool_registry();
        // R21/R30: connect the configured MCP servers through the
        // supervisor and publish their tools. A server that fails
        // to start is logged (with its health state) and skipped—
        // boot must never be blocked by an optional integration.
        // The supervisor is **kept**: it owns the servers' child
        // processes and reconnects them with bounded backoff. The
        // previous boot dropped it on the floor, so a server that
        // died after the first tick never came back.
        let (mcp_supervisor, mcp_clients, mcp_health) =
            register_configured_mcp_servers(
                &tool_registry_built,
                clock.now(),
                &workspace_root,
            )
            .await;
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
        let mut plugins_config = server_config
            .as_ref()
            .map(|config| config.plugins.clone())
            .unwrap_or_default();
        // The plugin bundles' code components install through the
        // same channel as a `[plugins]` config entry — after them, so
        // a name the config already owns keeps the config's
        // authority and risk.
        plugins_config
            .plugins
            .extend(super::super::boot::bundle_plugin_entries(&workspace_root));
        let static_plugins = crate::composition::StaticPlugins::new();
        // The deployment's one host: the production policy
        // (guards / hints / tracker / logging hook) plus every
        // configured plugin. Every agent this server builds gets a
        // clone, so the policy applies to chat runs too.
        let mut plugin_host =
            elysper::harness::Host::default_policy(workspace_root.clone());
        let mut plugins = crate::composition::Composition::build(
            &plugins_config,
            &static_plugins,
            &tool_registry,
            &mut plugin_host,
        )
        .await;
        // Multimodal attachment store (R11). Built before the kernel
        // mount because the `attachments` plugin publishes exactly
        // this store; the routes and the chat input gate read the
        // service, not a second copy.
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

        // The peer registry exists before the mount: the `task`
        // delegator the `subagents` service publishes resolves names
        // through it, and the default member is registered into it
        // after the tool surface has settled.
        let agent_registry = Arc::new(AgentRegistry::new());

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
        // call. Registered before `[tools]` is applied so the
        // surface section can name it.
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
        // One eval engine for the service and the route surface: the
        // plugin publishes this runner, `EvalsState` drives it, so a
        // suite report does not depend on which entry point asked.
        let eval_runner = Arc::new(
            elysper::eval::EvalRunner::new()
                .metric(Box::new(elysper::eval::SyncMetricAdapter(
                    elysper::eval::KeywordMetric,
                )))
                .with_clock(clock.clone()),
        );
        let evals = EvalsState::build_with_runner(
            &workspace_root,
            Arc::clone(&eval_runner),
        );

        // The effective core assembly: the resolved profile when it
        // declares core rows, the built-in set otherwise — an empty
        // resolution (no profile document, no legacy rows) mounts the
        // native set, the same shape a zero-config deployment has
        // always booted with.
        let effective_profile = match &resolved_profile {
            Some(profile) if !profile.core.is_empty() => profile.clone(),
            _ => native_profile(),
        };

        // The model-adapter inputs, built once here for the pre-mount
        // consumers: the compactor samples a provider before any
        // plugin is mounted, and the host's fallback catalogue is
        // this one. The mounted `llm` / `llm.default` services are
        // the authoritative values — AppState reads them back from
        // the kernel below — and the factory rebuilds from the same
        // row config, so both see the same deployment.
        let model_adapter_config = model_adapter_row_config(
            &effective_profile,
            workspace_config.clone(),
        );
        let provider_catalogue = Arc::new(super::catalogue_for_deployment(
            &model_adapter_config.workspace,
        ));
        let default_model =
            model_adapter_config.workspace.default_model.clone();
        let default_provider: Arc<dyn ModelProvider> =
            provider_catalogue.default_handle().map_err(|e| {
                tracing::error!(error = %e, "Failed to create default provider");
                format!(
                    "No LLM provider available: {e}. Configure.agents/config.toml or set \
                     OPENAI_API_KEY/ANTHROPIC_API_KEY environment variables."
                )
            })?;
        let default_provider = elysper::runtime::wrap_tier1_providers(
            default_provider,
            &model_adapter_config,
            clock.clone(),
            Arc::new(elysper::harness_tokio::TokioSleeper),
        );

        // The capability services the native plugins publish. Each is
        // built from the object the rest of the server already owns.
        let skill_registry =
            Arc::new(SkillRegistry::discover_files(&workspace_root));
        let sandbox_backend: Arc<dyn elysper::tool_shell::SandboxBackend> =
            Arc::new(elysper::tool_shell::NoopBackend::new());
        let sandbox_policy = elysper::tool_shell::ExecutionPolicy::default();
        let mut compactor = elysper::session::SummaryCompactor::new(
            Arc::clone(&default_provider),
            default_model.clone(),
        );
        if let Some(settings) = default_compaction {
            compactor = compactor.with_settings(settings);
        }
        let compactor: Arc<dyn elysper::session::Compactor> =
            Arc::new(compactor);
        let scheduler_service = scheduler_service(Arc::clone(&schedules.store));
        let delegator =
            Arc::new(TaskDelegator::new(Arc::clone(&agent_registry)));

        let ui_schemas =
            crate::ui::schema::load_ui_manifest(&effective_profile.ui)
                .map_err(|error| error.to_string())?;

        // Mount the selected plugin set. `build_plugins` resolves every
        // row through the host-bound factory directory and `mount_all`
        // validates, orders, and mounts the group; the handles live on
        // `AppState` for the process lifetime (and for the HMR remount
        // loop).
        let kernel_host = Arc::new(KernelHost {
            workspace_root: workspace_root.clone(),
            provider_catalogue: Arc::clone(&provider_catalogue),
            tool_registry: Arc::clone(&tool_registry),
            session_registry: Arc::clone(&session_manager),
            sandbox_backend,
            sandbox_policy,
            delegator,
            compactor,
            approval_policy: elysper::harness::ApprovalPolicy::default(),
            mcp_supervisor: Arc::clone(&mcp_supervisor),
            skill_registry,
            eval_runner,
            scheduler_service,
            attachment_store: Arc::clone(&attachment_store),
            plugin_host: plugin_host.clone(),
            // The kernel entry, not the composition, owns the
            // installed plugins' lifetime from here: mounting the
            // `process-plugins` row adopts this set, and unloading it
            // withdraws the tools and kills the plugin processes.
            process_registrations: Mutex::new(Some(
                plugins.take_registrations(),
            )),
            // Transitional agent-loop inputs (die with the host):
            // the values the four assembly sites used to thread.
            default_strategy: default_strategy.clone(),
            default_tool_restriction: default_tool_restriction.clone(),
            compaction_boot: default_compaction
                .filter(|settings| settings.enabled)
                .filter(|settings| settings.validate().is_ok())
                .map(|settings| elysper::runtime::CompactionBoot {
                    settings,
                    sleeper: crate::spawner::server_sleeper(),
                    timeout: elysper::session::DEFAULT_SUMMARY_TIMEOUT,
                }),
            spawner: Some(crate::spawner::server_spawner()),
            delegators: {
                let delegator: Arc<dyn elysper::harness::DelegatingTool> =
                    Arc::new(TaskDelegator::new(Arc::clone(&agent_registry)));
                vec![delegator]
            },
        });
        let (kernel, mounted) = mount_profile(
            Arc::clone(&kernel_host),
            &effective_profile,
            clock.clone(),
        )
        .map_err(|error| error.to_string())?;

        // The `model-adapter` services are the authoritative provider
        // surface: `llm` carries the catalogue, `llm.default` the
        // tier-1-wrapped default handle. A config-driven profile row
        // replaces the pre-mount values read above; AppState keeps
        // reading its own fields, now sourced from the services.
        let provider_catalogue = kernel
            .require::<ProviderCatalogue>(elysper::provider::KEY_LLM)
            .map_err(|error| {
                format!("the llm service is unavailable after mount: {error}")
            })?;
        let default_provider = kernel
            .require::<Arc<dyn ModelProvider>>(
                elysper::provider::KEY_LLM_DEFAULT,
            )
            .map_err(|error| {
                format!(
                    "the llm.default service is unavailable after mount: \
                     {error}"
                )
            })
            .map(|wrapped| Arc::clone(&*wrapped))?;
        let workspace_config = provider_catalogue.workspace().clone();

        // The loop is the server's only agent builder; a profile that
        // omits the row must fail here rather than run every session
        // without the deployment's strategy / restriction / compactor.
        kernel
            .require::<Arc<dyn elysper::harness::plugin::AgentLoopFactory>>(
                elysper::harness::plugin::KEY_AGENT_LOOP,
            )
            .map_err(|error| {
                format!(
                    "the agentLoop service is unavailable after mount: \
                     {error}; the profile must include the `agent-loop` row"
                )
            })?;
        let profile_snapshot = effective_profile.clone();

        // The supervisor's reconnect loop is the boot's to drive: one
        // tick resolves the `mcp` service, so unloading the plugin
        // stops the loop (the next `get` fails) and the explicit
        // teardown path withdraws the generations.
        let mcp = McpRuntime::start(
            kernel.clone(),
            Arc::clone(&tool_registry),
            Arc::clone(&mcp_supervisor),
            mcp_clients,
            mcp_health,
            clock.clone(),
        );

        // R34 (Task 16): apply the deployment's `[tools]` surface after
        // every registration source has had its say — builtins, the
        // mounted `skills` / `mcp` / `schedule-tool` plugins, and the
        // composition's host plugins. Unknown names are warnings,
        // never boot failures — see `apply_tools_config`.
        let tool_surface = apply_tools_config(
            tool_registry.as_ref(),
            &server_config
                .as_ref()
                .map(|c| c.tools.clone())
                .unwrap_or_default(),
        );
        Self::build_default_agent(
            &kernel,
            &default_provider,
            &tool_registry,
            tool_surface.policy(),
            default_max_iterations,
            plugin_host.clone(),
            Arc::clone(&agent_registry),
        )
        .await;

        // Tool schemas travel on the completion request's `tools`
        // channel, not in the prompt. The prompt context carries
        // skills (read from the `skills` service) + peer agents.
        let prompt_context = Arc::new(
            build_prompt_context(&kernel, &agent_registry, "agent").await,
        );

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
            workspace_config: parking_lot::RwLock::new(workspace_config),
            default_provider: parking_lot::RwLock::new(default_provider),
            provider_catalogue: parking_lot::RwLock::new(Arc::clone(
                &provider_catalogue,
            )),
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
            kernel,
            kernel_host,
            mount: parking_lot::Mutex::new(vec![mounted]),
            profile: parking_lot::RwLock::new(profile_snapshot),
            ui_schemas: parking_lot::RwLock::new(ui_schemas),
            mcp,
            plugin_host,
            #[cfg(any(test, feature = "test-utils"))]
            provider_override: parking_lot::Mutex::new(None),
            #[cfg(any(test, feature = "test-utils"))]
            catalogue_override: parking_lot::Mutex::new(None),
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
            rpc: Arc::new(crate::rpc::RpcRuntime::new()),
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
        // The layout's definitions become live agents: the files
        // are the storage, the registry is the runtime's view of
        // them. Registered here — after the state exists (the
        // builder closes over it), before the scheduler starts (a
        // task's agent target must resolve by then).
        super::super::boot::register_file_agents(&state);
        // `tasks/<name>/task.md` declarations sync into the store by
        // name; an active store entry with no declaration is
        // exported back to a file, so the store never holds state
        // the layout cannot show.
        super::tasks::sync_task_declarations(&state);
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
            operations_config,
            default_agent_name_value,
            default_max_iterations,
            default_compaction,
            default_tool_restriction,
            default_strategy,
        }
    }

    /// Register the default ReAct agent into the shared registry.
    /// Pulled out of `with_server_config` because
    /// the original was the largest single block in the boot—
    /// ~50 lines threading tool registry / provider / policy /
    /// restriction / strategy through the `ReActAgent::with_*`
    /// builder chain and four `if let Some` chains. In its own
    /// private fn the orchestrator stays at "wire → assemble".
    ///
    /// The deployment's host is passed in: the boot built its policy
    /// and composed the plugins into it, `AppState` keeps it, and this
    /// agent gets a clone — the same handle every run will get. The
    /// registry comes in too: the `subagents` plugin's delegator was
    /// built over it before the mount, so the boot creates it once and
    /// this call only registers the default member.
    #[allow(clippy::too_many_arguments)]
    async fn build_default_agent(
        kernel: &elysper::kernel::Context,
        default_provider: &Arc<dyn ModelProvider>,
        tool_registry: &Arc<ToolRegistry>,
        tool_surface_policy: Option<elysper::tool::ToolSurfacePolicy>,
        default_max_iterations: usize,
        plugin_host: elysper::harness::Host,
        agent_registry: Arc<AgentRegistry>,
    ) -> Arc<AgentRegistry> {
        // The default member is built through the mounted
        // `agentLoop` service, exactly like a session run: the
        // factory's defaults (delegators, strategy, restriction,
        // context manager, spawner, compactor) apply, and only the
        // member's own inputs are set here.
        let factory = kernel
            .require::<Arc<dyn elysper::harness::plugin::AgentLoopFactory>>(
                elysper::harness::plugin::KEY_AGENT_LOOP,
            )
            .map(|wrapped| Arc::clone(&*wrapped));
        let default_agent = match factory {
            Ok(factory) => {
                factory.create(&elysper::harness::plugin::RunConfig {
                    provider: Arc::clone(default_provider),
                    // The node's one registry — not a snapshot. A
                    // plugin's tools are registered here at bind
                    // time, and every per-run agent sees them
                    // because it binds the same handle.
                    tool_registry: Arc::clone(tool_registry),
                    workspace_root: std::path::PathBuf::new(),
                    descriptor: elysper::harness::default_descriptor(
                        elysper::harness::DEFAULT_SYSTEM_PROMPT.to_string(),
                    ),
                    prompt_context: Arc::new(
                        elysper::harness::PromptContext::default(),
                    ),
                    max_iterations: default_max_iterations,
                    delegators: Vec::new(),
                    tool_surface: tool_surface_policy,
                    strategy: None,
                    tool_restriction: None,
                    log_writer: None,
                    subagent_writers: None,
                    context_manager: None,
                    compactor: None,
                    host: Some(plugin_host),
                    spawner: None,
                })
            }
            Err(error) => {
                tracing::warn!(
                    %error,
                    "the agentLoop service is unavailable; the default \
                     member is not registered"
                );
                return agent_registry;
            }
        };
        if let Err(e) = agent_registry.put(AgentEntry::new(default_agent)).await
        {
            tracing::warn!(
                error = %e,
                "failed to register default agent; proceeding without it"
            );
        }
        agent_registry
    }
}

/// The `model-adapter` assembly input: the profile row's config when
/// it carries one, the file-loaded workspace with no tier-1
/// otherwise.
///
/// A malformed row config falls back like an absent one — the
/// factory's build reports the parse failure when the profile
/// mounts, which is the authoritative error surface.
fn model_adapter_row_config(
    profile: &crate::profile::Profile,
    workspace: elysper::provider::config::WorkspaceConfig,
) -> elysper::runtime::ModelAdapterConfig {
    let Some(row) = profile.core.iter().find(|e| e.id == "model-adapter")
    else {
        return elysper::runtime::ModelAdapterConfig {
            workspace,
            ..Default::default()
        };
    };
    let empty = row.config.as_object().is_none_or(serde_json::Map::is_empty);
    if empty {
        return elysper::runtime::ModelAdapterConfig {
            workspace,
            ..Default::default()
        };
    }
    serde_json::from_value(row.config.clone()).unwrap_or_else(|error| {
        tracing::warn!(
            error = %error,
            "model-adapter row config is malformed; the mount reports it"
        );
        elysper::runtime::ModelAdapterConfig {
            workspace,
            ..Default::default()
        }
    })
}
