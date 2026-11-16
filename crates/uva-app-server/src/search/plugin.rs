//! The `search` kernel plugin.
//!
//! Publishes the six-domain [`SearchService`]
//! under [`KEY_SEARCH`] and owns the model-facing `search` tool: the
//! registration's lifetime is the profile row's, so unloading the row
//! withdraws both the service and the tool.

use std::sync::Arc;

use uva::{
    kernel::{
        Context,
        EffectHandle,
        PLUGIN_PROTOCOL_VERSION,
        Plugin as KernelPlugin,
        PluginError,
        PluginManifest,
        RuntimeKind,
    },
    tool::{KEY_TOOLS, RegistrationToken, ToolRegistry},
};

use super::{SearchService, SearchSources, SharedSessionSearch};

/// The service key the `search` service is registered under.
pub const KEY_SEARCH: &str = "search";

/// The plugin id the `search` manifest declares.
const SEARCH_ID: &str = "search";

/// Label of the effect that withdraws the `search` tool registration.
const SEARCH_TOOL_EFFECT: &str = "search-tool";

/// The `search` kernel plugin: provides [`KEY_SEARCH`] and registers
/// the `search` tool.
pub struct SearchServicePlugin {
    manifest: PluginManifest,
    agent_registry: Arc<uva::agent::AgentRegistry>,
    session_search: SharedSessionSearch,
    workspace_root: std::path::PathBuf,
    clock: uva::core::SharedClock,
}

impl SearchServicePlugin {
    /// Build the plugin over the boot-time sources it cannot inject
    /// (the agent registry and the session corpus) plus the boot
    /// context's workspace root and clock.
    #[must_use]
    pub fn new(
        agent_registry: Arc<uva::agent::AgentRegistry>,
        session_search: SharedSessionSearch,
        workspace_root: impl Into<std::path::PathBuf>,
        clock: uva::core::SharedClock,
    ) -> Self {
        let mut manifest =
            PluginManifest::new(SEARCH_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_SEARCH.to_string());
        manifest.inject.push(KEY_TOOLS.to_string());
        manifest.inject.push(uva::context::KEY_MEMORY.to_string());
        Self {
            manifest,
            agent_registry,
            session_search,
            workspace_root: workspace_root.into(),
            clock,
        }
    }
}

impl KernelPlugin for SearchServicePlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let tools = ctx.require::<ToolRegistry>(KEY_TOOLS)?;
        let memory = ctx
            .require::<Arc<dyn uva::context::Memory>>(uva::context::KEY_MEMORY)
            .map(|wrapped| Arc::clone(&wrapped))?;
        // `build` already returns the shared service.
        let service = SearchService::build(SearchSources {
            tool_registry: Arc::clone(&tools),
            agent_registry: Arc::clone(&self.agent_registry),
            workspace_root: self.workspace_root.clone(),
            memory: Arc::clone(&memory),
            session_search: Arc::clone(&self.session_search),
            clock: self.clock.clone(),
        });
        let provided = ctx.provide_arc(KEY_SEARCH, Arc::clone(&service))?;
        let Some(token): Option<RegistrationToken> =
            uva::tool_search::register_search_tool_with_token(
                &tools,
                service.registry(),
            )
        else {
            return Err(PluginError::new(
                "the `tools` registry refused the `search` tool: a core \
                 tool already owns the name",
            ));
        };
        let rollback = token.clone();
        let registry = Arc::clone(&tools);
        let unregister = ctx.effect(SEARCH_TOOL_EFFECT, move || {
            registry.unregister_by_token(token);
            Ok(())
        });
        let effect = match unregister {
            Ok(effect) => effect,
            Err(error) => {
                tools.unregister_by_token(rollback);
                return Err(error);
            }
        };
        Ok(vec![provided, effect])
    }
}
