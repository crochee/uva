//! [`elysper_tool_plugins::task`] — the `task` tool plugin: multi-agent
//! delegation by running registered peers as sub-agents (gates,
//! worktree isolation, `@agent:` routing, subagent admission, batched
//! completion notification).
//!
//! Installs on the harness through the delegation seam:
//! `agent.with_delegator(Arc::new(TaskDelegator::new(peers)))`
//! (fallible — it returns `Err` if another installed delegator
//! already advertises `task`).

pub use elysper_tool_plugins::task::*;
