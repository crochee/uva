//! `AppState` construction: resolve the deployment config,
//! compose the tool surface, build the default agent, and
//! assemble every field. One concern—the boot.

use std::{path::PathBuf, sync::Arc};

use dashmap::DashMap;
use uva::{
    agent::{AgentEntry, AgentRegistry},
    core::{Clock, registry::Registry},
    provider::{config::WorkspaceConfig, traits::ModelProvider},
    session::{
        JsonlSessionSearch,
        SharedSessionSearch,
        manager::SessionRegistry,
    },
    skill::seed_default_skills,
    tool::ToolRegistry,
};

#[cfg(any(test, feature = "test-utils"))]
use super::tools::AppliedToolSurface;
use super::{
    AppState,
    Assembly,
    EvalsState,
    McpRuntime,
    ProviderCatalogue,
    SchedulesState,
    tools::plugin_tool_registry,
    usage::UsageMetrics,
};
use crate::{
    composition::kernel::{BootServices, mount_profile, native_profile},
    profile::{Profile, ProfileStore},
    state::boot::{
        agent_loop_row_config,
        apply_tools_config,
        load_default_compaction,
        load_default_max_iterations,
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

/// The provider surface the assembler installs after the mount.
///
/// The two constructors differ here and only here on providers: the
/// deployment reads the services its `model-adapter` row published, the
/// fixture installs the in-memory fake it built.
pub(crate) enum ProviderSeam {
    /// Read the mounted `model-adapter` row's services: the catalogue
    /// (`llm`) and the tier-1-wrapped default handle (`llm.default`).
    Mounted,
    /// Install the caller's catalogue, default handle, and workspace
    /// configuration. The fixture builds all three around its fake so
    /// `model: None` resolves in a test.
    #[cfg(any(test, feature = "test-utils"))]
    Injected {
        /// The catalogue every resolution path reads.
        catalogue: Arc<ProviderCatalogue>,
        /// The default handle `model: None` resolves to.
        default_provider: Arc<dyn ModelProvider>,
        /// The workspace the injected catalogue was built from.
        workspace: WorkspaceConfig,
    },
}

/// The lifecycle extras a boot installs around the mount.
///
/// Everything else the two constructors differ on is a value in
/// [`BootInputs`]; these are the *actions* only the deployment performs.
#[derive(Clone, Copy)]
pub(crate) enum BootMode {
    /// The deployment: connect the configured MCP servers through the
    /// mounted supervisor and drive its reconnect loop, apply the
    /// `[tools]` config to the registry, and register the default
    /// member through the mounted loop.
    Deployment,
    /// The fixture: no configured servers, no host policy, and its own
    /// canonical descriptor registered before the call. The tool
    /// surface stays empty.
    #[cfg(any(test, feature = "test-utils"))]
    Fixture,
}

/// The inputs the shared assembler needs.
///
/// One field per thing the deployment boot and the fixture resolve
/// differently. The assembler owns everything they resolve the same
/// way: the clock, the mount, the service reads, the session router,
/// and the state literal.
pub(crate) struct BootInputs {
    /// The workspace the rows build their files from.
    pub(crate) workspace_root: PathBuf,
    /// The resolved deployment configuration, kept on the state.
    pub(crate) server_config: Option<crate::config::ServerConfig>,
    /// The materialized profile document to mount. The fixture's
    /// `model-adapter` row carries its demo provider; the deployment's
    /// rows carry what the boot materialized from the config.
    pub(crate) profile: Profile,
    /// The transactional write path's document store; `None` in
    /// fixtures.
    pub(crate) profile_store: Option<ProfileStore>,
    /// The default model name the loop and the compaction plugin read.
    pub(crate) default_model: String,
    /// Where the provider surface comes from.
    pub(crate) providers: ProviderSeam,
    /// The one tool registry, before the mount publishes it.
    pub(crate) tool_registry: Arc<ToolRegistry>,
    /// The one peer-agent registry, before the mount reads it.
    pub(crate) agent_registry: Arc<AgentRegistry>,
    /// The one harness host; the deployment's carries the policy, the
    /// fixture's binds its registry and composes no plugins.
    pub(crate) plugin_host: uva::harness::Host,
    /// The session search service over `<workspace>/sessions`.
    pub(crate) session_search: SharedSessionSearch,
    /// The session-store row's directory; `None` keeps the row's
    /// default.
    pub(crate) sessions_dir: Option<PathBuf>,
    /// The default compaction policy; also the mount's compaction
    /// input.
    pub(crate) default_compaction: Option<uva::session::CompactionSettings>,
    /// The operations endpoints the config resolved.
    pub(crate) operations_config: Arc<crate::config::OperationEndpointConfig>,
    /// The configured default agent name.
    pub(crate) default_agent_name: Option<String>,
    /// The per-run iteration cap.
    pub(crate) default_max_iterations: usize,
    /// The cluster bus the deployment resolved from its config.
    pub(crate) cluster: Arc<crate::cluster::ClusterRuntime>,
    /// The RPC runtime: connected, or detached for a fixture.
    pub(crate) rpc: Arc<crate::rpc::RpcRuntime>,
    /// Which lifecycle extras to install.
    pub(crate) mode: BootMode,
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
    /// compute the bind address from `--host`/`UVA_HOST`/`host`
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
                     read; run `uva-app-server translate` to move it \
                     into a profile's model-adapter row"
                );
            }
        }

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
        // The deployment's one interception table: built before the
        // host, written by the default policy, and published by the
        // `hooks` row (ADR 0028).
        let hook_registry = uva::hook::HookRegistry::new();
        // The deployment's one host: the production policy
        // (guards / hints / tracker / logging hook). Every agent this
        // server builds gets a clone, so the policy applies to chat
        // runs too, and every clone dispatches this same table.
        let plugin_host = uva::harness::Host::default_policy(
            workspace_root.clone(),
            hook_registry.clone(),
        );
        // The peer registry exists before the mount: the `tool-task`
        // row's `task` tool resolves names through it, and the default
        // member is registered into it after the tool surface has
        // settled.
        let agent_registry = Arc::new(AgentRegistry::new());

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
            model_adapter_config.catalogue.default_model.clone();
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

        // The assembler mounts the selected plugin set, resolves the
        // services the rows published, and assembles the state. The
        // deployment's inputs are the ones the pre-steps above
        // resolved; the lifecycle extras only a deployment installs
        // (configured MCP servers, the `[tools]` surface, the loop's
        // default member) ride on [`BootMode::Deployment`].
        let cluster_config = server_config
            .as_ref()
            .map(|c| c.cluster.clone())
            .unwrap_or_default();
        let cluster = Arc::new(
            crate::cluster::ClusterRuntime::from_config(&cluster_config)
                .await
                .map_err(|e| format!("[cluster] {e}"))?,
        );
        // The state handle is bound before the self-management
        // registration below—its tools close over the Arc.
        let state = Arc::new(
            Self::assemble(BootInputs {
                workspace_root,
                server_config,
                profile: effective_profile,
                profile_store,
                default_model,
                providers: ProviderSeam::Mounted,
                tool_registry,
                agent_registry,
                plugin_host,
                session_search,
                sessions_dir: None,
                default_compaction,
                operations_config,
                default_agent_name: default_agent_name_value,
                default_max_iterations,
                cluster,
                rpc: Arc::new(crate::rpc::RpcRuntime::new()),
                mode: BootMode::Deployment,
            })
            .await?,
        );
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

    /// Mount `input.profile`, resolve the services its rows published,
    /// and assemble the state.
    ///
    /// This is the one construction path: [`AppState::new`] and
    /// [`AppState::for_test`] both land here, so a field added to the
    /// state cannot be wired in one boot and forgotten in the other.
    /// Everything they resolve differently arrives in [`BootInputs`];
    /// everything they resolve the same way — the clock, the mount,
    /// the provider resolution, the session router, the literal — is
    /// owned here.
    ///
    /// [`AppState::new`]: Self::new
    /// [`AppState::for_test`]: Self::for_test
    pub(crate) async fn assemble(input: BootInputs) -> Result<Self, String> {
        // One clock for the whole process: every timestamped
        // component (session log rows, operation snapshots, the
        // attachment store, the MCP supervisor's tick) reads it from
        // here rather than calling the wall clock itself.
        let clock = uva::core::SharedClock::system();

        // Sub-agent session state: the router the controller/persist
        // path uses and the writer slot the `tool-task` row reads. The
        // slot is empty until the mount publishes `sessions`; nothing
        // opens a child before boot finishes.
        let active_sessions = Arc::new(crate::state::ActiveSessions::new());
        let subagent_writer_slot = Arc::new(
            crate::session::subagent_router::SubagentWriterSlot::new(),
        );
        // The control surface's view of the sessions: empty until the
        // mount publishes `sessions`, exactly like the writer slot.
        let session_index_slot =
            Arc::new(crate::session::session_index::SessionIndexSlot::new());

        // The containers the mount hands the rows: the one tool
        // registry, the peer-agent registry, the harness host and the
        // child-session writer slot. Every capability — the `task`
        // tool brick, the approval stance, the session index, the
        // loop's defaults — is built by the row that publishes it.
        // One set of counters for the whole process, created before
        // the mount: the `tool-task` row reports delegated children
        // into the same atomics the session controllers write, and
        // `AppState` publishes them to `/api/v1/chat/usage`.
        let usage_metrics = Arc::new(UsageMetrics::default());
        let boot_services = Arc::new(BootServices {
            tool_registry: Arc::clone(&input.tool_registry),
            agent_registry: Arc::clone(&input.agent_registry),
            subagent_writers: Arc::clone(&subagent_writer_slot),
            session_index: Arc::clone(&session_index_slot),
            usage: Arc::clone(&usage_metrics),
            hook_registry: input.plugin_host.hook_registry().clone(),
            plugin_host: input.plugin_host.clone(),
        });
        // The one build recipe: the boot's mount and every hot reload
        // the assembly later performs build rows from these same
        // extras, so a remounted row is built the way the boot built
        // it.
        let extras = crate::composition::kernel::BootExtras {
            workspace_root: input.workspace_root.clone(),
            blocking: Some(crate::spawner::server_blocking()),
            compaction: Some(uva::session::CompactionPluginConfig {
                model: input.default_model.clone(),
                settings: input.default_compaction.unwrap_or_default(),
            }),
            sessions_dir: input.sessions_dir.clone(),
        };
        let (kernel, mounted) = mount_profile(
            Arc::clone(&boot_services),
            &input.profile,
            clock.clone(),
            extras.clone(),
        )
        .map_err(|error| error.to_string())?;

        // The effective assembly the mounted tree corresponds to: the
        // assembled rows only. `mount_profile` filters and mounts the
        // raw profile; recomputing the same filter here is idempotent
        // and is what the HMR diff and the settings page diff against.
        let assembly = crate::profile::effective(&input.profile);
        // The UI segment is assembled from the effective rows, so a
        // `ui-bundle` row switched off never reaches the loader.
        let ui_tree = crate::ui::tree::load_ui_tree(&assembly.ui)
            .map_err(|error| error.to_string())?;

        // The `model-adapter` services are the authoritative provider
        // surface: `llm` carries the catalogue, `llm.default` the
        // tier-1-wrapped default handle. The fixture installs the
        // catalogue it built instead.
        let (provider_catalogue, default_provider, workspace_config) =
            match input.providers {
                ProviderSeam::Mounted => {
                    let catalogue = kernel
                        .require::<ProviderCatalogue>(uva::provider::KEY_LLM)
                        .map_err(|error| {
                            format!(
                                "the llm service is unavailable after \
                                 mount: {error}"
                            )
                        })?;
                    let provider = kernel
                        .require::<Arc<dyn ModelProvider>>(
                            uva::provider::KEY_LLM_DEFAULT,
                        )
                        .map_err(|error| {
                            format!(
                                "the llm.default service is unavailable \
                                 after mount: {error}"
                            )
                        })
                        .map(|wrapped| Arc::clone(&*wrapped))?;
                    let workspace = catalogue.workspace().clone();
                    (catalogue, provider, workspace)
                }
                #[cfg(any(test, feature = "test-utils"))]
                ProviderSeam::Injected {
                    catalogue,
                    default_provider,
                    workspace,
                } => (catalogue, default_provider, workspace),
            };
        // The capability services the rows published. They are
        // resolved for the projection layer (`AppState` fields,
        // routes) — construction happened inside each plugin's
        // `apply`.
        let tool_registry = kernel
            .require::<uva::tool::ToolRegistry>(uva::tool::KEY_TOOLS)
            .map_err(|error| {
                format!("the tools service is unavailable after mount: {error}")
            })?;
        let session_manager = kernel
            .require::<SessionRegistry>(uva::session::KEY_SESSIONS)
            .map_err(|error| {
                format!(
                    "the sessions service is unavailable after mount: {error}"
                )
            })?;
        // The mount published `sessions`: build the child-session
        // router over it, then hand its writer source to the already
        // mounted `tool-task` tool through the slot.
        let subagent_router =
            Arc::new(crate::session::subagent_router::SubagentRouter::new(
                Arc::clone(&session_manager),
            ));
        subagent_writer_slot.set(Arc::new(
            crate::session::subagent_router::SessionSubagentWriters::new(
                Arc::clone(&subagent_router),
                Arc::clone(&session_manager),
            ),
        ));
        // The control surface reads the same sessions: the roster is
        // rebuilt from their logs, and a cold wake writes the woken
        // child's next round into its own session.
        session_index_slot.set(Arc::new(
            crate::session::session_index::ServerSessionIndex::new(Arc::clone(
                &session_manager,
            )),
        ));
        let attachment_store = kernel
            .require::<uva::attachment::AttachmentStore>(
                uva::attachment::KEY_ATTACHMENTS,
            )
            .map_err(|error| {
                format!(
                    "the attachments service is unavailable after mount: \
                     {error}"
                )
            })?;
        let eval_runner = kernel
            .require::<uva::eval::EvalRunner>(uva::eval::KEY_EVAL)
            .map_err(|error| {
                format!("the eval service is unavailable after mount: {error}")
            })?;
        let scheduler_service = kernel
            .require::<uva::scheduler::SchedulerService>(
                uva::scheduler::KEY_SCHEDULER,
            )
            .map_err(|error| {
                format!(
                    "the scheduler service is unavailable after mount: \
                     {error}"
                )
            })?;
        let mcp_supervisor = kernel
            .require::<uva::mcp::McpSupervisor>(uva::mcp::KEY_MCP)
            .map_err(|error| {
                format!("the mcp service is unavailable after mount: {error}")
            })?;

        // R21/R30: connect the configured MCP servers through the
        // mounted supervisor; a server that fails to start is logged
        // with its health state and skipped. The fixture configures no
        // servers and drives the supervisor itself.
        let mcp_registration = match input.mode {
            BootMode::Deployment => {
                let registration = register_configured_mcp_servers(
                    Arc::clone(&mcp_supervisor),
                    tool_registry.as_ref(),
                    clock.now(),
                    &input.workspace_root,
                )
                .await;
                // Smaller per-request load: MCP tools advertise name +
                // description (`Deferred`) and promote their schema on
                // the first call. Only `Direct` flips, so an operator's
                // explicit verdict stands.
                for d in tool_registry.descriptors().iter().filter(|d| {
                    d.name.starts_with("mcp__")
                        && d.exposure == uva::tool::ToolExposure::Direct
                }) {
                    tool_registry.set_exposure(
                        &d.name,
                        uva::tool::ToolExposure::Deferred,
                    );
                }
                Some(registration)
            }
            #[cfg(any(test, feature = "test-utils"))]
            BootMode::Fixture => None,
        };

        // The route surfaces read the same store/engine the plugins
        // published: one schedule store, one eval runner.
        let schedules = SchedulesState::from_store(
            Arc::clone(scheduler_service.store()),
            clock.clone(),
        );
        let evals = EvalsState::build_with_runner(
            &input.workspace_root,
            Arc::clone(&eval_runner),
        );

        // The loop is the server's only agent builder; a profile that
        // omits the row must fail here rather than run every session
        // without the deployment's strategy / restriction / compactor.
        kernel
            .require::<Arc<dyn uva::harness::plugin::AgentLoopFactory>>(
                uva::harness::plugin::KEY_AGENT_LOOP,
            )
            .map_err(|error| {
                format!(
                    "the agentLoop service is unavailable after mount: \
                     {error}; the profile must include the `agent-loop` row"
                )
            })?;

        // The supervisor's reconnect loop is the boot's to drive: one
        // tick resolves the `mcp` service, so unloading the plugin
        // stops the loop (the next `get` fails) and the explicit
        // teardown path withdraws the generations.
        let mcp = match mcp_registration {
            Some((mcp_clients, mcp_health)) => McpRuntime::start(
                kernel.clone(),
                Arc::clone(&tool_registry),
                mcp_supervisor,
                mcp_clients,
                mcp_health,
                clock.clone(),
            ),
            None => McpRuntime::detached(mcp_supervisor),
        };

        // R34 (Task 16): apply the deployment's `[tools]` surface after
        // every registration source has had its say — builtins, the
        // mounted `skills` / `mcp` / `schedule-tool` plugins, and the
        // composition's host plugins. Unknown names are warnings,
        // never boot failures — see `apply_tools_config`. The fixture
        // composes no host plugins, so its surface stays empty.
        let tool_surface = match input.mode {
            BootMode::Deployment => apply_tools_config(
                tool_registry.as_ref(),
                &input
                    .server_config
                    .as_ref()
                    .map(|c| c.tools.clone())
                    .unwrap_or_default(),
            ),
            #[cfg(any(test, feature = "test-utils"))]
            BootMode::Fixture => AppliedToolSurface::default(),
        };
        if matches!(input.mode, BootMode::Deployment) {
            Self::build_default_agent(
                &kernel,
                &default_provider,
                &tool_registry,
                tool_surface.policy(),
                input.default_max_iterations,
                input.plugin_host.clone(),
                Arc::clone(&input.agent_registry),
            )
            .await;
        }

        // The label registry is shared with HMR remounts; it must be
        // taken before the mounted handle is consumed.
        let mounted_realms = mounted.realms();
        let mounted_groups = mounted.into_groups();
        Ok(Self {
            clock,
            cluster: input.cluster,
            tool_registry,
            agent_registry: input.agent_registry,
            session_search: input.session_search,
            session_manager,
            workspace_root: input.workspace_root,
            default_model: input.default_model,
            workspace_config: parking_lot::RwLock::new(workspace_config),
            default_provider: parking_lot::RwLock::new(default_provider),
            provider_catalogue: parking_lot::RwLock::new(provider_catalogue),
            operations_config: input.operations_config,
            // The ReAct loop builds its own system prompt per-dispatch
            // via `uva::agent::PromptContext::assemble`; this is the
            // legacy default for callers that read
            // `AppState::system_prompt` directly.
            system_prompt: uva::agent::DEFAULT_SYSTEM_PROMPT.to_string(),
            server_config: input.server_config,
            default_max_iterations: input.default_max_iterations,
            default_compaction: input.default_compaction,
            tool_surface,
            tool_defs_cache: parking_lot::RwLock::new(None),
            mcp,
            assembly: Assembly::new(
                crate::state::app_state::assembly::BootedAssembly {
                    kernel,
                    boot_services,
                    mount: mounted_groups,
                    realms: mounted_realms,
                    extras,
                },
                input.profile,
                assembly,
                input.profile_store,
                ui_tree,
            ),
            plugin_host: input.plugin_host,
            #[cfg(any(test, feature = "test-utils"))]
            provider_override: parking_lot::Mutex::new(None),
            #[cfg(any(test, feature = "test-utils"))]
            catalogue_override: parking_lot::Mutex::new(None),
            active_sessions,
            spawn_gates: Arc::new(DashMap::new()),
            spawn_gate_timeout: super::sessions::COLD_OPEN_GATE_TIMEOUT,
            subagent_router,
            default_agent_name: parking_lot::RwLock::new(
                input.default_agent_name,
            ),
            usage_metrics,
            attachment_store,
            schedules,
            evals,
            rpc: input.rpc,
        })
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
    default_compaction: Option<uva::session::CompactionSettings>,
    default_tool_restriction: Option<Arc<uva::tool::ToolRestriction>>,
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
        .unwrap_or(uva::harness::DEFAULT_MAX_ITERATIONS);

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
    /// registry comes in too: the `tool-task` row's tool was built
    /// over it before the mount, so the boot creates it once and this
    /// call only registers the default member.
    #[allow(clippy::too_many_arguments)]
    async fn build_default_agent(
        kernel: &uva::kernel::Context,
        default_provider: &Arc<dyn ModelProvider>,
        tool_registry: &Arc<ToolRegistry>,
        tool_surface_policy: Option<uva::tool::ToolSurfacePolicy>,
        default_max_iterations: usize,
        plugin_host: uva::harness::Host,
        agent_registry: Arc<AgentRegistry>,
    ) -> Arc<AgentRegistry> {
        // The default member is built through the mounted
        // `agentLoop` service, exactly like a session run: the
        // factory's defaults (strategy, restriction, context manager,
        // spawner, compactor) apply, and only the
        // member's own inputs are set here.
        let factory = kernel
            .require::<Arc<dyn uva::harness::plugin::AgentLoopFactory>>(
                uva::harness::plugin::KEY_AGENT_LOOP,
            )
            .map(|wrapped| Arc::clone(&*wrapped));
        let default_agent = match factory {
            Ok(factory) => {
                factory.create(&uva::harness::plugin::RunConfig {
                    provider: Arc::clone(default_provider),
                    // The node's one registry — not a snapshot. A
                    // plugin's tools are registered here at bind
                    // time, and every per-run agent sees them
                    // because it binds the same handle.
                    tool_registry: Arc::clone(tool_registry),
                    workspace_root: std::path::PathBuf::new(),
                    descriptor: uva::harness::default_descriptor(
                        uva::agent::DEFAULT_SYSTEM_PROMPT.to_string(),
                    ),
                    prompt_context: Arc::new(
                        uva::agent::PromptContext::default(),
                    ),
                    max_iterations: default_max_iterations,
                    tool_surface: tool_surface_policy,
                    strategy: None,
                    tool_restriction: None,
                    log_writer: None,
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
    workspace: uva::provider::config::WorkspaceConfig,
) -> uva::runtime::ModelAdapterConfig {
    let Some(row) = profile.core.iter().find(|e| e.id == "model-adapter")
    else {
        return uva::runtime::ModelAdapterConfig {
            catalogue: workspace,
            ..Default::default()
        };
    };
    let empty = row.config.as_object().is_none_or(serde_json::Map::is_empty);
    if empty {
        return uva::runtime::ModelAdapterConfig {
            catalogue: workspace,
            ..Default::default()
        };
    }
    serde_json::from_value(row.config.clone()).unwrap_or_else(|error| {
        tracing::warn!(
            error = %error,
            "model-adapter row config is malformed; the mount reports it"
        );
        uva::runtime::ModelAdapterConfig {
            catalogue: workspace,
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
