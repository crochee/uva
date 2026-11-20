//! The `agentLoop` service: a per-run agent factory exposed as a kernel
//! plugin.
//!
//! # The service
//!
//! [`AgentLoopPlugin`](crate::plugin::AgentLoopPlugin) is a native (`RuntimeKind::Native`) kernel plugin
//! with id `agent-loop`. It injects the three services the loop needs
//! (`llm`, `tools`, `sessions`) and provides one key,
//! [`KEY_AGENT_LOOP`](crate::plugin::KEY_AGENT_LOOP). The value stored under that key is an
//! `Arc<dyn AgentLoopFactory>`: resolving it with
//! `ctx.require::<Arc<dyn AgentLoopFactory>>(KEY_AGENT_LOOP)` yields
//! `Arc<Arc<dyn AgentLoopFactory>>`, whose one deref is the factory.
//! The extra `Arc` is the kernel's service storage being `Sized`; a
//! `dyn` value is carried inside the stored `Arc`, not as the type
//! parameter.
//!
//! A plugin that provides the same key and the default plugin being
//! disabled swaps the loop, because the loop asks the service for an
//! [`Agent`] and never names [`ReActAgent`] itself.
//!
//! # One agent per run
//!
//! [`AgentLoopFactory::create`](crate::plugin::AgentLoopFactory::create) takes a [`RunConfig`](crate::plugin::RunConfig) and returns a
//! fresh agent. [`RunConfig`](crate::plugin::RunConfig) mirrors the server's agent assembly — the
//! provider, the tool registry, the descriptor and prompt context, the
//! writer and writer source, the context manager and compactor, the
//! per-run policy host, and the spawner — so one factory can serve every
//! turn of every session without sharing mutable run state. The default
//! implementation, [`ReActAgentFactory`](crate::plugin::ReActAgentFactory), builds a [`ReActAgent`] and is
//! the loop this crate has always run.
//!
//! # Lifecycle
//!
//! The plugin returns its provide handle from
//! [`apply`](uva_kernel::Plugin::apply), so unloading the plugin
//! revokes the service. The plugin also injects `llm`, `tools` and
//! `sessions`: when one of those providers unloads, the kernel parks
//! this plugin's fiber in `Pending` and the service disappears until the
//! provider is mounted again.

use std::{path::PathBuf, sync::Arc};

use uva_context::ContextManager;
use uva_core::spawn::Spawner;
use uva_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
};
use uva_provider::ModelProvider;
use uva_session::{Compactor, SessionLog};
use uva_tool::{ToolRegistry, ToolRestriction, ToolSurfacePolicy};

use crate::{
    agent::{
        Agent,
        AgentDescriptor,
        ReActAgent,
        ReasoningStrategy,
        host::{Diagnostic, Host},
    },
    prompt::PromptContext,
};

/// The service key the loop is resolved through.
pub const KEY_AGENT_LOOP: &str = "agentLoop";

// The services the loop reads; the manifest declares them so the kernel
// parks the plugin while any one of them is unavailable.
const LOOP_INJECTS: [&str; 3] = ["llm", "tools", "sessions"];

/// Builds the [`Agent`] one run dispatches.
///
/// A factory is shared, so `create` must take everything mutable from
/// `run` and return an agent that owns its per-run state. The loop asks
/// the `agentLoop` service for one agent per turn, so a factory that
/// returns a clone of some long-lived agent would share state across
/// turns — the contract is "a fresh agent per call".
pub trait AgentLoopFactory: Send + Sync {
    /// Build the agent for one run.
    fn create(&self, run: &RunConfig) -> Arc<dyn Agent>;
}

#[derive(Clone)]
/// The collected inputs for one run's [`ReActAgent`].
///
/// Every field mirrors the server's assembly, so moving a call site from
/// a hand-written `with_*` chain to the factory does not change what the
/// agent is built with. [`RunConfig::build`] applies only the optional
/// pieces that are present.
pub struct RunConfig {
    /// The run's model provider.
    pub provider: Arc<dyn ModelProvider>,
    /// The run's tool registry.
    pub tool_registry: Arc<ToolRegistry>,
    /// The run's workspace root (tool context, workspace-boundary guard).
    pub workspace_root: PathBuf,
    /// The agent descriptor: name, instructions, handoff metadata.
    pub descriptor: AgentDescriptor,
    /// The prompt manifest the run renders.
    pub prompt_context: Arc<PromptContext>,
    /// The iteration cap.
    pub max_iterations: usize,
    /// The deployment's tool-surface policy.
    pub tool_surface: Option<ToolSurfacePolicy>,
    /// The deployment's reasoning loop; `None` keeps
    /// [`ReActStrategy`](crate::agent::ReActStrategy).
    pub strategy: Option<Arc<dyn ReasoningStrategy>>,
    /// The deployment's allow/deny list.
    pub tool_restriction: Option<Arc<ToolRestriction>>,
    /// The session's one writer.
    pub log_writer: Option<Arc<dyn SessionLog>>,
    /// The caller-composed context manager.
    pub context_manager: Option<Arc<dyn ContextManager>>,
    /// The deployment's compaction engine, when one is configured.
    pub compactor: Option<Arc<dyn Compactor>>,
    /// The deployment's plugin host; `None` installs
    /// [`Host::default_policy`].
    pub host: Option<Host>,
    /// Where the run's detached work executes; `None` keeps the
    /// runtime-free inline spawner.
    pub spawner: Option<Arc<dyn Spawner>>,
}

impl RunConfig {
    #[must_use]
    /// Build the run's [`ReActAgent`].
    ///
    /// Mirrors the server's assembly: default the policy host, bind it
    /// to the registry (reporting each refusal), then apply every
    /// optional knob. Setter order is irrelevant — each setter rewrites
    /// one capability field.
    ///
    /// # Panics
    ///
    /// Panics when the final host bind is refused. It is impossible
    /// after the bind above claims the registry, and it is exactly what
    /// the server's assembly asserts.
    pub fn build(self) -> ReActAgent {
        let RunConfig {
            provider,
            tool_registry,
            workspace_root,
            descriptor,
            prompt_context,
            max_iterations,
            tool_surface,
            strategy,
            tool_restriction,
            log_writer,
            context_manager,
            compactor,
            host,
            spawner,
        } = self;

        let host = host.unwrap_or_else(|| {
            // No deployment table reached this run: the embedder gets
            // the production policy over an empty registry.
            Host::default_policy(
                workspace_root.clone(),
                uva_hook::HookRegistry::new(),
            )
        });
        report_refusals(host.bind(Arc::clone(&tool_registry)));

        let agent = ReActAgent::with_descriptor(
            provider,
            tool_registry,
            workspace_root,
            descriptor,
            prompt_context,
        );
        let agent = with_surface(agent, tool_surface);
        let agent = with_strategy(agent, strategy);
        let agent = with_restriction(agent, tool_restriction);
        let agent = with_writer(agent, log_writer);
        let agent = with_context_manager(agent, context_manager);
        let agent = with_compactor(agent, compactor);
        let agent = with_spawner(agent, spawner);
        agent
            .with_max_iterations(max_iterations)
            .with_host(host)
            .expect("host bind")
    }
}

#[derive(Debug, Default, Clone, Copy)]
/// The default [`AgentLoopFactory`]: one [`ReActAgent`] per run.
pub struct ReActAgentFactory;

impl AgentLoopFactory for ReActAgentFactory {
    fn create(&self, run: &RunConfig) -> Arc<dyn Agent> {
        Arc::new(run.clone().build())
    }
}

/// The `agent-loop` kernel plugin.
///
/// Mount it with [`uva_kernel::mount_all`] after the services it
/// injects are available. See the [module docs](crate::plugin) for the service's
/// resolution shape and the swap contract.
pub struct AgentLoopPlugin {
    factory: Arc<dyn AgentLoopFactory>,
    manifest: PluginManifest,
}

impl AgentLoopPlugin {
    #[must_use]
    /// A plugin that provides `agentLoop` through `factory`.
    pub fn new(factory: Arc<dyn AgentLoopFactory>) -> Self {
        let mut manifest =
            PluginManifest::new("agent-loop", PLUGIN_PROTOCOL_VERSION);
        manifest.provides = vec![KEY_AGENT_LOOP.to_string()];
        manifest.inject =
            LOOP_INJECTS.iter().map(|key| (*key).to_string()).collect();
        Self { factory, manifest }
    }
}

impl KernelPlugin for AgentLoopPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        let factory: Arc<dyn AgentLoopFactory> = Arc::clone(&self.factory);
        Ok(vec![ctx.provide(KEY_AGENT_LOOP, factory)?])
    }
}

fn report_refusals(refused: Vec<Diagnostic>) {
    for diagnostic in &refused {
        tracing::warn!(
            kind = ?diagnostic.kind,
            source = %diagnostic.source,
            detail = %diagnostic.detail,
            "host registration refused"
        );
    }
}

fn with_surface(
    agent: ReActAgent,
    policy: Option<ToolSurfacePolicy>,
) -> ReActAgent {
    match policy {
        Some(policy) => agent.with_tool_surface(policy),
        None => agent,
    }
}

fn with_strategy(
    agent: ReActAgent,
    strategy: Option<Arc<dyn ReasoningStrategy>>,
) -> ReActAgent {
    match strategy {
        Some(strategy) => agent.with_strategy(strategy),
        None => agent,
    }
}

fn with_restriction(
    agent: ReActAgent,
    restriction: Option<Arc<ToolRestriction>>,
) -> ReActAgent {
    match restriction {
        Some(restriction) => {
            agent.with_tool_restriction((*restriction).clone())
        }
        None => agent,
    }
}

fn with_writer(
    agent: ReActAgent,
    writer: Option<Arc<dyn SessionLog>>,
) -> ReActAgent {
    match writer {
        Some(writer) => agent.with_log_writer(writer),
        None => agent,
    }
}

fn with_context_manager(
    agent: ReActAgent,
    manager: Option<Arc<dyn ContextManager>>,
) -> ReActAgent {
    match manager {
        Some(manager) => agent.with_context_manager(manager),
        None => agent,
    }
}

fn with_compactor(
    agent: ReActAgent,
    compactor: Option<Arc<dyn Compactor>>,
) -> ReActAgent {
    match compactor {
        Some(compactor) => agent.with_compactor(compactor),
        None => agent,
    }
}

fn with_spawner(
    agent: ReActAgent,
    spawner: Option<Arc<dyn Spawner>>,
) -> ReActAgent {
    match spawner {
        Some(spawner) => agent.with_spawner(spawner),
        None => agent,
    }
}
