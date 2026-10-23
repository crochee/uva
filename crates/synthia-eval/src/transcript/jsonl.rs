//! JSONL sink for [`Transcript`].
//!
//! One trial per line. The whole document is line-oriented so a
//! downstream tool (`grep` / `jq` / DuckDB `read_json` / …) can
//! process a multi-trial file without any extra tooling.

use std::{io::Write, sync::Mutex};

use super::{Transcript, TranscriptSink};
use crate::EvalError;

/// Write one [`Transcript`] per `write` call,
/// encoded as JSON followed by `\n`. All writes serialise through
/// the inner `Mutex<W>` so a multi-trial `run_trials` does not
/// interleave bytes mid-line.
pub struct JsonlTranscriptSink<W: Write + Send> {
    /// The mutex-guarded writer. `Mutex` (not `RwLock`) because
    /// every write is a write-lock anyway and `parking_lot::Mutex`
    /// does not support `Write`-through without an inner
    /// `BufWriter` — both bring equal friction.
    writer: Mutex<W>,
}

impl<W: Write + Send> JsonlTranscriptSink<W> {
    /// Wrap a writer. The writer is not buffered — call sites
    /// that want throughput wrap with `std::io::BufWriter`
    /// first.
    #[must_use]
    pub fn new(writer: W) -> Self {
        Self {
            writer: Mutex::new(writer),
        }
    }

    /// Borrow the inner writer (e.g. to flush a `BufWriter` or
    /// `sync_data` a file). The borrow is taken outside the
    /// `Mutex` — a writer that requires exclusive access while a
    /// trial is mid-flight will deadlock; this method exists for
    /// post-run cleanup only.
    pub fn into_inner(self) -> W {
        self.writer.into_inner().unwrap_or_else(|e| e.into_inner())
    }
}

impl<W: Write + Send> TranscriptSink for JsonlTranscriptSink<W> {
    fn write(&self, transcript: &Transcript) -> Result<(), EvalError> {
        let line = serde_json::to_string(transcript)
            .map_err(|e| EvalError::Serialization(e.to_string()))?;
        let mut w = self
            .writer
            .lock()
            .map_err(|e| EvalError::Serialization(e.to_string()))?;
        w.write_all(line.as_bytes()).map_err(EvalError::Io)?;
        w.write_all(b"\n").map_err(EvalError::Io)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transcript::{
        SpanRecord,
        ToolCallRecord,
        Transcript,
        TranscriptOutcome,
        Usage,
    };

    fn sample_transcript(
        case_id: &str,
        outcome: TranscriptOutcome,
    ) -> Transcript {
        Transcript {
            case_id: case_id.into(),
            seed: Some("seed-1".into()),
            messages: Vec::new(),
            tool_calls: vec![ToolCallRecord::new(
                "echo",
                "tc1",
                serde_json::json!({"x": 1}),
                false,
                12,
            )],
            usage: Usage {
                input: 10,
                cache_read: 0,
                cache_write: 0,
                output: 5,
            },
            spans: vec![SpanRecord::new(
                "sample",
                "agent",
                vec![("k".into(), "v".into())],
                100,
            )],
            outcome,
            started_at: chrono::DateTime::parse_from_rfc3339(
                "2026-01-01T00:00:00Z",
            )
            .unwrap()
            .with_timezone(&chrono::Utc),
            finished_at: chrono::DateTime::parse_from_rfc3339(
                "2026-01-01T00:00:30Z",
            )
            .unwrap()
            .with_timezone(&chrono::Utc),
            attributes: Default::default(),
        }
        .with_attribute("model", "claude")
    }

    fn outcome_pass(score: f64) -> TranscriptOutcome {
        TranscriptOutcome::Pass { score }
    }

    /// A successful `write` produces one line ending in `\n`.
    #[test]
    fn write_appends_one_json_line() {
        let mut buf: Vec<u8> = Vec::new();
        let sink = JsonlTranscriptSink::new(&mut buf);
        sink.write(&sample_transcript("c1", outcome_pass(1.0)))
            .unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(s.lines().count(), 1, "expected exactly one line");
        assert!(s.ends_with('\n'));
    }

    /// Multiple `write` calls do not interleave bytes mid-line —
    /// the inner `Mutex<W>` serialises the append.
    #[test]
    fn multiple_writes_produce_one_line_each() {
        let mut buf: Vec<u8> = Vec::new();
        let sink = JsonlTranscriptSink::new(&mut buf);
        for i in 0..4 {
            sink.write(&sample_transcript(&format!("c{i}"), outcome_pass(1.0)))
                .unwrap();
        }
        let s = String::from_utf8(buf).unwrap();
        assert_eq!(s.lines().count(), 4);
        // Each line must individually round-trip back into the
        // transcript struct — no half-written JSON-lines.
        for line in s.lines() {
            assert!(!line.is_empty());
            let _: Transcript = serde_json::from_str(line).unwrap();
        }
    }

    /// Round-trip via JSON parse — the wire format is JSON, so a
    /// downstream consumer can read it without the sink runtime.
    #[test]
    fn round_trip_through_serde_json() {
        let t = sample_transcript("c2", outcome_pass(0.8));
        let mut buf: Vec<u8> = Vec::new();
        let sink = JsonlTranscriptSink::new(&mut buf);
        sink.write(&t).unwrap();
        let line = String::from_utf8(buf).unwrap();
        let back: Transcript = serde_json::from_str(line.trim_end()).unwrap();
        assert_eq!(back.case_id, "c2");
        assert_eq!(back.tool_calls.len(), 1);
        assert_eq!(back.tool_calls[0].tool_name, "echo");
        assert_eq!(back.usage.input, 10);
        assert_eq!(back.usage.output, 5);
        assert_eq!(back.spans.len(), 1);
        assert_eq!(
            back.attributes.get("model").map(String::as_str),
            Some("claude")
        );
        assert_eq!(back.outcome, TranscriptOutcome::Pass { score: 0.8 });
    }
}
