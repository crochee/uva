//! [`Transcript`] — one case's full evaluation footprint.
//!
//! A `Transcript` is the second axis of an evaluation: alongside
//! the metric scores (`EvalReport::results`) it records what the
//! agent actually did, what tools it called, how many tokens it
//! spent, and which spans were emitted. Wire as JSONL via
//! [`JsonlTranscriptSink`](crate::transcript::jsonl::JsonlTranscriptSink)
//! so a regression has both the score (`TestResult`) and the path
//! to it (`Transcript`).
//!
//! The transcript only references primitive types and types
//! already in `uva-core` / `uva-provider` — no OTel types
//! leak across the wire. The `SpanRecord` is a flat shape
//! (canonical name + attributes) so a fresh process can replay a
//! transcript without importing `uva-telemetry`'s opentelemetry
//! surface.

pub mod jsonl;
use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uva_core::Clock as _;

use crate::EvalError;

/// One message in a [`Transcript`]'s history — a minimal,
/// runtime-free wire projection of the provider's richer
/// `Message` type (role + flattened text). Defined locally so
/// `uva-eval` keeps its tokio-free guarantee
/// (`make check-no-runtime`); hosts convert their own
/// `Vec<Message>` when they build a `Transcript`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TranscriptMessage {
    /// The message role (`"system"` / `"user"` / `"assistant"` /
    /// `"tool"`).
    pub role: String,
    /// The flattened textual content of the message (text parts
    /// joined; non-text parts contribute nothing).
    pub text: String,
}

impl TranscriptMessage {
    /// Convenience constructor.
    #[must_use]
    pub fn new(role: impl Into<String>, text: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            text: text.into(),
        }
    }
}

/// Per-call tool invocation record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolCallRecord {
    /// Tool name (the model's `ToolUse.name`).
    pub tool_name: String,
    /// Tool-use id (echoed back in the `ToolResult`).
    pub call_id: String,
    /// JSON-serialised arguments, preserved verbatim from the
    /// provider's `ToolUse.input`.
    pub input: serde_json::Value,
    /// Whether the call returned an error.
    pub is_error: bool,
    /// Wall-clock duration of the call (best-effort; a backend
    /// that has no timer can report `Duration::ZERO`).
    pub elapsed_ms: u64,
}

impl ToolCallRecord {
    /// Convenience constructor for the common path.
    #[must_use]
    pub fn new(
        tool_name: impl Into<String>,
        call_id: impl Into<String>,
        input: serde_json::Value,
        is_error: bool,
        elapsed_ms: u64,
    ) -> Self {
        Self {
            tool_name: tool_name.into(),
            call_id: call_id.into(),
            input,
            is_error,
            elapsed_ms,
        }
    }
}

/// Flat span record — a structured replacement for an opentelemetry
/// span. The `name` is the canonical span name
/// (`"uva_llm_call"` / `"uva_agent_iteration"` etc.); the
/// `attributes` map carries the typed payload (e.g.
/// `("gen_ai.model", "claude-opus-4-20250514")`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SpanRecord {
    /// Canonical span name (mirrors the `#[instrument(name = …)]`).
    pub name: String,
    /// Span kind tag (`"agent"` / `"tool"` / `"llm"` / …).
    pub kind: String,
    /// Flat attributes as `(key, value)` pairs; values that
    /// themselves carry structure are JSON-stringified.
    pub attributes: Vec<(String, String)>,
    /// Wall-clock duration.
    pub elapsed_ms: u64,
}

impl SpanRecord {
    /// Convenience constructor for the common path.
    #[must_use]
    pub fn new(
        name: impl Into<String>,
        kind: impl Into<String>,
        attributes: Vec<(String, String)>,
        elapsed_ms: u64,
    ) -> Self {
        Self {
            name: name.into(),
            kind: kind.into(),
            attributes,
            elapsed_ms,
        }
    }
}

/// Per-trial token usage rollup.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Usage {
    /// Input tokens (prompt_tokens).
    pub input: usize,
    /// Cache-read tokens (`cache_read_tokens`).
    pub cache_read: usize,
    /// Cache-write tokens (`cache_creation_tokens`).
    pub cache_write: usize,
    /// Output tokens (completion_tokens).
    pub output: usize,
}

impl Usage {
    /// Total tokens consumed (input + output).
    #[must_use]
    pub fn total(&self) -> usize {
        self.input + self.output
    }

    /// Cache hit rate (0.0–1.0); `0.0` when no cache activity.
    #[must_use]
    pub fn cache_hit_rate(&self) -> f64 {
        let cached = self.cache_read;
        if cached + self.input == 0 {
            0.0
        } else {
            cached as f64 / (cached + self.input) as f64
        }
    }
}

/// What one trial produced: passed, failed, errored, or was
/// cancelled by the user.
///
/// The variant discriminates so a `pass_at` reducer can count
/// passes distinctly from `Pass { score }` reports — the
/// canonical pass@k estimator uses `Pass` only (per Anthropic
/// eval glossary: a `Fail` does count as a "non-success" in the
/// per-trial population, but does not lower `c`, the count of
/// passes; an `Error` is excluded from the per-trial population
/// entirely).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum TranscriptOutcome {
    /// The agent's output cleared every metric threshold.
    Pass {
        /// The headline score (mean across metrics).
        score: f64,
    },
    /// The agent's output failed at least one metric threshold.
    Fail {
        /// The headline score (mean across metrics).
        score: f64,
        /// Plain-text reason; redundant with
        /// `EvalReport::results` but useful when reading the
        /// transcript in isolation.
        reason: String,
    },
    /// The agent errored (network / panicking provider). Excluded
    /// from the pass@k count of trials.
    Error {
        /// The error message (transport failure, provider panic,
        /// …). Kept as a struct field (not a newtype tuple)
        /// because the internally-tagged serde representation
        /// does not support newtype variants carrying strings.
        message: String,
    },
    /// The trial was cancelled before completion.
    Cancelled,
}

impl TranscriptOutcome {
    /// `true` for [`Self::Pass`] only — convenience for the
    /// `pass_at` estimator.
    #[must_use]
    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Pass { .. })
    }

    /// Stable lowercase tag (matches `serde(rename_all)`).
    #[must_use]
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass { .. } => "pass",
            Self::Fail { .. } => "fail",
            Self::Error { .. } => "error",
            Self::Cancelled => "cancelled",
        }
    }
}

/// One trial's full evaluation footprint.
///
/// Wire as JSONL via [`JsonlTranscriptSink`](jsonl::JsonlTranscriptSink):
/// every trial lands one line so a regression can replay a single
/// row and diff what the agent did.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Transcript {
    /// Test-case id (mirrors `EvalReport::results[i].case_id`).
    pub case_id: String,
    /// Optional seed; absent for non-deterministic agents.
    pub seed: Option<String>,
    /// The full message history from the run.
    pub messages: Vec<TranscriptMessage>,
    /// Every tool call the trial executed (model-emitted).
    pub tool_calls: Vec<ToolCallRecord>,
    /// Token usage rollup.
    pub usage: Usage,
    /// Span records (flat shape; no OTel types cross the wire).
    pub spans: Vec<SpanRecord>,
    /// Pass/fail/error/cancelled verdict + per-trial score.
    pub outcome: TranscriptOutcome,
    /// Wall-clock trial start (UTC, RFC 3339 in JSON).
    pub started_at: DateTime<Utc>,
    /// Wall-clock trial finish (UTC).
    pub finished_at: DateTime<Utc>,
    /// Optional free-form attribute bag (model id, tag,
    /// environment label). The shape is `BTreeMap` so JSON
    /// serialisation is deterministic.
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
}

impl Transcript {
    /// Convenience constructor; the host fills in `tool_calls`,
    /// `spans`, `usage`, `outcome` as the trial progresses.
    ///
    /// Wall-clock stamps go through [`uva_core::Clock`]; a caller
    /// that needs deterministic timestamps uses [`Self::new_at`].
    #[must_use]
    pub fn new(case_id: impl Into<String>) -> Self {
        Self::new_at(case_id, uva_core::SharedClock::system().now())
    }

    /// [`Self::new`] with an explicit clock reading (AGENTS.md §3.8).
    #[must_use]
    pub fn new_at(case_id: impl Into<String>, now: DateTime<Utc>) -> Self {
        Self {
            case_id: case_id.into(),
            seed: None,
            messages: Vec::new(),
            tool_calls: Vec::new(),
            usage: Usage::default(),
            spans: Vec::new(),
            outcome: TranscriptOutcome::Cancelled,
            started_at: now,
            finished_at: now,
            attributes: BTreeMap::new(),
        }
    }

    /// Attach a seed (deterministic trials).
    #[must_use]
    pub fn with_seed(mut self, seed: impl Into<String>) -> Self {
        self.seed = Some(seed.into());
        self
    }

    /// Attach a typed attribute.
    #[must_use]
    pub fn with_attribute(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.attributes.insert(key.into(), value.into());
        self
    }

    /// Convenience: total wall-clock duration of the trial.
    #[must_use]
    pub fn elapsed(&self) -> chrono::Duration {
        self.finished_at - self.started_at
    }
}

/// The persistence seam for [`Transcript`]s.
///
/// Implementors write **one record per trial** — line-based
/// formats (JSONL) and tabular formats (Parquet) both fit the
/// signature. The trait is sync because the runner holds the
/// sink on its own task and pushes one row per trial; an async
/// transport wraps its backend in a `spawn_blocking`.
///
/// [`EvalError::Serialization`] and [`EvalError::Io`] cover
/// the two failure modes.
pub trait TranscriptSink: Send + Sync {
    /// Persist `transcript`. Called once per trial in
    /// `EvalRunner::run_trials`.
    ///
    /// # Errors
    ///
    /// Implementors return [`EvalError::Serialization`] when the
    /// record cannot be encoded, or [`EvalError::Io`] when the
    /// transport fails. The runner surfaces the error and the
    /// trial verdict flips to [`TranscriptOutcome::Error`].
    fn write(&self, transcript: &Transcript) -> Result<(), EvalError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `as_str()` and the `serde` wire form MUST agree — the JSONL
    /// sink and dashboard both key on the lowercase form.
    #[test]
    fn outcome_as_str_matches_serde() {
        use serde_json::Value;
        // The wire form is internally tagged:
        // `{"verdict":"pass","score":1.0}`. `as_str()` returns the
        // bare tag; the JSON comparison goes through the
        // `verdict` field.
        for (outcome, expected) in [
            (TranscriptOutcome::Pass { score: 1.0 }, "pass"),
            (
                TranscriptOutcome::Fail {
                    score: 0.4,
                    reason: "x".into(),
                },
                "fail",
            ),
            (
                TranscriptOutcome::Error {
                    message: "boom".into(),
                },
                "error",
            ),
            (TranscriptOutcome::Cancelled, "cancelled"),
        ] {
            let json: Value = serde_json::to_value(&outcome).unwrap();
            assert_eq!(
                json.get("verdict").and_then(Value::as_str),
                Some(expected),
                "mismatch for {outcome:?}"
            );
            assert_eq!(outcome.as_str(), expected);
        }
    }

    /// `is_pass` is the property the `pass_at` estimator relies
    /// on — pin so a future variant addition does not silently
    /// change behaviour.
    #[test]
    fn is_pass_only_true_for_pass_variant() {
        assert!(TranscriptOutcome::Pass { score: 1.0 }.is_pass());
        assert!(
            !TranscriptOutcome::Fail {
                score: 0.0,
                reason: "x".into(),
            }
            .is_pass()
        );
        assert!(
            !TranscriptOutcome::Error {
                message: "e".into(),
            }
            .is_pass()
        );
        assert!(!TranscriptOutcome::Cancelled.is_pass());
    }

    /// `Usage::cache_hit_rate` is `0.0` when there's nothing to
    /// measure (no cache + no input). Otherwise it's
    /// `cache_read / (cache_read + input)`.
    #[test]
    fn usage_cache_hit_rate_zero_when_no_activity() {
        assert_eq!(Usage::default().cache_hit_rate(), 0.0);
        let u = Usage {
            input: 0,
            cache_read: 0,
            ..Default::default()
        };
        assert_eq!(u.cache_hit_rate(), 0.0);
    }

    #[test]
    fn usage_cache_hit_rate_matches_definition() {
        let u = Usage {
            input: 100,
            cache_read: 50,
            cache_write: 0,
            output: 0,
        };
        // 50 / (50 + 100) = 0.333…
        assert!((u.cache_hit_rate() - (50.0 / 150.0)).abs() < 1e-9);
    }

    /// Total tokens = input + output, ignoring cache.
    #[test]
    fn usage_total_ignores_cache() {
        let u = Usage {
            input: 7,
            cache_read: 3,
            cache_write: 1,
            output: 5,
        };
        assert_eq!(u.total(), 12);
    }

    /// `Transcript::elapsed()` is the difference between the two
    /// timestamps.
    #[test]
    fn transcript_elapsed_is_diff() {
        let mut t = Transcript::new("c1");
        t.started_at =
            chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc);
        t.finished_at =
            chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:30Z")
                .unwrap()
                .with_timezone(&chrono::Utc);
        assert_eq!(t.elapsed(), chrono::Duration::seconds(30));
    }
}
