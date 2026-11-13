//! [`AgentCaps`] (immutable capability bundle owned by the agent) and [`AgentRuntime`] (per-run view passed to strategies). The bundle is grouped into planes so every consumer reads the axis it cares about: [`ModelCaps`] (what to sample), [`ToolCaps`] (what may be called), [`ControlCaps`] (how the loop is steered and bounded), [`EnvCaps`] (where and when it runs).
use std::{fmt, path::PathBuf, sync::Arc};

use uva_context::ContextManager;
use uva_core::{
    CancelToken,
    SharedClock,
    agent::AgentDescriptor,
    spawn::Spawner,
};
use uva_provider::ModelProvider;
use uva_session::{Compactor, SessionLog};
use uva_tool::{ToolRegistry, ToolRestriction, ToolSurfacePolicy};

use crate::{
    ReasoningStrategy,
    agent::{
        approval::ApprovalPolicy,
        re_act::{ReActStrategy, delegation::DelegatingTool},
    },
    prompt::PromptContext,
    seams::Seams,
};

#[derive(Clone)]
/// What to sample: the provider, the agent descriptor and the prompt manifest.
pub struct ModelCaps {
    pub provider: Arc<dyn ModelProvider>,
    pub descriptor: AgentDescriptor,
    pub prompt_context: Arc<PromptContext>,
}

#[derive(Clone)]
/// What may be called: the registry, the surface policy, the restriction and the agent's own delegators.
pub struct ToolCaps {
    pub registry: Arc<ToolRegistry>,
    pub surface: Option<Arc<ToolSurfacePolicy>>,
    pub restriction: Option<Arc<ToolRestriction>>,
    /// The delegating tools the agent installs directly. The loop merges
    /// each host-registered delegator into the same name-keyed table, so a
    /// plugin may install either way.
    pub delegators: Vec<Arc<dyn DelegatingTool>>,
}

#[derive(Clone)]
/// How the loop is steered and bounded: the seam layer, iteration cap, context manager, approval policy and compactor.
pub struct ControlCaps {
    pub max_iterations: usize,
    pub context_manager: Arc<dyn ContextManager>,
    /// The agent's seam layer: the registrations a plugin installed and
    /// the four interception points the loop dispatches through. One
    /// seam layer belongs to one agent — there is no global one.
    pub seams: Seams,
    /// Which calls the run stops to ask about. The default holds
    /// high-risk calls; a low-risk call never waits.
    pub approval: ApprovalPolicy,
    /// The run's compactor. `None` — the default — means the loop
    /// never compacts; the context manager's request-local pruning
    /// is a separate, always-on pass.
    pub compactor: Option<Arc<dyn Compactor>>,
}

#[derive(Clone)]
/// Where and when a run executes: workspace, clock, spawner, writers and the live-event capacity.
pub struct EnvCaps {
    pub workspace_root: PathBuf,
    pub clock: SharedClock,
    pub spawner: Arc<dyn Spawner>,
    /// The run's log writer. The loop stamps and buffers the
    /// structural rows through it and flushes them at the step
    /// boundary.
    pub log_writer: Option<Arc<dyn SessionLog>>,
    /// Writer source resolving the log writer of a session a
    /// delegated child runs in. The loop hands it to delegating
    /// tools (the `task` delegator), which resolve each child's
    /// writer before starting the child run. `None` (the
    /// default) means this agent delegates without persisting
    /// its children.
    pub subagent_writers: Option<Arc<dyn crate::input::SubagentWriters>>,
    /// How many live events the run buffers before dropping them.
    /// The durable log is the record; this is the notification
    /// channel's bound.
    pub event_capacity: usize,
}

#[derive(Clone)]
/// The immutable capability bundle one agent owns, grouped into the four planes above.
pub struct AgentCaps {
    pub strategy: Arc<dyn ReasoningStrategy>,
    pub model: ModelCaps,
    pub tools: ToolCaps,
    pub control: ControlCaps,
    pub env: EnvCaps,
}

impl AgentCaps {
    /// The bundle every [`ReActAgent`](crate::ReActAgent) starts from; each plane's default lives here and nowhere else.
    #[must_use]
    pub(crate) fn defaults(
        provider: Arc<dyn ModelProvider>,
        registry: Arc<ToolRegistry>,
        workspace_root: PathBuf,
        descriptor: AgentDescriptor,
        prompt_context: Arc<PromptContext>,
    ) -> Self {
        Self {
            strategy: Arc::new(ReActStrategy),
            model: ModelCaps {
                provider,
                descriptor,
                prompt_context,
            },
            tools: ToolCaps {
                registry,
                surface: None,
                restriction: None,
                delegators: Vec::new(),
            },
            control: ControlCaps {
                max_iterations: crate::agent::re_act::DEFAULT_MAX_ITERATIONS,
                context_manager: Arc::new(
                    uva_context::TruncatingContextManager,
                ),
                seams: Seams::new(crate::agent::host::Host::new()),
                approval: ApprovalPolicy::default(),
                compactor: None,
            },
            env: EnvCaps {
                workspace_root,
                clock: SharedClock::system(),
                spawner: Arc::new(uva_core::spawn::InlineSpawner),
                log_writer: None,
                subagent_writers: None,
                event_capacity: crate::agent::sink::DEFAULT_EVENT_CAPACITY,
            },
        }
    }

    /// The bundle a foreign [`ReasoningStrategy`] needs when it has only a provider: the harness defaults for descriptor,
    /// prompt, registry, steering, iteration bound and context manager.
    #[must_use]
    pub fn for_provider(provider: Arc<dyn ModelProvider>) -> Self {
        Self::defaults(
            provider,
            Arc::new(ToolRegistry::new()),
            PathBuf::new(),
            crate::agent::descriptor::default_descriptor(
                crate::prompt::DEFAULT_SYSTEM_PROMPT.to_string(),
            ),
            Arc::new(PromptContext::default()),
        )
    }

    /// The manifest one run renders: the per-dispatch context when the caller
    /// supplied one, else the agent's default, plus the host's prompt sections
    /// in registration order.
    ///
    /// Take it **once per run** — the loop's constructor does, and the
    /// single-shot strategy once per request. That boundary is the contract: a
    /// section a plugin registers *during* a run lands on the next run, never
    /// mid-run, because the system prompt has to stay byte-stable within a run
    /// for the provider's prefix cache to hold. With no sections registered
    /// this shares the manifest (`Arc::clone`); only a run that actually has a
    /// plugin section pays for the copy.
    pub(crate) fn run_prompt_context(
        &self,
        dispatch: Option<&Arc<PromptContext>>,
    ) -> Arc<PromptContext> {
        let base = dispatch.unwrap_or(&self.model.prompt_context);
        let sections = self.control.seams.prompt_sections();
        if sections.is_empty() {
            return Arc::clone(base);
        }
        Arc::new(base.as_ref().clone().with_sections(sections))
    }
}

/// Per-run view handed to a [`ReasoningStrategy::run`] call: owned [`Arc<AgentCaps>`] plus the per-run binding. The `Arc` lets
/// the strategy move the runtime into a spawned task without making every [`AgentCaps`] `Arc`-wrapped.
pub struct AgentRuntime {
    pub caps: Arc<AgentCaps>,
    pub cancel: Arc<dyn CancelToken>,
    pub subagent_depth: usize,
}

impl fmt::Debug for AgentRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AgentRuntime")
            .field("provider", &self.caps.model.provider.name())
            .field("tools", &self.caps.tools.registry.tool_count())
            .field("agent", &self.caps.model.descriptor.name)
            .field("max_iterations", &self.caps.control.max_iterations)
            .field("subagent_depth", &self.subagent_depth)
            .finish_non_exhaustive()
    }
}
