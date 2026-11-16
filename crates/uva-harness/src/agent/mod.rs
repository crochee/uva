//! The concrete agent runtime: the [`ReActAgent`] loop, the capability
//! bundle it is assembled from, the [`Host`](crate::agent::host::Host)
//! registration frontend and the reasoning-strategy seam.
//!
//! The vocabulary those pieces are described in — the [`Agent`]
//! contract, [`AgentInput`](crate::AgentInput), the steer inboxes, the
//! [`SubagentWriters`](crate::SubagentWriters) source and the
//! [`AgentRegistry`] peer catalog — lives in [`uva_agent`]; this module
//! re-exports every name, so neither the crate-root facade nor the deep
//! paths `uva_harness::agent::…` move.

pub use uva_agent::{Agent, AgentEntry, AgentRegistry};
pub use uva_core::agent::AgentDescriptor;

pub(crate) mod re_act;
pub use re_act::descriptor;

mod sink;
pub mod strategy;

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
pub use re_act::{DEFAULT_MAX_ITERATIONS, ReActAgent, ReActStrategy};
pub use sink::{EventSink, EventStream};
pub use strategy::{ReasoningStrategy, StrategyRegistry};
