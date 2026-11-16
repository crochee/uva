//! The fold's engine — one implementation of the surface's rules.
//!
//! The raw fold ([`crate::log_surface::fold_log_surface`]), the typed
//! fold ([`crate::surface::fold_surface`]), the incremental ledger
//! ([`crate::log_surface::SurfaceLedger`]) and the token meter all
//! grow their surface through [`SurfaceFold`], so they cannot drift
//! apart: the order they produce is the order the next LLM pass
//! sees, and a compaction's span means the same thing in each of
//! them.
//!
//! The engine holds three things: the rows, the ordinal each row was
//! written under, and the [`ModelOrder`] rule that decides where a
//! user turn lands while a tool call is unanswered.

use serde_json::Value;

use crate::{
    events::{ReplaceRange, SurfaceOp},
    surface::{FoldError, ModelOrder, validate_replace},
};

/// What kind of surface row an event is, and the tool calls it
/// names.
///
/// The calls are read off the payload, the same way the raw fold
/// reads them: the order's rule needs them to know which call is
/// still unanswered.
///
/// `None` for a log-only row: it contributes nothing to the
/// model-visible surface, so the fold has no kind to give it.
///
/// The match is exhaustive over the variants: a new variant does
/// not compile until it says which kind it is, so a row cannot
/// reach the model — or be kept from it — by accident. The kind
/// and the variant's declared
/// [`crate::events::SurfaceRole`] are held together by the
/// cross-check in `log_surface`'s tests: a surface-eligible row
/// carries a kind, a log-only row carries none.
pub(crate) fn row_view(
    event: &crate::events::SessionEvent,
    data: &Value,
) -> Option<(RowKind, Vec<String>)> {
    use crate::events::SessionEvent;

    let kind = match event {
        SessionEvent::UserMessage { .. } => Some(RowKind::User),
        SessionEvent::AssistantMessage { .. } => Some(RowKind::Assistant),
        SessionEvent::ToolResult { .. } => Some(RowKind::Tool),
        SessionEvent::Compaction { .. } => Some(RowKind::Compaction),
        // The log-only rows: they are written to the log and read
        // back as records of what happened. They never reach the
        // model-visible surface, so the fold has no kind for them.
        SessionEvent::AssistantChunk { .. }
        | SessionEvent::CompactionStart { .. }
        | SessionEvent::CompactionSummary { .. }
        | SessionEvent::CompactionEnd { .. }
        | SessionEvent::ToolCall { .. }
        | SessionEvent::ToolOutput { .. }
        | SessionEvent::Step { .. }
        | SessionEvent::Turn { .. }
        | SessionEvent::Iteration { .. }
        | SessionEvent::Warning { .. }
        | SessionEvent::SteeringGuard { .. }
        | SessionEvent::SteeringHint { .. }
        | SessionEvent::HookBlock { .. }
        | SessionEvent::SubagentEnter { .. }
        | SessionEvent::SubagentExit { .. }
        | SessionEvent::RequestHeader { .. }
        | SessionEvent::ApprovalAsked { .. }
        | SessionEvent::ApprovalDecided { .. }
        | SessionEvent::ApprovalGranted { .. }
        | SessionEvent::ToolDispatch { .. }
        | SessionEvent::RequestSnapshot { .. }
        | SessionEvent::Usage { .. }
        | SessionEvent::SandboxMode { .. }
        | SessionEvent::SessionShutdown { .. }
        | SessionEvent::ToolResultTransformed { .. }
        | SessionEvent::ParkedPrompt { .. }
        | SessionEvent::ParkConsumed { .. }
        | SessionEvent::Feedback { .. }
        | SessionEvent::Plugin { .. } => None,
    };
    let kind = kind?;
    let calls = calls_of(kind, data);
    Some((kind, calls))
}

/// The tool calls a row names.
///
/// An assistant turn asks for calls; a tool row answers them. The
/// calls are read off the row's payload, so every fold that reads the
/// same payload reads the same calls.
pub(crate) fn calls_of(kind: RowKind, payload: &Value) -> Vec<String> {
    match kind {
        RowKind::Assistant => tool_use_ids(payload),
        RowKind::Tool => tool_result_ids(payload),
        RowKind::User | RowKind::Compaction => Vec::new(),
    }
}

/// The tool calls an assistant payload asks for.
///
/// The blocks are read straight off the JSON, the way
/// [`crate::tool_pairing::collect_tool_use_ids`] reads them: the
/// payload's wire shape is the fold's contract, not the provider
/// type's.
pub(crate) fn tool_use_ids(payload: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    crate::tool_pairing::collect_tool_use_ids(payload, &mut ids);
    ids
}

/// The tool calls a tool payload answers.
///
/// A tool row names the call it answers twice: on the message's
/// `tool_call_id` and on the result part's `tool_use_id`. The set of
/// open calls does not care about the repetition.
pub(crate) fn tool_result_ids(payload: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    if let Some(id) = payload.get("tool_call_id").and_then(Value::as_str) {
        ids.push(id.to_string());
    }
    collect_tool_result_ids(payload, &mut ids);
    ids
}

/// Collect every tool-result block's call id, in the order the
/// blocks appear.
fn collect_tool_result_ids(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if matches!(
                map.get("type").and_then(Value::as_str),
                Some("tool_result")
            ) && let Some(id) =
                map.get("tool_use_id").and_then(Value::as_str)
            {
                out.push(id.to_string());
            }
            for nested in map.values() {
                collect_tool_result_ids(nested, out);
            }
        }
        Value::Array(items) => {
            for item in items {
                collect_tool_result_ids(item, out);
            }
        }
        _ => {}
    }
}

/// What a surface row is.
///
/// The kind decides how the row joins the surface: a user turn waits
/// for the tool result that answers an open call, an assistant turn
/// opens the calls it asks for, a tool result answers them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RowKind {
    /// A user turn.
    User,
    /// An assistant turn.
    Assistant,
    /// A tool result.
    Tool,
    /// A compaction summary.
    Compaction,
}

/// One surface-eligible row, normalised.
///
/// The payload is the caller's: the fold keeps the row's message, the
/// ledger its durable key, the token meter its price. The ordinal
/// and the tool calls travel beside it because the rules need them.
pub(crate) struct SurfaceRow<T> {
    /// The row's ordinal.
    pub(crate) seq: u64,
    /// What the row is.
    pub(crate) kind: RowKind,
    /// The row's payload.
    pub(crate) payload: T,
    /// The op the row asks for.
    pub(crate) op: SurfaceOp,
    /// The tool calls the row names: the calls an assistant turn
    /// asks for, the calls a tool result answers.
    pub(crate) calls: Vec<String>,
}

/// The surface under construction: the rows the next LLM pass sees.
///
/// The rows and their ordinals are index-parallel, exactly as
/// [`crate::surface::FoldedSurface`]'s two lists are.
#[derive(Clone, Debug, Default)]
pub(crate) struct SurfaceFold<T> {
    /// The rows, in the order the next LLM pass sees them.
    rows: Vec<T>,
    /// The ordinal each row was written under, index-parallel with
    /// `rows`.
    seqs: Vec<u64>,
    /// The order's rule: the open calls and the turns they hold back.
    order: ModelOrder<(T, u64)>,
    /// The highest ordinal on the surface.
    max_surface_seq: u64,
}

impl<T> SurfaceFold<T> {
    /// An empty surface.
    pub(crate) const fn new() -> Self {
        Self {
            rows: Vec::new(),
            seqs: Vec::new(),
            order: ModelOrder::new(),
            max_surface_seq: 0,
        }
    }

    /// The rows, in the order the next LLM pass sees them.
    pub(crate) fn rows(&self) -> &[T] {
        &self.rows
    }

    /// The ordinal each row was written under.
    pub(crate) fn seqs(&self) -> &[u64] {
        &self.seqs
    }

    /// The rows and their ordinals.
    pub(crate) fn into_parts(self) -> (Vec<T>, Vec<u64>) {
        (self.rows, self.seqs)
    }

    /// Apply one row.
    ///
    /// Returns the rows the op removed, so a caller that prices the
    /// surface can adjust its total.
    pub(crate) fn apply(
        &mut self,
        row: SurfaceRow<T>,
    ) -> Result<Vec<T>, FoldError> {
        let SurfaceRow {
            seq,
            kind,
            payload,
            op,
            calls,
        } = row;
        match op {
            SurfaceOp::AppendString(_) => {
                self.append(seq, kind, payload, calls);
                Ok(Vec::new())
            }
            SurfaceOp::Replace {
                start,
                end,
                source_event_seqs,
            } => self.replace(seq, payload, start, end, source_event_seqs),
        }
    }

    /// Release the held-back turns: the end of a surface.
    ///
    /// A turn whose call was never answered still belongs to the
    /// transcript — a dangling tool call is the repair path's
    /// problem, not the order's.
    pub(crate) fn finish(&mut self) {
        for (payload, seq) in self.order.release_all() {
            self.push(payload, seq);
        }
    }

    /// Append one row, applying the order's rule.
    fn append(
        &mut self,
        seq: u64,
        kind: RowKind,
        payload: T,
        calls: Vec<String>,
    ) {
        match kind {
            RowKind::User => {
                // A user turn waits for the tool result that answers
                // the open call: a provider rejects a transcript that
                // interleaves the two.
                if let Some((payload, seq)) = self.order.admit((payload, seq)) {
                    self.push(payload, seq);
                }
            }
            RowKind::Assistant => {
                self.push(payload, seq);
                self.order.open_calls(calls);
            }
            RowKind::Tool => {
                self.push(payload, seq);
                // The result comes before the turn it waited for.
                for (payload, seq) in self.order.answer_calls(calls) {
                    self.push(payload, seq);
                }
            }
            RowKind::Compaction => self.push(payload, seq),
        }
    }

    /// Replace a span of the surface with one row.
    fn replace(
        &mut self,
        seq: u64,
        payload: T,
        start: usize,
        end: usize,
        source_event_seqs: Vec<u64>,
    ) -> Result<Vec<T>, FoldError> {
        if self.order.is_holding() {
            return Err(FoldError::DeferredTurn { seq });
        }
        validate_replace(
            seq,
            ReplaceRange {
                start,
                end,
                source_event_seqs: &source_event_seqs,
            },
            &self.seqs,
            self.max_surface_seq,
        )?;
        let removed: Vec<T> = self.rows.splice(start..end, [payload]).collect();
        self.seqs.splice(start..end, [seq]);
        self.max_surface_seq = self.max_surface_seq.max(seq);
        Ok(removed)
    }

    /// Add one row to the surface.
    fn push(&mut self, payload: T, seq: u64) {
        self.rows.push(payload);
        self.seqs.push(seq);
        self.max_surface_seq = self.max_surface_seq.max(seq);
    }
}
