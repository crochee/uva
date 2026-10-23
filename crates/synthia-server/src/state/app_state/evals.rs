//! Evals state: in-memory registry of `EvalSuite`s backed by the
//! `EvalStore` under `<workspace>/evals/`, plus a runner that
//! scores every case through a caller-supplied real `EvalAgent`
//! (the route builds the harness-backed one).

use std::{collections::BTreeMap, sync::Arc};

use synthia::eval::{
    EvalReport,
    EvalRunner,
    EvalSuite,
    KeywordMetric,
    SyncMetricAdapter,
};
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct EvalsState {
    inner: Arc<RwLock<BTreeMap<String, EvalSuite>>>,
    store: synthia::eval::EvalStore,
    /// Wall clock the report's `generated_at` comes from
    /// (AGENTS.md §3.8: injected, never `Utc::now()`).
    clock: synthia::core::SharedClock,
}

impl EvalsState {
    pub fn build(workspace_root: &std::path::Path) -> Self {
        Self::build_with_clock(
            workspace_root,
            synthia::core::SharedClock::system(),
        )
    }

    /// [`Self::build`] with an explicit clock—the test seam that
    /// makes `generated_at` deterministic. Suites load from
    /// `<root>/evals/`; a store load failure degrades to an empty
    /// registry with a `warn!`.
    pub fn build_with_clock(
        workspace_root: &std::path::Path,
        clock: synthia::core::SharedClock,
    ) -> Self {
        let store = synthia::eval::EvalStore::new(workspace_root.to_path_buf());
        let mut map = BTreeMap::new();
        match store.load() {
            Ok(suites) => {
                for suite in suites {
                    map.insert(suite.name().to_string(), suite);
                }
            }
            Err(e) => {
                tracing::warn!("eval store load failed; starting empty: {e}");
            }
        }
        Self {
            inner: Arc::new(RwLock::new(map)),
            store,
            clock,
        }
    }

    pub async fn list(&self) -> Vec<EvalSuite> {
        let g = self.inner.read().await;
        g.values().cloned().collect()
    }

    pub async fn get(&self, name: &str) -> Option<EvalSuite> {
        let g = self.inner.read().await;
        g.get(name).cloned()
    }

    pub async fn create(&self, suite: EvalSuite) -> Result<(), String> {
        let mut g = self.inner.write().await;
        if g.contains_key(suite.name()) {
            return Err(format!("suite '{}' already exists", suite.name()));
        }
        self.store.upsert(&suite).map_err(|e| e.to_string())?;
        g.insert(suite.name().to_string(), suite);
        Ok(())
    }

    pub async fn remove(&self, name: &str) -> Result<bool, String> {
        let mut g = self.inner.write().await;
        if g.remove(name).is_none() {
            return Ok(false);
        }
        self.store.remove(name).map_err(|e| e.to_string())?;
        Ok(true)
    }

    /// Run a suite against a real [`EvalAgent`] and return the
    /// report.
    ///
    /// The caller supplies the agent (the route builds a
    /// [`crate::state::app_state::eval_agent::HarnessEvalAgent`]
    /// from the live provider + tool registry). The keyword metric
    /// is the one wired scorer; the report shape matches the Rust
    /// `EvalReport` exactly (the wire-shape front-end types in
    /// `synthia-web/src/api/types.ts` mirror it).
    pub async fn run(
        &self,
        name: &str,
        agent: &dyn synthia::eval::EvalAgent,
    ) -> Result<EvalReport, String> {
        let suite = {
            let g = self.inner.read().await;
            g.get(name).cloned()
        };
        let suite = suite.ok_or_else(|| format!("suite '{name}' not found"))?;

        let runner = EvalRunner::new()
            .metric(Box::new(SyncMetricAdapter(KeywordMetric)))
            .with_clock(self.clock.clone());
        let report =
            runner.run(agent, &suite).await.map_err(|e| e.to_string())?;
        Ok(EvalReport {
            suite_name: name.to_owned(),
            ..report
        })
    }
}
