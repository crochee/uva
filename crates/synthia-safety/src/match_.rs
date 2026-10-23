//! [`InjectionMatch`] — one detection by a [`crate::scanner::SafetyScanner`].
//!
//! A single scan may produce multiple matches in one `text`; a
//! [`CompositeScanner`] can fold matches from many scanners. The
//! caller (the loop) flattens them and decides per-stage policy.

use serde::{Deserialize, Serialize};

use crate::stage::{ScanStage, Severity};

/// One detection returned by a [`crate::scanner::SafetyScanner::scan`].
///
/// Wire fields:
///
/// | Field | Wire type | Notes |
/// |---|---|---|
/// | `stage` | `ScanStage` | The stage the match was found in. A scanner installed on multiple stages returns the same match per stage. |
/// | `severity` | `Severity` | `Low` / `Medium` / `High` / `Critical` (ordered). |
/// | `pattern` | `String` | The scanner-defined pattern name (the `name` field, not the regex source). Wire-stable; the harness's log filter keys off this. |
/// | `start` | `usize` | **Byte** offset of the match's first character in the scanned text. |
/// | `end` | `usize` | **Byte** offset one past the match's last character. |
///
/// `start` / `end` are **byte** offsets (not char), matching
/// `regex::Match::start()` / `end()` — the loop's display path never has
/// to think about Unicode-segmentation rules. A consumer that wants
/// human-readable character positions calls
/// `text.is_char_boundary(idx)` first to validate the index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InjectionMatch {
    /// The stage the match was found in.
    pub stage: ScanStage,
    /// Match severity. Ordered: `Low < Medium < High < Critical`.
    pub severity: Severity,
    /// Pattern name from the scanner (e.g. `"system_prompt_leak_ignore_previous"`).
    pub pattern: String,
    /// Byte offset of the match's first character.
    pub start: usize,
    /// Byte offset one past the match's last character.
    pub end: usize,
}

impl InjectionMatch {
    /// Convenience constructor the scanners call after they find a
    /// `regex::Match` in `haystack` at `stage`. `pattern` is the
    /// scanner's name (a stable identifier, not the regex itself).
    #[must_use]
    pub fn new(
        stage: ScanStage,
        severity: Severity,
        pattern: impl Into<String>,
        start: usize,
        end: usize,
    ) -> Self {
        Self {
            stage,
            severity,
            pattern: pattern.into(),
            start,
            end,
        }
    }

    /// Byte slice of the match from `haystack`. Returns `None` if the
    /// stored byte offsets are not on `haystack`'s char boundaries
    /// (truncation / on-purpose fuzz).
    #[must_use]
    pub fn slice<'t>(&self, haystack: &'t str) -> Option<&'t str> {
        if !haystack.is_char_boundary(self.start)
            || !haystack.is_char_boundary(self.end)
            || self.end < self.start
            || self.end > haystack.len()
        {
            return None;
        }
        haystack.get(self.start..self.end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Byte offsets must agree with `regex::Match::start/end()` so the
    /// loop can re-use them — pin via slice equality.
    #[test]
    fn byte_offsets_match_haystack() {
        let m = InjectionMatch::new(
            ScanStage::Input,
            Severity::High,
            "p",
            7usize,
            12usize,
        );
        assert_eq!(m.slice("ignore previous instructions"), Some("previ"));
    }

    /// Off-by-one: `end` is exclusive (matches `regex::Match::end()`).
    #[test]
    fn end_is_exclusive() {
        let m = InjectionMatch::new(ScanStage::Input, Severity::Low, "p", 0, 5);
        // 0..=4 inclusive → first five chars: "abcde".
        assert_eq!(m.slice("abcdef"), Some("abcde"));
        // Same start, end one past the last char → empty.
        let m = InjectionMatch::new(ScanStage::Input, Severity::Low, "p", 3, 3);
        assert_eq!(m.slice("abcdef"), Some(""));
    }

    /// Char-boundary sanity — fuzzy byte indices yield `None`.
    #[test]
    fn bad_byte_offsets_return_none() {
        // Index 1 is inside the 'é' (2 bytes in UTF-8) — not a boundary.
        let text = "aéb";
        let m = InjectionMatch::new(ScanStage::Input, Severity::Low, "p", 1, 2);
        assert!(m.slice(text).is_none());
    }

    /// Round-trip serialisation — the wire is `serde_json` (the harness
    /// emits `SystemEvent::PolicyViolation { …, pattern }` via the
    /// same derive).
    #[test]
    fn serde_round_trip() {
        let m = InjectionMatch::new(
            ScanStage::ToolCall,
            Severity::Critical,
            "tool_arg_spoof_rm_rf",
            9usize,
            19usize,
        );
        let json = serde_json::to_string(&m).unwrap();
        let back: InjectionMatch = serde_json::from_str(&json).unwrap();
        assert_eq!(back, m);
    }
}
