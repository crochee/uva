//! Per-session run configuration types for the HTTP server's
//! [`SessionController`](crate::session::controller::SessionController).
//!
//! [`AgentRunConfig`] carries the inputs the run factory requires;
//! [`RunOverrides`] carries per-run optional overrides. The
//! server layer wraps a config with these; the factory applies
//! them after construction so setter order is irrelevant.
//!
//! These types moved out of `uva-harness` (R126 refactor):
//! they describe how *this server* drives an agent run, not what
//! the agent loop itself needs. Library consumers that assemble
//! their own driver do not name them.

use std::{path::PathBuf, sync::Arc};

use uva::{
    agent::{AgentRegistry, PromptContext},
    context::ContextManager,
    harness::ReasoningStrategy,
    provider::traits::ModelProvider,
    session::{Compactor, SessionLog},
    tool::{ToolRegistry, ToolRestriction, ToolSurfacePolicy},
};

/// Per-session configuration consumed by the run factory inside
/// `SessionController` — provider, tool registry, workspace, system
/// prompt, the per-session manifest and the optional sync resolver
/// used to bind a named agent to its
/// [`AgentDescriptor`](uva::harness::AgentDescriptor).
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
    pub log_writer: Option<Arc<dyn SessionLog>>,
    /// The deployment's host: the steering policy (guards / hooks /
    /// hints / tracker) plus the plugins installed at boot. The run
    /// factory passes a clone to the agent it builds, so the policy
    /// and the plugin tools apply to chat runs too. `None` (tests,
    /// embedders) leaves the agent with the harness's empty default
    /// host.
    pub host: Option<uva::harness::Host>,
    /// The mounted `agentLoop` factory; the run's deployment-level
    /// knobs (strategy, restriction, context manager, compactor,
    /// spawner) resolve through it. The server always
    /// wires it (session creation fails when the service is parked or
    /// unloaded); `None` is the embedder/test shape and builds the
    /// agent directly from this config.
    pub agent_loop: Option<Arc<dyn uva::harness::plugin::AgentLoopFactory>>,
    /// Deployment-level tool-surface policy. The factory's defaults
    /// do not carry it — the boot computes the policy after the
    /// mount, from the registry's post-mount state — so it travels
    /// per-run.
    pub tool_surface: Option<ToolSurfacePolicy>,
}

/// Per-run overrides applied on top of [`AgentRunConfig`].
///
/// Deprecated placeholder: the agent-loop factory's defaults
/// absorbed every field (the run keeps only what is genuinely
/// per-run). Carried until the remaining call sites migrate.
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
