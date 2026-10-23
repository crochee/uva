//! [`AgentCaps`] (immutable capability bundle owned by the agent) and [`AgentRuntime`] (per-run view passed to strategies). The bundle is grouped into planes so every consumer reads the axis it cares about: [`ModelCaps`] (what to sample), [`ToolCaps`] (what may be called), [`ControlCaps`] (how the loop is steered and bounded), [`EnvCaps`] (where and when it runs).
use std::{fmt, path::PathBuf, sync::Arc};

use synthia_context::ContextManager;
use synthia_core::{
    CancelToken,
    SharedClock,
    agent::AgentDescriptor,
    spawn::Spawner,
};
use synthia_provider::ModelProvider;
use synthia_session::TypedEventSink;
use synthia_steering::Steering;
use synthia_tool::{ToolRegistry, ToolRestriction, ToolSurfacePolicy};

use crate::{
    ReasoningStrategy,
    agent::{
        checkpoint::Checkpointer,
        re_act::{ReActStrategy, synthetic::SyntheticTool},
    },
    log::LogWriter,
    prompt::PromptContext,
};

#[derive(Clone)]
pub struct ModelCaps {
    pub provider: Arc<dyn ModelProvider>,
    pub descriptor: AgentDescriptor,
    pub prompt_context: Arc<PromptContext>,
}

#[derive(Clone)]
pub struct ToolCaps {
    pub registry: Arc<ToolRegistry>,
    pub surface: Option<Arc<ToolSurfacePolicy>>,
    pub restriction: Option<Arc<ToolRestriction>>,
    pub synthetic: Vec<Arc<dyn SyntheticTool>>,
}

#[derive(Clone)]
pub struct ControlCaps {
    pub steering: Arc<Steering>,
    pub max_iterations: usize,
    pub context_manager: Arc<dyn ContextManager>,
}

#[derive(Clone)]
pub struct EnvCaps {
    pub workspace_root: PathBuf,
    pub clock: SharedClock,
    pub spawner: Arc<dyn Spawner>,
    pub checkpointer: Option<Arc<dyn Checkpointer>>,
    /// The run's log writer. The loop stamps and buffers the
    /// structural rows through it and flushes them at the step
    /// boundary.
    pub log_writer: Option<Arc<LogWriter>>,
    pub typed_sink: Option<TypedEventSink>,
}

#[derive(Clone)]
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
                synthetic: Vec::new(),
            },
            control: ControlCaps {
                steering: Arc::new(Steering::noop()),
                max_iterations: crate::agent::re_act::DEFAULT_MAX_ITERATIONS,
                context_manager: Arc::new(
                    synthia_context::TruncatingContextManager,
                ),
            },
            env: EnvCaps {
                workspace_root,
                clock: SharedClock::system(),
                spawner: Arc::new(synthia_core::spawn::InlineSpawner),
                checkpointer: None,
                log_writer: None,
                typed_sink: None,
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
