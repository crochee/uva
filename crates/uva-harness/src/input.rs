//! What a caller hands to one [`Agent::run`](crate::agent::Agent::run)
//! and the writer source the `task` tool opens child sessions with.
//!
//! The types live in [`uva_agent`]: [`AgentInput`] (multimodal content,
//! the prior transcript, the prompt-manifest override and the run's
//! steer channel), the [`RunInbox`] contract and its [`ChannelInbox`]
//! implementation, and [`SubagentWriters`]. This module re-exports them
//! so both the crate-root facade and the harness-internal
//! `crate::input::…` paths keep resolving. Nothing here adds a field or
//! a method: the owning crate is the single definition.

pub use uva_agent::{AgentInput, ChannelInbox, RunInbox, SubagentWriters};
