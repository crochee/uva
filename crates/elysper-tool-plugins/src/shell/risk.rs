//! Command-level risk classification.
//!
//! Kept beside the tool rather than in the transport file: the classifier
//! is the security decision, and the argv/redirect/compound rules are
//! easier to audit in one focused place.

use elysper_tool::Risk;

use super::{classify_segment_risk, has_shell_control_syntax};

/// Classify a whole shell command line.
pub(crate) fn classify_command_risk(command: &str) -> Risk {
    let command = command.trim();
    if command.is_empty() {
        return Risk::Unknown;
    }
    // A redirect writes a file (or feeds one); command substitution and
    // backticks hide a second command from this analysis. Neither is
    // safe to assume read-only.
    if command.contains('>')
        || command.contains('<')
        || command.contains("$(")
        || command.contains('`')
    {
        return Risk::High;
    }
    let mut unknown = false;
    for segment in shell_segments(command) {
        match classify_segment_risk(segment) {
            Risk::High => return Risk::High,
            // A segment this analyser cannot resolve, in a compound
            // construct it also cannot resolve, is not a command to
            // run unapproved: fail closed.
            Risk::Unknown if has_shell_control_syntax(segment) => {
                return Risk::High;
            }
            Risk::Unknown => unknown = true,
            Risk::Low => {}
        }
    }
    if unknown { Risk::Unknown } else { Risk::Low }
}

// Split on the shell operators that start a new command. Every segment
// must be safe for the whole command to be safe.
fn shell_segments(command: &str) -> Vec<&str> {
    let bytes = command.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut idx = 0;
    while idx < bytes.len() {
        if matches!(bytes[idx], b';' | b'\n' | b'|' | b'&') {
            let segment = command[start..idx].trim();
            if !segment.is_empty() {
                segments.push(segment);
            }
            idx += 1;
            if idx < bytes.len() && matches!(bytes[idx], b'&' | b'|') {
                idx += 1;
            }
            start = idx;
        } else {
            idx += 1;
        }
    }
    let tail = command[start..].trim();
    if !tail.is_empty() {
        segments.push(tail);
    }
    segments
}
