//! [`uva_eval`] — suites, metrics, runner, export.
//!
//! [`EvalSuite`] describes cases, [`Metric`] / [`AsyncMetric`] score
//! them ([`KeywordMetric`], [`LlmJudgeMetric`],
//! [`SchemaValidationMetric`]), and [`EvalRunner`] decides pass/fail
//! against a threshold before exporting JSON or CSV for a CI job to
//! read.
//!
//! The evaluation trajectory lives here too: `Transcript` /
//! `TranscriptSink` / `JsonlTranscriptSink` record what the agent did
//! per case (tool calls, token counts, spans). They are reachable
//! under this one path — the facade's former `uva::transcript`
//! module is gone.

pub use uva_eval::*;
