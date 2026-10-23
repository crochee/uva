//! [`synthia_harness`] — the runtime: [`Agent`] trait,
//! [`ReActAgent`] loop, events, prompt assembly, and the
//! reasoning-strategy seam.
//!
//! [`ReActAgent`] is the one-expression form of the tutorial in this
//! crate's root docs — chained `with_*` setters, no separate factory
//! type. Around it: the [`AgentRegistry`] catalog, the pluggable
//! [`ReasoningStrategy`] seam with the three strategies this workspace
//! [`Agent::run`] as the single entry point that streams a
//! session's [`AgentEvent`]s, and the
//! [`SyntheticTool`] seam a delegation plugin installs through.
//!
//! Sub-agent delegation itself is **not** here: the `task` tool, its
//! gates, worktree isolation and routing live in
//! `synthia::tool_task` (`synthia-tool-task`).
pub use synthia_harness::*;
