//! The config-driven assembly layer: the built-in plugin factories the
//! profile rows are built from.
//!
//! The deployment's dynamic plugin channel is gone. A third-party
//! tool attaches as an **MCP server**: a `mcp.json` file names it by
//! `command` + `args`, the server is spawned over stdio, and the
//! tools it publishes land in the agent's tool registry. The rows
//! below are the server's own native plugins — the ones the server
//! mounts itself.

pub(crate) mod kernel;
