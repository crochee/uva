//! Async [`AsyncMetric`] trait and the [`EvalRunner`] engine.
//!
//! Sync [`crate::Metric`] implementations plug in through
//! [`SyncMetricAdapter`].
//!
//! # Example
//!
//! ```rust
//! use uva_eval::{
//!     EvalRunner,
//!     KeywordMetric,
//!     Metric,
//!     runner::SyncMetricAdapter,
//! };
//!
//! struct AlwaysOne;
//!
//! impl Metric for AlwaysOne {
//!     fn name(&self) -> &'static str {
//!         "always_one"
//!     }
//!
//!     fn score(&self, _: &str, _: &str, _: &[&str]) -> f64 {
//!         1.0
//!     }
//! }
//!
//! let runner = EvalRunner::new()
//!     .metric(Box::new(SyncMetricAdapter(KeywordMetric)))
//!     .metric(Box::new(SyncMetricAdapter(AlwaysOne)))
//!     .threshold(0.8);
//! ```

use std::{collections::BTreeMap, sync::Arc};

use async_trait::async_trait;
use futures::StreamExt;
use uva_core::Clock;

use crate::{
    EvalError,
    EvalReport,
    EvalSuite,
    KEYWORD_METRIC_NAME,
    KeywordMetric,
    Metric,
    TestResult,
};

/// Async trait for evaluation metrics.
///
/// Implement this to add custom (possibly I/O-bound) scoring
/// logic — e.g. metrics that call another model.
#[async_trait]
pub trait AsyncMetric: Send + Sync + 'static {
    /// Metric name — the key in [`TestResult::scores`].
    fn name(&self) -> &'static str;

    /// Score the actual output; `0.0` (worst) to `1.0` (best).
    async fn score(
        &self,
        input: &str,
        actual_output: &str,
        expected_keywords: &[&str],
    ) -> f64;
}

/// A callable async agent under test.
///
/// [`EvalRunner::run`] calls [`EvalAgent::respond`] once per
/// [`crate::TestCase`] and scores the returned string.
#[async_trait]
pub trait EvalAgent: Send + Sync {
    /// Run the agent on the given input and return its response.
    ///
    /// # Errors
    ///
    /// Implementations return an error when the agent itself
    /// fails; the runner aborts the whole suite with
    /// [`EvalError::Agent`] so harness bugs are not mistaken
    /// for failing cases.
    async fn respond(&self, input: &str) -> Result<String, EvalError>;
}

/// Evaluation runner — executes a suite against an agent.
///
/// A case passes iff **every** configured metric scores ≥ the
/// threshold. With no metrics configured, the runner falls back
/// to keyword matching (scored under
/// [`KEYWORD_METRIC_NAME`]).
pub struct EvalRunner {
    metrics: Vec<Arc<dyn AsyncMetric>>,
    threshold: f64,
    /// Wall-clock source for the report's `generated_at`. Default
    /// [`uva_core::SystemClock`]; injectable via
    /// [`EvalRunner::with_clock`] so a report's timestamp is
    /// reproducible in tests.
    clock: uva_core::SharedClock,
    /// R-OPT-HPC: bounded concurrency for [`Self::run_trials`].
    /// `1` (sequential) by default. `0` is treated as `1`.
    parallelism: usize,
}

impl EvalRunner {
    /// New runner with no metrics and the default threshold 0.7.
    #[must_use]
    pub fn new() -> Self {
        Self {
            metrics: Vec::new(),
            threshold: 0.7,
            clock: uva_core::SharedClock::system(),
            parallelism: 1,
        }
    }

    /// Install the wall-clock source the report's `generated_at` is
    /// read from.
    #[must_use]
    pub fn with_clock(mut self, clock: uva_core::SharedClock) -> Self {
        self.clock = clock;
        self
    }

    /// Add a metric to score agent outputs with.
    #[must_use]
    pub fn metric(mut self, metric: Box<dyn AsyncMetric>) -> Self {
        self.metrics.push(Arc::from(metric));
        self
    }

    /// Set the minimum score threshold for a case to pass.
    #[must_use]
    pub fn threshold(mut self, threshold: f64) -> Self {
        self.threshold = threshold;
        self
    }

    /// Set the bounded concurrency for [`Self::run_trials`].
    ///
    /// `0` is treated as `1` (sequential). `n > 0` runs at most
    /// `n` trials in parallel via `futures::stream::buffer_unordered`.
    /// The default (`1`) makes the runner sequential — tests pass
    /// deterministically without requiring a parallelism budget.
    #[must_use]
    pub fn parallelism(mut self, n: usize) -> Self {
        self.parallelism = n.max(1);
        self
    }

    /// Execute the suite against the agent.
    ///
    /// For each case: call [`EvalAgent::respond`], score the
    /// response with every metric, mark the case passed iff all
    /// scores are ≥ the threshold, and aggregate into an
    /// [`EvalReport`].
    ///
    /// # Errors
    ///
    /// Returns [`EvalError::Agent`] if the agent fails on any
    /// case (the run aborts — a broken harness is not a failed
    /// case).
    pub async fn run(
        &self,
        agent: &dyn EvalAgent,
        suite: &EvalSuite,
    ) -> Result<EvalReport, EvalError> {
        let mut results = Vec::new();
        let mut passed_count = 0usize;
        let mut total_score = 0.0f64;
        let mut score_count = 0usize;

        for case in suite.cases() {
            let actual_output = agent.respond(&case.input).await?;
            let keywords: Vec<&str> =
                case.expected_keywords.iter().map(String::as_str).collect();
            let scores = self
                .score_case(&case.input, &actual_output, &keywords)
                .await;
            let case_total = scores.values().sum::<f64>();
            total_score += case_total;
            score_count += scores.len();
            let passed = scores.values().all(|s| *s >= self.threshold);
            if passed {
                passed_count += 1;
            }
            results.push(TestResult {
                case_id: case.id.clone(),
                actual_output,
                scores,
                passed,
            });
        }

        let average_score = if score_count > 0 {
            total_score / score_count as f64
        } else {
            0.0
        };
        Ok(EvalReport {
            suite_name: suite.name().to_string(),
            generated_at: self.clock.now(),
            results,
            average_score,
            passed: passed_count,
            total: suite.cases().len(),
            trials: 1,
            per_case_passes: BTreeMap::new(),
            per_case_attempts: BTreeMap::new(),
        })
    }

    /// Run the suite `n` times, bounded by `parallelism`.
    ///
    /// Each (case, trial) pair produces one entry in the
    /// returned report; `EvalReport::per_case_passes` and
    /// `EvalReport::per_case_attempts` are populated so
    /// `pass_at(k)` / `pass_pow(k)` / `per_case_pass_rate` give
    /// principled estimates.
    ///
    /// `parallelism = 1` is sequential; `parallelism = 4` runs up
    /// to 4 trials concurrently via `buffer_unordered`. The
    /// runner returns when every (case, trial) has settled.
    pub async fn run_trials(
        &self,
        agent: Arc<dyn EvalAgent>,
        suite: &EvalSuite,
        trials_per_case: usize,
    ) -> Result<EvalReport, EvalError> {
        let n = trials_per_case.max(1);
        let parallelism = self.parallelism;
        let cases: Vec<_> = suite.cases().to_vec();

        // Generate (case_index, trial_index) pairs in suite order.
        let pairs: Vec<(usize, usize)> = (0..cases.len())
            .flat_map(|ci| (0..n).map(move |ti| (ci, ti)))
            .collect();

        // Run the agent calls bounded by `parallelism`. The
        // output is one `Result<String, EvalError>` per pair; we
        // keep ordering so the report's `results` array is
        // deterministic by case.
        let agent_for_iter = Arc::clone(&agent);
        let agent_calls = futures::stream::iter(pairs.iter().copied().map(
            |(case_idx, _trial_idx)| {
                let agent = Arc::clone(&agent_for_iter);
                let input = cases[case_idx].input.clone();
                async move {
                    let r: Result<String, EvalError> =
                        agent.respond(&input).await;
                    (case_idx, r)
                }
            },
        ))
        .buffer_unordered(parallelism)
        .collect::<Vec<_>>()
        .await;

        // Sequential scoring + aggregation. Sequential because
        // metric implementations might carry internal state (a
        // judge model has a context window, etc.); parallelism
        // here would make that state racy.
        let mut results: Vec<TestResult> =
            Vec::with_capacity(agent_calls.len());
        let mut per_case_passes: BTreeMap<String, usize> = BTreeMap::new();
        let mut per_case_attempts: BTreeMap<String, usize> = BTreeMap::new();
        let mut passed_count = 0usize;
        let mut total_score = 0.0f64;
        let mut score_count = 0usize;

        for (case_idx, response) in agent_calls {
            let case = &cases[case_idx];
            match response {
                Ok(output) => {
                    let keywords: Vec<&str> = case
                        .expected_keywords
                        .iter()
                        .map(String::as_str)
                        .collect();
                    let scores =
                        self.score_case(&case.input, &output, &keywords).await;
                    let case_total: f64 = scores.values().sum();
                    total_score += case_total;
                    score_count += scores.len();
                    let passed = scores.values().all(|s| *s >= self.threshold);
                    if passed {
                        passed_count += 1;
                        *per_case_passes.entry(case.id.clone()).or_insert(0) +=
                            1;
                    }
                    *per_case_attempts.entry(case.id.clone()).or_insert(0) += 1;
                    results.push(TestResult {
                        case_id: case.id.clone(),
                        actual_output: output,
                        scores,
                        passed,
                    });
                }
                Err(e) => {
                    // The agent errored — record it as an attempt
                    // for the case (since the trial was attempted),
                    // but exclude from `passed_count` /
                    // `per_case_passes`.
                    *per_case_attempts.entry(case.id.clone()).or_insert(0) += 1;
                    // Surface the first agent error from the run
                    // by aborting, mirroring `run`'s contract.
                    return Err(e);
                }
            }
        }

        let average_score = if score_count > 0 {
            total_score / score_count as f64
        } else {
            0.0
        };
        Ok(EvalReport {
            suite_name: suite.name().to_string(),
            generated_at: self.clock.now(),
            results,
            average_score,
            passed: passed_count,
            total: cases.len() * n,
            trials: n,
            per_case_passes,
            per_case_attempts,
        })
    }

    /// Score one case: every configured metric, or the keyword
    /// fallback when the runner has no metrics.
    async fn score_case(
        &self,
        input: &str,
        actual_output: &str,
        keywords: &[&str],
    ) -> BTreeMap<String, f64> {
        let mut scores = BTreeMap::new();
        if self.metrics.is_empty() {
            let s = KeywordMetric.score(input, actual_output, keywords);
            scores.insert(KEYWORD_METRIC_NAME.to_string(), s);
            return scores;
        }
        for metric in &self.metrics {
            let s = metric.score(input, actual_output, keywords).await;
            scores.insert(metric.name().to_string(), s);
        }
        scores
    }
}

impl Default for EvalRunner {
    fn default() -> Self {
        Self::new()
    }
}

/// Wraps a sync [`Metric`] impl as an [`AsyncMetric`].
///
/// # Example
///
/// ```rust
/// use uva_eval::{EvalRunner, KeywordMetric, runner::SyncMetricAdapter};
///
/// let runner =
///     EvalRunner::new().metric(Box::new(SyncMetricAdapter(KeywordMetric)));
/// ```
pub struct SyncMetricAdapter<M: Metric>(pub M);

#[async_trait]
impl<M: Metric> AsyncMetric for SyncMetricAdapter<M> {
    fn name(&self) -> &'static str {
        self.0.name()
    }

    async fn score(
        &self,
        input: &str,
        actual_output: &str,
        expected_keywords: &[&str],
    ) -> f64 {
        self.0.score(input, actual_output, expected_keywords)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use async_trait::async_trait;

    use super::*;
    use crate::{EvalSuite, TestCase};

    struct EchoAgent;

    #[async_trait]
    impl EvalAgent for EchoAgent {
        async fn respond(&self, input: &str) -> Result<String, EvalError> {
            Ok(format!("echo: {input}"))
        }
    }

    struct FailingAgent;

    #[async_trait]
    impl EvalAgent for FailingAgent {
        async fn respond(&self, _input: &str) -> Result<String, EvalError> {
            Err(EvalError::Agent("boom".to_string()))
        }
    }

    /// Always-sleep agent — used to prove that `buffer_unordered`
    /// actually runs trials concurrently (the elapsed time falls
    /// below the sequential lower bound).
    struct SleepyEcho;

    #[async_trait]
    impl EvalAgent for SleepyEcho {
        async fn respond(&self, input: &str) -> Result<String, EvalError> {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(format!("echo: {input}"))
        }
    }

    struct FixedMetric(f64, &'static str);

    #[async_trait]
    impl AsyncMetric for FixedMetric {
        fn name(&self) -> &'static str {
            self.1
        }

        async fn score(&self, _: &str, _: &str, _: &[&str]) -> f64 {
            self.0
        }
    }

    #[tokio::test]
    async fn case_fails_when_score_below_threshold() {
        let suite = EvalSuite::new("s")
            .add_case(TestCase::new("c1", "hello").expect_contains("xyzabc"));

        let runner = EvalRunner::new()
            .metric(Box::new(SyncMetricAdapter(KeywordMetric)))
            .threshold(0.8);

        let report = runner.run(&EchoAgent, &suite).await.unwrap();
        assert_eq!(report.passed, 0);
        assert!(!report.results[0].passed);
        assert!(
            (report.results[0].scores[KEYWORD_METRIC_NAME] - 0.0).abs() < 1e-9
        );
    }

    #[tokio::test]
    async fn case_passes_only_if_all_metrics_meet_threshold() {
        let suite = EvalSuite::new("s").add_case(TestCase::new("c1", "x"));
        // One metric at 1.0, one at 0.5, threshold 0.7 → fail.
        let runner = EvalRunner::new()
            .metric(Box::new(FixedMetric(1.0, "good")))
            .metric(Box::new(FixedMetric(0.5, "weak")))
            .threshold(0.7);

        let report = runner.run(&EchoAgent, &suite).await.unwrap();
        assert_eq!(report.passed, 0);
        // Average spans both metrics: (1.0 + 0.5) / 2.
        assert!((report.average_score - 0.75).abs() < 1e-9);
    }

    #[tokio::test]
    async fn no_metrics_falls_back_to_keyword_match() {
        let suite = EvalSuite::new("s")
            .add_case(TestCase::new("hit", "a").expect_contains("echo"))
            .add_case(TestCase::new("miss", "b").expect_contains("nope"));

        let report = EvalRunner::new()
            .threshold(1.0)
            .run(&EchoAgent, &suite)
            .await
            .unwrap();

        assert_eq!(report.passed, 1);
        assert_eq!(report.results[0].scores.len(), 1);
        assert!(
            (report.results[0].scores[KEYWORD_METRIC_NAME] - 1.0).abs() < 1e-9
        );
        assert!(
            (report.results[1].scores[KEYWORD_METRIC_NAME] - 0.0).abs() < 1e-9
        );
    }

    #[tokio::test]
    async fn agent_error_aborts_the_run() {
        let suite = EvalSuite::new("s").add_case(TestCase::new("c1", "x"));
        let runner = EvalRunner::new()
            .metric(Box::new(SyncMetricAdapter(KeywordMetric)));
        let err = runner.run(&FailingAgent, &suite).await.unwrap_err();
        assert!(matches!(err, EvalError::Agent(_)), "got: {err}");
    }

    #[tokio::test]
    async fn empty_suite_yields_empty_report() {
        let suite = EvalSuite::new("empty");
        let runner = EvalRunner::new()
            .metric(Box::new(SyncMetricAdapter(KeywordMetric)));
        let report = runner.run(&EchoAgent, &suite).await.unwrap();
        assert!(report.results.is_empty());
        assert_eq!(report.total, 0);
        assert_eq!(report.passed, 0);
        assert!((report.average_score - 0.0).abs() < 1e-9);
    }

    #[tokio::test]
    async fn sync_metric_adapter_delegates_to_inner_metric() {
        let adapter = SyncMetricAdapter(KeywordMetric);
        let score = adapter.score("in", "hello world", &["hello"]).await;
        assert!((score - 1.0).abs() < 1e-9);
        assert_eq!(adapter.name(), KEYWORD_METRIC_NAME);
    }

    // --- R-OPT-HPC: run_trials + pass@k ----------------------------------

    /// `pass_at(k)` / `pass_pow(k)` are the canonical estimators.
    /// Pin them at the published boundary values so a future
    /// refactor that flips the formula does not silently change
    /// the `EvalReport` contract.
    #[test]
    fn pass_at_pass_pow_at_boundary_values() {
        let mut p = BTreeMap::new();
        let mut a = BTreeMap::new();
        p.insert("c_zero".to_string(), 0usize);
        p.insert("c_half".to_string(), 1usize);
        p.insert("c_full".to_string(), 4usize);
        a.insert("c_zero".to_string(), 4usize);
        a.insert("c_half".to_string(), 4usize);
        a.insert("c_full".to_string(), 4usize);
        let report = EvalReport {
            suite_name: "x".into(),
            generated_at: chrono::Utc::now(),
            results: Vec::new(),
            average_score: 0.0,
            passed: 0,
            total: 0,
            trials: 4,
            per_case_passes: p,
            per_case_attempts: a,
        };
        // c/n = 0 ⇒ pass_at(k) = 0 for all k.
        assert_eq!(report.pass_at(1).get("c_zero").copied(), Some(0.0));
        assert_eq!(report.pass_pow(1).get("c_zero").copied(), Some(0.0));
        // c/n = 1 ⇒ pass_at(k) = 1 for all k (full coverage).
        assert_eq!(report.pass_at(2).get("c_full").copied(), Some(1.0));
        // c/n = 0.25, k=2 ⇒ 1 − (1 − 0.25)^2 = 0.4375.
        assert!(
            (report.pass_at(2).get("c_half").copied().unwrap() - 0.4375).abs()
                < 1e-9
        );
        // c/n = 0.25, k=3 ⇒ 1 − (0.75)^3 = 0.578125.
        assert!(
            (report.pass_at(3).get("c_half").copied().unwrap() - 0.578_125)
                .abs()
                < 1e-9
        );
        // pass_pow(k=2) with c/n=0.25 ⇒ 0.0625.
        assert!(
            (report.pass_pow(2).get("c_half").copied().unwrap() - 0.0625).abs()
                < 1e-9
        );
        // k = 0 ⇒ 0.0 (k clamped).
        assert_eq!(report.pass_at(0).get("c_half").copied(), Some(0.0));
    }

    /// `run_trials` produces one entry per (case, trial) and
    /// populates `per_case_passes` / `per_case_attempts`
    /// correctly.
    #[tokio::test]
    async fn run_trials_populates_per_case_pass_counts() {
        let suite = EvalSuite::new("rt")
            .add_case(TestCase::new("c1", "hello").expect_contains("echo"))
            .add_case(TestCase::new("c2", "world").expect_contains("echo"));
        let runner = EvalRunner::new()
            .metric(Box::new(SyncMetricAdapter(KeywordMetric)))
            .threshold(0.8);
        let report = runner
            .run_trials(Arc::new(EchoAgent), &suite, 5)
            .await
            .unwrap();
        assert_eq!(report.trials, 5);
        assert_eq!(report.total, 10);
        assert_eq!(report.passed, 10);
        assert_eq!(report.per_case_attempts.get("c1").copied(), Some(5));
        assert_eq!(report.per_case_passes.get("c1").copied(), Some(5));
        assert_eq!(report.per_case_attempts.get("c2").copied(), Some(5));
        // pass_at(k=1) on c1 = 1 − (1 − 1)^1 = 1.
        assert!(
            (report.pass_at(1).get("c1").copied().unwrap() - 1.0).abs() < 1e-9
        );
        // per_case_pass_rate on c1 = 5/5 = 1.0.
        let rate = report.per_case_pass_rate();
        assert!((rate.get("c1").copied().unwrap() - 1.0).abs() < 1e-9);
    }

    /// Concurrency proof: 4 trials × 50 ms sleep, parallelism 2.
    /// Sequential would take ≥200 ms; concurrent takes ≥100 ms
    /// (the first pair finishes before the second starts).
    /// We assert < 150 ms to leave headroom for scheduling
    /// jitter on CI runners.
    #[tokio::test]
    async fn run_trials_runs_in_parallel() {
        let suite = EvalSuite::new("concurrency")
            .add_case(TestCase::new("c1", "x"))
            .add_case(TestCase::new("c2", "y"))
            .add_case(TestCase::new("c3", "z"))
            .add_case(TestCase::new("c4", "w"));
        let runner = EvalRunner::new()
            .metric(Box::new(SyncMetricAdapter(KeywordMetric)))
            .threshold(0.0)
            .parallelism(2);
        let started = Instant::now();
        let report = runner
            .run_trials(Arc::new(SleepyEcho), &suite, 1)
            .await
            .unwrap();
        let elapsed = started.elapsed();
        assert_eq!(report.total, 4);
        // Sequential baseline: 4 × 50 ms = 200 ms.
        assert!(
            elapsed < Duration::from_millis(150),
            "expected parallel execution: 4 × 50ms sleep with parallelism=2 \
             should finish in well under 150ms; took {elapsed:?}"
        );
    }

    /// `per_case_pass_rate` matches `passes / attempts` exactly.
    #[tokio::test]
    async fn per_case_pass_rate_matches_definitions() {
        // Build a synthetic report directly because the live runner
        // does not currently simulate non-pass trials cleanly. The
        // accessor is the contract, exercised through a manual
        // report.
        let mut p = BTreeMap::new();
        let mut a = BTreeMap::new();
        p.insert("c1".to_string(), 3);
        a.insert("c1".to_string(), 4);
        let report = EvalReport {
            suite_name: "rate".into(),
            generated_at: chrono::Utc::now(),
            results: Vec::new(),
            average_score: 0.0,
            passed: 3,
            total: 4,
            trials: 4,
            per_case_passes: p,
            per_case_attempts: a,
        };
        let rate = report.per_case_pass_rate();
        assert!((rate["c1"] - 0.75).abs() < 1e-9);
    }
}
