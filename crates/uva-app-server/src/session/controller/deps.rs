//! What a run needs: the provider, tool registry, prompt context and
//! per-agent knobs a controller hands to every agent run it starts.

use std::{path::PathBuf, sync::Arc, time::Duration};

/// Resolves a recorded provider / model pair back to a live handle.
///
/// The trait itself lives in `uva-provider` next to the `llm`
/// service it is the value of; it is re-exported here because the
/// controller's public surface ([`RunDependencies`]) names it. The
/// server's `ProviderCatalogue` is the implementation: a suspended
/// turn's request-snapshot row names the catalogue entry and the
/// model, and the resume needs that exact handle back.
pub use uva::provider::ProviderResolver;
use uva::{
    agent::PromptContext,
    provider::traits::ModelProvider,
    tool::registry::ToolRegistry,
};

use crate::state::UsageMetrics;

/// Minimal dependencies required to build an
/// [`AgentRunConfig`](crate::agent_run_config::AgentRunConfig) for
/// the session.
#[derive(Clone)]
pub struct RunDependencies {
    pub provider: Arc<dyn ModelProvider>,
    /// Resolves a recorded `provider` / `model` pair back to a live
    /// handle.
    ///
    /// A suspended turn continues under the provider it was sampled
    /// with; the log's request-snapshot row names that pair. The
    /// deployment default is not a substitute: sampling it would
    /// change the request envelope, and the resume would be refused
    /// as a configuration drift.
    pub(crate) provider_resolver: Option<Arc<dyn ProviderResolver>>,
    /// The catalogue key of [`Self::provider`]. The handle's
    /// `name()` is the *model* for an OpenAI-compatible provider,
    /// so the durable rows that record what a turn ran cannot
    /// derive this from the handle.
    ///
    /// Set through [`Self::with_default_provider_name`]; a
    /// deployment that leaves it empty writes an empty provider into
    /// its rows.
    pub(crate) default_provider_name: String,
    /// The node's one tool registry, shared by every agent.
    ///
    /// Not a snapshot: a plugin's tools, an MCP server's tools and a
    /// management-route registration all land here, and every run
    /// built from these dependencies advertises the live surface.
    pub tool_registry: Arc<ToolRegistry>,
    /// The deployment's background jobs.
    ///
    /// The controller listens to it: a job that finishes is delivered
    /// to the session that owns it, so the model learns that its
    /// command is done without polling for it. `None` in a host that
    /// mounts no `job` tool.
    pub(crate) jobs: Option<Arc<uva::tool_job::JobRegistry>>,
    /// Working directory handed to built-in tools via
    /// [`AgentRunConfig::workspace_root`](crate::agent_run_config::AgentRunConfig::workspace_root). Replaces the
    /// previous hard-coded `/tmp` so `read_file` / `shell`
    /// operate inside the user's project root, not the
    /// system temp dir.
    pub workspace_root: PathBuf,
    /// System prompt injected as the first message of every
    /// conversation. The ReAct loop builds its own system
    /// prompt from the descriptor via
    /// [`uva::agent::PromptContext::assemble`]; this
    /// field is the legacy/default fallback for callers that
    /// do not supply an explicit descriptor.
    pub system_prompt: String,
    /// Prompt-context manifest (skills + peer agents + tool
    /// definitions). Empty by default; populated by the server
    /// from the workspace's `.agents/skills/` directory and the
    /// registered [`AgentRegistry`](uva::agent::AgentRegistry) at startup.
    ///
    /// Stored as `Arc<PromptContext>` so cloning the
    /// [`RunDependencies`] (which the [`crate::state::AppState`]
    /// does on every session creation) bumps a refcount instead
    /// of deep-cloning the skills + peer-agents lists. The
    /// manifest is read-only after boot, so this is safe across
    /// concurrent dispatches.
    pub prompt_context: Arc<uva::agent::PromptContext>,
    /// Multi-agent registry. Held by reference so the run
    /// factory can resolve the configured agent synchronously
    /// inside `build_run_config` without going through an
    /// async dispatch boundary.
    pub agent_registry: Option<Arc<uva::agent::AgentRegistry>>,
    pub default_agent_name: Option<Arc<parking_lot::RwLock<Option<String>>>>,
    /// Wall-clock source for the session log's `ts` fields and for
    /// the operation snapshots this deployment publishes. Default
    /// [`uva::core::SystemClock`]; the server passes the single
    /// process clock it builds at boot, and a test injects a
    /// [`uva::core::FixedClock`] via [`RunDependencies::with_clock`].
    pub clock: uva::core::SharedClock,
    /// Default per-run iteration cap applied to every dispatch
    /// unless a per-run override is supplied via
    /// [`RunDependencies::with_max_iterations`]. Sourced from
    /// `agents.<name>.max_steps` at boot and forward to the
    /// factory, which installs it via
    /// [`uva::harness::ReActAgent::with_max_iterations`] so
    /// the loop owns a stable cap for the whole run.
    pub default_max_iterations: usize,
    /// R34: deployment-level tool-surface policy installed on every
    /// agent the run factory builds. `None` (the default) keeps the
    /// unfiltered R33 projection. Loaded from the `[tools]` config
    /// section at boot.
    pub tool_surface: Option<uva::tool::ToolSurfacePolicy>,
    /// The mounted `agentLoop` factory the run factory builds
    /// through (`None` builds directly from these dependencies —
    /// the embedder shape). Captured at session creation: a session
    /// spanning an HMR remount keeps the factory it was built with,
    /// and a session created after the remount samples the new one.
    pub agent_loop: Option<Arc<dyn uva::harness::plugin::AgentLoopFactory>>,
    /// Process-wide usage counters the controller's event funnel
    /// records into (token totals per `SystemEvent::Usage`, one turn
    /// per `SystemEvent::SessionEnded`). Defaults to a private
    /// [`UsageMetrics`], which keeps a run built without an
    /// `AppState` (tests, embedders) compiling and counting into a
    /// sink nobody reads; the server installs the shared one via
    pub usage_metrics: Arc<UsageMetrics>,
    /// Sub-agent session router. `None` (tests, embedders) drops
    /// delegated child events instead of inlining them into the
    /// parent — the child's answer still reaches the model through
    /// the `task` tool result. The server installs the process-wide
    /// router so every child gets a first-class session.
    /// The node's cluster bus + identity. Every controller publishes
    /// its frames here and holds its session lease through it; the
    /// test-only default is an in-process [`LocalBus`].
    ///
    /// [`LocalBus`]: crate::cluster::LocalBus
    pub cluster: Arc<crate::cluster::ClusterRuntime>,
    /// The deployment's attachment store (`AppState::attachment_store`).
    ///
    /// The one place multimodal payload bytes are kept: a parked prompt
    /// stores them here (content-addressed) and its log row carries the
    /// hash, so the row never holds the bytes and a restarted node can
    /// fetch them back. The default is an in-memory store, which keeps
    /// a controller built without an `AppState` (tests, embedders)
    /// working — its refs resolve for the life of the process and no
    /// further.
    pub attachment_store: Arc<uva::attachment::AttachmentStore>,
    /// The deployment's host (`AppState::plugin_host`), when this
    /// controller was spawned by the server.
    ///
    /// The host owns the deployment's steering policy and every
    /// registration the mounted rows made. Every run agent gets a
    /// clone and hands it to `ReActAgent::with_host`, so both apply
    /// to chat runs. A host clone shares the registrations (and the
    /// harness makes binding the already-bound registry a no-op), so
    /// per-run agents add no process. Tests and embedders leave this
    /// `None`; the assembly then installs `Host::default_policy`.
    pub host: Option<uva::harness::Host>,
    /// How long a held call waits for an operator's answer.
    ///
    /// `None` waits forever. The value comes from the deployment's
    /// approval policy (`approval.timeout_secs`); the controller is
    /// the layer that waits, so it owns the deadline.
    pub approval_timeout: Option<Duration>,
}

impl RunDependencies {
    /// Install this node's cluster bus + identity. The local default
    /// keeps every test and embedder working unchanged.
    #[must_use]
    pub fn with_cluster(
        mut self,
        cluster: Arc<crate::cluster::ClusterRuntime>,
    ) -> Self {
        self.cluster = cluster;
        self
    }

    /// Install the resolver that turns a recorded provider / model
    /// pair back into a live handle.
    #[must_use]
    pub fn with_provider_resolver(
        mut self,
        resolver: Arc<dyn ProviderResolver>,
    ) -> Self {
        self.provider_resolver = Some(resolver);
        self
    }

    /// Name the catalogue entry [`Self::provider`] was configured
    /// under. The durable rows that record what a turn ran need it:
    /// an OpenAI-compatible handle reports its *model* from `name()`,
    /// so a row built from the handle names the model twice.
    #[must_use]
    pub fn with_default_provider_name(
        mut self,
        name: impl Into<String>,
    ) -> Self {
        self.default_provider_name = name.into();
        self
    }

    pub fn new(
        provider: Arc<dyn ModelProvider>,
        tool_registry: Arc<ToolRegistry>,
        workspace_root: PathBuf,
        system_prompt: String,
    ) -> Self {
        Self {
            provider,
            provider_resolver: None,
            default_provider_name: String::new(),
            tool_registry,
            jobs: None,
            workspace_root,
            system_prompt,
            host: None,
            prompt_context: Arc::new(PromptContext::default()),
            agent_registry: None,
            default_agent_name: None,
            clock: uva::core::SharedClock::system(),
            default_max_iterations: uva::harness::DEFAULT_MAX_ITERATIONS,
            tool_surface: None,
            agent_loop: None,
            usage_metrics: Arc::new(UsageMetrics::default()),
            cluster: Arc::new(crate::cluster::ClusterRuntime::local()),
            attachment_store: Arc::new(
                uva::attachment::AttachmentStore::default(),
            ),
            approval_timeout: None,
        }
    }

    /// Install the deployment's background jobs.
    ///
    /// The controller listens to them: a job that finishes is
    /// delivered to the session that owns it.
    #[must_use]
    pub fn with_jobs(mut self, jobs: Arc<uva::tool_job::JobRegistry>) -> Self {
        self.jobs = Some(jobs);
        self
    }

    /// Install the deployment's attachment store — where a parked
    /// prompt's payload bytes live, keyed by the hash its log row
    /// carries.
    #[must_use]
    pub fn with_attachment_store(
        mut self,
        store: Arc<uva::attachment::AttachmentStore>,
    ) -> Self {
        self.attachment_store = store;
        self
    }

    /// Install the deployment's host, whose clone every run agent
    /// this controller builds receives.
    #[must_use]
    pub fn with_host(mut self, host: Option<uva::harness::Host>) -> Self {
        self.host = host;
        self
    }

    /// Install the wall-clock source this session's log rows are
    /// stamped from.
    #[must_use]
    pub fn with_clock(mut self, clock: uva::core::SharedClock) -> Self {
        self.clock = clock;
        self
    }

    /// R34: `Option`-flavoured installer for the deployment's
    /// tool-surface policy. `None` is a no-op (the agent keeps the
    /// unfiltered R33 projection), which is what a deployment with no
    /// `[tools]` section gets.
    #[must_use]
    pub fn with_optional_tool_surface(
        mut self,
        tool_surface: Option<uva::tool::ToolSurfacePolicy>,
    ) -> Self {
        self.tool_surface = tool_surface;
        self
    }

    /// Install the mounted `agentLoop` factory the run factory
    /// builds through. `None` (tests, embedders) keeps the direct
    /// assembly.
    #[must_use]
    pub fn with_agent_loop(
        mut self,
        factory: Option<Arc<dyn uva::harness::plugin::AgentLoopFactory>>,
    ) -> Self {
        self.agent_loop = factory;
        self
    }

    /// Install the process-wide usage counters this session's event
    /// funnel records into. `AppState` shares one `Arc` across every
    /// controller it spawns, so a closed session's tokens still count
    /// toward the process totals.
    #[must_use]
    pub fn with_usage_metrics(mut self, usage: Arc<UsageMetrics>) -> Self {
        self.usage_metrics = usage;
        self
    }

    /// Attach a populated prompt context so the agent's system
    /// prompt carries the skill/agent/tool manifest. The caller
    /// is expected to wrap the manifest in an `Arc` itself so
    /// multiple session controllers can share the same backing
    /// allocation.
    pub fn with_prompt_context(mut self, ctx: Arc<PromptContext>) -> Self {
        self.prompt_context = ctx;
        self
    }

    /// Override the per-run iteration cap. Useful for tests and
    /// for callers that want a different cap than the AppState
    /// default (e.g. a chat request pinning `max_steps` via
    /// a future API parameter). The agent-side
    /// `[1, 4096]` clamp still applies.
    pub fn with_max_iterations(mut self, max_iterations: usize) -> Self {
        self.default_max_iterations = max_iterations.clamp(1, 4096);
        self
    }

    /// Wire the multi-agent registry + configured default so
    /// the run factory can resolve the configured agent
    /// synchronously inside `build_run_config`.
    pub fn with_agent_registry(
        mut self,
        registry: Arc<uva::agent::AgentRegistry>,
        default_agent_name: Arc<parking_lot::RwLock<Option<String>>>,
    ) -> Self {
        self.agent_registry = Some(registry);
        self.default_agent_name = Some(default_agent_name);
        self
    }

    /// Install the deployment's approval deadline.
    ///
    /// A held call waits this long for an operator. `None` waits
    /// forever — the deployment default, and the right one where
    /// somebody is always watching. The deadline is the controller's
    /// business because the controller is what waits: the harness
    /// ends the turn when it holds a call, and the operator's answer
    /// comes back through the resume path.
    pub fn with_approval_timeout(mut self, timeout: Option<Duration>) -> Self {
        self.approval_timeout = timeout;
        self
    }
}
