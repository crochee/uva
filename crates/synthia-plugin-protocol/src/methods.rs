//! JSON-RPC method names for the dynamic plugin channel.
//!
//! Both sides of the channel name methods through these constants;
//! hard-coded `"synthia/…"` strings are not part of the contract.
//! [`GATE`] is special: it exists only for a plugin the host has
//! elevated to full parity, and is absent from a reduced-authority
//! conversation by design.

/// Capability negotiation. The plugin answers with
/// [`PluginCapabilities`](crate::PluginCapabilities), declaring which
/// of the `tools` / `observe` / `transform` roles it supports.
pub const INITIALIZE: &str = "synthia/initialize";

/// List the plugin's remote tool descriptors. The host sends the
/// request; the plugin answers with one
/// [`ToolSpec`](crate::ToolSpec) per tool.
pub const TOOLS_LIST: &str = "synthia/tools/list";

/// Invoke a remote tool. The host sends the tool name plus free-form
/// JSON arguments; the plugin answers with the tool result as JSON.
pub const TOOLS_CALL: &str = "synthia/tools/call";

/// Hand a tool result back to the plugin. The host sends the result to
/// rewrite; the plugin answers with the rewritten JSON, which the host
/// applies.
pub const TRANSFORM: &str = "synthia/transform";

/// Notification: an observable event
/// ([`PluginEvent`](crate::PluginEvent)) with no response.
pub const EVENT: &str = "synthia/event";

/// Gate a tool call. Only enabled when config grants the plugin full
/// parity; the plugin answers with a
/// [`PluginDecision`](crate::PluginDecision). A reduced-authority
/// plugin never sees this method.
pub const GATE: &str = "synthia/gate";
