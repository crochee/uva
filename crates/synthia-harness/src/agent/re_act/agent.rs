//! [`ReActAgent`]: the public agent type, its constructors, the [`Agent`](crate::agent::Agent) / [`RegistryItem`](synthia_core::registry::RegistryItem) impls, and the per-run runtime helpers. State machine lives in the `loop` module.
use std::{path::PathBuf, pin::Pin, sync::Arc};

use async_trait::async_trait;
use futures::{Stream, channel::mpsc};
use synthia_context::ContextManager;
use synthia_core::{agent::AgentDescriptor, registry::RegistryItem};
use synthia_provider::ModelProvider;
use synthia_session::TypedEventSink;
use synthia_steering::Steering;
use synthia_tool::{ToolRegistry, ToolRestriction, ToolSurfacePolicy};

use crate::{
    AgentCaps,
    agent::{
        AgentRuntime,
        ReasoningStrategy,
        checkpoint::Checkpointer,
        re_act::synthetic::SyntheticTool,
    },
    events::AgentEvent,
    input::AgentInput,
    log::LogWriter,
    prompt::PromptContext,
};

/// Concrete [`Agent`](crate::agent::Agent) implementing the ReAct loop. Holds an [`AgentCaps`] bundle; per-run state (cancel +
/// `subagent_depth`) comes from each [`Agent::run`](crate::agent::Agent::run) call's `input` and `cancel`.
pub struct ReActAgent {
    /// Immutable capability bundle; every setter rewrites one field, and
    /// `Clone` is cheap because every field holds an `Arc`.
    pub(super) caps: AgentCaps,
}

impl ReActAgent {
    /// Canonical constructor: assembles a [`ReActAgent`] with the harness's default descriptor + system prompt +
    /// `TruncatingContextManager` + `Steering::noop()` + `InlineSpawner` + `SharedClock::system()`. Every other knob keeps its
    /// default; per-run overrides go through the `with_*` setters.
    #[must_use]
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        tool_registry: Arc<ToolRegistry>,
    ) -> Self {
        Self::with_descriptor(
            provider,
            tool_registry,
            PathBuf::new(),
            super::descriptor::default_descriptor(
                crate::prompt::DEFAULT_SYSTEM_PROMPT.to_string(),
            ),
            Arc::new(PromptContext::default()),
        )
    }

    /// Assemble an agent around a caller-supplied descriptor, prompt context,
    /// and workspace root. Every other field takes the default
    /// [`Self::new`] would have chosen.
    #[must_use]
    pub fn with_descriptor(
        provider: Arc<dyn ModelProvider>,
        tool_registry: Arc<ToolRegistry>,
        workspace_root: PathBuf,
        descriptor: AgentDescriptor,
        prompt_context: Arc<PromptContext>,
    ) -> Self {
        Self {
            caps: AgentCaps::defaults(
                provider,
                tool_registry,
                workspace_root,
                descriptor,
                prompt_context,
            ),
        }
    }
}

#[async_trait]
impl crate::agent::Agent for ReActAgent {
    fn descriptor(&self) -> &crate::agent::AgentDescriptor {
        &self.caps.model.descriptor
    }

    async fn run(
        &self,
        input: AgentInput,
        cancel: Arc<dyn synthia_core::CancelToken>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'static>> {
        let (tx, rx) = mpsc::unbounded::<AgentEvent>();
        let sink = crate::agent::sink::EventSink::new(
            Arc::new(tx),
            self.caps.env.log_writer.clone(),
            self.caps.env.typed_sink.clone(),
        );
        let runtime = AgentRuntime {
            caps: Arc::new(self.caps.clone()),
            cancel,
            subagent_depth: input.subagent_depth,
        };
        let strategy = Arc::clone(&runtime.caps.strategy);
        let spawner = Arc::clone(&runtime.caps.env.spawner);
        let sink_for_panic = sink.clone();
        spawner.spawn(Box::pin(async move {
            if let Err(message) = crate::agent::re_act::panic::catching_panics(
                strategy.run(runtime, input, &sink),
            )
            .await
            {
                sink_for_panic.fail(format!("strategy panicked: {message}"));
            }
        }));
        Box::pin(rx)
    }
}

impl RegistryItem for ReActAgent {
    fn name(&self) -> &str {
        &self.caps.model.descriptor.name
    }

    fn description(&self) -> &str {
        &self.caps.model.descriptor.description
    }
}

impl ReActAgent {
    #[must_use]
    pub fn with_spawner(
        mut self,
        spawner: Arc<dyn synthia_core::spawn::Spawner>,
    ) -> Self {
        self.caps.env.spawner = spawner;
        self
    }

    #[must_use]
    pub fn with_steering(mut self, steering: Arc<Steering>) -> Self {
        self.caps.control.steering = steering;
        self
    }

    #[must_use]
    pub fn with_context_manager(
        mut self,
        manager: Arc<dyn ContextManager>,
    ) -> Self {
        self.caps.control.context_manager = manager;
        self
    }

    /// Install a synthetic-tool plugin. Returns `Err` with the name when an installed synthetic tool already advertises it —
    /// same-name dispatch has no defined precedence.
    pub fn with_synthetic_tool(
        mut self,
        tool: Arc<dyn SyntheticTool>,
    ) -> Result<Self, String> {
        let defs = tool.definitions();
        validate_synthetic(&self.caps.tools.synthetic, &defs)?;
        self.caps.tools.synthetic.push(tool);
        Ok(self)
    }

    #[must_use]
    pub fn with_max_iterations(mut self, max_iterations: usize) -> Self {
        self.caps.control.max_iterations = max_iterations.clamp(1, 4096);
        self
    }

    #[must_use]
    pub fn max_iterations(&self) -> usize {
        self.caps.control.max_iterations
    }

    #[must_use]
    pub fn with_tool_surface(mut self, policy: ToolSurfacePolicy) -> Self {
        self.caps.tools.surface = Some(Arc::new(policy));
        self
    }

    #[must_use]
    pub fn with_tool_restriction(
        mut self,
        restriction: ToolRestriction,
    ) -> Self {
        self.caps.tools.restriction = Some(Arc::new(restriction));
        self
    }

    #[must_use]
    pub fn with_strategy(
        mut self,
        strategy: Arc<dyn ReasoningStrategy>,
    ) -> Self {
        self.caps.strategy = strategy;
        self
    }

    #[must_use]
    pub fn with_checkpointer(
        mut self,
        checkpointer: Arc<dyn Checkpointer>,
    ) -> Self {
        self.caps.env.checkpointer = Some(checkpointer);
        self
    }

    #[must_use]
    pub fn with_workspace(mut self, root: impl Into<PathBuf>) -> Self {
        self.caps.env.workspace_root = root.into();
        self
    }

    #[must_use]
    pub fn with_typed_event_sink(mut self, sink: TypedEventSink) -> Self {
        self.caps.env.typed_sink = Some(sink);
        self
    }

    /// Install the run's log writer.
    ///
    /// The writer stamps each structural row's ordinal, buffers it,
    /// and flushes the buffer at every step boundary — the only
    /// moment the run waits for the disk. Without a writer the
    /// structural rows go to the typed sink the host wired.
    #[must_use]
    pub fn with_log_writer(mut self, writer: Arc<LogWriter>) -> Self {
        self.caps.env.log_writer = Some(writer);
        self
    }

    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.caps.model.descriptor.name = name.into();
        self
    }

    #[must_use]
    pub fn with_instructions(
        mut self,
        instructions: impl Into<String>,
    ) -> Self {
        self.caps.model.descriptor.instructions = instructions.into();
        self
    }

    /// Declare a structured-output JSON Schema: stamps the descriptor so the prompt assembler adds the requirement, and registers
    /// the matching `structured_output` tool so the model can answer the run through it. The tool is built first, so both always
    /// carry the same schema; a registry that already holds `structured_output` (two agents sharing one [`ToolRegistry`]) keeps
    /// the first schema and logs the conflict, so the setter stays infallible and chainable.
    #[must_use]
    pub fn with_output_schema(mut self, schema: serde_json::Value) -> Self {
        let tool = synthia_tool::structured_output_tool(schema.clone());
        self.caps.model.descriptor.output_schema = Some(schema.to_string());
        let inserted = self
            .caps
            .tools
            .registry
            .register_entry(synthia_tool::ToolEntry::new(tool));
        if !inserted {
            tracing::warn!(
                agent = %self.caps.model.descriptor.name,
                "structured_output already registered; keeping the first schema"
            );
        }
        self
    }
}

/// Reject a synthetic tool whose advertised name an installed one already claims: same-name dispatch has no defined precedence,
/// so the loop cannot tell which plugin should handle the call. The `Result` carries the conflicting tool name. The candidate's
/// already-built [`definitions`](SyntheticTool::definitions) arrive by reference — callers that hold them (the loop prebuilds
/// every definition once per run) never rebuild them here. Existing plugins are asked
/// [`claims`](SyntheticTool::claims) rather than `definitions`: building the advertised
/// `Vec<ToolDefinition>` per already-installed plugin made installing n plugins O(n²) allocations.
pub(crate) fn validate_synthetic(
    existing: &[Arc<dyn SyntheticTool>],
    new_definitions: &[synthia_provider::ToolDefinition],
) -> Result<(), String> {
    for def in new_definitions {
        if existing.iter().any(|i| i.claims(&def.name)) {
            return Err(def.name.clone());
        }
    }
    Ok(())
}
