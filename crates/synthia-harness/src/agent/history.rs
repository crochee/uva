//! The loop's read end of the session log: [`HistoryProjection`]
//! caches the model history [`synthia_session::derive_history`]
//! produced for the rows the loop last saw.
//!
//! A run with a [`LogWriter`](synthia_session::LogWriter) does not own
//! its transcript: the log does. `RunState.messages` is that
//! projection plus the request-local seams (the runtime-context
//! snapshot, hints, the context manager's prune), and the step
//! boundary re-derives the projection whenever the log's tail moved.
//!
//! The watermark is `(rows.len(), tail ordinal)`, numbered by
//! [`synthia_session::log_tail_seq`] — the fold's own numbering. The
//! count catches a row appended without an advancing ordinal (an old
//! or unstamped writer); the ordinal catches a rewrite in place.
//! Deriving is skipped while the watermark is unchanged, so a step
//! that wrote only log-only rows (a `step_start`, a usage row) pays
//! one row scan, not a fold.

use serde_json::Value;
use synthia_provider::Message;
use synthia_session::{HistoryError, derive_history, log_tail_seq};

/// The model history a session log derives, cached by log tail.
pub(crate) struct HistoryProjection {
    // `(row count, tail ordinal)` of the log the messages were
    // derived from; `(0, 0)` for an empty log.
    watermark: (usize, u64),
    messages: Vec<Message>,
}

impl HistoryProjection {
    /// Derive the projection from the whole log.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError`] when the log does not fold into a
    /// model surface.
    pub(crate) fn from_rows(rows: &[Value]) -> Result<Self, HistoryError> {
        Ok(Self {
            watermark: (rows.len(), log_tail_seq(rows)),
            messages: derive_history(rows)?,
        })
    }

    /// Re-derive when the log's tail moved; leaves the projection
    /// untouched and returns `false` when it did not.
    ///
    /// Returns `true` when the message list changed, not merely when
    /// a row was appended: a log-only row moves the tail but not the
    /// transcript, and rebuilding `RunState.messages` for it would
    /// throw away the request-local seams for nothing.
    ///
    /// A failed derive leaves the watermark where it was, so the next
    /// boundary retries the same rows instead of skipping them.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError`] when the log no longer folds into a
    /// model surface.
    pub(crate) fn refresh(
        &mut self,
        rows: &[Value],
    ) -> Result<bool, HistoryError> {
        let watermark = (rows.len(), log_tail_seq(rows));
        if watermark == self.watermark {
            return Ok(false);
        }
        let messages = derive_history(rows)?;
        let changed = messages != self.messages;
        self.messages = messages;
        self.watermark = watermark;
        Ok(changed)
    }

    pub(crate) fn messages(&self) -> &[Message] {
        &self.messages
    }

    /// The ordinal of the log's last row the messages were derived
    /// from.
    pub(crate) fn watermark(&self) -> u64 {
        self.watermark.1
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn user_row(seq: u64, text: &str) -> Value {
        json!({
            "type": "user_message",
            "seq": seq,
            "data": Message::user(text),
        })
    }

    fn log_row(seq: u64) -> Value {
        json!({"type": "step_start", "seq": seq, "data": {}})
    }

    #[test]
    fn from_rows_projects_the_whole_log() {
        let rows = [user_row(1, "hello")];
        let projection = HistoryProjection::from_rows(&rows).expect("derive");
        assert_eq!(projection.messages(), [Message::user("hello")]);
        assert_eq!(projection.watermark(), 1);
    }

    #[test]
    fn refresh_reports_a_transcript_change_only() {
        let mut rows = vec![user_row(1, "hello")];
        let mut projection =
            HistoryProjection::from_rows(&rows).expect("derive");

        // A log-only row moves the tail, not the transcript.
        rows.push(log_row(2));
        assert!(!projection.refresh(&rows).expect("refresh"));
        assert_eq!(projection.watermark(), 2);

        // A new message changes the transcript.
        rows.push(user_row(3, "again"));
        assert!(projection.refresh(&rows).expect("refresh"));
        assert_eq!(
            projection.messages(),
            [Message::user("hello"), Message::user("again")]
        );
        assert_eq!(projection.watermark(), 3);
    }

    fn bad_compaction(seq: u64) -> Value {
        serde_json::to_value(synthia_session::SessionEvent::Compaction {
            seq,
            ts: "2026-10-25T00:00:03Z".into(),
            data: json!({"summary": "s"}),
            surface_op: synthia_session::SurfaceOp::Replace {
                start: 0,
                end: 1,
                source_event_seqs: vec![9],
            },
        })
        .expect("compaction row serializes")
    }

    #[test]
    fn a_failed_refresh_keeps_the_watermark() {
        let mut rows = vec![user_row(1, "hello")];
        let mut projection =
            HistoryProjection::from_rows(&rows).expect("derive");
        rows.push(bad_compaction(2));
        assert!(
            projection.refresh(&rows).is_err(),
            "a rewrite the fold cannot prove must surface"
        );
        assert!(
            projection.refresh(&rows).is_err(),
            "a failed derive must not advance the watermark; the rows are retried"
        );
    }

    #[test]
    fn the_metadata_header_is_not_a_row() {
        let header =
            json!({"_meta": {"kind": "synthia_session_meta", "schema": 1}});
        let rows = vec![
            header,
            json!({"type": "user_message", "data": Message::user("hello")}),
        ];
        let projection = HistoryProjection::from_rows(&rows).expect("derive");
        assert_eq!(projection.messages(), [Message::user("hello")]);
        assert_eq!(
            projection.watermark(),
            1,
            "the header must not consume an ordinal"
        );
    }

    #[test]
    fn refresh_of_an_unchanged_tail_is_free() {
        let rows = [user_row(1, "hello")];
        let mut projection =
            HistoryProjection::from_rows(&rows).expect("derive");
        assert!(!projection.refresh(&rows).expect("refresh"));
        assert_eq!(projection.messages(), [Message::user("hello")]);
    }
}
