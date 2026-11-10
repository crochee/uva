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
    skill::seed_default_skills,
    tool::ToolRegistry,
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
    composition::kernel::{BootServices, mount_profile, native_profile},
    profile::Profile,
    state::boot::{
        apply_tools_config,
        load_default_compaction,
        load_default_max_iterations,
        load_default_strategy_alias,
        load_default_tool_restriction,
        register_configured_mcp_servers,
        warn_unapplied_agent_keys,
    },
};

/// Resolve the runtime [`WorkspaceConfig`] from the workspace's
/// `<workspace>/.agents/config.toml`.
///
/// The `model-adapter` row's config is authoritative: a profile that
/// carries the row wins, and the boot materializes the row from this
/// workspace configuration when the profile leaves it empty.
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
        profile_store: Option<crate::profile::ProfileStore>,
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
        // The shared tool container: the `tool-registry` row
        // publishes it; the bricks, the mounted plugins and the
        // composition register into it.
        let tool_registry = Arc::new(plugin_tool_registry());

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
        // The peer registry exists before the mount: the `task`
        // delegator the `subagents` service publishes resolves names
        // through it, and the default member is registered into it
        // after the tool surface has settled.
        let agent_registry = Arc::new(AgentRegistry::new());

        // Workflows persist under the workspace; schedules and evals
        // are read back from the services their rows publish after
        // the mount.
        let workflows = WorkflowsState::build(&workspace_root);

        // The effective core assembly: the resolved profile when it
        // declares core rows, the built-in set otherwise — an empty
        // resolution (no profile document, no legacy rows) mounts the
        // native set, the same shape a zero-config deployment has
        // always booted with.
        let mut effective_profile = match &resolved_profile {
            Some(profile) if !profile.core.is_empty() => profile.clone(),
            _ => native_profile(),
        };

        // The model-adapter config, resolved once: the mount
        // materializes it into the row when the profile leaves the
        // row's config empty, so the plugin builds the deployment's
        // catalogue itself.
        let model_adapter_config = model_adapter_row_config(
            &effective_profile,
            workspace_config.clone(),
        );
        let default_model =
            model_adapter_config.workspace.default_model.clone();
        // The factory builds from the row config only; an empty row
        // means "the deployment's workspace configuration".
        let model_adapter_config = serde_json::to_value(&model_adapter_config)
            .map_err(|error| {
                format!(
                    "the model-adapter row config does not serialize: {error}"
                )
            })?;
        materialize_row_config(
            &mut effective_profile,
            "model-adapter",
            model_adapter_config,
        );

        // The `agent-loop` row's config, materialized from the
        // deployment's agent table: the strategy **alias**, the
        // allow/deny tool lists, and the compaction policy. The row
        // builds its own loop from these, so the boot hands the row a
        // config instead of a pre-built strategy object.
        let agent_loop_config = agent_loop_row_config(
            server_config.as_ref(),
            default_agent_name_value.as_deref(),
            default_tool_restriction.as_deref(),
            default_compaction,
        );
        materialize_row_config(
            &mut effective_profile,
            "agent-loop",
            agent_loop_config,
        );

        // Mount the selected plugin set. `build_plugins` resolves every
        // row through the factory directory and `mount_all`
        // validates, orders, and mounts the group; the handles live on
        // `AppState` for the process lifetime (and for the HMR remount
        // loop).
        //
        // What the boot still owns are the **containers**: the one
        // tool registry, the peer-agent registry, the harness host,
        // and the `[plugins]` composition's registrations. Every
        // capability — the delegation backend, the approval stance,
        // the session index, the loop's defaults — is built by the
        // row that publishes it.
        let boot_services = Arc::new(BootServices {
            tool_registry: Arc::clone(&tool_registry),
            agent_registry: Arc::clone(&agent_registry),
            plugin_host: plugin_host.clone(),
            // The kernel entry, not the composition, owns the
            // installed plugins' lifetime from here: mounting the
            // `process-plugins` row adopts this set, and unloading it
            // withdraws the tools and kills the plugin processes.
            process_registrations: Mutex::new(Some(
                plugins.take_registrations(),
            )),
        });
        let (kernel, mounted) = mount_profile(
            Arc::clone(&boot_services),
            &effective_profile,
            clock.clone(),
            crate::composition::kernel::BootExtras {
                workspace_root: workspace_root.clone(),
                blocking: Some(crate::spawner::server_blocking()),
                compaction: Some(elysper::session::CompactionPluginConfig {
                    model: default_model.clone(),
                    settings: default_compaction.unwrap_or_default(),
                }),
                sessions_dir: None,
            },
        )
        .map_err(|error| error.to_string())?;

        // The effective assembly the mounted tree corresponds to: the
        // enabled rows only. The boot passes the raw (materialized)
        // profile to `mount_profile`, which filters and mounts it in two
        // waves; recomputing the same filter here is idempotent and is
        // what the HMR diff and the settings page diff against.
        let assembly = {
            let checks = crate::profile::KernelChecks(&kernel);
            crate::profile::effective(&effective_profile, &checks).profile
        };
        // The UI segment is assembled from the effective rows, so a
        // `ui-bundle` row switched off never reaches the loader.
        let ui_tree = crate::ui::tree::load_ui_tree(&assembly.ui)
            .map_err(|error| error.to_string())?;

        // The cross-domain search service is the `search` profile
        // row: its plugin builds the six-domain service from the
        // injected `tools`/`memory` services and registers the
        // model-facing `search` tool.

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
        // The capability services the rows published. The boot only
        // resolves them for the projection layer (`AppState` fields,
        // routes) — construction happened inside each plugin's
        // `apply`.
        let tool_registry = kernel
            .require::<elysper::tool::ToolRegistry>(elysper::tool::KEY_TOOLS)
            .map_err(|error| {
                format!("the tools service is unavailable after mount: {error}")
            })?;
        let session_manager = kernel
            .require::<SessionRegistry>(elysper::session::KEY_SESSIONS)
            .map_err(|error| {
                format!(
                    "the sessions service is unavailable after mount: {error}"
                )
            })?;
        let attachment_store = kernel
            .require::<elysper::attachment::AttachmentStore>(
                elysper::attachment::KEY_ATTACHMENTS,
            )
            .map_err(|error| {
                format!(
                    "the attachments service is unavailable after mount: \
                     {error}"
                )
            })?;
        let eval_runner = kernel
            .require::<elysper::eval::EvalRunner>(elysper::eval::KEY_EVAL)
            .map_err(|error| {
                format!("the eval service is unavailable after mount: {error}")
            })?;
        let scheduler_service = kernel
            .require::<elysper::scheduler::SchedulerService>(
                elysper::scheduler::KEY_SCHEDULER,
            )
            .map_err(|error| {
                format!(
                    "the scheduler service is unavailable after mount: \
                     {error}"
                )
            })?;
        let mcp_supervisor = kernel
            .require::<elysper::mcp::McpSupervisor>(elysper::mcp::KEY_MCP)
            .map_err(|error| {
                format!("the mcp service is unavailable after mount: {error}")
            })?;

        // R21/R30: connect the configured MCP servers through the
        // mounted supervisor; a server that fails to start is logged
        // with its health state and skipped.
        let (mcp_clients, mcp_health) = register_configured_mcp_servers(
            Arc::clone(&mcp_supervisor),
            tool_registry.as_ref(),
            clock.now(),
            &workspace_root,
        )
        .await;
        // Smaller per-request load: MCP tools advertise name +
        // description (`Deferred`) and promote their schema on the
        // first call. Only `Direct` flips, so an operator's explicit
        // verdict stands.
        for d in tool_registry.descriptors().iter().filter(|d| {
            d.name.starts_with("mcp__")
                && d.exposure == elysper::tool::ToolExposure::Direct
        }) {
            tool_registry
                .set_exposure(&d.name, elysper::tool::ToolExposure::Deferred);
        }

        // The route surfaces read the same store/engine the plugins
        // published: one schedule store, one eval runner.
        let schedules = SchedulesState::from_store(
            Arc::clone(scheduler_service.store()),
            clock.clone(),
        );
        let evals = EvalsState::build_with_runner(
            &workspace_root,
            Arc::clone(&eval_runner),
        );

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
        // channel, not in the prompt. The prompt manifest is a live
        // service snapshot (`AppState::prompt_context`), rebuilt per
        // session/agent build so a skills remount reaches the next
        // build.

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
            server_config,
            default_max_iterations,
            default_compaction,
            tool_surface,
            tool_defs_cache: parking_lot::RwLock::new(None),
            kernel,
            boot_services,
            mount: parking_lot::Mutex::new(mounted.into_groups()),
            profile: parking_lot::RwLock::new(profile_snapshot),
            assembly: parking_lot::RwLock::new(assembly),
            profile_store,
            profile_write: tokio::sync::Mutex::new(()),
            ui_tree: parking_lot::RwLock::new(ui_tree),
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
        super::super::boot::register_file_agents(&state).await;
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

        PerSectionConfig {
            operations_config,
            default_agent_name_value,
            default_max_iterations,
            default_compaction,
            default_tool_restriction,
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
pub(crate) fn model_adapter_row_config(
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

/// Materialize a row's config when the profile leaves it empty.
///
/// The **profile wins**: a row that already carries a config keeps it,
/// so a deployment can pin a row's configuration without editing the
/// server config. An empty row (`null` or `{}`) takes the boot's
/// materialized value.
pub(crate) fn materialize_row_config(
    profile: &mut crate::profile::Profile,
    id: &str,
    config: serde_json::Value,
) {
    let Some(row) = profile.core.iter_mut().find(|entry| entry.id == id) else {
        return;
    };
    let empty = match &row.config {
        serde_json::Value::Null => true,
        serde_json::Value::Object(map) => map.is_empty(),
        _ => false,
    };
    if empty {
        row.config = config;
    }
}

/// The `agent-loop` assembly input: the deployment's agent table,
/// materialized into the row's `config`.
///
/// The row owns its loop, so the boot hands the row the **values** the
/// agent table carries, not pre-built objects: the strategy alias (the
/// row resolves it), the allow/deny tool lists, and the compaction
/// policy. An absent value is `null` — "the deployment configured
/// nothing", which is not the same as an empty value.
///
/// The profile wins: the boot only fills a row whose config is empty.
pub(crate) fn agent_loop_row_config(
    server_config: Option<&crate::config::ServerConfig>,
    default_agent_name: Option<&str>,
    restriction: Option<&elysper::tool::ToolRestriction>,
    compaction: Option<elysper::session::CompactionSettings>,
) -> serde_json::Value {
    let strategy =
        load_default_strategy_alias(server_config, default_agent_name);
    let tool_restriction = restriction.map(|restriction| {
        serde_json::to_value(restriction).unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                "the agent-loop row's tool restriction does not \
                 serialize; the row keeps no restriction"
            );
            serde_json::Value::Null
        })
    });
    let compaction = compaction.map(|settings| {
        serde_json::to_value(settings).unwrap_or_else(|error| {
            tracing::warn!(
                error = %error,
                "the agent-loop row's compaction settings do not \
                 serialize; the row keeps no compactor"
            );
            serde_json::Value::Null
        })
    });
    serde_json::json!({
        "strategy": strategy,
        "tool_restriction": tool_restriction,
        "compaction": compaction,
    })
}
