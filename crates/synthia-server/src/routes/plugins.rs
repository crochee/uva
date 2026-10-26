//! Deployment plugin composition: `GET /api/v1/plugins`.
//!
//! [`Composition`] is the boot's record of what the `[plugins]` section
//! installed and what it skipped, and it has been write-only: an
//! operator could configure a plugin, watch boot log `plugin skipped;
//! boot continues`, and then have no way to ask *what* was skipped or
//! *why*. This route is that reader.
//!
//! Both sets are reported because either one alone is misleading. A
//! resolved list without the skips claims the deployment runs a
//! capability a process that never started does not provide; a skip
//! list without the resolved set cannot answer "then what does this
//! build actually have?". The skipped set carries the reason verbatim
//! as the composition recorded it (a spawn failure, a handshake
//! failure, an unlinked static name, a tool name the registry
//! refused), which is the string the operator needs to fix the entry.
//!
//! The composition is fixed after boot (a plugin's lifetime is its
//! handle, and the handles live on `AppState`), so this handler reads
//! no lock and allocates only its own projection.

use std::sync::Arc;

use axum::{Json, extract::State};
use serde::Serialize;
use synthia::{plugin_host::Authority, tool::Risk};

use crate::{
    composition::{Composition, PluginSource},
    state::AppState,
};

/// One plugin the deployment installed.
#[derive(Debug, Serialize)]
pub struct PluginInfo {
    /// The name from the `[plugins]` entry.
    pub name: String,
    /// `"static"` (compiled into this binary, installed in-process) or
    /// `"remote"` (a process reached over the plugin protocol).
    pub kind: &'static str,
    /// The authority the plugin holds: a remote entry gets what its
    /// config granted, a static one is in-process code and reports
    /// `full`.
    pub authority: Authority,
    /// The worst risk among the tools it claimed, or `unknown` when it
    /// claimed none.
    pub risk: Risk,
    /// The tool names it claimed, in registration order.
    pub tools: Vec<String>,
}

/// One configured entry that did not install.
#[derive(Debug, Serialize)]
pub struct SkippedPluginInfo {
    /// The configured name, or the registration a diagnostic named.
    pub name: String,
    /// Why the entry was skipped, as the composition recorded it.
    pub reason: String,
}

/// `GET /api/v1/plugins` response body.
///
/// Not a [`List`](crate::api::List): the composition is a boot-time
/// record, not a paginated collection, and the two halves are
/// distinguished by which array an entry is in rather than by a status
/// field the client would have to filter on.
#[derive(Debug, Serialize)]
pub struct PluginsResponse {
    pub resolved: Vec<PluginInfo>,
    pub skipped: Vec<SkippedPluginInfo>,
}

/// GET /api/v1/plugins - The deployment's plugin composition.
pub async fn list_plugins(
    State(state): State<Arc<AppState>>,
) -> Json<PluginsResponse> {
    Json(plugins_response(&state.plugins))
}

/// Project a composition onto the wire.
fn plugins_response(composition: &Composition) -> PluginsResponse {
    PluginsResponse {
        resolved: composition
            .resolved()
            .iter()
            .map(|plugin| PluginInfo {
                name: plugin.name.clone(),
                kind: kind(plugin.source),
                authority: plugin.authority,
                risk: plugin.risk,
                tools: plugin.tools.clone(),
            })
            .collect(),
        skipped: composition
            .skipped()
            .iter()
            .map(|plugin| SkippedPluginInfo {
                name: plugin.name.clone(),
                reason: plugin.reason.clone(),
            })
            .collect(),
    }
}

/// The wire name of a plugin's channel.
fn kind(source: PluginSource) -> &'static str {
    match source {
        PluginSource::Static => "static",
        PluginSource::Remote => "remote",
    }
}

#[cfg(test)]
mod tests;
