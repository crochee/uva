//! The server's single `ReActAgent` assembly point.
//!
//! Four call sites used to thread the `with_*` builder chain
//! separately (boot's default member, the session run factory,
//! dynamic agent registration, the eval agent), so every new
//! setter had to be installed in all four — and the four could
//! drift. [`AgentAssembly`] collects the pieces a caller has and
//! [`AgentAssembly::build`] is the one place the builder chain
//! (and the synthetic-tool install `expect`) lives.

use std::{path::PathBuf, sync::Arc};

use synthia::{
    context::ContextManager,
    harness::{
        AgentDescriptor,
        PromptContext,
        ReActAgent,
        ReasoningStrategy,
        SyntheticTool,
    },
    provider::traits::ModelProvider,
    session::TypedEventSink,
    steering::Steering,
    tool::{ToolRegistry, ToolRestriction, ToolSurfacePolicy},
};

/// The collected inputs for one server-side `ReActAgent`. Every
/// field a caller lacks stays `None` / empty; [`build`] applies
/// only what is present, so the four sites keep their own shape
/// while sharing one chain.
pub(crate) struct AgentAssembly {
    pub provider: Arc<dyn ModelProvider>,
    pub tool_registry: Arc<ToolRegistry>,
    pub workspace_root: PathBuf,
    pub descriptor: AgentDescriptor,
    pub prompt_context: Arc<PromptContext>,
    pub steering: Arc<Steering>,
    pub max_iterations: usize,
    /// Synthetic-tool plugins (the `task` delegator); installed in
    /// order, and a name clash is a programming error (`.expect`).
    pub synthetic: Vec<Arc<dyn SyntheticTool>>,
    /// Deployment `[tools]` surface policy.
    pub tool_surface: Option<ToolSurfacePolicy>,
    /// Deployment reasoning loop.
    pub strategy: Option<Arc<dyn ReasoningStrategy>>,
    /// Deployment allow/deny list.
    pub tool_restriction: Option<Arc<ToolRestriction>>,
    /// Typed-event sink for structural boundary events.
    pub typed_event_sink: Option<TypedEventSink>,
    /// Caller-composed context manager.
    pub context_manager: Option<Arc<dyn ContextManager>>,
}

impl AgentAssembly {
    /// The one `with_*` chain in the server. Setter order is
    /// irrelevant (each setter rewrites one capability field).
    pub(crate) fn build(self) -> ReActAgent {
        let mut agent = ReActAgent::with_descriptor(
            self.provider,
            self.tool_registry,
            self.workspace_root,
            self.descriptor,
            self.prompt_context,
        )
        .with_spawner(crate::spawner::server_spawner());
        for tool in self.synthetic {
            agent = agent
                .with_synthetic_tool(tool)
                .expect("synthetic tool install must succeed");
        }
        agent = agent
            .with_steering(self.steering)
            .with_max_iterations(self.max_iterations);
        if let Some(policy) = self.tool_surface {
            agent = agent.with_tool_surface(policy);
        }
        if let Some(strategy) = self.strategy {
            agent = agent.with_strategy(strategy);
        }
        if let Some(restriction) = self.tool_restriction {
            agent = agent.with_tool_restriction((*restriction).clone());
        }
        if let Some(sink) = self.typed_event_sink {
            agent = agent.with_typed_event_sink(sink);
        }
        if let Some(manager) = self.context_manager {
            agent = agent.with_context_manager(manager);
        }
        agent
    }
}
