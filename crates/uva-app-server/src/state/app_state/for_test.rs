//! The `test-utils` constructor: an `AppState` with in-memory
//! components and a fake provider, mirroring the production
//! wiring so route-level tests observe realistic state.

use std::{path::PathBuf, sync::Arc};

use uva::{
    agent::{AgentEntry, AgentRegistry},
    core::registry::Registry,
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
    ProviderCatalogue,
    boot::{BootInputs, BootMode, ProviderSeam},
    tools::plugin_tool_registry,
};
use crate::composition::kernel::native_profile;

impl AppState {
    /// Create an AppState for testing with in-memory components and no LLM provider dependency.
    ///
    /// Unlike the production `new()`, this uses `FakeProvider` with empty
    /// responses. The fixture mounts the same native plugin set the
    /// production boot mounts (so the `skill` tool, the capability
    /// services, and the prompt builder are the real ones), over
    /// in-memory components where the production boot uses disk.
    ///
    /// Construction goes through the same shared assembler as the
    /// production boot (`AppState::assemble`): this function resolves
    /// only the values a fixture differs on (the provider, the profile
    /// document, the detached runtimes) and hands the rest to the
    /// shared path.
    #[cfg(any(test, feature = "test-utils"))]
    pub async fn for_test(
        session_manager: SessionRegistry,
        workspace_root: PathBuf,
    ) -> Self {
        let workspace_config = WorkspaceConfig::from_env();
        let default_model = workspace_config.default_model.clone();

        // Create a minimal provider that returns empty responses (for route-level tests)
        let default_provider: Arc<dyn ModelProvider> =
            Arc::new(uva::test_support::FakeProvider::new(vec![]));

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
        let plugin_host = uva::harness::Host::new();
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

        // Same seams the production boot installs: the tests drive a
        // tokio runtime, so the fs work belongs on its blocking pool.
        // The session store the mount publishes is built from this
        // registry's directory; this handle supplies the directory and
        // nothing else.
        let session_manager = Arc::new(
            session_manager.with_blocking(crate::spawner::server_blocking()),
        );
        let session_search: SharedSessionSearch = Arc::new(
            JsonlSessionSearch::new(workspace_root.join("sessions"))
                .with_blocking(crate::spawner::server_blocking()),
        );

        // Test state: register the canonical ReAct descriptor (without
        // holding a live provider) so the registry has at least one
        // entry and the peer-agent manifest snapshot excludes it via
        // the `self_name` filter.
        let agent_registry = Arc::new(AgentRegistry::new());
        let _ = agent_registry
            .put(AgentEntry::new(Arc::new(uva::harness::ReActAgent::new(
                Arc::clone(&default_provider),
                Arc::new(ToolRegistry::new()),
            ))))
            .await;

        let mut profile = native_profile();
        // The fixture's row config carries the demo provider the
        // deployment catalogue would otherwise inject under `cfg(test)`;
        // the factory builds it from the row like a real deployment.
        let value = serde_json::json!({
            "providers": {
                "demo": {
                    "type": "demo",
                    "api_key_env": "NONE",
                    "default_model": default_model.clone(),
                }
            },
            "default_provider": "demo",
            "default_model": default_model.clone(),
        });
        if let Some(row) = profile
            .core
            .iter_mut()
            .find(|entry| entry.id == "model-adapter")
        {
            row.config = value;
        }

        Self::assemble(BootInputs {
            workspace_root: workspace_root.clone(),
            server_config,
            profile,
            profile_store: None,
            default_model,
            providers: ProviderSeam::Injected {
                catalogue: provider_catalogue,
                default_provider,
                workspace: workspace_config,
            },
            tool_registry,
            agent_registry,
            plugin_host,
            session_search,
            sessions_dir: Some(session_manager.sessions_root().to_path_buf()),
            default_compaction: None,
            operations_config,
            default_agent_name: None,
            default_max_iterations: uva::harness::DEFAULT_MAX_ITERATIONS,
            cluster: Arc::new(crate::cluster::ClusterRuntime::local()),
            rpc: Arc::new(crate::rpc::RpcRuntime::detached()),
            messages: Arc::new(
                crate::session::messages::SessionMessages::new(),
            ),
            mode: BootMode::Fixture,
        })
        .await
        .expect("the fixture's native plugin set mounts")
    }
}
