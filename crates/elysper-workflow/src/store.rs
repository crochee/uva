//! `WorkflowStore` — atomic JSON persistence for [`WorkflowSpec`]s.
//!
//! ## Layout
//!
//! ```text
//! <root>/
//!   workflows/
//!     <id>.json   ← one WorkflowSpec per file
//! ```
//!
//! Writes go through `temp + sync_all + rename` so a crash never leaves a
//! half-written spec behind; the directory is created lazily on the first
//! mutation. [`WorkflowStore::load`] mirrors `ScheduleStore`'s honest
//! recovery semantics: a file that cannot be read or decoded is skipped
//! with a `warn!` rather than failing the whole boot, because one damaged
//! document must not take the rest of the catalogue down with it.

use std::{fs, io::Write, path::PathBuf};

use tracing::warn;

use crate::{error::WorkflowError, spec::WorkflowSpec};

/// Directory name under the workspace root: `<root>/workflows/`.
const DIR_NAME: &str = "workflows";

/// Filesystem-backed catalogue of [`WorkflowSpec`] documents, keyed by id.
///
/// Stateless on purpose: the store holds no cache, so every mutation is a
/// disk write and every read is a directory scan. Callers that want a
/// hot in-memory view keep their own map and write through this store —
/// that is exactly how `elysper-server`'s `WorkflowsState` uses it, which
/// is what makes a restart (or a second node over a shared filesystem)
/// see the same catalogue.
#[derive(Debug, Clone)]
pub struct WorkflowStore {
    root: PathBuf,
}

impl WorkflowStore {
    /// Construct a store rooted at `workspace_root`; the `workflows/`
    /// directory is created lazily on the first mutation.
    #[must_use]
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            root: workspace_root.into(),
        }
    }

    /// Read every `*.json` document under `workflows/`, in id order.
    /// Unreadable or malformed files are skipped with a `warn!` — the
    /// honest-recovery posture borrowed from `ScheduleStore::load`.
    ///
    /// # Errors
    ///
    /// [`WorkflowError::Io`] when the directory cannot be listed at all.
    pub fn load(&self) -> Result<Vec<WorkflowSpec>, WorkflowError> {
        let dir = self.root.join(DIR_NAME);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut specs = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match WorkflowSpec::load(&path) {
                Ok(spec) => specs.push(spec),
                Err(err) => {
                    warn!(
                        workflow_file = %path.display(),
                        error = %err,
                        "workflow_store: skipping unreadable or malformed file",
                    );
                }
            }
        }
        specs.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(specs)
    }

    /// Insert or replace the spec keyed by `spec.id`, atomically.
    ///
    /// # Errors
    ///
    /// [`WorkflowError::InvalidSpec`] when the id is empty or contains a
    /// path separator (it names a file under `workflows/`);
    /// [`WorkflowError::Io`] on filesystem failure.
    pub fn upsert(&self, spec: &WorkflowSpec) -> Result<(), WorkflowError> {
        Self::validate_id(&spec.id)?;
        let dir = self.root.join(DIR_NAME);
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", spec.id));
        let tmp = path.with_extension("json.tmp");
        let raw = serde_json::to_string_pretty(spec).map_err(|err| {
            WorkflowError::Document {
                message: err.to_string(),
            }
        })?;
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(raw.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Delete the spec keyed by `id`. Returns `true` when a file was
    /// removed.
    ///
    /// # Errors
    ///
    /// [`WorkflowError::InvalidSpec`] when the id could never name a
    /// store file; [`WorkflowError::Io`] on filesystem failure.
    pub fn remove(&self, id: &str) -> Result<bool, WorkflowError> {
        Self::validate_id(id)?;
        let path = self.root.join(DIR_NAME).join(format!("{id}.json"));
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err.into()),
        }
    }

    fn validate_id(id: &str) -> Result<(), WorkflowError> {
        if id.is_empty()
            || id.contains('/')
            || id.contains('\\')
            || id.contains("..")
        {
            return Err(WorkflowError::InvalidSpec {
                message: format!(
                    "workflow id must be a plain file name: {id:?}"
                ),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::spec::{AgentStep, Step};

    fn one_step_spec(id: &str) -> WorkflowSpec {
        WorkflowSpec {
            id: id.to_string(),
            steps: vec![Step::Agent(AgentStep {
                id: "s1".to_string(),
                agent: "coder".to_string(),
                prompt: "do it".to_string(),
                gate: None,
                isolation: false,
            })],
            phases: Vec::new(),
        }
    }

    #[test]
    fn round_trip_survives_store_recreation() {
        let dir = TempDir::new().unwrap();
        let store = WorkflowStore::new(dir.path());
        store.upsert(&one_step_spec("release")).unwrap();

        // A brand-new handle over the same root (the restart case).
        let reloaded = WorkflowStore::new(dir.path()).load().unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].id, "release");
        assert_eq!(reloaded[0], one_step_spec("release"));
    }

    #[test]
    fn upsert_replaces_same_id() {
        let dir = TempDir::new().unwrap();
        let store = WorkflowStore::new(dir.path());
        store.upsert(&one_step_spec("release")).unwrap();
        let mut v2 = one_step_spec("release");
        v2.steps.clear();
        v2.steps.push(Step::Agent(AgentStep {
            id: "other".to_string(),
            agent: "reviewer".to_string(),
            prompt: "check".to_string(),
            gate: None,
            isolation: false,
        }));
        store.upsert(&v2).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1, "same id must replace, not append");
        assert_eq!(loaded[0], v2);
    }

    #[test]
    fn corrupted_file_is_skipped_not_fatal() {
        let dir = TempDir::new().unwrap();
        let store = WorkflowStore::new(dir.path());
        store.upsert(&one_step_spec("good")).unwrap();
        std::fs::write(dir.path().join("workflows/bad.json"), "{ not json")
            .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(
            loaded.iter().map(|s| s.id.as_str()).collect::<Vec<_>>(),
            vec!["good"],
            "the damaged file must be skipped, the healthy one kept"
        );
    }

    #[test]
    fn remove_reports_presence_honestly() {
        let dir = TempDir::new().unwrap();
        let store = WorkflowStore::new(dir.path());
        store.upsert(&one_step_spec("gone")).unwrap();
        assert!(store.remove("gone").unwrap());
        assert!(!store.remove("gone").unwrap());
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn path_escaping_ids_are_refused() {
        let dir = TempDir::new().unwrap();
        let store = WorkflowStore::new(dir.path());
        let mut spec = one_step_spec("../escape");
        assert!(store.upsert(&spec).is_err());
        spec.id = "a/b".to_string();
        assert!(store.upsert(&spec).is_err());
        spec.id = String::new();
        assert!(store.upsert(&spec).is_err());
        assert!(store.remove("../escape").is_err());
    }

    #[test]
    fn missing_directory_loads_empty() {
        let dir = TempDir::new().unwrap();
        let store = WorkflowStore::new(dir.path());
        assert!(store.load().unwrap().is_empty());
    }
}
