//! The protocol adapters: one core, many doors.
//!
//! The Harness core keeps **one** internal event stream — sessions,
//! turns, tool calls, permission requests, cancellation — and says no
//! protocol at all. Every client boundary is an adapter over that
//! stream:
//!
//! | adapter | speaks | for |
//! |---|---|---|
//! | [`acp`] | ACP v1 (Agent Client Protocol) | the headless control plane: editors, IDEs, any ACP-capable client |
//! | [`agui`] | AG-UI | the web front-end's protocol-shaped door |
//!
//! ACP is the *native* language of the headless control plane, not a
//! universal one: a browser does not speak ACP, and a cloud
//! scheduler does not either. Those boundaries get their own adapter
//! (AG-UI for the browser; A2A or UHP for a cloud that dispatches
//! the harness as an agent), and the core stays the same core.
//!
//! # The rule the adapters follow
//!
//! An adapter reads the core stream and writes one protocol's frames.
//! It never reaches past the stream into the harness's internals, and
//! the core never learns which protocol is watching it: the core
//! publishes events, the adapters translate.

pub mod acp;
pub mod agui;
