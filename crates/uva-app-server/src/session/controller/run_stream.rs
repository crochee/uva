//! The run-stream factory seam: how the controller obtains an agent
//! run's event stream, and the production impl over
//! [`uva::harness::Agent`].

use std::{pin::Pin, sync::Arc};

use futures::{Stream, StreamExt};
use uva::{
    core::AtomicCancelToken,
    harness::{AgentEvent, AgentInput},
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
/// [`uva::harness::Agent`].
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

        // The one assembly point: the run's per-run inputs plus the
        // factory's deployment defaults (strategy, restriction,
        // context manager, compactor, spawner). A config without a
        // factory (an embedder, a test) builds the same `RunConfig`
        // directly.
        let run = uva::harness::plugin::RunConfig {
            provider,
            tool_registry,
            workspace_root,
            descriptor: resolved_descriptor.unwrap_or_else(|| {
                uva::harness::default_descriptor(system_prompt)
            }),
            prompt_context,
            max_iterations: config
                .max_iterations
                .unwrap_or(uva::harness::DEFAULT_MAX_ITERATIONS),
            // Delegation is a registry tool (`tool-task`) now: the run
            // dispatches `task` through the tool registry like every
            // other tool, so the factory has no delegation-specific
            // wiring to install.
            tool_surface: config
                .tool_surface
                .clone()
                .or(overrides.tool_surface.clone()),
            strategy: overrides.strategy,
            tool_restriction: overrides.tool_restriction,
            log_writer: config.log_writer,
            context_manager: overrides.context_manager,
            compactor: overrides.compactor,
            host: config.host.clone(),
            spawner: None,
        };
        let agent = match &config.agent_loop {
            Some(factory) => factory.create(&run),
            None => {
                // The embedder path keeps the server spawner the
                // factory would install.
                let mut run = run;
                run.spawner = Some(crate::spawner::server_spawner());
                Arc::new(run.build())
            }
        };
        // The run's own event stream, forwarded unchanged: `once`
        // awaits the run, `flatten` drains the stream it produced.
        Box::pin(
            futures::stream::once(
                async move { agent.run(input, cancel).await },
            )
            .flatten(),
        )
    }
}
