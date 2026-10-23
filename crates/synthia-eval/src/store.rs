//! `EvalStore` — atomic JSON persistence for [`EvalSuite`]s.
//!
//! ## Layout
//!
//! ```text
//! <root>/
//!   evals/
//!     <name>.json   ← one EvalSuite per file, keyed by suite name
//! ```
//!
//! Same contract as `synthia-workflow`'s `WorkflowStore`: writes go
//! through `temp + sync_all + rename`, the directory is created lazily,
//! and [`EvalStore::load`] skips unreadable or malformed files with a
//! `warn!` instead of failing the boot — one damaged suite must not take
//! the rest of the catalogue down with it.

use std::{fs, io::Write, path::PathBuf};

use tracing::warn;

use crate::{EvalError, EvalSuite};

/// Directory name under the workspace root: `<root>/evals/`.
const DIR_NAME: &str = "evals";

/// Filesystem-backed catalogue of [`EvalSuite`] documents, keyed by
/// suite name. Stateless on purpose: every mutation is a disk write and
/// every read is a directory scan, so a restart (or a second node over a
/// shared filesystem) sees the same catalogue.
#[derive(Debug, Clone)]
pub struct EvalStore {
    root: PathBuf,
}

impl EvalStore {
    /// Construct a store rooted at `workspace_root`; the `evals/`
    /// directory is created lazily on the first mutation.
    #[must_use]
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            root: workspace_root.into(),
        }
    }

    /// Read every `*.json` document under `evals/`, in name order.
    /// Unreadable or malformed files are skipped with a `warn!`.
    ///
    /// # Errors
    ///
    /// [`EvalError::Io`] when the directory cannot be listed at all.
    pub fn load(&self) -> Result<Vec<EvalSuite>, EvalError> {
        let dir = self.root.join(DIR_NAME);
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut suites = Vec::new();
        for entry in fs::read_dir(&dir)? {
            let Ok(entry) = entry else {
                continue;
            };
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("json") {
                continue;
            }
            match fs::read_to_string(&path) {
                Ok(raw) => match serde_json::from_str::<EvalSuite>(&raw) {
                    Ok(suite) => suites.push(suite),
                    Err(err) => {
                        warn!(
                            eval_file = %path.display(),
                            error = %err,
                            "eval_store: skipping malformed file",
                        );
                    }
                },
                Err(err) => {
                    warn!(
                        eval_file = %path.display(),
                        error = %err,
                        "eval_store: skipping unreadable file",
                    );
                }
            }
        }
        suites.sort_by(|a, b| a.name().cmp(b.name()));
        Ok(suites)
    }

    /// Insert or replace the suite keyed by its name, atomically.
    ///
    /// # Errors
    ///
    /// [`EvalError::Agent`] when the suite name is empty or contains a
    /// path separator (it names a file under `evals/`) — reported through
    /// the stringly variant to keep the error enum closed;
    /// [`EvalError::Io`] on filesystem failure.
    pub fn upsert(&self, suite: &EvalSuite) -> Result<(), EvalError> {
        Self::validate_name(suite.name())?;
        let dir = self.root.join(DIR_NAME);
        fs::create_dir_all(&dir)?;
        let path = dir.join(format!("{}.json", suite.name()));
        let tmp = path.with_extension("json.tmp");
        let raw = serde_json::to_string_pretty(suite)
            .map_err(|err| EvalError::Serialization(err.to_string()))?;
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(raw.as_bytes())?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &path)?;
        Ok(())
    }

    /// Delete the suite keyed by `name`. Returns `true` when a file was
    /// removed.
    ///
    /// # Errors
    ///
    /// [`EvalError::Agent`] when the name could never name a store file;
    /// [`EvalError::Io`] on filesystem failure.
    pub fn remove(&self, name: &str) -> Result<bool, EvalError> {
        Self::validate_name(name)?;
        let path = self.root.join(DIR_NAME).join(format!("{name}.json"));
        match fs::remove_file(&path) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err.into()),
        }
    }

    fn validate_name(name: &str) -> Result<(), EvalError> {
        if name.is_empty()
            || name.contains('/')
            || name.contains('\\')
            || name.contains("..")
        {
            return Err(EvalError::Agent(format!(
                "eval suite name must be a plain file name: {name:?}"
            )));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::TestCase;

    #[test]
    fn round_trip_survives_store_recreation() {
        let dir = TempDir::new().unwrap();
        let store = EvalStore::new(dir.path());
        let suite = EvalSuite::new("smoke").add_case(TestCase {
            id: "c1".to_string(),
            input: "say ok".to_string(),
            expected_keywords: vec!["ok".to_string()],
            expected_output: Some("ok".to_string()),
        });
        store.upsert(&suite).unwrap();

        // A brand-new handle over the same root (the restart case).
        let reloaded = EvalStore::new(dir.path()).load().unwrap();
        assert_eq!(reloaded.len(), 1);
        assert_eq!(reloaded[0].name(), "smoke");
        assert_eq!(
            serde_json::to_string(&reloaded[0]).unwrap(),
            serde_json::to_string(&suite).unwrap()
        );
    }

    #[test]
    fn upsert_replaces_same_name() {
        let dir = TempDir::new().unwrap();
        let store = EvalStore::new(dir.path());
        store.upsert(&EvalSuite::new("smoke")).unwrap();
        let v2 = EvalSuite::new("smoke").add_case(TestCase {
            id: "c1".to_string(),
            input: "x".to_string(),
            expected_keywords: Vec::new(),
            expected_output: None,
        });
        store.upsert(&v2).unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(loaded.len(), 1, "same name must replace, not append");
        assert_eq!(loaded[0].cases().len(), 1);
    }

    #[test]
    fn corrupted_file_is_skipped_not_fatal() {
        let dir = TempDir::new().unwrap();
        let store = EvalStore::new(dir.path());
        store.upsert(&EvalSuite::new("good")).unwrap();
        std::fs::write(dir.path().join("evals/bad.json"), "{ not json")
            .unwrap();
        let loaded = store.load().unwrap();
        assert_eq!(
            loaded
                .iter()
                .map(|s| s.name().to_string())
                .collect::<Vec<_>>(),
            vec!["good".to_string()],
            "the damaged file must be skipped, the healthy one kept"
        );
    }

    #[test]
    fn remove_reports_presence_honestly() {
        let dir = TempDir::new().unwrap();
        let store = EvalStore::new(dir.path());
        store.upsert(&EvalSuite::new("gone")).unwrap();
        assert!(store.remove("gone").unwrap());
        assert!(!store.remove("gone").unwrap());
        assert!(store.load().unwrap().is_empty());
    }

    #[test]
    fn path_escaping_names_are_refused() {
        let dir = TempDir::new().unwrap();
        let store = EvalStore::new(dir.path());
        assert!(store.upsert(&EvalSuite::new("../escape")).is_err());
        assert!(store.upsert(&EvalSuite::new("a/b")).is_err());
        assert!(store.upsert(&EvalSuite::new("")).is_err());
        assert!(store.remove("../escape").is_err());
    }

    #[test]
    fn missing_directory_loads_empty() {
        let dir = TempDir::new().unwrap();
        let store = EvalStore::new(dir.path());
        assert!(store.load().unwrap().is_empty());
    }
}
