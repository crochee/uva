//! The `test-utils` constructor: an `AppState` with in-memory
//! components and a fake provider, mirroring the production
//! wiring so route-level tests observe realistic state.

use std::{path::PathBuf, sync::Arc};

use dashmap::DashMap;
use synthia::{
    core::registry::Registry,
    harness::{AgentEntry, AgentRegistry},
    provider::{config::WorkspaceConfig, traits::ModelProvider},
    session::{
        JsonlSessionSearch,
        SharedSessionSearch,
        manager::SessionRegistry,
    },
    skill::register_skill_tool,
    tool::ToolRegistry,
};

use super::{
    AppState,
    EvalsState,
    ProviderCatalogue,
    SchedulesState,
    WorkflowsState,
    tools::{AppliedToolSurface, plugin_tool_registry},
    usage::UsageMetrics,
};
use crate::{
    session::controller::SessionController,
    state::boot::build_prompt_context,
};

impl AppState {
    /// Create an AppState for testing with in-memory components and no LLM provider dependency.
    ///
    /// Unlike the production `new()`, this uses `FakeProvider` with empty
    /// responses. FragmentRegistry and SkillRegistry are initialized with
    /// the same built-in registrations as `new()` so that tests observe
    /// realistic wiring without needing external dependencies.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn for_test(
        session_manager: SessionRegistry,
        workspace_root: PathBuf,
    ) -> Self {
        let workspace_config = WorkspaceConfig::from_env();
        let default_model = workspace_config.default_model.clone();

        // Create a minimal provider that returns empty responses (for route-level tests)
        let default_provider: Arc<dyn ModelProvider> =
            Arc::new(synthia::test_support::FakeProvider::new(vec![]));

        // The catalogue every resolution path reads. A fixture builds
        // its own over whatever providers it wires (see
        // `ProviderCatalogue::with_handle`); this default one offers
        // exactly the fake, under the workspace's configured default
        // provider name, so `model: None` and an explicit selection of
        // that name both resolve.
        let provider_catalogue = ProviderCatalogue::default().with_handle(
            &workspace_config.default_provider,
            &default_model,
            Arc::clone(&default_provider),
        );

        let tool_registry_built = plugin_tool_registry();
        // Mirror the production wiring: the skill tool is part
        // of the default tool surface, not an opt-in extension.
        register_skill_tool(&tool_registry_built);
        let tool_registry = Arc::new(tool_registry_built);
        // The fixture composes no plugins, but the host exists and
        // is bound to the shared registry so every agent built from
        // this state has one place for plugin registrations — the
        // same shape the production boot installs.
        let plugin_host = synthia::harness::Host::new();
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

        let agent_registry = Arc::new(AgentRegistry::new());
        // Test state: register the canonical ReAct descriptor
        // (without holding a live provider) so the registry has
        // at least one entry and the peer-agent manifest snapshot
        // excludes it via the `self_name` filter.
        let _ = agent_registry
            .put(AgentEntry::new(Arc::new(
                synthia::harness::ReActAgent::new(
                    Arc::clone(&default_provider),
                    Arc::new(ToolRegistry::new()),
                ),
            )))
            .await;
        let prompt_context = Arc::new(
            build_prompt_context(&workspace_root, &agent_registry, "agent")
                .await,
        );

        let clock = synthia::core::SharedClock::system();
        // Same seams the production boot installs: the tests drive a
        // tokio runtime, so the fs work belongs on its blocking pool.
        let session_search: SharedSessionSearch = Arc::new(
            JsonlSessionSearch::new(workspace_root.join("sessions"))
                .with_blocking(crate::spawner::server_blocking()),
        );
        // Mirror the production wiring: an in-memory long-term
        // tier (tests seed entries through `Memory::store`) and
        // the cross-domain search service over it.
        let memory: Arc<dyn synthia::context::Memory> =
            Arc::new(synthia::context::InMemoryMemory::new());
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
            synthia::tool_search::register_search_tool(
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
        let evals = EvalsState::build(&workspace_root);

        let session_manager = Arc::new(
            session_manager.with_blocking(crate::spawner::server_blocking()),
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
        Self {
            clock,
            cluster: Arc::new(crate::cluster::ClusterRuntime::local()),
            tool_registry,
            agent_registry,
            session_search,
            session_manager,
            input_queue: crate::session::input_queue::InputQueue::default(),
            workspace_root: workspace_root.clone(),
            default_model,
            workspace_config,
            default_provider,
            provider_catalogue: Arc::new(provider_catalogue),
            auth_config,
            cors_config,
            operations_config,
            system_prompt: synthia::harness::DEFAULT_SYSTEM_PROMPT.to_string(),
            prompt_context,
            steering: Arc::new(synthia::steering::Steering::default_policy(
                workspace_root.clone(),
            )),
            server_config,
            default_max_iterations: synthia::harness::DEFAULT_MAX_ITERATIONS,
            default_compaction: None,
            default_strategy: None,
            default_tool_restriction: None,
            tool_surface: AppliedToolSurface::default(),
            tool_defs_cache: parking_lot::RwLock::new(None),
            mcp_clients: Vec::new(),
            mcp_health: Vec::new(),
            // The fixture composes no plugins; its host is empty
            // and already bound above.
            plugin_host,
            plugins: crate::composition::Composition::empty(),
            active_sessions,
            spawn_gates: Arc::new(DashMap::new()),
            spawn_gate_timeout: super::sessions::COLD_OPEN_GATE_TIMEOUT,
            subagent_router,
            default_agent_name: parking_lot::RwLock::new(None),
            usage_metrics: Arc::new(UsageMetrics::default()),
            attachment_store: Arc::new(
                synthia::attachment::AttachmentStore::new_fs(
                    workspace_root.join(".attachments"),
                )
                .unwrap_or_else(|_| {
                    synthia::attachment::AttachmentStore::default()
                }),
            ),
            memory,
            search,
            schedules,
            workflows,
            workflow_host,
            evals,
        }
    }
}
