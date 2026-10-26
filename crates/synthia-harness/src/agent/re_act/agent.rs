//! [`ReActAgent`]: the public agent type, its constructors, the [`Agent`](crate::agent::Agent) / [`RegistryItem`](synthia_core::registry::RegistryItem) impls, and the per-run runtime helpers. State machine lives in the `loop` module.
use std::{path::PathBuf, pin::Pin, sync::Arc};

use async_trait::async_trait;
use futures::{Stream, channel::mpsc};
use synthia_context::ContextManager;
use synthia_core::{agent::AgentDescriptor, registry::RegistryItem};
use synthia_provider::ModelProvider;
use synthia_session::LogWriter;
use synthia_steering::Steering;
use synthia_tool::{ToolRegistry, ToolRestriction, ToolSurfacePolicy};

use crate::{
    AgentCaps,
    agent::{
        AgentRuntime,
        ReasoningStrategy,
        checkpoint::Checkpointer,
        re_act::delegation::{DelegatingTool, Delegators},
    },
    events::AgentEvent,
    input::AgentInput,
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
        // Per-run overrides win: a delegated child's input names
        // the child session's writer and carries the writer
        // source for its own children; otherwise the agent's
        // configured handles apply. Both live on the runtime's
        // caps so the loop and its delegating tools see them.
        let log_writer = input
            .log_writer
            .clone()
            .or_else(|| self.caps.env.log_writer.clone());
        let subagent_writers = input
            .subagent_writers
            .clone()
            .or_else(|| self.caps.env.subagent_writers.clone());
        let sink = crate::agent::sink::EventSink::new(
            Arc::new(tx),
            log_writer.clone(),
        );
        let mut caps = self.caps.clone();
        caps.env.log_writer = log_writer;
        caps.env.subagent_writers = subagent_writers;
        let runtime = AgentRuntime {
            caps: Arc::new(caps),
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

    /// Install the run's compactor.
    ///
    /// Without one the loop never compacts: history stays whatever
    /// the log derives, and only the context manager's
    /// request-local pruning shortens a request. A compactor is
    /// what makes a summary durable.
    #[must_use]
    pub fn with_compactor(
        mut self,
        compactor: Arc<dyn synthia_session::Compactor>,
    ) -> Self {
        self.caps.control.compactor = Some(compactor);
        self
    }

    /// Install the agent's plugin host.
    ///
    /// The host is per agent: it holds the registrations a plugin
    /// installed (observers, gates, transformers, prompt sections,
    /// tools) and the arbitration they run through.
    ///
    /// The host is bound to the agent's tool registry here, so the
    /// host's tools are callable — and withdrawing a tool
    /// registration (dropping the plugin's handle) withdraws it from
    /// the registry too. A tool whose name a core tool already owns
    /// is refused, and the host records the collision as a
    /// diagnostic.
    ///
    /// A host already bound to a different registry keeps that
    /// binding: the refusal is recorded on the host, and the
    /// deployment's boot — which owns the composition — is where a
    /// refused bind is reported to the operator.
    #[must_use]
    pub fn with_host(mut self, host: crate::agent::host::Host) -> Self {
        let _refused = host.bind(Arc::clone(&self.caps.tools.registry));
        self.caps.control.host = host;
        self
    }

    /// Install a delegating tool. Returns `Err` with the name when a
    /// delegator the agent already installed advertises it — same-name
    /// dispatch has no defined precedence.
    ///
    /// A host-registered delegator joins the same table at run start. The
    /// precedence is by source, not chronological: the agent's own installs
    /// are merged first, then the host's rows in registration order, and the
    /// first owner of a name keeps it. A host row registered before the agent
    /// is built therefore still loses to `with_delegator`; the same delegator
    /// installed both ways leaves one entry; a name only the host owns is
    /// dispatched normally.
    pub fn with_delegator(
        mut self,
        tool: Arc<dyn DelegatingTool>,
    ) -> Result<Self, String> {
        let installed = Delegators::from_installed(&self.caps.tools.delegators);
        if let Some(taken) = installed.conflict(&tool.definitions()) {
            return Err(taken);
        }
        self.caps.tools.delegators.push(tool);
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

    /// Replace the run-wide approval policy.
    ///
    /// The policy decides which calls are held for an approval; the
    /// default holds a call whose tool declared it high risk.
    #[must_use]
    pub fn with_approval_policy(
        mut self,
        policy: crate::agent::approval::ApprovalPolicy,
    ) -> Self {
        self.caps.control.approval = policy;
        self
    }

    #[must_use]
    pub fn with_workspace(mut self, root: impl Into<PathBuf>) -> Self {
        self.caps.env.workspace_root = root.into();
        self
    }

    /// Install the run's log writer.
    ///
    /// The writer is the session's one writer: it stamps each
    /// structural row's ordinal, buffers it, and flushes the buffer
    /// at every step boundary — the only moment the run waits for
    /// the disk. Without a writer the run keeps no structural log.
    #[must_use]
    pub fn with_log_writer(mut self, writer: Arc<LogWriter>) -> Self {
        self.caps.env.log_writer = Some(writer);
        self
    }

    /// Install the writer source for delegated children.
    ///
    /// The loop hands the source to the `task` delegator, which
    /// resolves each child's session writer before starting the
    /// child run — so the child's harness writes its model facts
    /// into the child's own session log. Without one, this
    /// agent's children stream live but persist nothing.
    #[must_use]
    pub fn with_subagent_writers(
        mut self,
        writers: Arc<dyn crate::input::SubagentWriters>,
    ) -> Self {
        self.caps.env.subagent_writers = Some(writers);
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
