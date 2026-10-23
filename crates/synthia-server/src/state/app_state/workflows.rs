//! Workflows state: in-memory registry of `WorkflowSpec`s with
//! CRUD helpers, write-through persisted by `WorkflowStore` under
//! `<workspace>/workflows/` so a restart (or a second node over a
//! shared filesystem) sees the same catalogue.

use std::{collections::BTreeMap, path::Path, sync::Arc};

use synthia::workflow::{
    WorkflowCaps,
    WorkflowError,
    WorkflowPlan,
    WorkflowSpec,
    WorkflowStore,
};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct WorkflowsState {
    inner: Arc<RwLock<BTreeMap<String, WorkflowSpec>>>,
    store: WorkflowStore,
}

impl WorkflowsState {
    /// Build the registry from `<root>/workflows/`. A store load
    /// failure degrades to an empty registry with a `warn!` — the
    /// same honest-recovery posture `SchedulesState::build` takes.
    pub fn build(workspace_root: &Path) -> Self {
        let store = WorkflowStore::new(workspace_root.to_path_buf());
        let mut map = BTreeMap::new();
        match store.load() {
            Ok(specs) => {
                for spec in specs {
                    map.insert(spec.id.clone(), spec);
                }
            }
            Err(e) => {
                tracing::warn!(
                    "workflow store load failed; starting empty: {e}"
                );
            }
        }
        Self {
            inner: Arc::new(RwLock::new(map)),
            store,
        }
    }

    pub async fn list(&self) -> Vec<WorkflowSpec> {
        let g = self.inner.read().await;
        g.values().cloned().collect()
    }

    pub async fn get(&self, id: &str) -> Option<WorkflowSpec> {
        let g = self.inner.read().await;
        g.get(id).cloned()
    }

    pub async fn create(&self, spec: WorkflowSpec) -> Result<(), String> {
        let mut g = self.inner.write().await;
        if g.contains_key(&spec.id) {
            return Err(format!("workflow '{}' already exists", spec.id));
        }
        // Plan with default caps so caps-violation errors surface at
        // create time, not run time.
        spec.plan(&WorkflowCaps::default())
            .map_err(|e| e.to_string())?;
        self.store.upsert(&spec).map_err(|e| e.to_string())?;
        g.insert(spec.id.clone(), spec);
        Ok(())
    }

    pub async fn replace(
        &self,
        id: &str,
        spec: WorkflowSpec,
    ) -> Result<(), String> {
        if spec.id != id {
            return Err(format!(
                "url id '{id}' does not match spec.id '{}'",
                spec.id
            ));
        }
        let mut g = self.inner.write().await;
        spec.plan(&WorkflowCaps::default())
            .map_err(|e| e.to_string())?;
        self.store.upsert(&spec).map_err(|e| e.to_string())?;
        g.insert(spec.id.clone(), spec);
        Ok(())
    }

    pub async fn remove(&self, id: &str) -> Result<bool, String> {
        let mut g = self.inner.write().await;
        if g.remove(id).is_none() {
            return Ok(false);
        }
        self.store.remove(id).map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// Plan a spec against the current caps. Returns the plan or an
    /// error string. Exposed so the route handler can validate a
    /// submitted document without running it.
    pub async fn plan(
        &self,
        spec: &WorkflowSpec,
    ) -> Result<WorkflowPlan, WorkflowError> {
        spec.plan(&WorkflowCaps::default())
    }
}
