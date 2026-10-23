//! Evaluation transcripts from [`synthia_eval`] — the
//! trajectory + usage record a `run_trials` evaluation leaves
//! behind.
//!
//! The `transcript` feature is opt-in (not in the default set). It
//! re-exports the eval crate's transcript surface so a consumer
//! wiring a regression harness reaches one path:
//!
//! - [`Transcript`] — one trial's full footprint (messages, tool
//!   calls, usage, spans, outcome, timestamps).
//! - [`TranscriptSink`] / [`JsonlTranscriptSink`] — one row per
//!   trial, JSONL wire.
//! - [`TranscriptOutcome`] / [`ToolCallRecord`] / [`SpanRecord`] /
//!   [`Usage`] — the payload shapes.
//!
//! ```rust,ignore
//! use std::{fs::File, io::BufWriter, sync::Arc};
//!
//! use synthia::transcript::{JsonlTranscriptSink, Transcript, TranscriptSink};
//!
//! let sink: Arc<dyn TranscriptSink> =
//!     Arc::new(JsonlTranscriptSink::new(BufWriter::new(File::create("t.jsonl")?)));
//! sink.write(&transcript)?;
//! ```
//!
//! ```bash
//! cargo run --example transcript_demo -p synthia-eval
//! ```

pub use synthia_eval::{
    JsonlTranscriptSink,
    SpanRecord,
    ToolCallRecord,
    Transcript,
    TranscriptOutcome,
    TranscriptSink,
    Usage,
};
