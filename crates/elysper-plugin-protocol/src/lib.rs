//! # elysper-plugin-protocol
//!
//! The wire contract for Elysper's **dynamic plugin channel**: the
//! JSON-RPC method names, the capability handshake, the event and
//! decision vocabulary, and the stable error codes. It is
//! **serde-only** — [`serde`] and [`serde_json`] are the entire
//! dependency surface — so a plugin author in any language can read
//! this crate (or the JSON shapes it documents) without inheriting
//! the harness, a runtime, or a provider stack.
//!
//! ## Layout
//!
//! - [`methods`] — the `elysper/*` method-name constants. Never
//!   hard-code the strings elsewhere; both sides of the channel agree
//!   through these.
//! - [`wire`] — the request/response payload types: capability
//!   negotiation, remote tool descriptors, observed events, gate
//!   decisions, and error codes.
//!
//! ## The conversation
//!
//! JSON-RPC 2.0 over stdio or HTTP, reusing the MCP transport
//! abstraction on the host side.
//!
//! | Method | Host sends | Plugin sends back |
//! |---|---|---|
//! | [`methods::INITIALIZE`] | initialize request | [`PluginCapabilities`] |
//! | [`methods::TOOLS_LIST`] | list request | `Vec<`[`ToolSpec`]`>` |
//! | [`methods::TOOLS_CALL`] | tool name + JSON arguments | tool result JSON |
//! | [`methods::TRANSFORM`] | tool result JSON | rewritten result JSON |
//! | [`methods::EVENT`] | [`PluginEvent`] | nothing (notification) |
//! | [`methods::PUBLISH`] | nothing (notification) | [`PluginPublish`] |
//! | [`methods::GATE`] | the call to gate | [`PluginDecision`] (elevated only) |
//!
//! The handshake declares the roles a plugin can take. `tools`,
//! `observe` and `transform` are the default authority's;
//! `publish` is the plugin's own voice — the notification's
//! direction is reversed, so the role is what lets the host
//! *listen*. `gate` is the escalation: it is how a plugin says it
//! implements [`methods::GATE`] and a precondition of the host's
//! promotion — a plugin that does not announce it is never gated,
//! and the host refuses to promote it rather than block every call
//! it would be asked about.
//!
//! ## Deliberately not the harness's vocabulary
//!
//! [`PluginEvent`] and [`PluginDecision`] are an **independent, stable
//! wire vocabulary**, not a mirror of the harness's internal
//! `HookEvent` / `HookDecision` types. Internal Rust enums evolve with
//! the implementation; a wire contract built from them would turn an
//! internal rename into a protocol break for every external plugin.
//! `elysper-plugin-host` maps between the two, so this crate never
//! depends on the harness (and the channel stays acyclic).
//!
//! Two consequences are intentional:
//!
//! - [`PluginEvent`] covers only what a remote plugin can perceive
//!   (tool and step/turn boundaries), not the harness's full event
//!   vocabulary.
//! - [`PluginDecision`] defines all four variants, including
//!   [`PluginDecision::Block`] and [`PluginDecision::Ask`], even though
//!   a reduced-authority plugin is never *allowed* to return them. The
//!   host decides whether to accept them; the type stays complete so
//!   the elevated path has a vocabulary.
//!
//! ## Example
//!
//! ```
//! use elysper_plugin_protocol::{PluginDecision, PluginEvent, methods};
//!
//! assert_eq!(methods::GATE, "elysper/gate");
//!
//! let event = PluginEvent::AfterTool {
//!     tool_name: "read".to_string(),
//!     call_id: "c1".to_string(),
//!     is_error: false,
//! };
//! let json = serde_json::to_value(&event).unwrap();
//! assert_eq!(json["type"], "after_tool");
//!
//! let decision = PluginDecision::Block {
//!     reason: "no".to_string(),
//! };
//! assert_eq!(serde_json::to_value(&decision).unwrap()["type"], "block");
//! ```

pub mod methods;
pub mod wire;

pub use wire::{
    PluginCapabilities,
    PluginDecision,
    PluginError,
    PluginEvent,
    PluginPublish,
    ToolSpec,
};

/// The README, **compiled**.
///
/// The `rust` fences in [`README.md`](../README.md) are doctests, so
/// a wiring recipe that stops compiling fails
/// `cargo test -p elysper-plugin-protocol`.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct Readme;
