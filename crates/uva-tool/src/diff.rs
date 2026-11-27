//! The file change a tool reports.
//!
//! A tool that changes a file says **what** it changed: the path, the
//! text that was there, and the text that is there now. The report
//! rides the tool result's metadata, not its content — the model sees
//! the same answer either way, and the *client* (an editor rendering a
//! diff, a headless consumer printing one) gets the change as data.
//!
//! # Why the report is metadata
//!
//! The model-facing text of a file tool is a sentence ("Replaced the
//! text on line 12 of '/srv/app.rs'."). The change itself is not text
//! for the model — it is text for the *user*. Putting it in the
//! content would spend the context window on bytes the model does not
//! need; putting it in the metadata keeps the model's view unchanged
//! and makes the change machine-readable.
//!
//! # The shape is the protocol's shape
//!
//! The members are the ACP `Diff` content block's members
//! (`path`, `old_text`, `new_text`): the client that renders diffs
//! needs no translation, and a deployment's own consumer reads the
//! same three members the protocol names.
//!
//! # The report is bounded
//!
//! A file's whole text, twice, is a lot of bytes to carry in a live
//! frame. [`MAX_DIFF_TEXT_BYTES`] caps each side; a change whose text
//! does not fit is reported as **truncated** — and a truncated diff is
//! not a diff (it would show changes that are not there), so the
//! consumer shows the summary instead of a wrong picture.

use serde::{Deserialize, Serialize};

use crate::types::ToolOutput;

/// The metadata key under which a tool reports the file change it
/// made.
///
/// The key is reserved: a tool that changes a file writes its report
/// here, and a consumer reads it from here.
pub const DIFF_METADATA_KEY: &str = "diff";

/// The most bytes one side of a reported change may carry.
///
/// A file's whole text, twice, is a lot of bytes to carry in a live
/// frame; 128 KiB a side covers ordinary source files and keeps the
/// frame bounded.
pub const MAX_DIFF_TEXT_BYTES: usize = 128 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// One file change a tool reports.
///
/// `old_text` is `None` for a file that did not exist before the
/// change; `new_text` is the text the file holds now. A **truncated**
/// report carries no text at all — see [`FileDiff::new`].
pub struct FileDiff {
    /// The absolute path of the file that changed.
    pub path: String,
    /// The text the file held before the change, if it held any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    /// The text the file holds after the change.
    pub new_text: String,
    /// Whether the text did not fit [`MAX_DIFF_TEXT_BYTES`].
    ///
    /// A truncated report is **not** a diff: the text it would show
    /// is not the file's text, so a consumer shows the tool's own
    /// summary instead of a picture that is not true.
    #[serde(default)]
    pub truncated: bool,
}

impl FileDiff {
    #[must_use]
    /// The report for a change, cut to fit the cap.
    ///
    /// A side longer than [`MAX_DIFF_TEXT_BYTES`] does not fit: the
    /// report is then marked [`truncated`](Self::truncated) and
    /// carries no text — a half-diff is worse than no diff.
    pub fn new(
        path: impl Into<String>,
        old_text: Option<String>,
        new_text: impl Into<String>,
    ) -> Self {
        let new_text = new_text.into();
        let old_fits = old_text
            .as_ref()
            .is_none_or(|text| text.len() <= MAX_DIFF_TEXT_BYTES);
        let new_fits = new_text.len() <= MAX_DIFF_TEXT_BYTES;
        if old_fits && new_fits {
            return Self {
                path: path.into(),
                old_text,
                new_text,
                truncated: false,
            };
        }
        Self {
            path: path.into(),
            old_text: None,
            new_text: String::new(),
            truncated: true,
        }
    }

    #[must_use]
    /// The report as the metadata value a tool result carries.
    pub fn to_metadata(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    #[must_use]
    /// The report a tool result carries, if it carries one.
    pub fn from_output(output: &ToolOutput) -> Option<Self> {
        let value = output.metadata.get(DIFF_METADATA_KEY)?;
        serde_json::from_value(value.clone()).ok()
    }

    #[must_use]
    /// The report a tool result's metadata carries, if it carries one.
    ///
    /// The metadata map is the wire's shape of the report: the tool
    /// result carries it, and every consumer reads it from there.
    pub fn from_metadata(
        metadata: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<Self> {
        let value = metadata.get(DIFF_METADATA_KEY)?;
        serde_json::from_value(value.clone()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_reports_both_sides() {
        let diff = FileDiff::new("/srv/app.rs", Some("old".into()), "new");
        assert_eq!(diff.path, "/srv/app.rs");
        assert_eq!(diff.old_text.as_deref(), Some("old"));
        assert_eq!(diff.new_text, "new");
        assert!(!diff.truncated);
    }

    #[test]
    fn a_new_file_has_no_old_text() {
        let diff = FileDiff::new("/srv/app.rs", None, "new");
        assert!(diff.old_text.is_none());
        assert!(!diff.truncated);
    }

    #[test]
    fn a_change_too_big_to_carry_is_not_a_diff() {
        let big = "x".repeat(MAX_DIFF_TEXT_BYTES + 1);
        let diff = FileDiff::new("/srv/app.rs", Some(big.clone()), big);
        assert!(diff.truncated);
        assert!(
            diff.old_text.is_none() && diff.new_text.is_empty(),
            "a truncated report carries no text"
        );
    }

    #[test]
    fn the_report_rides_the_output_metadata() {
        let diff = FileDiff::new("/srv/app.rs", Some("old".into()), "new");
        let output = ToolOutput::text("Replaced the text.")
            .with_metadata(DIFF_METADATA_KEY, diff.to_metadata());
        let read = FileDiff::from_output(&output).expect("the report is there");
        assert_eq!(read, diff);
    }

    #[test]
    fn an_output_without_a_report_says_so() {
        assert!(FileDiff::from_output(&ToolOutput::text("done")).is_none());
    }
}
