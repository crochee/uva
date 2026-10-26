//! The server's single `ReActAgent` assembly point.
//!
//! Four call sites used to thread the `with_*` builder chain
//! separately (boot's default member, the session run factory,
//! dynamic agent registration, the eval agent), so every new
//! setter had to be installed in all four — and the four could
//! drift. [`AgentAssembly`] collects the pieces a caller has and
//! [`AgentAssembly::build`] is the one place the builder chain
//! (and the delegator install `expect`) lives.

use std::{path::PathBuf, sync::Arc};

use synthia::{
    context::ContextManager,
    harness::{
        AgentDescriptor,
        DelegatingTool,
        PromptContext,
        ReActAgent,
        ReasoningStrategy,
    },
    provider::traits::ModelProvider,
    session::LogWriter,
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
    /// Delegating tools (the `task` delegator); installed in order,
    /// and a name clash is a programming error (`.expect`).
    pub delegators: Vec<Arc<dyn DelegatingTool>>,
    /// Deployment `[tools]` surface policy.
    pub tool_surface: Option<ToolSurfacePolicy>,
    /// Deployment reasoning loop.
    pub strategy: Option<Arc<dyn ReasoningStrategy>>,
    /// Deployment allow/deny list.
    pub tool_restriction: Option<Arc<ToolRestriction>>,
    /// The session's one writer: the agent's structural boundary
    /// events go into it.
    pub log_writer: Option<Arc<LogWriter>>,
    /// Writer source resolving the sessions this agent's
    /// delegated children run in; installed so the `task`
    /// delegator can hand each child its own writer.
    pub subagent_writers: Option<Arc<dyn synthia::harness::SubagentWriters>>,
    /// Caller-composed context manager.
    pub context_manager: Option<Arc<dyn ContextManager>>,
    /// The deployment's plugin host.
    ///
    /// One host is built at boot: the composition installs every
    /// configured plugin into it, [`AppState`](crate::state::AppState)
    /// keeps it for the process lifetime, and every agent this server
    /// builds gets a clone. A clone shares the host's registrations
    /// (it is one policy, not a copy), so a plugin installed once is
    /// visible to every agent — and the per-run agents that serve
    /// chat are no exception.
    pub host: Option<synthia::harness::Host>,
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
        for tool in self.delegators {
            agent = agent
                .with_delegator(tool)
                .expect("delegator install must succeed");
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
        if let Some(writer) = self.log_writer {
            agent = agent.with_log_writer(writer);
        }
        if let Some(writers) = self.subagent_writers {
            agent = agent.with_subagent_writers(writers);
        }
        if let Some(manager) = self.context_manager {
            agent = agent.with_context_manager(manager);
        }
        if let Some(host) = self.host {
            agent = agent.with_host(host);
        }
        agent
    }
}
