//! Per-session run configuration types for the HTTP server's
//! [`SessionController`](crate::session::controller::SessionController).
//!
//! [`AgentRunConfig`] carries the inputs the run factory requires;
//! [`RunOverrides`] carries per-run optional overrides. The
//! server layer wraps a config with these; the factory applies
//! them after construction so setter order is irrelevant.
//!
//! These types moved out of `elysper-harness` (R126 refactor):
//! they describe how *this server* drives an agent run, not what
//! the agent loop itself needs. Library consumers that assemble
//! their own driver do not name them.

use std::{path::PathBuf, sync::Arc};

use elysper::{
    context::ContextManager,
    harness::{AgentRegistry, PromptContext, ReasoningStrategy},
    provider::traits::ModelProvider,
    session::{Compactor, LogWriter},
    tool::{ToolRegistry, ToolRestriction, ToolSurfacePolicy},
};

/// Per-session configuration consumed by the run factory inside
/// `SessionController` — provider, tool registry, workspace, system
/// prompt, the per-session manifest and the optional sync resolver
/// used to bind a named agent to its
/// [`AgentDescriptor`](elysper::harness::AgentDescriptor).
#[derive(Clone)]
pub struct AgentRunConfig {
    /// The model backing the run.
    pub provider: Arc<dyn ModelProvider>,
    /// The tools the run may advertise and dispatch.
    pub tool_registry: Arc<ToolRegistry>,
    /// Absolute working directory passed to built-in tools.
    pub workspace_root: PathBuf,
    /// System prompt injected as the first message of every
    /// conversation.
    pub system_prompt: String,
    /// Manifest context (skills + peer agents + tool manifest).
    pub prompt_context: Arc<PromptContext>,
    /// Selected agent name; resolves through `agent_registry`.
    pub agent_name: Option<String>,
    /// Per-run iteration cap (clamped `[1, 4096]`).
    pub max_iterations: Option<usize>,
    /// Named agent catalog.
    pub agent_registry: Option<Arc<AgentRegistry>>,
    /// The session's one writer: the agent's structural boundary
    /// events go into it.
    pub log_writer: Option<Arc<LogWriter>>,
    /// Writer source resolving the log writer of a session a
    /// delegated child runs in. The run factory installs it on
    /// the agent, so the `task` delegator hands each child its
    /// own session's writer and the child's harness writes its
    /// model facts there.
    pub subagent_writers: Option<Arc<dyn elysper::harness::SubagentWriters>>,
    /// The deployment's host: the steering policy (guards / hooks /
    /// hints / tracker) plus the plugins installed at boot. The run
    /// factory passes a clone to the agent it builds, so the policy
    /// and the plugin tools apply to chat runs too. `None` (tests,
    /// embedders) leaves the agent with the harness's empty default
    /// host.
    pub host: Option<elysper::harness::Host>,
}

/// Per-run overrides applied on top of [`AgentRunConfig`].
#[derive(Default, Clone)]
pub struct RunOverrides {
    /// Explicit context-manager override.
    pub context_manager: Option<Arc<dyn ContextManager>>,
    /// Deployment-level tool-surface policy.
    pub tool_surface: Option<ToolSurfacePolicy>,
    /// The reasoning loop this run should use.
    pub strategy: Option<Arc<dyn ReasoningStrategy>>,
    /// The agent's own allow/deny list.
    pub tool_restriction: Option<Arc<ToolRestriction>>,
    /// The deployment's compaction engine, installed when the
    /// configured policy is enabled and valid.
    pub compactor: Option<Arc<dyn Compactor>>,
}
