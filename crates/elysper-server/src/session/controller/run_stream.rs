//! The run-stream factory seam: how the controller obtains an agent
//! run's event stream, and the production impl over
//! [`elysper::harness::Agent`].

use std::{pin::Pin, sync::Arc};

use futures::{Stream, StreamExt};
use elysper::{
    core::AtomicCancelToken,
    harness::{Agent, AgentEvent, AgentInput},
    tool_task::TaskDelegator,
};

use crate::agent_run_config::{AgentRunConfig, RunOverrides};

/// Factory abstraction so the controller can be unit-tested without
/// starting a real agent run.
pub trait RunStreamFactory: Send + Sync + 'static {
    fn run_stream(
        &self,
        config: AgentRunConfig,
        overrides: RunOverrides,
        input: AgentInput,
        cancel: Arc<AtomicCancelToken>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'static>>;
}

/// Production implementation that delegates to
/// [`elysper::harness::Agent`].
#[derive(Debug, Clone, Default)]
pub struct AgentRunStreamFactory;

impl RunStreamFactory for AgentRunStreamFactory {
    fn run_stream(
        &self,
        config: AgentRunConfig,
        overrides: RunOverrides,
        input: AgentInput,
        cancel: Arc<AtomicCancelToken>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'static>> {
        let provider = Arc::clone(&config.provider);
        let tool_registry = Arc::clone(&config.tool_registry);
        let workspace_root = config.workspace_root.clone();
        let system_prompt = config.system_prompt.clone();
        let prompt_context = config.prompt_context.clone();
        let registry = config.agent_registry.clone();
        let resolved_descriptor = if let (Some(reg), Some(n)) =
            (registry.as_ref(), config.agent_name.as_ref())
        {
            match reg.resolve_sync(n).map(|a| a.descriptor().clone()) {
                Some(d) => Some(d),
                None => {
                    tracing::warn!(
                        agent_name = %n,
                        "agent_registry resolve miss; falling back to default descriptor"
                    );
                    None
                }
            }
        } else {
            None
        };

        // The override cascade lands in the assembly's optional
        // fields; `build` is the only `with_*` chain.
        let agent = crate::agent_assembly::AgentAssembly {
            provider,
            tool_registry,
            workspace_root,
            descriptor: resolved_descriptor.unwrap_or_else(|| {
                elysper::harness::default_descriptor(system_prompt)
            }),
            prompt_context,
            max_iterations: config
                .max_iterations
                .unwrap_or(elysper::harness::DEFAULT_MAX_ITERATIONS),
            // Sub-agent delegation via the `task` plugin.
            delegators: registry
                .map(|r| {
                    vec![Arc::new(TaskDelegator::new(Arc::clone(&r)))
                        as Arc<dyn elysper::harness::DelegatingTool>]
                })
                .unwrap_or_default(),
            tool_surface: overrides.tool_surface,
            strategy: overrides.strategy,
            tool_restriction: overrides.tool_restriction,
            log_writer: config.log_writer,
            subagent_writers: config.subagent_writers.clone(),
            context_manager: overrides.context_manager,
            compactor: overrides.compactor,
            // The deployment's plugin host: a clone shares the
            // registrations, so a plugin installed at boot is
            // callable from this run. Its `with_host` binds the
            // node's one registry, which is already the host's
            // binding — a no-op.
            host: config.host,
        }
        .build();
        let agent = Arc::new(agent) as Arc<dyn Agent>;
        Box::pin(async_stream::stream! {
            let mut inner = agent.run(input, cancel).await;
            while let Some(item) = inner.next().await {
                yield item;
            }
        })
    }
}
