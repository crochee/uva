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
    skill::SkillRegistry,
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
    tools::{AppliedToolSurface, plugin_tool_registry},
    usage::UsageMetrics,
};
use crate::{
    composition::kernel::{
        KernelHost,
        mount_profile,
        native_profile,
        scheduler_service,
    },
    session::controller::SessionController,
    state::boot::build_prompt_context,
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
        // Mirror the production wiring: an in-memory long-term
        // tier (tests seed entries through `Memory::store`) and
        // the cross-domain search service over it.
        let memory: Arc<dyn elysper::context::Memory> =
            Arc::new(elysper::context::InMemoryMemory::new());

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

        let attachment_store = Arc::new(
            elysper::attachment::AttachmentStore::new_fs(
                workspace_root.join(".attachments"),
            )
            .unwrap_or_else(|_| {
                elysper::attachment::AttachmentStore::default()
            }),
        );

        let search =
            crate::search::SearchService::build(crate::search::SearchSources {
                tool_registry: Arc::clone(&tool_registry),
                agent_registry: Arc::clone(&agent_registry),
                workspace_root: workspace_root.clone(),
                memory: Arc::clone(&memory),
                session_search: Arc::clone(&session_search),
                clock: clock.clone(),
            });
        {
            elysper::tool_search::register_search_tool(
                &tool_registry,
                search.registry(),
            );
        }

        // The three management subsystems under test. Schedules,
        // workflows and evals use the real stores rooted at the
        // test workspace — the persistence round-trip is part of
        // what these tests verify.
        let schedules = SchedulesState::build(&workspace_root);
        let workflows = WorkflowsState::build(&workspace_root);
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

        let skill_registry =
            Arc::new(SkillRegistry::discover_files(&workspace_root));
        let sandbox_backend: Arc<dyn elysper::tool_shell::SandboxBackend> =
            Arc::new(elysper::tool_shell::NoopBackend::new());
        let compactor: Arc<dyn elysper::session::Compactor> =
            Arc::new(elysper::session::SummaryCompactor::new(
                Arc::clone(&default_provider),
                default_model.clone(),
            ));
        let scheduler_service = scheduler_service(Arc::clone(&schedules.store));
        let delegator =
            Arc::new(TaskDelegator::new(Arc::clone(&agent_registry)));
        let mcp_supervisor = Arc::new(elysper::mcp::McpSupervisor::new());
        let kernel_host = Arc::new(KernelHost {
            workspace_root: workspace_root.clone(),
            provider_catalogue: Arc::clone(&provider_catalogue),
            tool_registry: Arc::clone(&tool_registry),
            session_registry: Arc::clone(&session_manager),
            sandbox_backend,
            sandbox_policy: elysper::tool_shell::ExecutionPolicy::default(),
            delegator,
            compactor,
            approval_policy: elysper::harness::ApprovalPolicy::default(),
            mcp_supervisor: Arc::clone(&mcp_supervisor),
            skill_registry,
            eval_runner,
            scheduler_service,
            attachment_store: Arc::clone(&attachment_store),
            plugin_host: plugin_host.clone(),
            // The fixture composes no host plugins, so there is
            // nothing for the entry to adopt; the row still mounts,
            // proving the empty set is the no-op the boot shape
            // promises a deployment without `[plugins]`.
            process_registrations: std::sync::Mutex::new(None),
            // Transitional agent-loop inputs (die with the host).
            default_strategy: None,
            default_tool_restriction: None,
            compaction_boot: None,
            // The server's tokio spawner: a run built through the
            // agent-loop factory detaches onto the runtime, exactly
            // like the production boot.
            spawner: Some(crate::spawner::server_spawner()),
            delegators: Vec::new(),
        });
        let profile = native_profile();
        let (kernel, mounted) = mount_profile(
            Arc::clone(&kernel_host),
            &profile,
            elysper::core::SharedClock::system(),
        )
        .expect("the fixture's native plugin set mounts");

        let prompt_context = Arc::new(
            build_prompt_context(&kernel, &agent_registry, "agent").await,
        );

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
            prompt_context,
            server_config,
            default_max_iterations: elysper::harness::DEFAULT_MAX_ITERATIONS,
            default_compaction: None,
            default_strategy: None,
            default_tool_restriction: None,
            tool_surface: AppliedToolSurface::default(),
            tool_defs_cache: parking_lot::RwLock::new(None),
            mcp,
            kernel,
            kernel_host,
            mount: parking_lot::Mutex::new(vec![mounted]),
            profile: parking_lot::RwLock::new(profile),
            ui_schemas: parking_lot::RwLock::new(Vec::new()),
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
            memory,
            search,
            schedules,
            workflows,
            workflow_host,
            evals,
            rpc: Arc::new(crate::rpc::RpcRuntime::detached()),
        }
    }
}
