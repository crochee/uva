//! # uva-app-server
//!
//! The application crate: an axum REST + SSE surface over the library
//! pieces, and the only crate in the workspace allowed to reach for a
//! concrete runtime, `anyhow`, or a database driver.
//!
//! It is deliberately **not** part of the assembly story for library
//! consumers — `uva` (the facade) plus `crates/uva-harness` are.
//! What this crate is good for is showing how the pieces are wired end
//! to end: one process clock, one tool registry, one host policy,
//! one agent registry, and a run factory that every chat dispatch goes
//! through.
//!
//! ## The boot
//!
//! ```text
//! clap args ─┐
//!            ├─ resolve_server_config ──► AppState ◄── routes
//! env vars  ─┘        (one read)             │
//!                                     [agents]/[tools]/auth/cors/
//!                                     mcp_servers applied
//!
//! [`config::resolve_server_config`] resolves the deployment file once
//! (`--config`, then `<dir>/config.toml`, then `<dir>/.uva/config.toml`)
//! and everything read at boot comes from that value; the bind address
//! resolves from `--host`/`--port`, `UVA_HOST`/`UVA_PORT`, and
//! that config, in that order. `server::boot_server` returns the router
//! and the resolved address together, which is what `main` binds.
//!
//! ## Surfaces
//!
//! - `POST /api/v1/chat/sessions/…` — the chat surface: create, send,
//!   stream (SSE), cancel, regenerate, feedback.
//! - `/api/v1/{agents,skills,tools,memory,sessions,models}` — management,
//!   behind the API-key layer (`[auth]`).
//! - `/livez`, `/readyz`, `/metrics` — probes and Prometheus, mounted
//!   outside the auth layer because an orchestrator hits them every
//!   second.
//!
//! ## What is here
//!
//! - [`state::AppState`] — the assembled process state and the one
//!   `SessionController` per `(user, session)`.
//! - [`config`] — the deployment types, the YAML bridge the CLI's
//!   `--config` file goes through, and the resolver above.
//! - [`session`] — the controller: parked turns, run factory, event
//!   stream, durability, compaction checkpoints.
//! - [`shutdown`] — the stop budget: the deadlines the graceful
//!   shutdown path runs under, and the controller drain.
//! - [`routes`] — the HTTP handlers, thin by design.
//! - [`middleware`] — auth, tracing, trace-context propagation, error
//!   envelope, response headers, RED metrics.
//!
//! See `DEPLOYMENT.md` for the deployment contract (config keys,
//! variables, probes, Nginx notes).

/// How long a test waits for a thing that is supposed to arrive.
///
/// The waits in this crate's tests are **not** assertions about
/// latency — they are waits for work that is already under way
/// (a controller's decision, a row's arrival). A deadline that is
/// tight enough to fail on a busy machine turns a slow runner into a
/// red suite, and a red suite that nobody can reproduce is worse
/// than no suite: it hides the real failures behind noise.
///
/// Thirty seconds is far longer than any of these waits needs on an
/// idle machine and still bounded, so a genuinely broken path fails
/// instead of hanging.
#[cfg(test)]
pub(crate) const TEST_DEADLINE: std::time::Duration =
    std::time::Duration::from_secs(30);

pub mod adapters;
pub mod agent_run_config;
pub mod agent_run_config_types;
pub mod api;
pub mod boot;
pub mod build_info;
pub mod cli;
pub mod cluster;
pub mod composition;
pub mod config;
pub mod event_stream;
pub mod gateway;
pub mod profile;
pub mod routes;
pub mod rpc;
pub mod search;
pub mod session;
pub mod shutdown;
pub mod spawner;
pub mod state;
pub mod translate;
pub mod ui;

pub use config::server::ServerConfig;
pub use event_stream::EventBroadcaster;
pub use state::{AppState, AppliedToolSurface, UsageMetrics, UsageSnapshot};
