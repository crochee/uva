//! Evaluation framework for uva agents.
//!
//! Ports the `traitclaw-eval` design: [`EvalSuite`]/[`TestCase`]
//! builders, a sync [`Metric`] trait plus its async twin
//! [`AsyncMetric`], and an [`EvalRunner`] that scores every case
//! against every configured metric. A case passes iff **all** of
//! its metric scores are ≥ the runner's threshold; with no
//! metrics configured the runner falls back to keyword matching
//! over the case's `expect_contains` keywords.
//!
//! Built-in metrics: [`KeywordMetric`] (fraction of expected
//! keywords present), [`LlmJudgeMetric`] (LLM-as-judge scoring
//! behind the local [`JudgeProvider`] seam), and
//! [`SchemaValidationMetric`] (JSON-Schema subset validation via
//! `uva_core::schema::validate_against_schema`).
//!
//! Reports export as pretty JSON or CSV
//! ([`EvalReport::export_json`] / [`EvalReport::export_csv`]).
//!
//! # Quick start
//!
//! ```rust
//! use async_trait::async_trait;
//! use uva_eval::{
//!     EvalAgent,
//!     EvalError,
//!     EvalRunner,
//!     EvalSuite,
//!     KeywordMetric,
//!     SyncMetricAdapter,
//!     TestCase,
//! };
//!
//! struct EchoAgent;
//!
//! #[async_trait]
//! impl EvalAgent for EchoAgent {
//!     async fn respond(&self, input: &str) -> Result<String, EvalError> {
//!         Ok(format!("echo: {input}"))
//!     }
//! }
//!
//! # async fn run() -> Result<(), EvalError> {
//! let suite = EvalSuite::new("quality").add_case(
//!     TestCase::new("greeting", "Say hello").expect_contains("echo"),
//! );
//!
//! let report = EvalRunner::new()
//!     .metric(Box::new(SyncMetricAdapter(KeywordMetric)))
//!     .threshold(0.8)
//!     .run(&EchoAgent, &suite)
//!     .await?;
//!
//! assert_eq!(report.total, 1);
//! assert_eq!(report.passed, 1);
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]

pub mod export;
pub mod metrics;
pub mod plugin;
pub mod runner;
pub mod store;
pub mod transcript;

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
pub use metrics::{
    JudgeProvider,
    LlmJudgeMetric,
    SchemaValidationMetric,
    parse_score,
};
pub use plugin::{EvalPlugin, EvalPluginConfig, KEY_EVAL};
pub use runner::{AsyncMetric, EvalAgent, EvalRunner, SyncMetricAdapter};
use serde::{Deserialize, Serialize};
pub use store::EvalStore;
use thiserror::Error;
pub use transcript::{
    SpanRecord,
    ToolCallRecord,
    Transcript,
    TranscriptMessage,
    TranscriptOutcome,
    TranscriptSink,
    Usage,
    jsonl::JsonlTranscriptSink,
};

/// Errors produced by the evaluation framework.
#[derive(Debug, Error)]
pub enum EvalError {
    /// The agent under test failed to answer a case.
    #[error("agent error: {0}")]
    Agent(String),
    /// A judge provider call failed.
    #[error("judge provider error: {0}")]
    Judge(String),
    /// Serializing a report failed.
    #[error("serialization error: {0}")]
    Serialization(String),
    /// Writing an export file failed.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
}

/// Metric name used for the runner's no-metrics keyword fallback
/// (and by [`KeywordMetric`]).
pub const KEYWORD_METRIC_NAME: &str = "keyword_match";

/// A suite of evaluation test cases.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalSuite {
    /// Suite name. `pub(crate)` so the `Serialize` derive can
    /// read it directly; consumers use the [`Self::name`] getter.
    pub(crate) name: String,
    /// Cases in insertion order. `pub(crate)` for the same reason.
    pub(crate) cases: Vec<TestCase>,
}

impl EvalSuite {
    /// Create a new, empty evaluation suite.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            cases: Vec::new(),
        }
    }

    /// Add a test case to the suite (builder style).
    #[must_use]
    pub fn add_case(mut self, case: TestCase) -> Self {
        self.cases.push(case);
        self
    }

    /// The suite name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// All test cases, in insertion order.
    #[must_use]
    pub fn cases(&self) -> &[TestCase] {
        &self.cases
    }
}

/// A single evaluation test case.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestCase {
    /// Test case identifier (also the CSV `case_id`).
    pub id: String,
    /// Input prompt handed to the agent under test.
    pub input: String,
    /// Expected keywords (for [`KeywordMetric`] and the
    /// runner's no-metrics fallback).
    pub expected_keywords: Vec<String>,
    /// Optional expected exact output, recorded with the case.
    pub expected_output: Option<String>,
}

impl TestCase {
    /// Create a new test case with no expectations.
    #[must_use]
    pub fn new(id: impl Into<String>, input: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            input: input.into(),
            expected_keywords: Vec::new(),
            expected_output: None,
        }
    }

    /// Expect the output to contain the given keyword
    /// (case-insensitive substring match).
    #[must_use]
    pub fn expect_contains(mut self, keyword: impl Into<String>) -> Self {
        self.expected_keywords.push(keyword.into());
        self
    }

    /// Record an expected exact output for the case.
    #[must_use]
    pub fn expect_output(mut self, output: impl Into<String>) -> Self {
        self.expected_output = Some(output.into());
        self
    }
}

/// A single test case result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TestResult {
    /// Test case ID.
    pub case_id: String,
    /// The actual output produced by the agent.
    pub actual_output: String,
    /// Metric scores (`BTreeMap` so export ordering is
    /// deterministic), each in `0.0..=1.0`.
    pub scores: BTreeMap<String, f64>,
    /// Whether the case passed (all scores ≥ threshold).
    pub passed: bool,
}

/// An evaluation report: every case result plus aggregates.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvalReport {
    /// Suite name.
    pub suite_name: String,
    /// When the run finished (UTC, RFC 3339 in JSON).
    pub generated_at: DateTime<Utc>,
    /// Individual test results, in suite order.
    pub results: Vec<TestResult>,
    /// Average score across all case × metric scores.
    pub average_score: f64,
    /// Number of cases that passed.
    pub passed: usize,
    /// Total number of cases.
    pub total: usize,
    /// R-OPT-HPC: number of trials per case (set by
    /// `EvalRunner::run_trials(n)`; `1` for `EvalRunner::run`).
    /// Used by `pass_at(k)` / `pass_pow(k)` / `per_case_pass_rate`.
    #[serde(default = "default_trials")]
    pub trials: usize,
    /// R-OPT-HPC: per-case pass counts (filled by `run_trials`;
    /// empty for `run`). Together with `trials` it gives
    /// `per_case_pass_rate`.
    ///
    /// Layout: `case_id -> count_of_passes_across_trials`.
    /// `tests` are excluded from the counts (transcript carries
    /// the `Error` verdict for them; `pass_at` only counts
    /// `Pass { score }`).
    #[serde(default)]
    pub per_case_passes: BTreeMap<String, usize>,
    /// R-OPT-HPC: total trials attempted per case. Together
    /// with `per_case_passes` it gives `per_case_pass_rate`.
    /// Counts `Pass` + `Fail` + `Cancelled`; excludes
    /// `Error` (a backend fault, not a real trial outcome).
    #[serde(default)]
    pub per_case_attempts: BTreeMap<String, usize>,
}

impl EvalReport {
    /// Per-case pass rate (0.0–1.0): `passes / attempts`. Cases
    /// that errored out (no recorded attempt) are reported as
    /// `0.0`.
    #[must_use]
    pub fn per_case_pass_rate(&self) -> BTreeMap<String, f64> {
        self.per_case_passes
            .iter()
            .map(|(k, passes)| {
                let attempts =
                    self.per_case_attempts.get(k).copied().unwrap_or(0);
                let rate = if attempts == 0 {
                    0.0
                } else {
                    *passes as f64 / attempts as f64
                };
                (k.clone(), rate)
            })
            .collect()
    }

    /// `pass_at(k)` — probability that **at least one** of the
    /// `k` sampled trials passed, computed from the per-case
    /// pass rate `c/n`.
    ///
    /// The standard estimator is `1 − (1 − c/n)^k`. When `k = 0`
    /// the result is `0.0`; when `c = 0` the result is `0.0`; when
    /// `c = n` the result is `1.0`. Reads what `run_trials`
    /// recorded in `per_case_passes` / `per_case_attempts`.
    ///
    /// `k` is clamped to `[1, total_trials]` so a caller asking
    /// for `pass_at(0)` is still well-defined (returns `0.0`).
    #[must_use]
    pub fn pass_at(&self, k: u32) -> BTreeMap<String, f64> {
        let mut out = BTreeMap::new();
        for (case_id, attempts) in &self.per_case_attempts {
            let c = self.per_case_passes.get(case_id).copied().unwrap_or(0);
            let n = *attempts;
            if n == 0 || k == 0 {
                out.insert(case_id.clone(), 0.0);
                continue;
            }
            let k_clamped = k.min(n as u32);
            let p = c as f64 / n as f64;
            let prob = 1.0 - (1.0 - p).powi(k_clamped as i32);
            out.insert(case_id.clone(), prob);
        }
        out
    }

    /// `pass_pow(k)` — probability that **all** `k` sampled
    /// trials passed. `(c/n)^k`, clamped to `0.0` when `c = 0`,
    /// `1.0` when `c = n`.
    #[must_use]
    pub fn pass_pow(&self, k: u32) -> BTreeMap<String, f64> {
        let mut out = BTreeMap::new();
        for (case_id, attempts) in &self.per_case_attempts {
            let c = self.per_case_passes.get(case_id).copied().unwrap_or(0);
            let n = *attempts;
            if n == 0 || k == 0 {
                out.insert(case_id.clone(), 0.0);
                continue;
            }
            let k_clamped = k.min(n as u32);
            let p = c as f64 / n as f64;
            out.insert(case_id.clone(), p.powi(k_clamped as i32));
        }
        out
    }

    /// Human-readable summary: pass-rate and average score.
    ///
    /// `pass-rate` here is `passed / total` where `total` is the
    /// `tests` field (1 per case × number of trials when
    /// populated by `run_trials`). This makes a one-trial report
    /// and a ten-trial report read identically for a 100% pass
    /// case.
    #[must_use]
    pub fn summary(&self) -> String {
        let rate = if self.total > 0 {
            self.passed as f64 / self.total as f64 * 100.0
        } else {
            0.0
        };
        format!(
            "Eval Report: {}\n  Passed: {}/{} ({:.1}%)\n  Average Score: {:.2}",
            self.suite_name, self.passed, self.total, rate, self.average_score,
        )
    }
}

impl Default for EvalReport {
    fn default() -> Self {
        Self {
            suite_name: String::new(),
            generated_at: DateTime::UNIX_EPOCH,
            results: Vec::new(),
            average_score: 0.0,
            passed: 0,
            total: 0,
            trials: default_trials(),
            per_case_passes: BTreeMap::new(),
            per_case_attempts: BTreeMap::new(),
        }
    }
}

fn default_trials() -> usize {
    1
}

/// Trait for synchronous evaluation metrics.
///
/// Score the actual output against the expected criteria and
/// return a value in `0.0..=1.0` (1.0 = best). Use
/// [`SyncMetricAdapter`] (or implement [`AsyncMetric`]
/// directly) to feed a sync metric to [`EvalRunner`].
pub trait Metric: Send + Sync + 'static {
    /// Metric name — the key under which the runner records the
    /// score in [`TestResult::scores`].
    fn name(&self) -> &'static str;

    /// Score the actual output.
    fn score(
        &self,
        input: &str,
        actual_output: &str,
        expected_keywords: &[&str],
    ) -> f64;
}

/// Built-in keyword matching metric.
///
/// Scores the fraction of expected keywords found in the output
/// (case-insensitive substring match). With no expected
/// keywords the score is `1.0` — there is nothing to fail.
pub struct KeywordMetric;

impl Metric for KeywordMetric {
    fn name(&self) -> &'static str {
        KEYWORD_METRIC_NAME
    }

    fn score(
        &self,
        _input: &str,
        actual_output: &str,
        expected_keywords: &[&str],
    ) -> f64 {
        if expected_keywords.is_empty() {
            return 1.0;
        }
        let output_lower = actual_output.to_lowercase();
        let matched = expected_keywords
            .iter()
            .filter(|kw| output_lower.contains(&kw.to_lowercase()))
            .count();
        matched as f64 / expected_keywords.len() as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suite_builder_accumulates_cases_in_order() {
        let suite = EvalSuite::new("test_suite")
            .add_case(TestCase::new("t1", "Hello"))
            .add_case(TestCase::new("t2", "World"));
        assert_eq!(suite.name(), "test_suite");
        let cases = suite.cases();
        assert_eq!(cases.len(), 2);
        assert_eq!(cases[0].id, "t1");
        assert_eq!(cases[1].input, "World");
    }

    #[test]
    fn case_builder_records_expectations() {
        let tc = TestCase::new("t1", "prompt")
            .expect_contains("keyword1")
            .expect_contains("keyword2")
            .expect_output("exact output");
        assert_eq!(tc.expected_keywords.len(), 2);
        assert_eq!(tc.expected_output, Some("exact output".into()));
    }

    #[test]
    fn keyword_metric_scores_fraction_of_matches() {
        let m = KeywordMetric;
        assert!(
            (m.score("in", "hello world foo", &["hello", "world"]) - 1.0).abs()
                < 1e-9
        );
        assert!(
            (m.score("in", "hello there", &["hello", "world"]) - 0.5).abs()
                < 1e-9
        );
        assert!(
            (m.score("in", "nothing here", &["hello", "world"]) - 0.0).abs()
                < 1e-9
        );
    }

    #[test]
    fn keyword_metric_matches_case_insensitively() {
        let m = KeywordMetric;
        assert!(
            (m.score("in", "HELLO World", &["hello", "WORLD"]) - 1.0).abs()
                < 1e-9
        );
    }

    #[test]
    fn keyword_metric_empty_keywords_score_one() {
        let m = KeywordMetric;
        assert!((m.score("in", "anything", &[]) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn report_summary_shows_pass_rate_and_average() {
        let generated_at =
            chrono::DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc);
        let report = EvalReport {
            suite_name: "suite".into(),
            generated_at,
            results: Vec::new(),
            average_score: 0.75,
            passed: 3,
            total: 4,
            trials: 1,
            per_case_passes: BTreeMap::new(),
            per_case_attempts: BTreeMap::new(),
        };
        let s = report.summary();
        assert!(s.contains("3/4"), "summary: {s}");
        assert!(s.contains("75.0%"), "summary: {s}");
        assert!(s.contains("0.75"), "summary: {s}");
    }
    #[test]
    fn report_summary_empty_suite_avoids_division_by_zero() {
        let generated_at =
            chrono::DateTime::parse_from_rfc3339("2026-01-01T12:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc);
        let report = EvalReport {
            suite_name: "empty".into(),
            generated_at,
            results: Vec::new(),
            average_score: 0.0,
            passed: 0,
            total: 0,
            trials: 1,
            per_case_passes: BTreeMap::new(),
            per_case_attempts: BTreeMap::new(),
        };
        assert!(report.summary().contains("0/0 (0.0%)"));
    }
}
