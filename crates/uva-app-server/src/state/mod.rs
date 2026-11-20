//! Shared server state container — [`AppState`] and friends.
//!
//! ## Layout
//!
//! | Module | Responsibility |
//! |---|---|
//! | `app_state` | The `AppState` struct + accessors; its `boot` / `for_test` / `sessions` / `usage` / `tools` submodules hold the constructors, the controller cache, the usage counters, and the `AppliedToolSurface` record of what `[tools]` did. |
//! | `boot`      | The boot-time helpers (`load_default_*`, `register_configured_mcp_servers`, `apply_tools_config`, `build_prompt_context`, …) — one free function per concern, called in order from `AppState::with_server_config`. |
//! | `tests`     | The test suite, split by concern into one file per surface (registry fallback, prompt assembler, surface application, MCP boot, …). |
//!
//! ## Boot helpers
//!
//! The boot helpers are `pub(crate)` — they are internal
//! steps of the boot the public surface never names. The
//! `pub(crate) use` re-exports below give the `tests/`
//! submodules a single path to import them from
//! (`crate::state::xxx`) without leaking them outside the
//! crate.

mod app_state;
pub(crate) mod boot;

#[cfg(test)]
pub(crate) mod tests;

pub(crate) use app_state::MustExistError;
#[cfg(any(test, feature = "test-utils"))]
pub(crate) use app_state::catalogue_for_deployment;
// The boot's row-config seams: the value the boot materializes into a
// row, and the step that fills an empty row with it. The tests pin
// both — a deployment's profile row and the server config must not
// disagree about who owns a row's configuration.
#[cfg(test)]
pub(crate) use app_state::materialize_row_config;
#[cfg(test)]
pub(crate) use app_state::plugin_tool_registry;
#[cfg(test)]
pub(crate) use app_state::seeded_tool_registry;
/// The session-resolution module: the transcript-location knowledge
/// and the named policies a route asks for by name.
pub(crate) use app_state::session_resolution;
pub use app_state::{
    AppState,
    AppliedToolSurface,
    Assembly,
    DelegatedUsage,
    HarnessEvalAgent,
    ModelSelectionError,
    ProviderCatalogue,
    UsageMetrics,
    UsageSnapshot,
};
#[cfg(test)]
pub(crate) use boot::agent_loop_row_config;

/// Active session controllers keyed by `(user_id, session_id)`.
///
/// A type alias (not a newtype): every use site wants the map's own
/// API, and the key's meaning is fixed by [`AppState`]'s
/// field of this type.
pub type ActiveSessions = dashmap::DashMap<
    (String, String),
    std::sync::Arc<crate::session::controller::SessionController>,
>;

/// One cold-open gate per `(user_id, session_id)`.
///
/// The map holds a **weak** reference to each gate: the session's
/// controller owns the lock (and with it the only strong reference),
/// so an entry cannot outlive its controller and the map cannot grow
/// with the number of sessions the process has ever opened.
pub type SessionSpawnGates =
    dashmap::DashMap<(String, String), std::sync::Weak<tokio::sync::Mutex<()>>>;
// The strategy resolver the `agent-loop` row's config needs: it
// turns the alias the deployment's agent table names into a strategy.
// It is a crate-internal step of the boot, so it travels the same
// `pub(crate)` path as the rest.
pub(crate) use boot::resolve_strategy;
#[cfg(test)]
pub(crate) use boot::{
    apply_tools_config,
    build_prompt_context,
    load_default_compaction,
    load_default_max_iterations,
    load_default_strategy_alias,
    load_default_tool_restriction,
    register_configured_mcp_servers,
    unapplied_agent_keys,
};
