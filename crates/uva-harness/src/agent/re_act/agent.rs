//! [`ReActAgent`]: the public agent type, its constructors, the [`Agent`](crate::agent::Agent) / [`RegistryItem`](uva_core::registry::RegistryItem) impls, and the per-run runtime helpers. State machine lives in the `loop` module.
use std::{path::PathBuf, pin::Pin, sync::Arc};

use async_trait::async_trait;
use futures::{Stream, channel::mpsc};
use uva_context::ContextManager;
use uva_core::{agent::AgentDescriptor, registry::RegistryItem};
use uva_events::AgentEvent;
use uva_provider::ModelProvider;
use uva_session::SessionLog;
use uva_tool::{ToolRegistry, ToolRestriction, ToolSurfacePolicy};

use crate::{
    AgentCaps,
    agent::{AgentRuntime, ReasoningStrategy},
    input::AgentInput,
    prompt::PromptContext,
    seams::Seams,
};

/// Concrete [`Agent`](crate::agent::Agent) implementing the ReAct loop. Holds an [`AgentCaps`] bundle; per-run state (cancel)
/// comes from each [`Agent::run`](crate::agent::Agent::run) call's `input` and `cancel`.
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
        cancel: Arc<dyn uva_core::CancelToken>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'static>> {
        let (tx, rx) = mpsc::unbounded::<AgentEvent>();
        // Per-run override wins: a delegated child's input names the
        // child session's writer; otherwise the agent's configured
        // handle applies. It lives on the runtime's caps so the loop
        // sees it.
        let log_writer = input
            .log_writer
            .clone()
            .or_else(|| self.caps.env.log_writer.clone());
        let sink = crate::agent::sink::EventSink::with_capacity(
            Arc::new(tx),
            log_writer.clone(),
            self.caps.env.event_capacity,
        );
        let mut caps = self.caps.clone();
        caps.env.log_writer = log_writer;
        let runtime = AgentRuntime {
            caps: Arc::new(caps),
            cancel,
        };
        let strategy = Arc::clone(&runtime.caps.strategy);
        let spawner = Arc::clone(&runtime.caps.env.spawner);
        let live = sink.live_counter();
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
        Box::pin(crate::agent::sink::EventStream::new(rx, live))
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
        spawner: Arc<dyn uva_core::spawn::Spawner>,
    ) -> Self {
        self.caps.env.spawner = spawner;
        self
    }

    /// Cap how many live events one run buffers before it drops them.
    ///
    /// The durable log is unaffected: a dropped live event still has
    /// its row, and `SystemEvent::EventsDropped` reports the gap.
    #[must_use]
    pub fn with_event_capacity(mut self, capacity: usize) -> Self {
        self.caps.env.event_capacity = capacity;
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
        compactor: Arc<dyn uva_session::Compactor>,
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
    /// binding, and a tool whose name a core tool owns stays out of
    /// the registry; both refusals are returned so the caller — the
    /// deployment's boot, which owns the composition — reports them
    /// instead of reading the host's diagnostic history.
    pub fn with_host(
        self,
        host: crate::agent::host::Host,
    ) -> Result<Self, Vec<crate::agent::host::Diagnostic>> {
        self.with_seams(Seams::new(host))
    }

    /// Install the agent's seam layer.
    ///
    /// The loop dispatches through this layer and no other interception
    /// surface. A caller that already holds a [`Seams`] — the shape the
    /// kernel-plugin path builds — installs it here; the legacy
    /// [`ReActAgent::with_host`] wraps a [`Host`](crate::agent::host::Host)
    /// in one.
    ///
    /// Binding follows the same contract as `with_host`: the layer's
    /// tools join this agent's registry here, and a refused bind is
    /// returned so the caller — the deployment's boot, which owns the
    /// composition — reports it.
    pub fn with_seams(
        mut self,
        seams: Seams,
    ) -> Result<Self, Vec<crate::agent::host::Diagnostic>> {
        let refused = seams.bind(Arc::clone(&self.caps.tools.registry));
        self.caps.control.seams = seams;
        if refused.is_empty() {
            Ok(self)
        } else {
            Err(refused)
        }
    }

    /// Install the deployment's external hook registry.
    ///
    /// The registry is the contract crate's revocable external
    /// surface: registers on one of the seven mount points, returns a
    /// handle, and dropping the handle withdraws the registration.
    /// Its registrations dispatch *before* the host's typed ones on
    /// every pass, through the same arbitration — the first `Block`
    /// short-circuits, `Ask`s collect from both surfaces, and a
    /// `Block` at `tool/pre-execute` still makes the loop write the
    /// durable `hook_block` row.
    ///
    /// A deployment normally reaches this method through the
    /// `agentLoop` factory, which resolves the mounted `hooks`
    /// service; a consumer assembling the agent by hand installs the
    /// registry here.
    #[must_use]
    pub fn with_hook_registry(
        mut self,
        registry: uva_hook::HookRegistry,
    ) -> Self {
        self.caps.control.seams =
            self.caps.control.seams.clone().with_hook_registry(registry);
        self
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

    /// Replace the run-wide approval policy.
    ///
    /// The policy decides which calls are held for an approval; the
    /// default holds a call whose tool declared it high risk.
    #[must_use]
    pub fn with_approval_policy(
        mut self,
        policy: uva_approval::ApprovalPolicy,
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
    pub fn with_log_writer(mut self, writer: Arc<dyn SessionLog>) -> Self {
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
        let tool = uva_tool::structured_output_tool(schema.clone());
        self.caps.model.descriptor.output_schema = Some(schema.to_string());
        let inserted = self
            .caps
            .tools
            .registry
            .register_entry(uva_tool::ToolEntry::new(tool));
        if !inserted {
            tracing::warn!(
                agent = %self.caps.model.descriptor.name,
                "structured_output already registered; keeping the first schema"
            );
        }
        self
    }
}
