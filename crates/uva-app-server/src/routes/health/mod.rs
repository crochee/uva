//! The probe and catalogue surfaces.
//!
//! - [`version`] — the build identity baked into the binary.
//! - [`livez`] — liveness: "is this process still serving?".
//! - [`readyz`] — readiness: "should traffic be routed here now?".
//! - [`list_models`] — the model catalogue the UI's model picker
//!   reads.
//!
//! The gateway serves the HTTP spellings of these (`/version`,
//! `/livez`, `/readyz`); the core answers them as methods
//! (`health/version`, `health/livez`, `health/readyz`).
/// Minimal probe response body shared by the liveness and readiness
/// probes.
use std::sync::Arc;

use schemars::JsonSchema;
use serde::Serialize;

use crate::state::AppState;

#[derive(Serialize)]
pub struct ProbeResponse {
    /// `"ok"` when the probe passes, `"unavailable"` when it does
    /// not.
    pub status: &'static str,
    /// Names of readiness checks that failed. Omitted when every
    /// check passes; only meaningful for readiness.
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    pub failed: Vec<&'static str>,
}
/// The build identity.
///
/// Captures the values `crate::build_info` bakes into the binary at
/// compile time. A deployment reaches for them when triaging "which
/// build is running on prod?": the value is whatever was linked into
/// the binary, not what the runtime environment says now, so a
/// Kubernetes pod can be inspected without registry access.
#[derive(Serialize)]
pub struct VersionResponse {
    /// Cargo package version (e.g. `0.1.0`).
    pub version: &'static str,
    /// Full git commit SHA the binary was built from, or `"unknown"`
    /// when `UVA_GIT_SHA` was unset at compile time.
    pub git_sha: &'static str,
    /// First 7 hex chars of `git_sha`, for display.
    pub short_sha: &'static str,
    /// ISO-8601 wall-clock of the build moment, or `"unknown"`.
    pub build_time: &'static str,
    /// Target triple the binary was compiled for.
    pub target: &'static str,
    /// Compile profile (`release` for CI builds, `debug` for
    /// `cargo run`).
    pub profile: &'static str,
    /// One-line summary in `git describe --long --dirty` style.
    pub long_version: String,
}
/// The build identity.
///
/// The fields are `&'static str` because they are baked at compile
/// time via `option_env!`, so the answer touches no shared state and
/// never blocks.
pub fn version() -> VersionResponse {
    VersionResponse {
        version: crate::build_info::VERSION,
        git_sha: crate::build_info::GIT_SHA,
        short_sha: crate::build_info::short_sha(),
        build_time: crate::build_info::BUILD_TIME,
        target: crate::build_info::TARGET,
        profile: crate::build_info::PROFILE,
        long_version: crate::build_info::long_version(),
    }
}
/// Liveness — "can this process still serve?".
///
/// Liveness answers exactly one question: "can this process still
/// answer at all?". If the handler runs at all, the answer is yes —
/// so it reports `ok` unconditionally without touching shared
/// state. Dependency health belongs on readiness: a liveness
/// failure gets the pod restarted, which must be reserved for
/// unrecoverable states (deadlocked runtime, exhausted executor).
pub fn livez() -> ProbeResponse {
    ProbeResponse {
        status: "ok",
        failed: Vec::new(),
    }
}
/// Readiness — "should traffic be routed here *now*?".
///
/// Unlike liveness it inspects in-process facts via
/// [`AppState::readiness_checks`]. A failing check yields
/// `"unavailable"` plus the failing check names, so an operator can
/// see *why* the instance is not ready from the answer itself.
pub fn readyz(state: Arc<AppState>) -> ProbeResponse {
    let failed: Vec<&'static str> = state
        .readiness_checks()
        .into_iter()
        .filter(|(_, passed)| !passed)
        .map(|(name, _)| name)
        .collect();
    if failed.is_empty() {
        return ProbeResponse {
            status: "ok",
            failed,
        };
    }
    tracing::warn!(checks = ?failed, "readiness probe failing");
    ProbeResponse {
        status: "unavailable",
        failed,
    }
}
/// Bare models-listing response.
#[derive(Clone, JsonSchema, Serialize)]
pub struct ModelsResponse {
    /// One entry per configured provider.
    pub models: Vec<ModelEntry>,
    /// The provider a turn runs under when it names none.
    pub default_provider: String,
    /// The model a turn runs under when it names none.
    pub default_model: String,
}
/// One provider's advertised model.
#[derive(Clone, JsonSchema, Serialize)]
pub struct ModelEntry {
    /// The provider's `[providers.<name>]` entry name.
    pub provider: String,
    /// The provider's default model id.
    pub model: String,
    /// The model's context window, in tokens.
    pub context_window: usize,
    /// Whether the provider accepts tool calls.
    pub supports_tools: bool,
    /// Whether the provider streams.
    pub supports_streaming: bool,
}
/// The model catalogue.
///
/// The body is fully determined at startup
/// (`workspace_config.providers` + `default_model` +
/// `default_provider`) and never mutates over the binary's lifetime,
/// so a caller may cache it freely.
pub fn list_models(state: Arc<AppState>) -> ModelsResponse {
    let config = state.workspace_config.read();
    ModelsResponse {
        models: config
            .providers
            .iter()
            .map(|(name, entry)| ModelEntry {
                provider: name.clone(),
                model: entry
                    .default_model
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                context_window: entry.context_window.unwrap_or(128_000),
                supports_tools: entry.supports_tools.unwrap_or(true),
                supports_streaming: entry.supports_streaming.unwrap_or(true),
            })
            .collect(),
        default_provider: config.default_provider.clone(),
        default_model: config.default_model.clone(),
    }
}
#[cfg(test)]
mod tests;
