//! Shared gateway support for uva's hosts.
//!
//! Two halves:
//!
//! - [`Gateway`] — the **process**: launch `uva-app-server
//!   serve`, wait for `/readyz` to answer `200`, restart it with a
//!   1s → 5s backoff when it dies, and reap it on drop. The TUI and
//!   the desktop shell both need exactly this.
//! - [`GatewayClient`] — the **protocol**: JSON-RPC 2.0 over the
//!   gateway's WebSocket (`/rpc`), correlating responses by `id`,
//!   fanning notifications out to subscribers, and replaying
//!   subscriptions after a reconnect.
//!
//! The public surface uses `futures` only (no `tokio::` paths), so
//! `make check-public-api-runtime` needs no exemption for this crate.
//! `tokio` is an implementation detail.

#![forbid(unsafe_code)]

mod client;
mod supervisor;
#[cfg(test)]
mod tests;

pub use client::{
    ClientError,
    GatewayClient,
    NotificationHandler,
    Subscription,
};
pub use supervisor::{Gateway, GatewayError, GatewayOptions, GatewayState};
