//! [`uva_agent`] — the agent vocabulary: the [`Agent`] contract a
//! paradigm implements, [`AgentInput`] (the payload, history, prompt
//! manifest and steer channel one run takes), the steer inboxes
//! (`RunInbox` / `ChannelInbox`), the `SubagentWriters` source that
//! opens child sessions, the `SubagentUsage` account a delegated
//! round reports its consumption to, the `AgentRegistry` peer catalog
//! and the prompt manifest (`PromptContext` / `PromptSection`).
//!
//! `uva::harness` carries the same names at their existing paths,
//! because the harness's frozen facade re-exports the crate; this
//! module is the direct-vocabulary path the `task` tool or a host
//! uses when it does not need the loop.
pub use uva_agent::*;
