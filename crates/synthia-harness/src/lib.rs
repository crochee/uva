//! Runtime-agnostic AI agent harness: the [`ReActAgent`] loop, the
//! [`ReasoningStrategy`] seam that swaps it, the [`SyntheticTool`]
//! seam, and the [`AgentRegistry`] multi-agent catalog. See
//! `README.md` for the tour and `SEAMS.md` for the integration map.
//!
//! # The seam rule
//!
//! A type is `dyn` iff a downstream crate must install it at runtime
//! — by `Arc`, by name through a registry, or from a config string.
//! Everything the harness owns stays concrete or generic. Read
//! accessors stay when in-tree code or an external consumer's
//! plausible integration path uses them; setters' round-trip
//! getters with zero references are deleted.

pub mod agent;
pub mod events;
mod input;
mod log;
mod prompt;

pub use agent::{
    Agent,
    AgentCaps,
    AgentDescriptor,
    AgentEntry,
    AgentRegistry,
    AgentRuntime,
    DEFAULT_MAX_ITERATIONS,
    EventSink,
    ReActAgent,
    ReActStrategy,
    ReasoningStrategy,
    StrategyRegistry,
    SyntheticCall,
    SyntheticTool,
    checkpoint::{
        CheckpointError,
        CheckpointSnapshot,
        Checkpointer,
        InMemoryCheckpointer,
    },
    descriptor::default_descriptor,
    strategy::{
        prepare_messages_for_shot,
        single_shot_request,
        single_shot_request_shared,
    },
};
pub use events::{
    AgentEvent,
    AgentMeta,
    SessionEndReason,
    SystemEvent,
    WarningKind,
};
pub use input::AgentInput;
pub use log::{Durability, LogWriter};
pub use prompt::{DEFAULT_SYSTEM_PROMPT, PromptContext};
