//! [`uva_harness`] — the runtime: [`Agent`] trait,
//! [`ReActAgent`] loop, events, prompt assembly, and the
//! reasoning-strategy seam.
//!
//! [`ReActAgent`] is the one-expression form of the tutorial in this
//! crate's root docs — chained `with_*` setters, no separate factory
//! type. Around it: the [`AgentRegistry`] catalog, the pluggable
//! [`ReasoningStrategy`] seam with the three strategies this workspace
//! [`Agent::run`] as the single entry point that streams a
//! session's [`AgentEvent`]s, and the
//! [`DelegatingTool`] seam a delegation plugin installs through.
//!
//! Sub-agent delegation itself is **not** here: the `task` tool, its
//! gates, worktree isolation and routing live in
//! `uva::tool_task` (`uva-tool-plugins`).
//!
//! Every interception point is one registry: [`Host`] holds the
//! hooks, gates, guards, hints, trackers, transformers and prompt
//! sections a plugin installs, and dropping a registration withdraws
//! it. Resume is driven by the session log (`uva::session`) —
//! there is no snapshot store.
pub use uva_harness::*;
