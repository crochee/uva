//! [`elysper_tool`] — the tool seam and the registry: the paradigm
//! every tool plugin implements.
//!
//! Implement [`Tool`] (four methods; `#[derive(Tool)]` from
//! [`crate::macros`] generates them), register it in a
//! [`ToolRegistry`], and the agent loop dispatches it with a
//! [`Context`] and a [`ToolOutput`] contract. What the model is told
//! about is a separate decision from what the registry can execute:
//! [`ToolExposure`] (deferred schemas) and [`ToolSurfacePolicy`]
//! (named groups + a per-request cap) narrow the advertised surface
//! without removing a tool from the registry, plus the `workspace`
//! path confinement a file-touching tool enforces.
//!
//! The agent-facing tool **implementations** are separate plugin
//! crates, each behind its own feature: `elysper::tool_read`,
//! `elysper::tool_write`, `elysper::tool_shell`, `elysper::tool_todo`,
//! `elysper::tool_web`, `elysper::tool_task`, `elysper::tool_scheduler`
//! and `elysper::tool_search`. This crate ships the
//! paradigm and the two synthetic contracts the harness injects itself
//! (`__get_full_output`, `structured_output`) — not the parts. A
//! boundary a tool needs beyond a path is the plugin's own:
//! `elysper::tool_shell` carries the OS execution policy
//! (`ExecutionPolicy` + `SandboxBackend`) because it is the plugin
//! that runs processes; `elysper::tool_search` carries the
//! `Registry` it fans a query across.

pub use elysper_tool::*;
