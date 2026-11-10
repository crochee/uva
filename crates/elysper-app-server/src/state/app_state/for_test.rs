//! The `test-utils` constructor: an `AppState` with in-memory
//! components and a fake provider, mirroring the production
//! wiring so route-level tests observe realistic state.

use std::{path::PathBuf, sync::Arc};

use dashmap::DashMap;
use elysper::{
    core::registry::Registry,
    harness::{AgentEntry, AgentRegistry},
    provider::{config::WorkspaceConfig, traits::ModelProvider},
    session::{
        JsonlSessionSearch,
        SharedSessionSearch,
        manager::SessionRegistry,
    },
    tool::ToolRegistry,
};

use super::{
    AppState,
    EvalsState,
    McpRuntime,
    ProviderCatalogue,
    SchedulesState,
    WorkflowsState,
    tools::{AppliedToolSurface, plugin_tool_registry},
    usage::UsageMetrics,
};
use crate::{
    composition::kernel::{BootServices, mount_profile, native_profile},
    session::controller::SessionController,
};

impl AppState {
    /// Create an AppState for testing with in-memory components and no LLM provider dependency.
    ///
    /// Unlike the production `new()`, this uses `FakeProvider` with empty
    /// responses. The fixture mounts the same native plugin set the
    /// production boot mounts (so the `skill` tool, the capability
    /// services, and the prompt builder are the real ones), over
    /// in-memory components where the production boot uses disk.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn for_test(
        session_manager: SessionRegistry,
        workspace_root: PathBuf,
    ) -> Self {
        let workspace_config = WorkspaceConfig::from_env();
        let default_model = workspace_config.default_model.clone();

        // Create a minimal provider that returns empty responses (for route-level tests)
        let default_provider: Arc<dyn ModelProvider> =
            Arc::new(elysper::test_support::FakeProvider::new(vec![]));

        // The catalogue every resolution path reads. A fixture builds
        // its own over whatever providers it wires (see
        // `ProviderCatalogue::with_handle`); this default one offers
        // exactly the fake, under the workspace's configured default
        // provider name, so `model: None` and an explicit selection of
        // that name both resolve.
        let provider_catalogue =
            Arc::new(ProviderCatalogue::default().with_handle(
                &workspace_config.default_provider,
                &default_model,
                Arc::clone(&default_provider),
            ));

        let tool_registry = Arc::new(plugin_tool_registry());
        // The fixture composes no host plugins, but the host exists and
        // is bound to the shared registry so every agent built from
        // this state has one place for plugin registrations — the
        // same shape the production boot installs.
        let plugin_host = elysper::harness::Host::new();
        let refused = plugin_host.bind(Arc::clone(&tool_registry));
        assert!(
            refused.is_empty(),
            "the fixture's fresh host binds its own registry: {refused:?}"
        );

        // Same resolution as `new()`, without an explicit path: a test
        // that writes `config.toml` into its tempdir observes the same
        // sections the server would.
        let server_config =
            crate::config::resolve_server_config(&workspace_root, None);
        let operations_config = Arc::new(
            server_config
                .as_ref()
                .map(|c| c.operations.clone())
                .unwrap_or_default(),
        );

        let clock = elysper::core::SharedClock::system();
        // Same seams the production boot installs: the tests drive a
        // tokio runtime, so the fs work belongs on its blocking pool.
        let session_manager = Arc::new(
            session_manager.with_blocking(crate::spawner::server_blocking()),
        );
        let session_search: SharedSessionSearch = Arc::new(
            JsonlSessionSearch::new(workspace_root.join("sessions"))
                .with_blocking(crate::spawner::server_blocking()),
        );

        let agent_registry = Arc::new(AgentRegistry::new());
        // Test state: register the canonical ReAct descriptor
        // (without holding a live provider) so the registry has
        // at least one entry and the peer-agent manifest snapshot
        // excludes it via the `self_name` filter.
        let _ = agent_registry
            .put(AgentEntry::new(Arc::new(
                elysper::harness::ReActAgent::new(
                    Arc::clone(&default_provider),
                    Arc::new(ToolRegistry::new()),
                ),
            )))
            .await;

        let agent_registry = Arc::new(AgentRegistry::new());
        // Test state: register the canonical ReAct descriptor
        // (without holding a live provider) so the registry has
        // at least one entry and the peer-agent manifest snapshot
        // excludes it via the `self_name` filter.
        let _ = agent_registry
            .put(AgentEntry::new(Arc::new(
                elysper::harness::ReActAgent::new(
                    Arc::clone(&default_provider),
                    Arc::new(ToolRegistry::new()),
                ),
            )))
            .await;

        // The fixture's management surfaces read the services the
        // rows publish (see the reads after the mount).
        let workflows = WorkflowsState::build(&workspace_root);

        let boot_services = Arc::new(BootServices {
            tool_registry: Arc::clone(&tool_registry),
            agent_registry: Arc::clone(&agent_registry),
            plugin_host: plugin_host.clone(),
            // The fixture composes no host plugins, so there is
            // nothing for the entry to adopt; the row still mounts,
            // proving the empty set is the no-op the boot shape
            // promises a deployment without `[plugins]`.
            process_registrations: std::sync::Mutex::new(None),
        });
        let mut profile = native_profile();
        // The fixture's row config carries the demo provider the
        // deployment catalogue would otherwise inject under `cfg(test)`;
        // the factory builds it from the row like a real deployment.
        let value = serde_json::json!({
            "workspace": {
                "providers": {
                    "demo": {
                        "type": "demo",
                        "api_key_env": "NONE",
                        "default_model": default_model,
                    }
                },
                "default_provider": "demo",
                "default_model": default_model,
            }
        });
        if let Some(row) = profile
            .core
            .iter_mut()
            .find(|entry| entry.id == "model-adapter")
        {
            row.config = value;
        }
        let (kernel, mounted) = mount_profile(
            Arc::clone(&boot_services),
            &profile,
            elysper::core::SharedClock::system(),
            crate::composition::kernel::BootExtras {
                workspace_root: workspace_root.clone(),
                blocking: Some(crate::spawner::server_blocking()),
                compaction: Some(elysper::session::CompactionPluginConfig {
                    model: default_model.clone(),
                    settings: elysper::session::CompactionSettings::default(),
                }),
                sessions_dir: Some(
                    session_manager.sessions_root().to_path_buf(),
                ),
            },
        )
        .expect("the fixture's native plugin set mounts");

        // The effective assembly the fixture's tree corresponds to.
        let assembly = {
            let checks = crate::profile::KernelChecks(&kernel);
            crate::profile::effective(&profile, &checks).profile
        };
        let ui_tree = crate::ui::tree::load_ui_tree(&assembly.ui)
            .expect("the fixture's UI segment loads");

        let active_sessions: Arc<
            DashMap<(String, String), Arc<SessionController>>,
        > = Arc::new(DashMap::new());
        let subagent_router =
            Arc::new(crate::session::subagent_router::SubagentRouter::new(
                Arc::clone(&session_manager),
                Arc::clone(&active_sessions),
            ));

        // Workflow bridge over the same registry the fixture
        // registered its default agent into — cloned here because
        // the struct literal below moves the original.
        let workflow_host = super::workflow_host::as_arc(
            super::workflow_host::ServerWorkflowHost::new(Arc::clone(
                &agent_registry,
            )),
        );
        let mcp_supervisor = kernel
            .require::<elysper::mcp::McpSupervisor>(elysper::mcp::KEY_MCP)
            .expect("the fixture mounts the mcp row");
        // The fixture's route surfaces read the services the rows
        // published (one store / one engine), exactly like the boot.
        let schedules = SchedulesState::from_store(
            Arc::clone(
                kernel
                    .require::<elysper::scheduler::SchedulerService>(
                        elysper::scheduler::KEY_SCHEDULER,
                    )
                    .expect("the fixture mounts the scheduler row")
                    .store(),
            ),
            elysper::core::SharedClock::system(),
        );
        let eval_runner = kernel
            .require::<elysper::eval::EvalRunner>(elysper::eval::KEY_EVAL)
            .expect("the fixture mounts the eval row");
        let evals = EvalsState::build_with_runner(&workspace_root, eval_runner);
        let session_manager = kernel
            .require::<SessionRegistry>(elysper::session::KEY_SESSIONS)
            .expect("the fixture mounts the session-store row");
        let attachment_store = kernel
            .require::<elysper::attachment::AttachmentStore>(
                elysper::attachment::KEY_ATTACHMENTS,
            )
            .expect("the fixture mounts the attachments row");
        let mcp = McpRuntime::detached(mcp_supervisor);
        Self {
            clock,
            cluster: Arc::new(crate::cluster::ClusterRuntime::local()),
            tool_registry,
            agent_registry,
            session_search,
            session_manager,
            workspace_root: workspace_root.clone(),
            default_model,
            workspace_config: parking_lot::RwLock::new(workspace_config),
            default_provider: parking_lot::RwLock::new(default_provider),
            provider_catalogue: parking_lot::RwLock::new(provider_catalogue),
            operations_config,
            system_prompt: elysper::harness::DEFAULT_SYSTEM_PROMPT.to_string(),
            server_config,
            default_max_iterations: elysper::harness::DEFAULT_MAX_ITERATIONS,
            default_compaction: None,
            tool_surface: AppliedToolSurface::default(),
            tool_defs_cache: parking_lot::RwLock::new(None),
            mcp,
            kernel,
            boot_services,
            mount: parking_lot::Mutex::new(mounted.into_groups()),
            profile: parking_lot::RwLock::new(profile),
            assembly: parking_lot::RwLock::new(assembly),
            profile_store: None,
            profile_write: tokio::sync::Mutex::new(()),
            ui_tree: parking_lot::RwLock::new(ui_tree),
            // The fixture composes no host plugins; its host is empty
            // and already bound above.
            plugin_host,
            #[cfg(any(test, feature = "test-utils"))]
            provider_override: parking_lot::Mutex::new(None),
            #[cfg(any(test, feature = "test-utils"))]
            catalogue_override: parking_lot::Mutex::new(None),
            plugins: crate::composition::Composition::empty(),
            active_sessions,
            spawn_gates: Arc::new(DashMap::new()),
            spawn_gate_timeout: super::sessions::COLD_OPEN_GATE_TIMEOUT,
            subagent_router,
            default_agent_name: parking_lot::RwLock::new(None),
            usage_metrics: Arc::new(UsageMetrics::default()),
            attachment_store,
            schedules,
            workflows,
            workflow_host,
            evals,
            rpc: Arc::new(crate::rpc::RpcRuntime::detached()),
        }
    }
}
