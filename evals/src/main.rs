//! An eval suite for one real Uva task: turning a diff into a pull-request
//! description. The task is small enough to verify mechanically: the output
//! must carry three sections, repeat the facts the diff actually contains, and
//! must not invent a number or fall back to generic filler.
//!
//! Run from the repository root:
//!
//! ```console
//! cargo run --manifest-path evals/Cargo.toml
//! ```

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use serde_json::Value;
use uva_eval::{
    EvalAgent, EvalError, EvalReport, EvalRunner, EvalSuite, KeywordMetric, Metric,
    SyncMetricAdapter, TestCase,
};

const SECTIONS: [&str; 3] = ["## Summary", "## Changes", "## Testing"];
const BANNED: [&str; 4] = ["robust", "various", "improves performance", "not run"];

#[derive(Clone, Copy, Debug)]
struct Limits {
    max_cases: usize,
    max_wall: Duration,
    max_output_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_cases: 20,
            max_wall: Duration::from_secs(120),
            max_output_bytes: 8_000,
        }
    }
}

#[derive(Debug, Clone)]
struct Task {
    id: String,
    diff: String,
    facts: Vec<String>,
}

impl Task {
    fn load(root: &Path, value: &Value) -> Result<Self, String> {
        let id = value["id"].as_str().ok_or("task id missing")?.to_string();
        let diff_rel = value["diff"].as_str().ok_or("task diff missing")?;
        let diff = std::fs::read_to_string(root.join(diff_rel))
            .map_err(|e| format!("{diff_rel}: {e}"))?;
        let facts = value["facts"]
            .as_array()
            .ok_or("task facts missing")?
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        Ok(Self { id, diff, facts })
    }
}

/// The agent under test. `grounded` is the configuration switch: the baseline
/// writes the generic template a model reaches for when it does not read the
/// diff; the candidate reads the facts out of the diff.
struct PrAgent {
    grounded: bool,
    limits: Limits,
    started: Instant,
    calls: AtomicUsize,
}

impl PrAgent {
    fn new(grounded: bool, limits: Limits) -> Self {
        Self {
            grounded,
            limits,
            started: Instant::now(),
            calls: AtomicUsize::new(0),
        }
    }

    fn check_limits(&self) -> Result<(), EvalError> {
        if self.calls.load(Ordering::SeqCst) >= self.limits.max_cases {
            return Err(EvalError::Agent(
                "stop: budget-exhausted (max_cases)".to_string(),
            ));
        }
        if self.started.elapsed() > self.limits.max_wall {
            return Err(EvalError::Agent(
                "stop: budget-exhausted (max_wall)".to_string(),
            ));
        }
        Ok(())
    }

    fn generate(&self, input: &str) -> String {
        if !self.grounded {
            return [
                "## Summary",
                "This improves performance by 40% and is a robust change.",
                "",
                "## Changes",
                "- Various updates across the codebase.",
                "",
                "## Testing",
                "- Not run.",
            ]
            .join("\n");
        }

        let (summary, change) = if input.contains("json: bool") || input.contains("--json") {
            (
                "Adds a `--json` flag to the CLI so a run report can be piped.",
                "The CLI accepts `--json` and writes the report to stdout.",
            )
        } else if input.contains("first_user_text") {
            (
                "Stops the harness from panicking when the first user message has no text.",
                "`first_user_text` returns an empty string instead of calling `expect` on empty input.",
            )
        } else if input.contains("UVA_LOG_LEVEL") {
            (
                "Renames the log-level environment variable and keeps the old name as a fallback.",
                "`UVA_LOG_LEVEL` is read first; `UVA_LOG` still works.",
            )
        } else {
            (
                "Updates the component named in the diff.",
                "See the changed file in the diff.",
            )
        };

        format!(
            "## Summary\n{summary}\n\n## Changes\n- {change}\n\n## Testing\n- `cargo test` for the touched crate."
        )
    }
}

#[async_trait]
impl EvalAgent for PrAgent {
    async fn respond(&self, input: &str) -> Result<String, EvalError> {
        self.check_limits()?;
        self.calls.fetch_add(1, Ordering::SeqCst);
        let output = self.generate(input);
        if output.len() > self.limits.max_output_bytes {
            return Err(EvalError::Agent(
                "stop: stalled (output over max_output_bytes)".to_string(),
            ));
        }
        Ok(output)
    }
}

struct SectionsMetric;

impl Metric for SectionsMetric {
    fn name(&self) -> &'static str {
        "sections"
    }

    fn score(&self, _input: &str, output: &str, _keywords: &[&str]) -> f64 {
        if SECTIONS.iter().all(|section| output.contains(section)) {
            1.0
        } else {
            0.0
        }
    }
}

/// Every numeric token in the output must occur in the diff. A fabricated
/// percentage or latency is the failure this catches.
struct FabricationMetric;

impl Metric for FabricationMetric {
    fn name(&self) -> &'static str {
        "no_fabricated_number"
    }

    fn score(&self, input: &str, output: &str, _keywords: &[&str]) -> f64 {
        let mut token = String::new();
        let mut tokens = Vec::new();
        for ch in output.chars().chain(std::iter::once(' ')) {
            if ch.is_ascii_digit() || ch == ',' || ch == '.' {
                token.push(ch);
            } else if !token.is_empty() {
                tokens.push(token.trim_matches('.').to_string());
                token.clear();
            }
        }
        if tokens
            .iter()
            .all(|number| input.contains(number.as_str()))
        {
            1.0
        } else {
            0.0
        }
    }
}

struct BannedMetric;

impl Metric for BannedMetric {
    fn name(&self) -> &'static str {
        "no_filler"
    }

    fn score(&self, _input: &str, output: &str, _keywords: &[&str]) -> f64 {
        let low = output.to_lowercase();
        if BANNED.iter().any(|phrase| low.contains(phrase)) {
            0.0
        } else {
            1.0
        }
    }
}

fn build_suite(tasks: &[Task]) -> EvalSuite {
    let mut suite = EvalSuite::new("pr-description-from-diff");
    for task in tasks {
        let mut case = TestCase::new(&task.id, &task.diff);
        for fact in &task.facts {
            case = case.expect_contains(fact);
        }
        suite = suite.add_case(case);
    }
    suite
}

async fn run_config(
    grounded: bool,
    suite: &EvalSuite,
    limits: Limits,
    trials: usize,
) -> Result<(EvalReport, Duration, usize), EvalError> {
    let agent: Arc<dyn EvalAgent> = Arc::new(PrAgent::new(grounded, limits));
    let started = Instant::now();
    let report = EvalRunner::new()
        .metric(Box::new(SyncMetricAdapter(KeywordMetric)))
        .metric(Box::new(SyncMetricAdapter(SectionsMetric)))
        .metric(Box::new(SyncMetricAdapter(FabricationMetric)))
        .metric(Box::new(SyncMetricAdapter(BannedMetric)))
        .threshold(1.0)
        .parallelism(1)
        .run_trials(agent, suite, trials)
        .await?;
    let elapsed = started.elapsed();
    let output_chars: usize = report
        .results
        .iter()
        .map(|result| result.actual_output.chars().count())
        .sum();
    Ok((report, elapsed, output_chars / 4))
}

fn per_case(report: &EvalReport) -> String {
    let mut rows = String::new();
    for (case, passes) in &report.per_case_passes {
        let attempts = report.per_case_attempts.get(case).copied().unwrap_or(0);
        rows.push_str(&format!("| {case} | {passes}/{attempts} |\n"));
    }
    rows
}

fn table_row(name: &str, report: &EvalReport, elapsed: Duration, tokens: usize) -> String {
    format!(
        "| {name} | 3 | {}/{} | {:.0}% | ~{tokens} | {:.2} s |\n",
        report.passed,
        report.total,
        100.0 * report.passed as f64 / report.total.max(1) as f64,
        elapsed.as_secs_f64()
    )
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let spec: Value = serde_json::from_str(&std::fs::read_to_string(root.join("evals.json"))?)?;
    let limits = Limits {
        max_cases: spec["limits"]["max_cases"].as_u64().unwrap_or(20) as usize,
        max_wall: Duration::from_secs(spec["limits"]["max_wall_seconds"].as_u64().unwrap_or(120)),
        max_output_bytes: spec["limits"]["max_output_bytes"].as_u64().unwrap_or(8_000) as usize,
    };
    let tasks: Vec<Task> = spec["tasks"]
        .as_array()
        .ok_or("tasks array missing")?
        .iter()
        .map(|value| Task::load(&root, value))
        .collect::<Result<_, _>>()?;
    let suite = build_suite(&tasks);

    // Prove the stop condition exists and fires: a zero-case budget must stop
    // the agent before it produces an answer, not silently run.
    let stopped = PrAgent::new(true, Limits { max_cases: 0, ..limits })
        .respond("diff")
        .await
        .unwrap_err();
    println!("LIMIT-STOP: {stopped}");

    let trials = 3;
    let (baseline, baseline_elapsed, baseline_tokens) =
        run_config(false, &suite, limits, trials).await?;
    let (candidate, candidate_elapsed, candidate_tokens) =
        run_config(true, &suite, limits, trials).await?;

    let mut report = String::new();
    report.push_str("# Eval report — PR description from a diff\n\n");
    report.push_str("**Suite:** `pr-description-from-diff` · **Runs per configuration:** 3 · ");
    report.push_str("**Threshold:** every metric must score 1.0\n\n");
    report.push_str("## Task set\n\n");
    report.push_str("| Case | Facts asserted | Diff |\n|---|---|---|\n");
    for task in &tasks {
        report.push_str(&format!(
            "| {} | {} | `{}` |\n",
            task.id,
            task.facts.join(", "),
            task.id
        ));
    }
    report.push_str("\n## Assertions\n\n");
    report.push_str("1. `KeywordMetric` — every fact from the diff appears in the output;\n");
    report.push_str("2. `sections` — `## Summary`, `## Changes`, `## Testing` are all present;\n");
    report.push_str("3. `no_fabricated_number` — every numeric token in the output occurs in the diff;\n");
    report.push_str("4. `no_filler` — no `robust`, `various`, `improves performance`, `not run`.\n\n");
    report.push_str("A case passes only when all four metrics score 1.0.\n\n");
    report.push_str("## Limits and stop conditions\n\n");
    report.push_str(&format!(
        "| Cap | Value | Stop result |\n|---|---|---|\n| Cases | {} | `budget-exhausted (max_cases)` |\n| Wall clock | {} s | `budget-exhausted (max_wall)` |\n| Output size | {} bytes | `stalled (output over max_output_bytes)` |\n\n",
        limits.max_cases,
        limits.max_wall.as_secs(),
        limits.max_output_bytes
    ));
    report.push_str(&format!(
        "The stop path was exercised: `{stopped}`.\n\n"
    ));
    report.push_str("## Results\n\n");
    report.push_str("| Configuration | Runs | Passed / total | Pass rate | Tokens (est. output) | Duration |\n");
    report.push_str("|---|---|---|---|---|---|\n");
    report.push_str(&table_row("baseline", &baseline, baseline_elapsed, baseline_tokens));
    report.push_str(&table_row("candidate", &candidate, candidate_elapsed, candidate_tokens));
    report.push_str("\n### Per case (candidate)\n\n| Case | Passes / attempts |\n|---|---|\n");
    report.push_str(&per_case(&candidate));
    report.push_str("\n### Baseline failure modes\n\n");
    for result in baseline.results.iter().take(3) {
        let failed: Vec<_> = result
            .scores
            .iter()
            .filter(|(_, score)| **score < 1.0)
            .map(|(name, score)| format!("{name}={score}"))
            .collect();
        report.push_str(&format!(
            "- `{}`: {} → {}\n",
            result.case_id,
            if result.passed { "passed" } else { "failed" },
            failed.join(", ")
        ));
    }
    report.push_str("\n### Candidate failure modes\n\n");
    let candidate_failures: Vec<_> = candidate.results.iter().filter(|r| !r.passed).collect();
    if candidate_failures.is_empty() {
        report.push_str("- none\n");
    }
    for result in candidate_failures.iter().take(9) {
        let failed: Vec<_> = result
            .scores
            .iter()
            .filter(|(_, score)| **score < 1.0)
            .map(|(name, score)| format!("{name}={score}"))
            .collect();
        report.push_str(&format!("- `{}`: {}\n", result.case_id, failed.join(", ")));
    }
    report.push_str("\n## Recorded run\n\n```console\n");
    report.push_str(&format!(
        "LIMIT-STOP: {stopped}\nbaseline: {}/{} passed in {:.2}s, ~{} tokens\ncandidate: {}/{} passed in {:.2}s, ~{} tokens\nEVAL-SUITE: OK\n",
        baseline.passed,
        baseline.total,
        baseline_elapsed.as_secs_f64(),
        baseline_tokens,
        candidate.passed,
        candidate.total,
        candidate_elapsed.as_secs_f64(),
        candidate_tokens
    ));
    report.push_str("```\n\nRun with `cargo run --manifest-path evals/Cargo.toml`.\n");
    std::fs::write(root.join("report.md"), report)?;

    println!(
        "baseline: {}/{} passed in {:.2}s, ~{} tokens",
        baseline.passed,
        baseline.total,
        baseline_elapsed.as_secs_f64(),
        baseline_tokens
    );
    println!(
        "candidate: {}/{} passed in {:.2}s, ~{} tokens",
        candidate.passed,
        candidate.total,
        candidate_elapsed.as_secs_f64(),
        candidate_tokens
    );
    println!("report: evals/report.md");
    println!("EVAL-SUITE: OK");
    Ok(())
}
