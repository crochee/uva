//! [`synthia_tool_task`] — the `task` tool plugin: multi-agent
//! delegation by running registered peers as sub-agents (gates,
//! worktree isolation, `@agent:` routing, subagent admission, batched
//! completion notification).
//!
//! Installs on the harness through the synthetic-tool seam:
//! `agent.with_synthetic_tool(Arc::new(TaskDelegator::new(peers)))`
//! (fallible — it returns `Err` if another installed synthetic tool
//! already advertises `task`).

pub use synthia_tool_task::*;
