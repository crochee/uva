//! Where in the agent loop a safety scan applies, and how severe a
//! match is when one is found.
//!
//! [`ScanStage`] mirrors the four cross-cycles in the ReAct loop the
//! harness consults an installed [`SafetyScanner`] at:
//!
//! | Stage | Where it fires |
//! |---|---|
//! | [`Input`](ScanStage::Input) | Before the user's prompt enters the context manager (`prepare_user_messages`) |
//! | [`ToolCall`](ScanStage::ToolCall) | Before a model-emitted tool call dispatches (`execute_tool_inner`) |
//! | [`ToolOutput`](ScanStage::ToolOutput) | Before a tool's result joins the assistant turn (`commit_all_results`) |
//! | [`Output`](ScanStage::Output) | After the assistant turn is committed (`commit_assistant`) |
//!
//! [`Severity`] is the model's own ladder — the harness's gating
//! thresholds (`>= High` short-circuits; `== Critical` triggers an
//! `[REDACTED:…]` rewrite) are documented at the call sites in
//! `synthia-harness`, not here, so adding a new severity in this crate
//! is a pure extension.
//!
//! Both enums carry an `ALL` const array — zero-allocation iteration
//! for the hot match-dispatch path inside
//! [`crate::composite::CompositeScanner::scan`].

use serde::{Deserialize, Serialize};

/// Where in the agent loop a safety scan applies.
///
/// Order is stable (alphabetic on the variant name when re-serialised)
/// — the wire format pinned by `SystemEvent::PolicyViolation` mirrors
/// these four tags. Adding a new variant is a wire-compatible change as
/// long as consumers handle the unknown stage as "informational, log
/// and continue".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStage {
    /// The user's prompt, before it joins the LLM call.
    Input,
    /// A model-emitted tool call (`tool_name`, `input`), before the
    /// tool dispatches.
    ToolCall,
    /// A tool's textual output, before the assistant turn is
    /// committed.
    ToolOutput,
    /// The assistant's own turn text, after streaming completes and
    /// before it joins the next LLM call's history.
    Output,
}

impl ScanStage {
    /// All four stages. Iteration is zero-allocation (`&'static [Self; 4]`).
    pub const ALL: [Self; 4] =
        [Self::Input, Self::ToolCall, Self::ToolOutput, Self::Output];

    /// Stable lowercase tag for log queries and metrics — matches
    /// the `serde(rename_all = "snake_case")` wire form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Input => "input",
            Self::ToolCall => "tool_call",
            Self::ToolOutput => "tool_output",
            Self::Output => "output",
        }
    }
}

/// How severe a [`crate::match_::InjectionMatch`] is.
///
/// The harness's policy thresholds, documented at the call sites
/// (`synthia-harness/src/agent/re_act/loop/sample.rs`), read:
///
/// | Severity | Harness behaviour |
/// |---|---|
/// | `Low` | Informational; `tracing::warn!` + `SpanKind::Guardrail` span; the run continues. |
/// | `Medium` | Same — informational. |
/// | `High` | Input / tool-call stage: run ends with `SessionEndReason::Refused(violation)`; tool-output / output stage: redaction. |
/// | `Critical` | Output stage replaces the assistant message with `[REDACTED: prompt-injection pattern "{name}"]`. |
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// Informational only; never short-circuits the run.
    Low,
    /// Informational only; never short-circuits the run.
    Medium,
    /// Input / tool-call stages end the run; output stages redact.
    High,
    /// Assistant output is rewritten to a `[REDACTED:…]` placeholder.
    Critical,
}

impl Severity {
    /// All four severities, ordered low → critical.
    pub const ALL: [Self; 4] =
        [Self::Low, Self::Medium, Self::High, Self::Critical];

    /// Stable lowercase tag — matches `serde(rename_all = "snake_case")`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Critical => "critical",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `as_str()` and `serde` must agree — the dashboard filter reads
    /// by the lowercase tag, so a divergence would silently drop rows.
    #[test]
    fn stage_as_str_matches_serde() {
        for stage in ScanStage::ALL {
            let json = serde_json::to_string(&stage).unwrap();
            // serde writes `{"…":null}` for a unit-only enum; the
            // value sits at the index 0.
            let inner: String = serde_json::from_str(&json).unwrap();
            assert_eq!(inner, stage.as_str(), "mismatch for {stage:?}");
        }
    }

    /// Severity ordering is the policy's basis — swapping two values
    /// here changes which patterns end the run.
    #[test]
    fn severity_ordering_pinned() {
        assert!(Severity::Low < Severity::Medium);
        assert!(Severity::Medium < Severity::High);
        assert!(Severity::High < Severity::Critical);
    }

    /// `as_str()` and `serde` must agree — see [`ScanStage`] test.
    #[test]
    fn severity_as_str_matches_serde() {
        for sev in Severity::ALL {
            let json = serde_json::to_string(&sev).unwrap();
            let inner: String = serde_json::from_str(&json).unwrap();
            assert_eq!(inner, sev.as_str(), "mismatch for {sev:?}");
        }
    }

    /// `ALL` arrays must be exhaustive; pin the array length at 4.
    #[test]
    fn stage_all_length_pinned() {
        assert_eq!(ScanStage::ALL.len(), 4);
        assert_eq!(Severity::ALL.len(), 4);
    }
}
