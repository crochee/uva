//! The `~/.agents/` layout: where definitions live, and which one
//! wins.
//!
//! Uva keeps its **definitions** — skills, agents, tasks, MCP
//! server declarations — in a two-layer home:
//!
//! - the **global** layer: `~/.agents/` (canonical) and
//!   `~/.uva/` (legacy, read-only);
//! - the **project** layer: `<workspace-root>/.agents/`.
//!
//! Each layer may hold its own definitions directly and may hold
//! **plugin bundles**: a `plugins/<name>/` directory with a
//! `plugin.json` manifest and any subset of `skills/`, `agents/`,
//! `tasks/` and `mcp.json`. A bundle is an isomorphic subtree — the
//! same shape as the layer that holds it — so a plugin is just
//! another place a definition can come from.
//!
//! # Precedence
//!
//! Four sources, in this order:
//!
//! 1. the project's own definitions;
//! 2. the project's plugin bundles;
//! 3. the global definitions;
//! 4. the global plugin bundles.
//!
//! The first source that offers a name wins; a later source with the
//! same name is dropped. That ordering is what
//! [`merge_by_name`](crate::layout::merge_by_name)
//! implements.
//!
//! # Runtime state is not here
//!
//! This directory holds **definitions and configuration only**.
//! Sessions, schedules, workflows, evals and attachments are runtime
//! state and stay in the workspace root — they are the deployment's
//! durable truth, not the user's configuration.
//!
//! # No runtime, no async
//!
//! The module is std-only: it reads directories and parses a
//! manifest. Nothing here blocks a runtime thread or names one.

use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

/// The manifest file a plugin bundle carries.
pub const PLUGIN_MANIFEST: &str = "plugin.json";

/// The MCP server declaration file.
pub const MCP_CONFIG: &str = "mcp.json";

/// The directory the definitions live in, under each root.
pub const AGENTS_DIR: &str = ".agents";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// Which layer a definition came from.
pub enum Layer {
    /// The project layer: `<workspace-root>/.agents/`.
    Project,
    /// The global layer: `~/.agents/` (or the legacy `~/.uva/`).
    Global,
}

impl Layer {
    #[must_use]
    /// The layer's short name, for logs and operator-facing labels.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Project => "project",
            Self::Global => "global",
        }
    }
}

impl std::fmt::Display for Layer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// A definition domain: a directory of named definitions.
pub enum Domain {
    /// Skills: `skills/<name>/SKILL.md`.
    Skills,
    /// Agents: `agents/<name>/agent.md`.
    Agents,
    /// Tasks: `tasks/<name>/task.md`.
    Tasks,
}

impl Domain {
    #[must_use]
    /// The domain's directory name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Skills => "skills",
            Self::Agents => "agents",
            Self::Tasks => "tasks",
        }
    }

    #[must_use]
    /// Whether the legacy global root contributes to this domain.
    ///
    /// Only skills do: the legacy `~/.uva/` tree predates the
    /// agents and tasks domains, so it has nothing to offer them.
    pub fn reads_legacy_home(self) -> bool {
        matches!(self, Self::Skills)
    }
}

impl std::fmt::Display for Domain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One place a definition may come from.
pub struct SourceDir {
    /// The directory the definitions live in.
    pub path: PathBuf,
    /// Whether the source belongs to the project or the global layer.
    pub layer: Layer,
    /// The plugin bundle the source belongs to, if any.
    pub plugin: Option<String>,
}

impl SourceDir {
    #[must_use]
    /// A source that belongs to no plugin bundle.
    pub fn direct(path: PathBuf, layer: Layer) -> Self {
        Self {
            path,
            layer,
            plugin: None,
        }
    }

    #[must_use]
    /// A source inside a plugin bundle.
    pub fn bundled(path: PathBuf, layer: Layer, plugin: String) -> Self {
        Self {
            path,
            layer,
            plugin: Some(plugin),
        }
    }
}

#[must_use]
/// The global definitions root: `$HOME/.agents`.
///
/// `None` when the environment has no `$HOME` — a container without
/// a home directory, say. The caller then has only the project layer.
pub fn home_root() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(AGENTS_DIR))
}

#[must_use]
/// The legacy global root: `$HOME/.uva`.
///
/// The canonical global root is [`home_root`]; this one is the
/// migration source and stays readable.
pub fn legacy_home_root() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".uva"))
}

#[must_use]
/// The global root, falling back to the project's own directory.
///
/// A process without a `$HOME` still has a project layer, and that
/// layer is the sensible stand-in for the global one: the
/// definitions are still found, they are just not global.
pub fn home_root_or(workspace_root: &Path) -> PathBuf {
    match home_root() {
        Some(root) => root,
        None => workspace_root.join(AGENTS_DIR),
    }
}

#[must_use]
/// Every source a domain's definitions may come from, in precedence
/// order.
///
/// The order is the contract: the project layer's own definitions,
/// then its plugin bundles, then the global layer's own
/// definitions, then its plugin bundles, and — for the domains that
/// read it — the legacy global root last.
pub fn sources(workspace_root: &Path, domain: Domain) -> Vec<SourceDir> {
    let mut out = Vec::new();

    let project_root = workspace_root.join(AGENTS_DIR);
    out.push(SourceDir::direct(
        project_root.join(domain.as_str()),
        Layer::Project,
    ));
    for (name, bundle) in plugin_bundles(&project_root) {
        out.push(SourceDir::bundled(
            bundle.join(domain.as_str()),
            Layer::Project,
            name,
        ));
    }

    let Some(global_root) = home_root() else {
        return out;
    };
    out.push(SourceDir::direct(
        global_root.join(domain.as_str()),
        Layer::Global,
    ));
    for (name, bundle) in plugin_bundles(&global_root) {
        out.push(SourceDir::bundled(
            bundle.join(domain.as_str()),
            Layer::Global,
            name,
        ));
    }

    if domain.reads_legacy_home()
        && let Some(legacy_root) = legacy_home_root()
    {
        out.push(SourceDir::direct(
            legacy_root.join(domain.as_str()),
            Layer::Global,
        ));
    }
    out
}

#[must_use]
/// Every `mcp.json` the deployment may declare servers in, in
/// precedence order.
///
/// The shape mirrors [`sources`]: the project's own file, the
/// project's plugin bundles, the global file, the global plugin
/// bundles, and the legacy global file.
///
/// Each entry carries the **namespace** the file's servers are
/// registered under: the plugin bundle's name, or the empty string
/// for a file that belongs to no bundle. A namespaced server is
/// registered as `<plugin>/<server>`, so two bundles may declare a
/// server of the same name without shadowing each other.
pub fn mcp_config_paths(workspace_root: &Path) -> Vec<(String, PathBuf)> {
    let mut out = Vec::new();

    let project_root = workspace_root.join(AGENTS_DIR);
    out.push((String::new(), project_root.join(MCP_CONFIG)));
    for (name, bundle) in plugin_bundles(&project_root) {
        out.push((name, bundle.join(MCP_CONFIG)));
    }

    let Some(global_root) = home_root() else {
        return out;
    };
    out.push((String::new(), global_root.join(MCP_CONFIG)));
    for (name, bundle) in plugin_bundles(&global_root) {
        out.push((name, bundle.join(MCP_CONFIG)));
    }
    if let Some(legacy_root) = legacy_home_root() {
        out.push((String::new(), legacy_root.join(MCP_CONFIG)));
    }
    out
}

#[must_use]
/// The plugin bundles under one definitions root.
///
/// A directory counts as a bundle when it carries a
/// [`PLUGIN_MANIFEST`]. A bundle without a manifest is not a
/// bundle: the manifest is what names it, and an unnamed directory
/// would have nothing to namespace its servers under.
pub fn plugin_bundles(root: &Path) -> Vec<(String, PathBuf)> {
    let plugins_dir = root.join("plugins");
    let Ok(entries) = std::fs::read_dir(&plugins_dir) else {
        return Vec::new();
    };
    let mut out: Vec<(String, PathBuf)> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| {
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().into_owned();
            (name, path)
        })
        .filter(|(_, path)| path.join(PLUGIN_MANIFEST).is_file())
        .collect();
    out.sort();
    out
}

#[must_use]
/// Keep the first item of each name, in order.
///
/// The items arrive in precedence order, so the first occurrence of
/// a name is the winning one and every later occurrence is dropped.
/// This is the single merge rule the whole layout uses.
pub fn merge_by_name<T>(
    items: Vec<T>,
    name_of: impl for<'a> Fn(&'a T) -> &'a str,
) -> Vec<T> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out = Vec::with_capacity(items.len());
    for item in items {
        if seen.insert(name_of(&item).to_string()) {
            out.push(item);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_sources_are_ordered_project_first() {
        let workspace = tempfile::tempdir().expect("a temp workspace");
        // The sources are ordered by layer, not by the filesystem:
        // the project layer wins, so it comes first.
        let sources = sources(workspace.path(), Domain::Skills);
        assert!(sources.len() >= 2);
        assert_eq!(sources[0].layer, Layer::Project);
        assert_eq!(sources[0].plugin, None);
        assert!(sources[0].path.ends_with("skills"));
    }

    #[test]
    fn a_domain_without_the_legacy_home_stops_at_the_global_layer() {
        let workspace = tempfile::tempdir().expect("a temp workspace");
        let sources = sources(workspace.path(), Domain::Agents);
        assert!(
            sources
                .iter()
                .all(|source| !source.path.to_string_lossy().contains(".uva")),
            "agents must not read the legacy root: {sources:?}"
        );
    }

    #[test]
    fn merge_by_name_keeps_the_first_occurrence() {
        let items = vec![
            ("dup".to_string(), 1),
            ("other".to_string(), 2),
            ("dup".to_string(), 3),
        ];
        let merged =
            merge_by_name(items, |item: &(String, i32)| item.0.as_str());
        assert_eq!(
            merged,
            vec![("dup".to_string(), 1), ("other".to_string(), 2)]
        );
    }

    #[test]
    fn a_bundle_needs_a_manifest_to_be_a_bundle() {
        let root = tempfile::tempdir().expect("a temp root");
        let plugins = root.path().join("plugins");
        std::fs::create_dir_all(plugins.join("named")).expect("a bundle");
        std::fs::create_dir_all(plugins.join("unnamed")).expect("a directory");
        std::fs::write(
            plugins.join("named").join(PLUGIN_MANIFEST),
            r#"{"name":"named"}"#,
        )
        .expect("a manifest");

        let bundles = plugin_bundles(root.path());
        assert_eq!(bundles.len(), 1);
        assert_eq!(bundles[0].0, "named");
    }
}
