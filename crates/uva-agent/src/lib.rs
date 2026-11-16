//! Agent vocabulary shared by the loop and everything that drives it:
//! the [`Agent`] contract, the [`AgentInput`] a caller hands one run,
//! the steer channel ([`RunInbox`] / [`ChannelInbox`]), the
//! [`SubagentWriters`] source that opens child sessions, the
//! [`SubagentUsage`] account a delegated round reports its consumption
//! to, the [`AgentRegistry`] peer catalog and the prompt manifest
//! ([`PromptContext`] / [`PromptSection`]).
//!
//! # What lives here
//!
//! Vocabulary only, no loop. A crate that has to name `AgentInput`,
//! resolve a peer, open a child session or assemble a prompt depends on
//! this crate and the payload types it names (`uva-core`,
//! `uva-provider`, `uva-session`, `uva-approval`, `uva-events`), not on
//! the loop. `uva-harness` keeps the concrete machinery — `ReActAgent`,
//! `AgentCaps`, `Host`, the loop — and re-exports every name here.
//!
//! # Path continuity
//!
//! The vocabulary used to live at `uva_harness::{AgentInput, …}`,
//! `uva_harness::agent::{Agent, AgentRegistry, …}` and
//! `uva_harness::agent::host::PromptSection`. `uva-harness` re-exports
//! every name from its crate root and from the module it came from, so
//! the frozen surface — and every consumer import path — stays where it
//! was.
//!
//! # Runtime neutrality
//!
//! The crate is runtime-free: its own code is std + `futures`, and the
//! executor belongs to the caller. The `Agent` contract only promises a
//! `Stream`, and the steer inbox only promises a non-blocking drain.

mod input;
mod prompt;
mod registry;

use std::{pin::Pin, sync::Arc};

use async_trait::async_trait;
use futures::Stream;
pub use input::{
    AgentInput,
    ChannelInbox,
    RunInbox,
    SubagentUsage,
    SubagentWriters,
};
pub use prompt::{DEFAULT_SYSTEM_PROMPT, PromptContext, PromptSection};
pub use registry::{AgentEntry, AgentRegistry};
pub use uva_core::agent::AgentDescriptor;
use uva_core::{CancelToken, registry::RegistryItem};
use uva_events::AgentEvent;

/// Every concrete agent paradigm (ReAct, pipeline, planner,
/// router, …) implements this trait. The contract is deliberately
/// minimal so new paradigms can be added without changing the
/// call sites that drive them.
#[async_trait]
pub trait Agent: RegistryItem + Send + Sync {
    /// The catalog view of this agent: name, capabilities, tools,
    /// handoffs. The registry serves list/filter queries from it
    /// without waking the agent.
    fn descriptor(&self) -> &AgentDescriptor;

    /// Stream one session's [`AgentEvent`]s live; config comes from the agent.
    async fn run(
        &self,
        input: AgentInput,
        cancel: Arc<dyn CancelToken>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'static>>;
}
