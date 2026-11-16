//! [`uva_tool_task`] — the `task` tool as an ordinary tool: a model
//! call names a registered peer agent, and the tool runs it as a child
//! run (child session, depth cap, one child per call,
//! cancellation) and returns the child's final text.
//!
//! It registers in the tool registry like any other tool — through
//! [`TaskToolPlugin`] — so exposure,
//! restriction, validation and the model's visible list are the
//! registry's mechanisms.
//!
//! ```rust,no_run
//! use std::sync::Arc;
//!
//! use uva::{
//!     agent::AgentRegistry,
//!     tool::ToolRegistry,
//!     tool_task::{TaskTool, TaskToolPlugin},
//! };
//!
//! # fn wire(peers: Arc<AgentRegistry>, tools: Arc<ToolRegistry>) {
//! let tool = TaskTool::new(peers);
//! let plugin = TaskToolPlugin::new(Arc::new(tool));
//! // mount `plugin` over the kernel context that provides `tools`.
//! # }
//! ```

pub use uva_tool_task::*;
