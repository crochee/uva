//! Agent discovery — walk the two-layer `~/.agents` layout.
//!
//! The sources come from [`elysper_core::layout`], so the agent
//! domain obeys exactly the precedence the layout defines:
//!
//! 1. the project's own `agents/` directory;
//! 2. the project's plugin bundles;
//! 3. the global `~/.agents/agents/` directory;
//! 4. the global plugin bundles;
//! 5. (legacy) the global `~/.elysper/agents/` directory.
//!
//! A definition whose files do not parse is **skipped**, not fatal:
//! one broken agent must not take the whole registry down.

use std::path::{Path, PathBuf};

use elysper_core::layout::{self, Domain, Layer};

use crate::format::{AgentDefinition, AgentFileError};

/// One agent definition, with the source it came from.
#[derive(Debug, Clone)]
pub struct DiscoveredAgent {
    /// The definition itself.
    pub definition: AgentDefinition,
    /// Where it came from.
    pub location: Location,
}

/// Where a discovered agent came from.
#[derive(Debug, Clone, PartialEq, Eq)]
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `$HOME` is process-global, and discovery reads it for the
    /// global layer. Every test that calls [`discover`] holds this
    /// lock and points `HOME` at its own temp directory, so a sibling
    /// test running in parallel can never see another test's global
    /// agents. Without both, the tests race: the crate passed alone
    /// and failed under `make test-crates`.
    static HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct ScopedHome {
        _guard: std::sync::MutexGuard<'static, ()>,
        previous: Option<std::ffi::OsString>,
        dir: tempfile::TempDir,
    }

    impl ScopedHome {
        fn enter() -> Self {
            let guard = HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let dir = tempfile::tempdir().expect("a temp home");
            let previous = std::env::var_os("HOME");
            // SAFETY: the lock is held for the guard's lifetime, so no
            // other thread in this process reads or writes `HOME`
            // concurrently.
            unsafe { std::env::set_var("HOME", dir.path()) };
            Self {
                _guard: guard,
                previous,
                dir,
            }
        }

        fn path(&self) -> &Path {
            self.dir.path()
        }
    }

    impl Drop for ScopedHome {
        fn drop(&mut self) {
            // SAFETY: the lock is still held (the guard drops after
            // this body), so the restore cannot race a reader.
            unsafe {
                match self.previous.take() {
                    Some(home) => std::env::set_var("HOME", home),
                    None => std::env::remove_var("HOME"),
                }
            }
        }
    }

    fn write_agent(root: &Path, name: &str, body: &str) {
        let directory = root.join(name);
        std::fs::create_dir_all(&directory).expect("a directory");
        std::fs::write(
            directory.join("agent.md"),
            format!("---\nname: {name}\ndescription: d\n---\n\n{body}"),
        )
        .expect("the agent file");
    }

    #[test]
    fn discovery_finds_the_project_layer_first() {
        let _home = ScopedHome::enter();
        let workspace = tempfile::tempdir().expect("a temp workspace");
        let project = workspace.path().join(".agents").join("agents");
        std::fs::create_dir_all(&project).expect("the agents directory");
        write_agent(&project, "reviewer", "review things");

        let found = discover(workspace.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].definition.name(), "reviewer");
        assert_eq!(found[0].definition.instructions, "review things");
        assert_eq!(
            found[0].location,
            Location::Direct {
                layer: Layer::Project
            }
        );
    }

    #[test]
    fn a_broken_definition_is_skipped_not_fatal() {
        let _home = ScopedHome::enter();
        let workspace = tempfile::tempdir().expect("a temp workspace");
        let project = workspace.path().join(".agents").join("agents");
        std::fs::create_dir_all(&project).expect("the agents directory");
        write_agent(&project, "good", "fine");
        std::fs::create_dir_all(project.join("bad")).expect("a directory");
        std::fs::write(project.join("bad").join("agent.md"), "{")
            .expect("a broken file");

        let found = discover(workspace.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].definition.name(), "good");
    }

    #[test]
    fn the_project_layer_wins_over_the_global_one() {
        let home = ScopedHome::enter();
        let workspace = tempfile::tempdir().expect("a temp workspace");

        let project = workspace.path().join(".agents").join("agents");
        std::fs::create_dir_all(&project).expect("the agents directory");
        write_agent(&project, "dup", "project version");

        let global = home.path().join(".agents").join("agents");
        std::fs::create_dir_all(&global).expect("the agents directory");
        write_agent(&global, "dup", "global version");

        let found = discover(workspace.path());
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].definition.instructions, "project version");
    }
}
