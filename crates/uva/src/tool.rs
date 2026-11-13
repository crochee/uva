//! [`uva_tool`] — the tool seam and the registry: the paradigm
//! every tool plugin implements.
//!
//! Implement [`Tool`] (five methods), register it in a
//! [`ToolRegistry`], and the agent loop dispatches it with a
//! [`Context`] and a [`ToolOutput`] contract. What the model is told
//! about is a separate decision from what the registry can execute:
//! [`ToolExposure`] (deferred schemas) and [`ToolSurfacePolicy`]
//! (named groups + a per-request cap) narrow the advertised surface
//! without removing a tool from the registry, plus the `workspace`
//! path confinement a file-touching tool enforces.
//!
//! The agent-facing tool **implementations** are separate plugin
//! crates, each behind its own feature: `uva::tool_read`,
//! `uva::tool_write`, `uva::tool_shell`, `uva::tool_todo`,
//! `uva::tool_web`, `uva::tool_task`, `uva::tool_scheduler`
//! and `uva::tool_search`. This crate ships the
//! paradigm and the two synthetic contracts the harness injects itself
//! (`__get_full_output`, `structured_output`) — not the parts. A
//! boundary a tool needs beyond a path is the plugin's own:
//! `uva::tool_shell` carries the OS execution policy
//! (`ExecutionPolicy` + `SandboxBackend`) because it is the plugin
//! that runs processes; `uva::tool_search` carries the
//! `Registry` it fans a query across.

pub use uva_tool::*;
