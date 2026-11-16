//! ReAct loop: [`ReActAgent`] factory, [`ReActStrategy`] default, the state machine in `loop`, stream translator in `stream`, the `state` types, and the `transcript` the loop writes into. See `crate::agent::ReasoningStrategy` to swap the loop without rebuilding the agent.
/// Default per-run iteration cap. Override via [`ReActAgent::with_max_iterations`].
pub const DEFAULT_MAX_ITERATIONS: usize = 25;

mod agent;
pub mod descriptor;
mod r#loop;
mod panic;
pub(crate) mod projection;
mod state;
pub(crate) mod stream;
mod transcript;

pub use agent::ReActAgent;
use async_trait::async_trait;
use stream::ChunkAssembler;

use crate::{
    agent::{caps::AgentRuntime, strategy::ReasoningStrategy},
    input::AgentInput,
};

/// Think → act → observe, with tools: the loop this crate has always run,
/// expressed as a [`ReasoningStrategy`] so it can be swapped without rebuilding
/// the agent.
#[derive(Debug, Default)]
pub struct ReActStrategy;

#[async_trait]
impl ReasoningStrategy for ReActStrategy {
    fn name(&self) -> &str {
        "react"
    }

    async fn run(
        &self,
        runtime: AgentRuntime,
        input: AgentInput,
        sink: &crate::agent::sink::EventSink,
    ) {
        crate::agent::re_act::r#loop::drive_from_runtime(runtime, sink, input)
            .await;
    }
}

// --- Tests ---------------------------------------------------------------

#[cfg(test)]
pub(crate) mod tests;
