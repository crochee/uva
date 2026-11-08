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

use elysper::{plugin_host::Authority, tool::Risk};
use serde::Serialize;

use crate::{
    composition::{Composition, PluginSource},
    rpc::RpcError,
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
/// One plugin bundle the deployment's layout holds.
#[derive(Debug, Serialize)]
pub struct BundleInfo {
    /// The bundle's name (its directory, as its manifest confirms).
    pub name: String,
    /// Its version, when the manifest declares one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// What it does, when the manifest says.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Whether the bundle declares a code component — a subprocess
    /// the host runs over the plugin protocol. A bundle without one
    /// still contributes definitions (skills, agents, tasks).
    pub has_code: bool,
    /// Where the bundle was found: `project` (`<workspace>/.agents`)
    /// or `global` (`~/.agents`).
    pub origin: &'static str,
}

/// `plugins/list` response body.
///
/// Not a [`List`](crate::api::List): the composition is a boot-time
/// record, not a paginated collection, and the halves are
/// distinguished by which array an entry is in rather than by a
/// status field the client would have to filter on.
#[derive(Debug, Serialize)]
pub struct PluginsResponse {
    pub resolved: Vec<PluginInfo>,
    pub skipped: Vec<SkippedPluginInfo>,
    /// The plugin bundles the layout holds, whether or not they
    /// installed a code component — a bundle is also a definition
    /// carrier (skills, agents, tasks), and this is the operator's
    /// view of what the deployment declared.
    pub bundles: Vec<BundleInfo>,
}

/// `plugins/list` — the deployment's plugin composition and bundles.
pub async fn list_plugins(
    state: Arc<AppState>,
) -> Result<PluginsResponse, RpcError> {
    Ok(plugins_response(&state.plugins, &state.workspace_root))
}

/// Project a composition and the layout's bundles onto the wire.
fn plugins_response(
    composition: &Composition,
    workspace_root: &std::path::Path,
) -> PluginsResponse {
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
        bundles: bundle_infos(workspace_root),
    }
}

/// Every plugin bundle in the two-layer layout, in precedence order.
fn bundle_infos(workspace_root: &std::path::Path) -> Vec<BundleInfo> {
    let project = workspace_root.join(elysper::core::layout::AGENTS_DIR);
    let mut out: Vec<BundleInfo> =
        elysper::core::layout::plugin_bundles(&project)
            .into_iter()
            .map(|(name, path)| bundle_info(name, path, "project"))
            .collect();
    if let Some(global) = elysper::core::layout::home_root() {
        out.extend(
            elysper::core::layout::plugin_bundles(&global)
                .into_iter()
                .map(|(name, path)| bundle_info(name, path, "global")),
        );
    }
    out
}

/// One bundle onto the wire: the manifest's facts when it parses,
/// the directory's name and defaults when it does not.
fn bundle_info(
    name: String,
    path: std::path::PathBuf,
    origin: &'static str,
) -> BundleInfo {
    match elysper::core::layout::plugin_manifest(&path) {
        Some(manifest) => BundleInfo {
            name: manifest.name,
            version: manifest.version,
            description: manifest.description,
            has_code: manifest.code.is_some(),
            origin,
        },
        None => BundleInfo {
            name,
            version: None,
            description: None,
            has_code: false,
            origin,
        },
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
