//! # elysper-tool-task
//!
//! The `task` tool plugin: multi-agent delegation — everything a
//! parent agent needs to run child agents, and nothing the harness
//! needs to know about it.
//!
//! This crate is the concrete answer to "how do tools that need
//! the loop itself plug in?": its [`TaskDelegator`] implements
//! [`elysper_harness::DelegatingTool`], so installing it on a
//! [`elysper_harness::ReActAgent`] gives the model a `task(agent,
//! prompt)` tool whose calls run a registered peer as a child run:
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use elysper_harness::{AgentRegistry, ReActAgent};
//! use elysper_tool_plugins::task::TaskDelegator;
//!
//! # async fn wrap(agent: ReActAgent) -> ReActAgent {
//! let peers = Arc::new(AgentRegistry::new());
//! // peers.put(AgentEntry::new(/* a child agent */)).await;
//! agent
//!     .with_delegator(Arc::new(TaskDelegator::new(peers)))
//!     .expect("no delegator already advertises `task`")
//! # }
//! ```
//!
//! With no delegator installed the agent is a single-loop
//! harness with zero delegation surface — the loop code has no
//! `task` branch of its own, only the generic interceptor seam.
//!
//! ## What is here
//!
//! - [`mod@core`] — the `task` tool: [`TaskSpec`] parsing, the wire
//!   definition, and [`TaskDelegator`] (depth-capped child runs,
//!   events forwarded as `AgentEvent::Agent` + `AgentMeta`).
//! - [`gate`] — a verification command run after a child
//!   finishes; pass / fail / timeout is surfaced in the tool
//!   result.
//! - [`worktree`] — git-worktree isolation: the child runs in a
//!   fresh detached worktree; dirty trees are committed to
//!   `elysper/agent-<ulid>` on cleanup.
//!
//! ## What is deliberately *not* here
//!
//! The [`elysper_harness::AgentRegistry`] itself (the catalog the
//! delegator resolves peers from) stays in `elysper-harness` — this
//! crate consumes it, never re-defines it.

pub mod core;
pub mod gate;
/// The `subagents` kernel service: the delegation backend as a plugin.
pub mod plugin;
pub mod worktree;

pub use core::{
    MAX_SUBAGENT_DEPTH,
    TASK_TOOL_NAME,
    TaskDelegator,
    TaskSpec,
    task_tool_definition,
    task_tool_definition_with_features,
};

pub use gate::{
    CommandOutcome,
    CommandRunner,
    CommandStopReason,
    GateError,
    GateSpec,
    GateVerdict,
    StdCommandRunner,
    apply_gate_to_output,
    run_gate,
    verdict_summary,
};
pub use plugin::{KEY_SUBAGENTS, SubagentPlugin};
pub use worktree::{
    WorktreeCleanupResult,
    WorktreeError,
    WorktreeInfo,
    WorktreeSpec,
    cleanup_worktree,
    create_worktree,
};
