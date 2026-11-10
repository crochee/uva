//! Hot reload: mtime polling, and the diff that decides which fibers
//! are remounted.
//!
//! `notify` is deliberately not a dependency: the profile is one small
//! file read at boot and on change, and a poll every second costs one
//! `stat` while keeping the kernel runtime-neutral. A change is a
//! `(mtime, length)` difference, which every filesystem the server runs
//! on reports.

use std::{
    fs,
    io,
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde::Serialize;
use serde_json::{Map, Value};

use super::model::{Entry, PluginFactories, Profile};

/// One file's identity for polling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    modified: Option<SystemTime>,
    len: u64,
}

/// Poll one or more profile files for changes.
///
/// The first [`poll`](Self::poll) records the current stamps and returns
/// `false`, so constructing a watcher never reports a spurious change.
#[derive(Debug)]
pub struct ProfileWatcher {
    paths: Vec<PathBuf>,
    stamps: Vec<Option<FileStamp>>,
    initialized: bool,
}

impl ProfileWatcher {
    /// Watch `paths`.
    #[must_use]
    pub fn new(paths: Vec<PathBuf>) -> Self {
        let stamps = vec![None; paths.len()];
        Self {
            paths,
            stamps,
            initialized: false,
        }
    }

    /// The watched paths, in order.
    #[must_use]
    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    /// Return whether any watched file changed since the previous poll.
    ///
    /// # Errors
    ///
    /// Propagates a `stat` failure other than "not found". A file that
    /// appears or disappears counts as a change on the next poll.
    pub fn poll(&mut self) -> io::Result<bool> {
        let compare = self.initialized;
        let mut changed = false;
        for (path, previous) in self.paths.iter().zip(self.stamps.iter_mut()) {
            let current = stamp(path)?;
            if compare && *previous != current {
                changed = true;
            }
            *previous = current;
        }
        self.initialized = true;
        Ok(changed)
    }
}

fn stamp(path: &Path) -> io::Result<Option<FileStamp>> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(Some(FileStamp {
            modified: metadata.modified().ok(),
            len: metadata.len(),
        })),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

/// What changed between two resolved profiles, and therefore which
/// fibers a running process must remount.
///
/// `added`, `removed`, and `remounted` name **core** ids only: a UI
/// bundle change is the frontend loader's concern and never remounts a
/// core fiber.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ProfileDiff {
    /// Ids present in the new assembly but not the old one.
    pub added: Vec<String>,
    /// Ids present in the old assembly but not the new one.
    pub removed: Vec<String>,
    /// Ids whose non-volatile configuration or runtime changed.
    pub remounted: Vec<String>,
    /// Ids whose only configuration change was in a volatile field.
    pub volatile_changed: Vec<String>,
    /// The profile disabled HMR; the new configuration needs a restart.
    pub restart_required: bool,
}

impl ProfileDiff {
    /// Whether nothing at all changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.removed.is_empty()
            && self.remounted.is_empty()
            && self.volatile_changed.is_empty()
            && !self.restart_required
    }
}

/// Diff two resolved profiles.
///
/// Only ids whose effective configuration changed appear in
/// `remounted`; a change confined to a factory's
/// [`volatile_fields`](super::PluginFactory::volatile_fields) appears in
/// `volatile_changed` instead, because the plugin reads the new value on
/// its next access without being torn down.
///
/// When the new profile disables HMR, the diff reports
/// `restart_required` and empty id lists: a running process does not
/// reload, and the operator must restart to pick the new configuration
/// up. Turning HMR back on in the new profile takes effect for the next
/// file change, so that direction is a normal diff.
#[must_use]
pub fn diff_profiles(
    previous: &Profile,
    next: &Profile,
    factories: &PluginFactories,
) -> ProfileDiff {
    let mut diff = ProfileDiff::default();

    for entry in &next.core {
        match find(&previous.core, &entry.id) {
            None => diff.added.push(entry.id.clone()),
            Some(old) => classify(old, entry, factories, &mut diff),
        }
    }
    for entry in &previous.core {
        if find(&next.core, &entry.id).is_none() {
            diff.removed.push(entry.id.clone());
        }
    }

    cascade_remounts(previous, factories, &mut diff);

    if !next.hmr && !diff.is_empty() {
        return ProfileDiff {
            restart_required: true,
            ..ProfileDiff::default()
        };
    }
    diff
}

/// Pull a changed plugin's injectors into the remount set,
/// transitively.
///
/// A plugin that injects a service resolved the provider's value at
/// its own mount; leaving it mounted after the provider remounts
/// would pin the withdrawn value for the rest of the process's life.
/// Every previous row that injects a key a **remounted** row provides
/// joins `remounted` (never `added` — the row never left the profile),
/// and the closure repeats over the newly pulled rows.
///
/// Removals do **not** cascade: unloading the provider withdraws its
/// services, and the kernel parks the injectors itself; they must not
/// be rebuilt (a rebuild would mount a dependent with no provider and
/// fail the group, taking the removal's rollback down with it).
fn cascade_remounts(
    previous: &Profile,
    factories: &PluginFactories,
    diff: &mut ProfileDiff,
) {
    use std::collections::{HashMap, HashSet, VecDeque};

    // The service graph as the previous assembly declared it: what
    // each mounted row provides, and which rows inject which keys.
    let mut provides_of: HashMap<String, Vec<String>> = HashMap::new();
    let mut injectors_of: HashMap<String, Vec<String>> = HashMap::new();
    for entry in &previous.core {
        let Some(factory) = factories.get(&entry.id) else {
            continue;
        };
        let manifest = factory.manifest();
        provides_of.insert(entry.id.clone(), manifest.provides.clone());
        for key in &manifest.inject {
            injectors_of
                .entry(key.clone())
                .or_default()
                .push(entry.id.clone());
        }
    }

    let mut pending: VecDeque<String> =
        diff.remounted.iter().cloned().collect();
    let mut seen: HashSet<String> = pending.iter().cloned().collect();
    while let Some(id) = pending.pop_front() {
        let Some(keys) = provides_of.get(&id) else {
            continue;
        };
        for key in keys {
            let Some(dependents) = injectors_of.get(key) else {
                continue;
            };
            for dependent in dependents.clone() {
                if seen.insert(dependent.clone()) {
                    diff.remounted.push(dependent.clone());
                    pending.push_back(dependent);
                }
            }
        }
    }
}

fn find<'a>(entries: &'a [Entry], id: &str) -> Option<&'a Entry> {
    entries.iter().find(|entry| entry.id == id)
}

fn classify(
    old: &Entry,
    new: &Entry,
    factories: &PluginFactories,
    diff: &mut ProfileDiff,
) {
    if old.runtime != new.runtime || old.capabilities != new.capabilities {
        diff.remounted.push(new.id.clone());
        return;
    }
    if equivalent(&old.config, &new.config) {
        return;
    }
    let volatile = factories.volatile_fields(&new.id);
    if equivalent(
        &masked(&old.config, volatile),
        &masked(&new.config, volatile),
    ) {
        diff.volatile_changed.push(new.id.clone());
    } else {
        diff.remounted.push(new.id.clone());
    }
}

/// Whether two configurations mean the same thing.
///
/// JSON `null` has no TOML form: a null-valued key is dropped when a
/// document is serialized and cannot come back. Such a key is
/// *unrepresentable state*, not a configuration difference — without
/// this, every document round trip over a materialized config (the
/// boot rows carry unset credentials) reads as a change and remounts
/// the row plus everything that injects it.
fn equivalent(left: &Value, right: &Value) -> bool {
    let left = toml_view(left);
    let right = toml_view(right);
    left == right || (is_empty(&left) && is_empty(&right))
}

/// Whether a configuration carries nothing at all.
fn is_empty(config: &Value) -> bool {
    config.is_null() || config.as_object().is_some_and(Map::is_empty)
}

/// A configuration as TOML can express it: null-valued keys dropped.
fn toml_view(config: &Value) -> Value {
    match config {
        Value::Object(map) => {
            let mut view = Map::new();
            for (key, value) in map {
                if value.is_null() {
                    continue;
                }
                let value = toml_view(value);
                if value.is_null() {
                    continue;
                }
                view.insert(key.clone(), value);
            }
            Value::Object(view)
        }
        Value::Array(items) => {
            Value::Array(items.iter().map(toml_view).collect())
        }
        other => other.clone(),
    }
}

/// A copy of `config` with the volatile top-level fields removed.
fn masked(config: &Value, volatile: &[&str]) -> Value {
    let mut masked = config.clone();
    if let Some(object) = masked.as_object_mut() {
        for field in volatile {
            object.remove(*field);
        }
    }
    masked
}
