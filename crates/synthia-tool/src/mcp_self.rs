//! Naming convention for the deployment's self-management MCP
//! tools.
//!
//! A `synthia-server` instance publishes one server named `self`
//! whose tools all begin with [`MCP_SELF_TOOL_PREFIX`]
//! (`mcp__self__<domain>`). The tools are operator-facing (an
//! agent manages itself by calling them through MCP), **not**
//! model-facing, so every agent-side projection strips them via
//! [`is_mcp_self_tool`].
//!
//! The sentinel is hoisted to this crate so harness / HTTP
//! routes / synthetic MCP servers all filter against one source
//! of truth—hard-coded copies drift the first time the MCP
//! naming scheme moves.

/// Tool-name prefix the deployment's self-management MCP server
/// advertises under (`mcp__self__<tool>`).
pub const MCP_SELF_TOOL_PREFIX: &str = "mcp__self__";

/// True when `name` is a self-management MCP tool (server-facing
/// only: must stay out of the model-facing projection and the
/// operator-facing public catalog).
#[must_use]
pub fn is_mcp_self_tool(name: &str) -> bool {
    name.starts_with(MCP_SELF_TOOL_PREFIX)
}
