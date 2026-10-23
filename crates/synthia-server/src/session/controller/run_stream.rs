//! The run-stream factory seam: how the controller obtains an agent
//! run's event stream, and the production impl over
//! [`synthia::harness::Agent`].

use std::{pin::Pin, sync::Arc};

use futures::{Stream, StreamExt};
use synthia::{
    harness::{Agent, AgentEvent, AgentInput},
    tool_task::TaskDelegator,
};
use tokio_util::sync::CancellationToken;

use crate::agent_run_config::{AgentRunConfig, RunOverrides};

/// Factory abstraction so the controller can be unit-tested without
/// starting a real agent run.
pub trait RunStreamFactory: Send + Sync + 'static {
    fn run_stream(
        &self,
        config: AgentRunConfig,
        overrides: RunOverrides,
        input: AgentInput,
        cancel: Arc<CancellationToken>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'static>>;
}

/// Production implementation that delegates to
/// [`synthia::harness::Agent`].
#[derive(Debug, Clone, Default)]
pub struct AgentRunStreamFactory;

impl RunStreamFactory for AgentRunStreamFactory {
    fn run_stream(
        &self,
        config: AgentRunConfig,
        overrides: RunOverrides,
        input: AgentInput,
        cancel: Arc<CancellationToken>,
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
                synthia::harness::default_descriptor(system_prompt)
            }),
            prompt_context,
            steering: Arc::clone(&config.steering),
            max_iterations: config
                .max_iterations
                .unwrap_or(synthia::harness::DEFAULT_MAX_ITERATIONS),
            // Sub-agent delegation via the `task` plugin.
            synthetic: registry
                .map(|r| {
                    vec![Arc::new(TaskDelegator::new(Arc::clone(&r)))
                        as Arc<dyn synthia::harness::SyntheticTool>]
                })
                .unwrap_or_default(),
            tool_surface: overrides.tool_surface,
            strategy: overrides.strategy,
            tool_restriction: overrides.tool_restriction,
            typed_event_sink: config.typed_event_sink,
            context_manager: overrides.context_manager,
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
