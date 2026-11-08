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
    /// The one eval engine this deployment drives. The boot builds it
    /// with the keyword metric and publishes the same `Arc` as the
    /// `eval` service, so the route, the self-management tool and a
    /// service consumer all score through one runner.
    runner: std::sync::Arc<EvalRunner>,
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
        let runner = std::sync::Arc::new(
            EvalRunner::new()
                .metric(Box::new(SyncMetricAdapter(KeywordMetric)))
                .with_clock(clock.clone()),
        );
        Self::build_with_runner(workspace_root, runner)
    }

    /// [`Self::build`] with the engine the `eval` service publishes.
    ///
    /// The caller owns the runner so the service and this state share
    /// one instance; the clock still stamps the report.
    pub fn build_with_runner(
        workspace_root: &std::path::Path,
        runner: std::sync::Arc<EvalRunner>,
    ) -> Self {
        Self {
            store: elysper::eval::EvalStore::new(workspace_root.to_path_buf()),
            runner,
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

        let report = self
            .runner
            .run(agent, &suite)
            .await
            .map_err(|e| e.to_string())?;
        Ok(EvalReport {
            suite_name: name.to_owned(),
            ..report
        })
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use async_trait::async_trait;
    use elysper::eval::{
        EvalAgent,
        EvalError,
        EvalRunner,
        EvalSuite,
        TestCase,
    };

    use super::EvalsState;

    struct Echo(&'static str);

    #[async_trait]
    impl EvalAgent for Echo {
        async fn respond(&self, _input: &str) -> Result<String, EvalError> {
            Ok(self.0.to_string())
        }
    }

    /// The state drives the runner the `eval` service published: a
    /// threshold only that runner carries decides the report.
    #[tokio::test]
    async fn run_scores_through_the_mounted_runner() {
        let dir = tempfile::tempdir().expect("tempdir");
        // No metric means the keyword fallback scores 1.0; a 1.5
        // threshold can therefore never pass. The default state runner
        // (threshold 0.7) would pass.
        let runner = Arc::new(EvalRunner::new().threshold(1.5));
        let state = EvalsState::build_with_runner(dir.path(), runner);
        state
            .create(
                EvalSuite::new("suite").add_case(
                    TestCase::new("case", "hi").expect_contains("ok"),
                ),
            )
            .await
            .expect("store the suite");

        let report = state
            .run("suite", &Echo("ok"))
            .await
            .expect("run the suite");
        assert_eq!(report.total, 1);
        assert_eq!(
            report.passed, 0,
            "the mounted runner's threshold is what scores the case"
        );
    }
}
