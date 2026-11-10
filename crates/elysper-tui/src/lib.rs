//! A terminal projection of the elysper gateway.
//!
//! The crate is the TUI half of the App Server's projection story: it
//! connects to an `elysper-app-server serve` gateway — the same
//! `/rpc` WebSocket the browser and the desktop shell speak, through
//! the shared [`elysper_gateway`] client — and paints the session
//! projection with ratatui. It is an application crate
//! (`publish = false`), so it may depend on `elysper-gateway` and on a
//! concrete runtime.
//!
//! The layers exist so the interactive part is testable without a tty:
//!
//! - [`transport`] — the domain adapter over the gateway client
//!   (method mapping, id correlation, the catch-all notification
//!   subscription, the re-follow, the liveness probe);
//! - [`app`] — the pure reducer (envelopes in, [`app::Action`]s out);
//! - [`domains`] — one column table per browsable domain (list
//!   method, row pointer, columns, operations); the single place a
//!   new directory domain is added;
//! - [`render`] — ratatui drawing only;
//! - [`ui_schema`] — the terminal's copy of the deployment's UI tree
//!   (the `ui/schema` wire, read with this crate's own DTOs);
//! - [`views`] — the §6.1 skeleton's region painters (sidebar,
//!   conversation, todo, composer, settings) that [`render`]
//!   dispatches to;
//! - `main` — clap, the terminal guard, and the `tokio::select!` loop
//!   over crossterm events, transport events and a render tick.
//!
//! # Wire contract
//!
//! The gateway speaks JSON-RPC 2.0 over the WebSocket at `/rpc`: one
//! text message is one envelope. The `Content-Length` framing lives
//! between the gateway and its core and never reaches this crate. `id`
//! may be a string or a number and is echoed verbatim. The core
//! announces `ready` after boot — the gateway broadcasts it to every
//! live connection, and a connection that missed the boot hears it
//! from the transport's liveness probe; `approval/request`
//! notifications carry an `approval_id` that is *not* the JSON-RPC
//! `id`, and the answer is an ordinary `approval/respond` request.
//!
//! # Restart
//!
//! Process supervision is the gateway crate's: a dead `serve` is
//! relaunched with a 1s → 5s backoff, and a dead core is restarted the
//! same way underneath it. What the TUI keeps is its own subscription:
//! the `sessions/follow` the transport recorded is re-issued whenever
//! the connection is new again — on a core's `ready` broadcast, on a
//! supervisor restart, and when the liveness probe first answers after
//! an outage — so the transcript rebuilds itself from the baseline
//! without the operator doing anything.

#![forbid(unsafe_code)]

pub mod app;
pub mod domains;
pub mod render;
pub mod transport;
pub mod ui_schema;
pub mod views;

#[cfg(test)]
pub(crate) mod tests;
