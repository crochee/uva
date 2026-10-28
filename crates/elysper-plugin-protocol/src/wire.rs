//! The wire payload types: capability negotiation, remote tool
//! descriptors, observed events, gate decisions, and error codes.
//!
//! All types are plain `serde` derives so the JSON shape is the
//! contract. Enums are internally tagged with a `"type"` field and
//! snake_case variant names, which is what a non-Rust author reads.

use serde::{Deserialize, Serialize};

/// What a remote plugin declares it supports, returned for
/// [`INITIALIZE`](crate::methods::INITIALIZE).
///
/// The wire form is sparse: an absent key means "not supported", which
/// deserialises to `false` (the fail-closed reading — an omitted or
/// unrecognised capability grants nothing).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize,
)]
#[serde(default)]
pub struct PluginCapabilities {
    /// The plugin serves remote tools: it answers
    /// [`TOOLS_LIST`](crate::methods::TOOLS_LIST) and
    /// [`TOOLS_CALL`](crate::methods::TOOLS_CALL).
    pub tools: bool,
    /// The plugin accepts [`EVENT`](crate::methods::EVENT)
    /// notifications to observe the run.
    pub observe: bool,
    /// The plugin can rewrite tool results through
    /// [`TRANSFORM`](crate::methods::TRANSFORM).
    pub transform: bool,
    /// The plugin answers [`GATE`](crate::methods::GATE) — the
    /// escalation capability.
    ///
    /// Announcing it grants nothing: the host speaks
    /// [`GATE`](crate::methods::GATE) only to a
    /// plugin its config promoted to full authority, and a promotion
    /// *requires* this flag. A plugin that leaves it out can never be
    /// gated, so promoting it is refused at install instead of leaving
    /// every call in the host to fail closed.
    pub gate: bool,
    /// The plugin publishes facts of its own to the host through
    /// [`PUBLISH`](crate::methods::PUBLISH) notifications.
    ///
    /// The inverse of [`observe`](Self::observe): `observe` carries
    /// the host's events *to* the plugin, `publish` carries the
    /// plugin's events *from* it. A plugin that leaves the key out
    /// is never heard — its `elysper/publish` notifications are
    /// dropped, not answered, because a notification has no reply
    /// the host could refuse it with.
    pub publish: bool,
}

/// A remote tool descriptor, shaped like an MCP tool: a name, a
/// description, and a JSON Schema for the arguments.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// The tool's callable name.
    pub name: String,
    /// What the tool does, for the model's prompt.
    pub description: String,
    /// JSON Schema for the `arguments` object passed to
    /// [`TOOLS_CALL`](crate::methods::TOOLS_CALL).
    pub input_schema: serde_json::Value,
}

/// An observable event sent as a
/// [`EVENT`](crate::methods::EVENT) notification.
///
/// This is the *wire* vocabulary; it is intentionally narrower than
/// the harness's internal hook events and independent of them.
/// Serialised internally tagged by `"type"`, e.g.
/// `{"type":"before_tool","tool_name":"read",…}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PluginEvent {
    /// A tool call is about to run.
    BeforeTool {
        /// The tool being called.
        tool_name: String,
        /// The host's identifier for this call.
        call_id: String,
        /// The JSON arguments the tool will receive.
        arguments: serde_json::Value,
    },
    /// A tool call has completed.
    AfterTool {
        /// The tool that ran.
        tool_name: String,
        /// The host's identifier for this call.
        call_id: String,
        /// Whether the call failed.
        is_error: bool,
    },
    /// An agent-loop iteration is starting.
    BeforeStep {
        /// Zero-based loop iteration.
        iteration: u32,
    },
    /// An agent-loop iteration has finished.
    AfterStep {
        /// Zero-based loop iteration.
        iteration: u32,
    },
    /// A turn is starting for the named agent.
    TurnStart {
        /// The agent whose turn is starting.
        agent_name: String,
    },
    /// A turn has ended.
    TurnEnd {
        /// The agent whose turn ended.
        agent_name: String,
        /// Why the turn ended, as the host reports it.
        reason: String,
    },
}

/// An event a plugin publishes to the host, sent as a
/// [`PUBLISH`](crate::methods::PUBLISH) notification.
///
/// The inverse direction of [`PluginEvent`]: that type is what the
/// host tells a plugin it observes, and this is what the plugin
/// tells the host on its own initiative. The kind is the plugin's
/// own vocabulary — the host records it, it does not interpret it —
/// and the payload is free-form JSON, so the same stable-wire rule
/// holds: a rename inside the host is not a protocol break.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginPublish {
    /// Plugin-defined event kind, e.g. `"custom"`.
    pub kind: String,
    /// Plugin-defined payload, as the plugin wrote it.
    pub payload: serde_json::Value,
}

/// A plugin's answer to [`GATE`](crate::methods::GATE).
///
/// All four variants exist even though a reduced-authority plugin may
/// only return [`PluginDecision::Allow`] and
/// [`PluginDecision::Terminate`]: the host decides what it accepts,
/// and the type stays complete so the elevated path has a vocabulary.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PluginDecision {
    /// Let the call proceed.
    Allow,
    /// Veto the call; `reason` is shown to the model.
    Block {
        /// Why the call was blocked.
        reason: String,
    },
    /// Pause for a human decision; `reason` explains the request.
    Ask {
        /// Why the call needs approval.
        reason: String,
    },
    /// Stop the run; `reason` is surfaced to the caller.
    Terminate {
        /// Why the run is being stopped.
        reason: String,
    },
}

/// The stable error codes of the channel, serialised as snake_case
/// strings (`"unavailable"`, `"timeout"`, …).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum PluginError {
    /// The plugin process or endpoint cannot be reached.
    Unavailable,
    /// The call exceeded its deadline.
    Timeout,
    /// The host's policy refused the call.
    Denied,
    /// The request did not match the protocol.
    BadRequest,
    /// The plugin failed internally.
    Internal,
}
