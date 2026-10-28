//! Workflows state: the `WorkflowStore` under
//! `<workspace>/workflows/`, and nothing else.
//!
//! The store is the only truth. A node keeps no in-memory catalogue:
//! every read goes to the shared filesystem, so two nodes over one
//! workspace see the same list the moment a write lands, and a write
//! needs no invalidation frame. A single JSON read is cheaper than
//! the alternative — a cache that every mutating route would have to
//! remember to invalidate.

use std::path::Path;

use elysper::workflow::{
    WorkflowCaps,
    WorkflowError,
    WorkflowPlan,
    WorkflowSpec,
    WorkflowStore,
};

#[derive(Clone)]
pub struct WorkflowsState {
    store: WorkflowStore,
}

impl WorkflowsState {
    /// Build the registry from `<root>/workflows/`. A store load
    /// failure degrades to an empty list with a `warn!` — the same
    /// honest-recovery posture `SchedulesState` takes.
    pub fn build(workspace_root: &Path) -> Self {
        Self {
            store: WorkflowStore::new(workspace_root.to_path_buf()),
        }
    }

    /// The suite the store holds, sorted by id.
    pub async fn list(&self) -> Vec<WorkflowSpec> {
        let mut specs = match self.store.load() {
            Ok(specs) => specs,
            Err(e) => {
                tracing::warn!(
                    "workflow store load failed; reporting none: {e}"
                );
                return Vec::new();
            }
        };
        specs.sort_by(|left, right| left.id.cmp(&right.id));
        specs
    }

    /// One spec, straight from the store.
    pub async fn get(&self, id: &str) -> Option<WorkflowSpec> {
        self.list().await.into_iter().find(|spec| spec.id == id)
    }

    pub async fn create(&self, spec: WorkflowSpec) -> Result<(), String> {
        if self.get(&spec.id).await.is_some() {
            return Err(format!("workflow '{}' already exists", spec.id));
        }
        // Plan with default caps so caps-violation errors surface at
        // create time, not run time.
        spec.plan(&WorkflowCaps::default())
            .map_err(|e| e.to_string())?;
        self.store.upsert(&spec).map_err(|e| e.to_string())
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
        spec.plan(&WorkflowCaps::default())
            .map_err(|e| e.to_string())?;
        self.store.upsert(&spec).map_err(|e| e.to_string())
    }

    pub async fn remove(&self, id: &str) -> Result<bool, String> {
        if self.get(id).await.is_none() {
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
