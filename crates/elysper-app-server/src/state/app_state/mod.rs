//! `AppState`: top-level shared state container for the server.
//!
//! ## Layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | `boot` | `AppState::new` / `with_server_config`: resolve configs, build the registry, wire every field. |
//! | `for_test` | The `test-utils` constructor with in-memory components and a fake provider. |
//! | `sessions` | `get_or_create_session_controller`: the per-`(user, session)` controller cache. |
//! | `usage` | `UsageMetrics` counters + `UsageSnapshot`. |
//! | `tools` | The default plugin tool surface + the `AppliedToolSurface` record. |
//! | `schedules` | `ScheduleStore` + `Scheduler` for the `/api/v1/schedules/*` HTTP surface. |
//! | `workflows` | In-memory `WorkflowSpec` registry + plan validation for `/api/v1/workflows/*`. |
//! | `evals` | In-memory `EvalSuite` registry + keyword-metric runner for `/api/v1/evals/*`. |

use std::{path::PathBuf, sync::Arc};

use elysper::{
    harness::AgentRegistry,
    provider::{config::WorkspaceConfig, traits::ModelProvider},
    session::{SharedSessionSearch, manager::SessionRegistry},
    tool::ToolRegistry,
};

mod boot;
pub(crate) mod cluster_ops;
#[cfg(any(test, feature = "test-utils"))]
pub(crate) mod demo_provider;
pub(crate) mod eval_agent;
mod evals;
mod mcp;
mod schedules;
pub(crate) mod self_mcp;
mod sessions;
pub(crate) mod tasks;
mod tools;
mod usage;
mod workflow_host;
mod workflows;

#[cfg(any(test, feature = "test-utils"))]
mod for_test;

/// The catalogue and its selection error live in the runtime
/// assembly crate; the server consumes them through the facade
/// (R121).
pub use elysper::runtime::{ModelSelectionError, ProviderCatalogue};
pub use eval_agent::HarnessEvalAgent;
pub use evals::EvalsState;
pub use mcp::McpRuntime;
pub use schedules::SchedulesState;
#[cfg(test)]
pub(crate) use sessions::PARKED_PROMPT_WINDOW;
pub use tools::AppliedToolSurface;
#[cfg(test)]
pub(crate) use tools::plugin_tool_registry;
pub use usage::{UsageMetrics, UsageSnapshot};
pub use workflows::WorkflowsState;

/// Build the deployment's catalogue from a workspace config, with the
/// test/demo seam applied: keyless `demo`/`fake` entries get their
/// echo handles injected through the catalogue's prebuilt-handle
/// seam, so test workspaces resolve `demo` without an API key.
/// Outside test builds the function is the plain catalogue build.
#[must_use]
pub(crate) fn catalogue_for_deployment(
    config: &elysper::provider::config::WorkspaceConfig,
) -> ProviderCatalogue {
    #[cfg(any(test, feature = "test-utils"))]
    {
        // Build the catalogue from the real entries only, then inject
        // the demo handles: the builder would otherwise log a
        // "provider is not available" warning for a type it cannot
        // build before the seam gets to it.
        let mut real = config.clone();
        real.providers.retain(|_, entry| {
            !matches!(entry.r#type.as_str(), "demo" | "fake")
        });
        let mut catalogue = ProviderCatalogue::from_workspace_config(&real);
        for (name, entry) in &config.providers {
            if matches!(entry.r#type.as_str(), "demo" | "fake") {
                catalogue = catalogue.with_handle(
                    name,
                    entry.default_model.as_deref().unwrap_or("unknown"),
                    std::sync::Arc::new(demo_provider::DemoEchoProvider),
                );
            }
        }
        catalogue
    }
    #[cfg(not(any(test, feature = "test-utils")))]
    {
        ProviderCatalogue::from_workspace_config(config)
    }
}

pub struct AppState {
    /// The node's one tool registry.
    ///
    /// Every agent this process builds shares this handle — the boot
    /// agent, each per-run session agent, eval agents, dynamically
    /// registered agents — so a tool registered at any point (a
    /// plugin at boot, an MCP server, a management route) is visible
    /// to every later run. `ToolRegistry` already carries its own
    /// interior `parking_lot::RwLock`, so this is the only lock: an
    /// outer one would force agents to clone a snapshot instead of
    /// sharing the live surface.
    pub tool_registry: Arc<ToolRegistry>,
    /// Multi-agent registry. Populated at startup with the
    /// canonical [`ReActAgent`](elysper::harness::ReActAgent) descriptor so any panel/judge
    /// agents registered later surface in the system prompt as
    /// handoff targets. The `self` agent is excluded from its
    pub agent_registry: Arc<AgentRegistry>,
    pub session_search: SharedSessionSearch,
    pub session_manager: Arc<SessionRegistry>,
    pub workspace_root: PathBuf,
    pub default_model: String,
    pub workspace_config: parking_lot::RwLock<WorkspaceConfig>,
    pub default_provider: parking_lot::RwLock<Arc<dyn ModelProvider>>,
    /// Every provider this deployment configures, keyed by its
    /// `[providers.<name>]` entry name—the map a turn's `model`
    /// selection resolves against.
    ///
    /// [`Self::default_provider`] is `catalogue.default_handle()`:
    /// the handle for `workspace_config.default_provider`. It is kept
    /// as its own field because it is the fallback every caller
    /// without an override uses, and because a fixture may replace it
    /// with a fake; the catalogue is what makes the *other* providers
    /// selectable at all.
    pub provider_catalogue: parking_lot::RwLock<Arc<ProviderCatalogue>>,
    /// R29: gate for the `POST.../operation` endpoint. Read by
    /// `create_router` to decide whether to register the route.
    pub operations_config: Arc<crate::config::OperationEndpointConfig>,
    /// Active session controllers keyed by `(user_id, session_id)`.
    ///
    /// Per-session event broadcasters live on the controller
    /// itself; SSE/WebSocket subscribers obtain a
    /// `tokio::sync::broadcast::Receiver` via
    /// [`SessionController::subscribe`](crate::session::controller::SessionController::subscribe).
    pub active_sessions: Arc<crate::state::ActiveSessions>,
    /// One gate per `(user_id, session_id)` for the cold-open path.
    ///
    /// Opening a session for the first time is a read-modify-write of
    /// [`Self::active_sessions`] that also rebuilds the controller from
    /// the log and re-queues a parked prompt — work that must happen
    /// once, not once per concurrent request. Every open for the
    /// session queues on the gate, warm ones included: a request that
    /// arrives while a cold open is in flight waits for it, bounded by
    /// the timeout below. Private: it is a spawn detail, not server
    /// state a route may inspect.
    ///
    /// The map holds a **weak** reference to each gate: the session's
    /// controller owns the lock, so an entry lives exactly as long as
    /// its controller and the map cannot grow with the number of
    /// sessions the process has ever opened.
    spawn_gates: Arc<crate::state::SessionSpawnGates>,
    /// How long a cold open waits for that gate before failing.
    ///
    /// The production value is `COLD_OPEN_GATE_TIMEOUT` in
    /// `sessions.rs`; a test shortens it to prove the bound without
    /// waiting half a minute.
    pub(crate) spawn_gate_timeout: std::time::Duration,
    /// Sub-agent session router: gives every delegated child a
    /// first-class session of its own (see
    /// `crate::session::subagent_router`). Installed once at boot
    /// over the same registry + controller cache the HTTP surface
    /// uses, and threaded into every session controller's
    /// [`RunDependencies`](crate::session::controller::RunDependencies).
    pub subagent_router: Arc<crate::session::subagent_router::SubagentRouter>,
    /// Pre-rendered default system prompt. The agent loop
    /// currently builds its own prompt from the descriptor
    /// via [`elysper::harness::PromptContext::assemble`],
    /// so this field exists as a lock-free default for
    /// legacy callers that read it directly. Stored on
    /// `AppState` so the read is cheap on every dispatch.
    pub system_prompt: String,
    /// Pre-built prompt manifest (skills + peer agents + tool
    /// definitions) consumed by the
    /// [`elysper::harness::PromptContext::assemble`]
    /// method. Built once at startup and shared via `Arc` into
    /// every [`crate::session::controller::RunDependencies`]—
    /// the manifest never mutates after boot, so wrapping it
    /// in an `Arc` lets every dispatch and every per-agent
    /// `build_react_agent` call share the same allocation
    /// instead of deep-cloning the full skills + peer-agent
    /// list on each one.
    pub prompt_context: Arc<elysper::harness::PromptContext>,
    /// Configured default agent name. Loaded from
    /// `config.agent.default` at startup. `None` falls back to
    /// the first agent registered in [`Self::agent_registry`].
    ///
    /// Backed by [`parking_lot::RwLock`] (not
    /// [`tokio::sync::RwLock`]) so the run factory can resolve
    /// the default agent name synchronously without awaiting.
    pub default_agent_name: parking_lot::RwLock<Option<String>>,
    /// Default per-run iteration cap applied to every dispatch
    /// unless a per-run override is supplied. Loaded from
    /// `agents.<name>.max_steps` in the server config; falls
    /// back to
    /// [`elysper::harness::DEFAULT_MAX_ITERATIONS`]
    /// when unset. The agent field's own `[1, 4096]` clamp is
    /// honoured here too.
    pub default_max_iterations: usize,
    /// R16: optional compaction policy resolved from
    /// `agents.<name>.compaction` at boot. Forwarded to every
    /// session controller's `RunDependencies`.
    pub default_compaction: Option<elysper::session::CompactionSettings>,
    /// The deployment configuration the boot resolved (R53/R54), kept
    /// whole so the operator-facing surfaces can answer "what was this
    /// process started with?" without re-reading the file—and so
    /// `max_agents` and the bind address have one source.
    pub server_config: Option<crate::config::ServerConfig>,
    /// R58: the agent's allow/deny list resolved from
    /// `agents.<default>.{allowed_tools,denied_tools}` at boot.
    /// Forwarded to every run and installed on the registered agents.
    pub default_tool_restriction: Option<Arc<elysper::tool::ToolRestriction>>,
    /// R50: the reasoning loop resolved from
    /// `agents.<default>.strategy` at boot. Forwarded to every
    /// session controller's `RunDependencies` and installed on the
    /// registered agents, so a session run and a registered agent use
    /// the same loop. `None` keeps ReAct.
    pub default_strategy:
        Option<Arc<dyn elysper::harness::agent::ReasoningStrategy>>,
    /// R21: live MCP clients for the configured servers. Held for
    /// the process lifetime so their child processes stay alive
    /// (`StdioTransport` kills its child on drop) and so a
    /// consumer can address the servers by name.
    /// The MCP supervisor, its live clients, and the reconnect tick
    /// loop the boot owns.
    ///
    /// The supervisor holds the servers' child processes and
    /// reconnects them with bounded backoff;
    /// `McpRuntime` drives [`elysper::mcp::McpSupervisor::tick`].
    /// Unloading `mcp` stops the loop and withdraws the generations,
    /// so no remote tool outlives the service.
    pub mcp: McpRuntime,
    /// The kernel isolation scope every native service is registered
    /// in. Consumers resolve services here; the graph is what the
    /// mount validated.
    pub kernel: elysper::kernel::Context,
    /// The host objects the factory directory builds from, kept so a
    /// hot reload can construct an `added` or `remounted` plugin
    /// without rebuilding the boot.
    pub(crate) kernel_host: Arc<crate::composition::kernel::KernelHost>,
    /// The mounted plugin groups. The boot group comes first; an HMR
    /// remount appends one group for the rows it rebuilt, so an
    /// untouched plugin's fiber and effects stay where they were.
    /// Dropping a handle unloads every service in that group.
    pub mount: parking_lot::Mutex<Vec<elysper::kernel::MountHandle>>,
    /// The effective core assembly: the built-in set when the profile
    /// declares no explicit row, otherwise the profile's rows. The HMR
    /// loop diffs against this value.
    pub profile: parking_lot::RwLock<crate::profile::Profile>,
    /// The `[ui]` segment projected as render schemas.
    pub ui_schemas: parking_lot::RwLock<Vec<crate::ui::schema::UiSchema>>,
    /// The deployment's host, built once at boot with
    /// [`Host::default_policy`](elysper::harness::Host::default_policy)
    /// (the steering policy) and shared with every agent.
    ///
    /// [`Self::plugins`] installed every configured plugin into it;
    /// the host owns the registrations (observers, gates,
    /// transformers, prompt sections, tools) those installs made.
    /// Every agent construction hands this clone to
    /// `ReActAgent::with_host`, so the plugin's tools are callable
    /// from the boot agent and from the per-run agents that serve
    /// chat alike. A clone shares the registrations, so there is one
    /// policy, not one per agent.
    pub plugin_host: elysper::harness::Host,
    /// Test-utils override for the default provider (see
    /// [`Self::override_default_provider_for_test`]).
    #[cfg(any(test, feature = "test-utils"))]
    pub(crate) provider_override:
        parking_lot::Mutex<Option<Arc<dyn ModelProvider>>>,
    /// Test-utils override for the mounted catalogue (see
    /// [`Self::override_catalogue_for_test`]).
    #[cfg(any(test, feature = "test-utils"))]
    pub(crate) catalogue_override:
        parking_lot::Mutex<Option<Arc<ProviderCatalogue>>>,
    /// Task 15: the deployment's plugin composition, installed into
    /// [`Self::plugin_host`] at boot.
    ///
    /// It owns every installed plugin's `PluginHandle`, and a
    /// handle's drop withdraws its registrations — so this field
    /// must live as long as the host. The resolved and skipped
    /// sets are the operator-facing record ("this deployment runs
    /// these plugins; these entries were skipped, and why").
    pub plugins: crate::composition::Composition,
    /// R34: the effective tool surface after the `[tools]` config
    /// section was applied to the registry at boot. Unknown config
    /// names were dropped with a warning (see
    /// `AppliedToolSurface::skipped`), so this is what an operator
    /// can trust was actually written. The run factory installs
    /// `AppliedToolSurface::policy` on every agent it builds.
    pub tool_surface: AppliedToolSurface,
    /// R34: per-state cache for the `GET /api/v1/tools` projection,
    /// keyed on [`ToolRegistry::version`]. Per-state rather than a
    /// process-wide static so two servers in one process cannot
    /// serve each other's list; every registry mutation that changes
    /// the model-facing list bumps the version, so a hit is only ever
    /// the same registry at the same version.
    pub tool_defs_cache: parking_lot::RwLock<
        Option<(u64, Arc<Vec<elysper::provider::ToolDefinition>>)>,
    >,
    /// Process-wide usage counters surfaced by
    /// `GET /api/v1/chat/usage`.
    ///
    /// Shared with **every** session controller this state spawns
    /// (installed on their [`RunDependencies`] and, from there, on
    /// the controller's event funnel—the single write site, which
    /// records each run's `SystemEvent::Usage` totals and counts the
    /// run on `SystemEvent::SessionEnded`). Read-only via
    /// [`AppState::usage_metrics`]; the `Arc` is what lets the
    /// counters outlive any one controller, so tokens spent by a
    /// closed session still show up in the process totals.
    ///
    /// [`RunDependencies`]: crate::session::controller::RunDependencies
    pub usage_metrics: Arc<UsageMetrics>,
    /// Cluster bus + this node's identity. `mode = "local"` (the
    /// default) carries an in-process [`LocalBus`]; `mode = "redis"`
    /// shares leases and frames with the other nodes.
    ///
    /// [`LocalBus`]: crate::cluster::LocalBus
    pub cluster: Arc<crate::cluster::ClusterRuntime>,
    /// Multimodal attachment store (R11). Backs the
    /// `/api/v1/attachments` REST surface and lets the agent
    /// runtime fetch the inline base64 for a stored image
    /// without round-tripping the original uploader.
    pub attachment_store: Arc<elysper::attachment::AttachmentStore>,
    /// Long-term memory tier (files under the workspace's
    /// `.elysper/memory*` scopes). The same handle the search
    /// service's memory domain recalls from.
    pub memory: Arc<dyn elysper::context::Memory>,
    /// The cross-domain search engine: one `elysper_search`
    /// registry over tool / mcp / skill / memory / agent / session,
    /// registered as the model-facing `search` tool (deferred
    /// exposure—capability 1) and projected by
    /// `GET /api/v1/search` (capability 2).
    pub search: Arc<crate::search::SearchService>,
    /// Schedules subsystem—disk-backed `ScheduleStore` plus a
    /// runtime-neutral `Scheduler`. Backs
    /// `/api/v1/schedules/{list,get,create,patch,delete,tick}`.
    pub schedules: SchedulesState,
    /// Workflows subsystem—in-memory `WorkflowSpec` registry
    /// plus plan validation. Backs `/api/v1/workflows/*`. Real
    /// execution lands in a follow-up turn (the runtime needs a
    /// `WorkflowHost` that calls back into the agent harness).
    pub workflows: WorkflowsState,
    /// The workflow-runtime bridge—every planned call in a
    /// workflow run dispatches through `workflow_host.spawn_agent`.
    /// The bridge is the `ServerWorkflowHost` (see the
    /// `workflow_host` module of this crate): it resolves the
    /// step's agent name against the deployment's `AgentRegistry`
    /// and runs the agent to completion on the step's prompt, so
    /// the drained text becomes the call's outcome. An unknown
    /// agent name is a typed per-step failure ("agent `x` is not
    /// registered in this deployment"), not a host error.
    pub workflow_host: Arc<dyn elysper::workflow::WorkflowHost>,
    /// Evals subsystem—in-memory `EvalSuite` registry plus a
    /// keyword-only runner. Backs `/api/v1/evals/*`. LLM-judge /
    /// schema-validation metrics land with a real `EvalAgent`.
    pub evals: EvalsState,
    /// The process's RPC runtime: the outbound envelope channel and
    /// the per-session follow subscribers.
    pub rpc: Arc<crate::rpc::RpcRuntime>,
    /// The process's single wall-clock source. Every component that
    /// stamps a timestamp (the session log, operation snapshots, the
    /// attachment store, the MCP supervisor's tick) reads it from
    /// here instead of calling `chrono::Utc::now()` on its own, so a
    /// test can pin the whole deployment's clock in one place.
    pub clock: elysper::core::SharedClock,
}

impl AppState {
    /// Unload one mounted native plugin.
    ///
    /// Returns `false` when no plugin with `id` is owned (including a
    /// second call for an id already disposed). Consumers the plugin
    /// served are parked by the kernel, not unmounted.
    pub fn dispose_plugin(&self, id: &str) -> bool {
        let mut handles = self.mount.lock();
        let mut disposed = false;
        for handle in handles.iter_mut().rev() {
            if handle.dispose_plugin(id) {
                disposed = true;
                break;
            }
        }
        // A group whose rows were all disposed owns nothing; dropping it
        // now keeps the vector from growing with every reload.
        handles.retain(|handle| !handle.is_empty());
        disposed
    }

    /// The lifecycle state of `id` in any mounted group.
    ///
    /// `None` means the id is not mounted: never mounted, disposed by
    /// [`Self::dispose_plugin`] (the empty group is pruned), or the
    /// kernel was shut down.
    #[must_use]
    pub fn plugin_state(
        &self,
        id: &str,
    ) -> Option<elysper::kernel::PluginState> {
        self.mount
            .lock()
            .iter()
            .find_map(|handle| handle.plugin_state(id))
    }

    /// The UI schemas the frontend loader should render.
    #[must_use]
    pub fn ui_schemas(&self) -> Vec<crate::ui::schema::UiSchema> {
        self.ui_schemas.read().clone()
    }

    /// The profile the mounted core set currently corresponds to.
    #[must_use]
    pub fn current_profile(&self) -> crate::profile::Profile {
        self.profile.read().clone()
    }

    /// Reconcile the mount with a freshly resolved profile.
    ///
    /// Only the ids the diff names are touched: `removed` rows are
    /// unloaded, and `added` / `remounted` rows are built and mounted as
    /// one new group (the kernel orders and validates that group). A
    /// `volatile_changed` row is not remounted; the stored profile
    /// moves forward so the next diff is against the new value. If the
    /// new profile turns HMR off, nothing is touched and
    /// `ProfileDiff::restart_required` tells the caller to report a
    /// restart.
    ///
    /// The build happens **before** any unload, so a bad row cannot
    /// take a working plugin down. A mount failure after disposal
    /// remounts the previous rows as a best-effort rollback.
    ///
    /// # Residuals
    ///
    /// Remounting the `mcp` row republishes the service, but the
    /// boot-owned tick loop stays stopped: `McpRuntime` is not
    /// restartable in place and its boot-time clients were
    /// dropped by [`Self::unload_plugin`]. A process restart is needed
    /// to re-supervise the configured servers. A turn that started
    /// before the reload keeps the service handles it resolved; only
    /// the next turn sees the new graph (this is the desired
    /// snapshot behaviour, not a gap).
    ///
    /// # Errors
    ///
    /// Returns a message when the new `[ui]` segment or a changed core
    /// row cannot be built; the mounted set is left as it was.
    pub async fn reload_profile(
        &self,
        resolved: crate::profile::Profile,
    ) -> Result<crate::profile::ProfileDiff, String> {
        use std::collections::BTreeSet;

        use crate::{
            composition::kernel::profile_factories,
            profile::{BootContext, build_plugins, diff_profiles},
            ui::schema::load_ui_manifest,
        };

        let next = resolved;
        let previous = self.current_profile();
        let factories = profile_factories(Arc::clone(&self.kernel_host));
        let diff = diff_profiles(&previous, &next, &factories);
        if diff.restart_required {
            return Ok(diff);
        }
        let schemas =
            load_ui_manifest(&next.ui).map_err(|error| error.to_string())?;

        let changed: BTreeSet<&str> = diff
            .added
            .iter()
            .chain(diff.remounted.iter())
            .map(String::as_str)
            .collect();
        let boot = BootContext::new(self.workspace_root.clone())
            .with_host(Arc::clone(&self.kernel_host))
            .with_clock(self.clock.clone());
        let mount_rows = rows_for(&next, &changed);
        let plugins = build_plugins(&mount_rows, &factories, &boot)
            .map_err(|error| error.to_string())?;

        for id in diff.removed.iter().chain(diff.remounted.iter()) {
            self.unload_plugin(id).await;
        }
        if !plugins.is_empty() {
            match elysper::kernel::mount_all(&self.kernel, &plugins) {
                Ok(mounted) => self.mount.lock().push(mounted),
                Err(error) => {
                    tracing::error!(
                        %error,
                        "the profile remount failed; restoring the old rows"
                    );
                    // The rollback set includes the removed ids: the
                    // failed group disposed them, and restoring only
                    // `changed` would leave them unmounted.
                    let rollback_ids: BTreeSet<&str> = diff
                        .added
                        .iter()
                        .chain(diff.remounted.iter())
                        .chain(diff.removed.iter())
                        .map(String::as_str)
                        .collect();
                    let rollback_rows = rows_for(&previous, &rollback_ids);
                    if let Ok(old) =
                        build_plugins(&rollback_rows, &factories, &boot)
                        && let Ok(mounted) =
                            elysper::kernel::mount_all(&self.kernel, &old)
                    {
                        self.mount.lock().push(mounted);
                    }
                    return Err(error.to_string());
                }
            }
        }

        // A remounted `model-adapter` redefined the provider surface:
        // AppState's own fields re-sync with the mounted services so
        // sessions created after the reload sample the new default,
        // while in-flight sessions keep the handles they resolved.
        if let Ok(catalogue) = self
            .kernel
            .require::<ProviderCatalogue>(elysper::provider::KEY_LLM)
        {
            *self.workspace_config.write() = catalogue.workspace().clone();
            *self.provider_catalogue.write() = catalogue;
            if let Ok(default) = self.kernel.require::<Arc<dyn ModelProvider>>(
                elysper::provider::KEY_LLM_DEFAULT,
            ) {
                *self.default_provider.write() = Arc::clone(&*default);
            }
        }

        *self.ui_schemas.write() = schemas;
        *self.profile.write() = next;
        Ok(diff)
    }

    /// Unload one plugin with the teardown its service needs.
    ///
    /// `mcp` is the one service whose value owns live resources: its
    /// tick loop, the generation registrations, and the transports /
    /// child processes. Stop all three **before** withdrawing the
    /// service so no remote tool outlives it and no timer keeps
    /// running against a dropped supervisor. Every other id is a plain
    /// [`Self::dispose_plugin`].
    pub async fn unload_plugin(&self, id: &str) -> bool {
        if id == elysper::mcp::KEY_MCP {
            self.mcp.shutdown(&self.tool_registry).await;
        }
        self.dispose_plugin(id)
    }

    /// Tear the kernel down: stop the MCP tick loop (withdrawing its
    /// generations and transports) and drop the mount handle, which
    /// unloads every service and fiber in reverse order.
    pub async fn shutdown_kernel(&self) {
        self.mcp.shutdown(&self.tool_registry).await;
        // Drain in reverse so a later group (a reload's remount) is
        // dropped before the boot group its rows may inject from.
        for mounted in self.mount.lock().drain(..).rev() {
            drop(mounted);
        }
    }

    /// The mounted `attachments` service, or `None` when the plugin is
    /// unloaded.
    ///
    /// Multimodal input admission reads this: an unloaded store means
    /// image / audio parts are refused, not silently dropped.
    #[must_use]
    pub fn attachments(
        &self,
    ) -> Option<Arc<elysper::attachment::AttachmentStore>> {
        self.kernel
            .get::<elysper::attachment::AttachmentStore>(
                elysper::attachment::KEY_ATTACHMENTS,
            )
            .ok()
            .flatten()
    }

    /// The mounted `agentLoop` service, or `None` when its plugin is
    /// unloaded.
    #[must_use]
    pub fn agent_loop(
        &self,
    ) -> Option<Arc<dyn elysper::harness::plugin::AgentLoopFactory>> {
        self.kernel
            .get::<Arc<dyn elysper::harness::plugin::AgentLoopFactory>>(
                elysper::harness::plugin::KEY_AGENT_LOOP,
            )
            .ok()
            .flatten()
            .map(|service| Arc::clone(service.as_ref()))
    }

    /// Resolve an agent by name through the standard ladder:
    /// `explicit > configured default > first registered`.
    ///
    /// Uses [`AgentRegistry::resolve_sync`] under the
    /// [`parking_lot::RwLock`] so callers in synchronous contexts
    /// (e.g. `SessionController::build_run_config`) can resolve
    /// the running agent without `await`.
    pub fn resolve_agent_name(
        registry: &AgentRegistry,
        default_agent_name: Option<&str>,
        explicit: Option<&str>,
    ) -> Option<String> {
        if let Some(name) = explicit
            && let Some(arc) = registry.resolve_sync(name)
        {
            return Some(arc.descriptor().name.clone());
        }
        if let Some(default_name) = default_agent_name
            && let Some(arc) = registry.resolve_sync(default_name)
        {
            return Some(arc.descriptor().name.clone());
        }
        // `first_name` delegates to `names()`, so answering this
        // builds the full sorted name list — acceptable at request
        // cadence (once per message dispatch, not per token).
        registry.first_name()
    }

    /// Instance convenience for the static helper. Mirrors the
    /// ladder `explicit > configured default > first registered`.
    pub fn resolve_agent_name_for(
        &self,
        explicit: Option<&str>,
    ) -> Option<String> {
        Self::resolve_agent_name(
            &self.agent_registry,
            self.default_agent_name.read().as_deref(),
            explicit,
        )
    }

    /// Resolve a turn's `model` selection to the provider that runs
    /// it.
    ///
    /// Accepted spellings, on every surface that carries a model
    /// selection:
    ///
    /// - `None` / blank—the deployment's default provider;
    /// - `"<provider>/<model>"`—the shape `/api/v1/models`
    ///   advertises per entry, and what the chat UI posts. The split
    ///   is at the *first* slash, so a model id that contains one
    ///   (`meta-llama/llama-3.3-70b-instruct`) still resolves;
    /// - a bare `"<model>"` that exactly one configured provider
    ///   advertises—the Anthropic protocol's `model` field is a
    ///   bare model id, so `/v1/messages` needs this spelling.
    ///
    /// A value that resolves to nothing is an error naming it: an
    /// override must never silently run the default, or the user's
    /// model selection is a no-op they cannot see.
    ///
    /// [`ProviderCatalogue::resolve`] documents how each mismatch is
    /// reported; the wire layer maps the variants onto its status
    /// codes.
    pub(crate) fn resolve_provider(
        &self,
        selection: Option<&str>,
    ) -> Result<Arc<dyn ModelProvider>, ModelSelectionError> {
        // The mounted catalogue is authoritative: the cached field is
        // a mirror, and a reload that removed the provider row must
        // fail selections instead of serving the withdrawn value.
        let catalogue = self.live_catalogue().ok_or_else(|| {
            ModelSelectionError::Unavailable {
                provider: selection.unwrap_or("default").to_string(),
                reason: "the model-adapter plugin is not mounted".to_string(),
            }
        })?;
        catalogue.resolve(selection)
    }

    /// The mounted catalogue, when the `model-adapter` plugin is
    /// active.
    pub(crate) fn live_catalogue(&self) -> Option<Arc<ProviderCatalogue>> {
        #[cfg(any(test, feature = "test-utils"))]
        if let Some(catalogue) = self.catalogue_override.lock().clone() {
            return Some(catalogue);
        }
        self.kernel
            .get::<ProviderCatalogue>(elysper::provider::KEY_LLM)
            .ok()
            .flatten()
    }

    /// The mounted default provider handle.
    ///
    /// The `test-utils` build lets a fixture substitute one (the
    /// session runs need scripted responses, and the compiled-in demo
    /// handle cannot carry them); every other build reads the service.
    pub(crate) fn live_default_provider(
        &self,
    ) -> Option<Arc<dyn ModelProvider>> {
        #[cfg(any(test, feature = "test-utils"))]
        if let Some(provider) = self.provider_override.lock().clone() {
            return Some(provider);
        }
        self.kernel
            .get::<Arc<dyn ModelProvider>>(elysper::provider::KEY_LLM_DEFAULT)
            .ok()
            .flatten()
            .map(|wrapped| Arc::clone(&*wrapped))
    }

    /// Substitute the mounted catalogue for a scripted fixture
    /// (`test-utils` builds only).
    #[cfg(any(test, feature = "test-utils"))]
    pub fn override_catalogue_for_test(
        &self,
        catalogue: Arc<ProviderCatalogue>,
    ) {
        *self.catalogue_override.lock() = Some(catalogue);
    }

    /// Substitute the default provider for a scripted fixture
    /// (`test-utils` builds only).
    #[cfg(any(test, feature = "test-utils"))]
    pub fn override_default_provider_for_test(
        &self,
        provider: Arc<dyn ModelProvider>,
    ) {
        *self.provider_override.lock() = Some(provider);
    }

    /// The configured provider id behind a model selection.
    pub(crate) fn provider_id(
        &self,
        selection: Option<&str>,
    ) -> Option<String> {
        self.live_catalogue()
            .and_then(|catalogue| catalogue.provider_id(selection))
    }

    /// Default user_id for the single-tenant deployments. A real
    /// auth middleware would override this with the resolved
    /// `RequestUserId` extension; until then we mirror the
    /// [`elysper::session::manager::SERVER_DEFAULT_USER_ID`]
    /// constant so all sink / controller / log paths agree on
    /// one user_id without taking it as a parameter on every
    /// route handler.
    pub fn default_user_id(&self) -> &str {
        elysper::session::manager::SERVER_DEFAULT_USER_ID
    }

    /// Snapshot the in-process usage counters for `/api/v1/chat/usage`.
    pub fn usage_metrics(&self) -> &UsageMetrics {
        self.usage_metrics.as_ref()
    }

    /// How many agents this deployment may hold
    /// (`max_agents` in the deployment config, default
    /// [`crate::config::DEFAULT_MAX_AGENTS`]).
    ///
    /// Enforced by `POST /api/v1/agents`: a public deployment must not be
    /// able to grow its agent registry without bound.
    pub fn max_agents(&self) -> usize {
        self.server_config
            .as_ref()
            .map(|config| config.max_agents)
            .unwrap_or(crate::config::DEFAULT_MAX_AGENTS)
    }
}

impl AppState {
    /// Evaluate the readiness sub-checks backing the `/readyz` probe.
    ///
    /// Each tuple is `(check_name, passed)`. The server reports
    /// ready only when every check passes. Checks are cheap
    /// in-process reads—no network round-trips—so a probe
    /// firing once per second cannot pile up latency:
    /// - `chat_service`: the chat surface (the sole agent
    ///   interaction surface) is initialized. `create_router`
    ///   awaits it eagerly before the listener binds, so a
    ///   failure here means the router was built through a
    ///   non-standard path.
    /// - `agent_registry`: at least one agent is registered,
    ///   so dispatch has something to resolve.
    pub fn readiness_checks(&self) -> Vec<(&'static str, bool)> {
        vec![("agent_registry", self.agent_registry.first_name().is_some())]
    }
}

/// The rows of `profile` whose ids are in `ids`, with no UI rows.
fn rows_for(
    profile: &crate::profile::Profile,
    ids: &std::collections::BTreeSet<&str>,
) -> crate::profile::Profile {
    crate::profile::Profile {
        core: profile
            .core
            .iter()
            .filter(|entry| ids.contains(entry.id.as_str()))
            .cloned()
            .collect(),
        ui: Vec::new(),
        hmr: profile.hmr,
    }
}
