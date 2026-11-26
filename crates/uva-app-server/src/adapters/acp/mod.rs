//! The ACP adapter: the Agent Client Protocol over the core's stream.
//!
//! ACP is the headless control plane's *native* language: an editor
//! (Zed, a JetBrains IDE), a terminal client, any ACP-capable
//! program speaks it. The adapter turns the core's own event stream
//! into ACP's session updates and answers ACP's methods with the
//! deployment's own sessions — the same sessions, the same durable
//! transcript, the same held calls the deployment's own surfaces
//! see.
//!
//! # What the adapter is not
//!
//! The adapter does not own the conversation's meaning. The core
//! publishes the events; the adapter decides what the wire says.
//! That is why the protocol's message types come from the SDK rather
//! than from a local copy: the shape a client decodes is the shape
//! this agent writes.
//!
//! # The pieces
//!
//! - [`agent`] — the methods: the handshake, the sessions, the turn.
//! - [`files`] — the client that owns a session's files: the
//!   `fs/*` delegation a capable client advertises in `initialize`.
//! - [`terminal`] — the client that owns a session's terminal: the
//!   `terminal/*` delegation a capable client advertises in
//!   `initialize`.
//! - [`projection`] — the vocabulary: the core's events become the
//!   protocol's session updates, in both directions (live and
//!   replay).
//! - [`turn`] — one turn, from the prompt to the stop reason.
//! - [`approval`] — the gate that holds a risky call for the
//!   operator.
//! - [`stdio`] — the transport: the process's pipes.
//! - [`channel`] — the endpoint: the ACP frames on the core's own
//!   channel, for the clients that reach the deployment over the
//!   gateway's WebSocket.
//!
//! # The parity
//!
//! The reference harnesses — DeepSeek Harness (`dsh`), pi and Codex
//! — each answer an ACP client with the same behaviour: a handshake
//! that names the capabilities, a session that outlives the process,
//! a turn that streams its deltas, a held call that asks the
//! operator, and a cancellation the client can issue at any time.
//! This adapter reproduces that behaviour on the deployment's own
//! machinery. The differences are the ones the capabilities declare:
//! no session modes, no slash commands, no client-side file system or
//! terminal delegation, no per-session MCP servers.

pub mod agent;
pub mod approval;
pub mod channel;
pub mod files;
pub mod projection;
pub mod stdio;
pub mod terminal;
#[cfg(test)]
mod terminal_tests;
pub mod turn;

pub use agent::AcpAgent;
pub use stdio::run;
