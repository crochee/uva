//! [`SafetyScanner`] — the async trait every detection backend implements.
//!
//! [`RegexInjectionScanner`](crate::regex::RegexInjectionScanner) is the
//! shipped default; a downstream can wrap a network-backed model
//! grader, a token-level hallucination detector, or a vendor's own
//! classifier behind the same trait without touching the loop.

use async_trait::async_trait;

use crate::{match_::InjectionMatch, stage::ScanStage};

/// A plug-in detector the harness consults at four distinct
/// points in the agent loop.
///
/// The trait is `async` to make room for a network-backed model grader
/// without breaking the call sites — a regex scanner's body is fully
/// synchronous inside and returns the `Vec` via
/// `futures::future::ready(…)`, so there is no allocation or `.await`
/// cost on the hot loop.
///
/// # Contract
///
/// - `scan` is **pure with respect to `&self`**: two calls with the
///   same `(stage, text)` return equal vectors. Backends that hold
///   mutable state across calls must serialise that state behind an
///   internal lock; the harness calls `scan` from many iterations of
///   the run, possibly in parallel, and a buggy scanner that mutates
///   its own state would produce flaky output.
/// - `name` is a stable, all-lowercase identifier used by logs and
///   metrics. Two scanners that report the same name are
///   indistinguishable downstream — pick unique names per backend.
/// - The `Vec<InjectionMatch>` may be empty; an empty result is the
///   common case (no detection). Backends should avoid allocating a
///   fresh `Vec` per match in the hot path; pre-allocate when the
///   match count is bounded.
#[async_trait]
pub trait SafetyScanner: Send + Sync {
    /// Stable, all-lowercase identifier — used by log filters and
    /// metrics to attribute detections to a backend.
    fn name(&self) -> &str;

    /// Inspect `text` (already extracted to a `&str`) at `stage` and
    /// return every detection.
    ///
    /// `text` is the textual projection of the input. For
    /// [`ToolCall`](ScanStage::ToolCall) it is the tool's name + the
    /// JSON-serialised `input`; for [`ToolOutput`](ScanStage::ToolOutput)
    /// it is the joined text parts; for
    /// [`Input`](ScanStage::Input) / [`Output`](ScanStage::Output)
    /// it is the raw prompt / answer text.
    async fn scan(&self, stage: ScanStage, text: &str) -> Vec<InjectionMatch>;
}
