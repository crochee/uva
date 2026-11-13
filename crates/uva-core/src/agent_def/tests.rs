//! Tests for the agent-definition discovery walk.

use std::path::Path;

use super::*;
use crate::layout::Layer;

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
