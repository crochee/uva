//! Evals state: the `EvalStore` under `<workspace>/evals/`, plus a
//! runner that scores every case through a caller-supplied real
//! `EvalAgent` (the route builds the harness-backed one).
//!
//! The store is the only truth: every read goes to the shared
//! filesystem, so a peer's write needs no invalidation and two nodes
//! over one workspace never disagree about the catalogue.

use elysper::eval::{
    EvalReport,
    EvalRunner,
    EvalSuite,
    KeywordMetric,
    SyncMetricAdapter,
};

#[derive(Clone)]
pub struct EvalsState {
    store: elysper::eval::EvalStore,
    /// Wall clock the report's `generated_at` comes from
    /// (AGENTS.md §3.8: injected, never `Utc::now()`).
    clock: elysper::core::SharedClock,
}

impl EvalsState {
    pub fn build(workspace_root: &std::path::Path) -> Self {
        Self::build_with_clock(
            workspace_root,
            elysper::core::SharedClock::system(),
        )
    }

    /// [`Self::build`] with an explicit clock—the test seam that
    /// makes `generated_at` deterministic. Suites load from
    /// `<root>/evals/`; a store load failure degrades to an empty
    /// registry with a `warn!`.
    pub fn build_with_clock(
        workspace_root: &std::path::Path,
        clock: elysper::core::SharedClock,
    ) -> Self {
        Self {
            store: elysper::eval::EvalStore::new(workspace_root.to_path_buf()),
            clock,
        }
    }

    /// The suites the store holds, sorted by name.
    pub async fn list(&self) -> Vec<EvalSuite> {
        let mut suites = match self.store.load() {
            Ok(suites) => suites,
            Err(e) => {
                tracing::warn!("eval store load failed; reporting none: {e}");
                return Vec::new();
            }
        };
        suites.sort_by(|left, right| left.name().cmp(right.name()));
        suites
    }

    pub async fn get(&self, name: &str) -> Option<EvalSuite> {
        self.list()
            .await
            .into_iter()
            .find(|suite| suite.name() == name)
    }

    pub async fn create(&self, suite: EvalSuite) -> Result<(), String> {
        if self.get(suite.name()).await.is_some() {
            return Err(format!("suite '{}' already exists", suite.name()));
        }
        self.store.upsert(&suite).map_err(|e| e.to_string())
    }

    pub async fn remove(&self, name: &str) -> Result<bool, String> {
        if self.get(name).await.is_none() {
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
    /// `elysper-web/src/api/types.ts` mirror it).
    pub async fn run(
        &self,
        name: &str,
        agent: &dyn elysper::eval::EvalAgent,
    ) -> Result<EvalReport, String> {
        let suite = self
            .get(name)
            .await
            .ok_or_else(|| format!("suite '{name}' not found"))?;

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
