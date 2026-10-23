//! Per-session run configuration types for the HTTP server's
//! [`SessionController`](crate::session::controller::SessionController).
//!
//! [`AgentRunConfig`] carries the inputs the run factory requires;
//! [`RunOverrides`] carries per-run optional overrides. The
//! server layer wraps a config with these; the factory applies
//! them after construction so setter order is irrelevant.
//!
//! These types moved out of `synthia-harness` (R126 refactor):
//! they describe how *this server* drives an agent run, not what
//! the agent loop itself needs. Library consumers that assemble
//! their own driver do not name them.

use std::{path::PathBuf, sync::Arc};

use synthia::{
    context::ContextManager,
    harness::{AgentRegistry, PromptContext, ReasoningStrategy},
    provider::traits::ModelProvider,
    session::TypedEventSink,
    steering::Steering,
    tool::{ToolRegistry, ToolRestriction, ToolSurfacePolicy},
};

/// Per-session configuration consumed by the run factory inside
/// `SessionController` — provider, tool registry, workspace, system
/// prompt, the per-session manifest and the optional sync resolver
/// used to bind a named agent to its
/// [`AgentDescriptor`](synthia::harness::AgentDescriptor).
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
    /// Steering bundle (guards / hooks / hints / tracker).
    pub steering: Arc<Steering>,
    /// Named agent catalog.
    pub agent_registry: Option<Arc<AgentRegistry>>,
    /// Typed-event sink for structural boundary events.
    pub typed_event_sink: Option<TypedEventSink>,
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
}
