//! Agent discovery — walk the two-layer `~/.agents` layout.
//!
//! The sources come from [`crate::layout`], so the agent
//! domain obeys exactly the precedence the layout defines:
//!
//! 1. the project's own `agents/` directory;
//! 2. the project's plugin bundles;
//! 3. the global `~/.agents/agents/` directory;
//! 4. the global plugin bundles;
//! 5. (legacy) the global `~/.uva/agents/` directory.
//!
//! A definition whose files do not parse is **skipped**, not fatal:
//! one broken agent must not take the whole registry down.

use std::path::{Path, PathBuf};

use super::format::{AgentDefinition, AgentFileError};
use crate::layout::{self, Domain, Layer};

#[derive(Debug, Clone)]
/// One agent definition, with the source it came from.
pub struct DiscoveredAgent {
    /// The definition itself.
    pub definition: AgentDefinition,
    /// Where it came from.
    pub location: Location,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Where a discovered agent came from.
pub enum Location {
    /// A definition directory directly under a layer's `agents/`.
    Direct {
        /// The layer the directory belongs to.
        layer: Layer,
    },
    /// A definition directory inside a plugin bundle.
    Bundled {
        /// The layer the bundle belongs to.
        layer: Layer,
        /// The bundle's name.
        plugin: String,
    },
}

/// Discover every agent visible to a workspace.
///
/// `workspace_root` is the project root the agent runtime owns; the
/// global layer is read from `$HOME`.
#[must_use]
/// Discover every agent visible to a workspace.
///
/// `workspace_root` is the project root the agent runtime owns; the
/// global layer is read from `$HOME`.
pub fn discover(workspace_root: &Path) -> Vec<DiscoveredAgent> {
    let mut found = Vec::new();
    for source in layout::sources(workspace_root, Domain::Agents) {
        for directory in agent_directories(&source.path) {
            match AgentDefinition::read(&directory) {
                Ok(definition) => found.push(DiscoveredAgent {
                    definition,
                    location: match source.plugin.clone() {
                        Some(plugin) => Location::Bundled {
                            layer: source.layer,
                            plugin,
                        },
                        None => Location::Direct {
                            layer: source.layer,
                        },
                    },
                }),
                Err(error) => {
                    tracing::warn!(
                        path = %directory.display(),
                        %error,
                        "skipping malformed agent definition"
                    );
                }
            }
        }
    }
    // The first source that offers a name wins; the layout's
    // precedence order already puts them in that order.
    layout::merge_by_name(found, |agent| agent.definition.name())
}

/// The agent directories under one source directory.
///
/// Every direct child of the source directory is an agent directory
/// — the layout is `agents/<name>/agent.md`, one level deep. A
/// directory without an `agent.md` is skipped by the reader.
fn agent_directories(source: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(source) else {
        return Vec::new();
    };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.path())
        .collect();
    out.sort();
    out
}

/// Read one agent definition, mapping the failure onto a
/// [`Result`] the callers can propagate.
///
/// # Errors
///
/// Propagates [`AgentFileError`].
pub fn read(directory: &Path) -> Result<AgentDefinition, AgentFileError> {
    AgentDefinition::read(directory)
}
