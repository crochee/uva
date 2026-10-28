//! The unified [`Agent`] runtime contract and the pieces assembled around it: the loop, the registry catalog, and the event types child runs wrap their traces in. Sub-agent delegation lives in `elysper-tool-task`.
use std::{pin::Pin, sync::Arc};

use async_trait::async_trait;
use futures::Stream;
pub use elysper_core::agent::AgentDescriptor;
use elysper_core::{CancelToken, registry::RegistryItem};

use crate::{events::AgentEvent, input::AgentInput};

pub(crate) mod re_act;
pub use re_act::descriptor;
mod registry;

mod sink;
pub mod strategy;

/// The approval workflow: the policy that decides which calls are
/// held, the request a held call becomes, and the decisions that
/// resume a suspended run.
pub mod approval;
pub mod caps;
/// The plugin host: the per-agent registry and its arbitration.
pub mod host;

pub use caps::{
    AgentCaps,
    AgentRuntime,
    ControlCaps,
    EnvCaps,
    ModelCaps,
    ToolCaps,
};
pub use re_act::{
    DEFAULT_MAX_ITERATIONS,
    ReActAgent,
    ReActStrategy,
    delegation::{DelegatingTool, DelegationCall},
};
pub use registry::{AgentEntry, AgentRegistry};
pub use sink::{EventSink, EventStream};
pub use strategy::{ReasoningStrategy, StrategyRegistry};

/// Every concrete agent paradigm (ReAct, pipeline, planner,
/// router, …) implements this trait. The contract is deliberately
/// minimal so new paradigms can be added without changing the
/// call sites that drive them.
#[async_trait]
pub trait Agent: RegistryItem + Send + Sync {
    fn descriptor(&self) -> &AgentDescriptor;

    /// Stream one session's [`AgentEvent`]s live; config comes from the agent.
    async fn run(
        &self,
        input: AgentInput,
        cancel: Arc<dyn CancelToken>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'static>>;
}
