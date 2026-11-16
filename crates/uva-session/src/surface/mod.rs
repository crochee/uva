//! Surface fold — replay a [`SessionEvent`] log into the
//! ordered list of model-visible messages.
//!
//! Mirrors `deepseek-harness`'s `core/session/src/surface.ts`:
//! `fold_surface` is a pure function that walks the typed event
//! log and emits a `Vec<serde_json::Value>` in the order the next
//! LLM pass sees. It validates `SurfaceOp::Replace`
//! `source_event_seqs` against the previous-log prefix so an
//! out-of-band rewrite cannot shadow an event the model has
//! never seen — and against the surface's own rows, so a range
//! that names the right rows in the wrong coordinates is refused
//! instead of shadowing the wrong ones.
//!
//! The module also carries [`assemble_chunks`], the join that
//! rebuilds a turn's assistant text from the log-only
//! `AssistantChunk` stream records, and [`ModelOrder`], the rule
//! that decides the order: a user turn that arrives while a tool
//! call is unanswered waits for the result that answers it.
//!
//! ## Invariants
//!
//! 1. Only the events whose declared [`SessionEvent::surface_role`]
//!    is [`crate::events::SurfaceRole::Message`] or
//!    [`crate::events::SurfaceRole::OpOnly`] project to the surface —
//!    `UserMessage`, `AssistantMessage`, `ToolResult` and
//!    `Compaction`. Log-only events (`ToolCall`, `Step`, `Turn`,
//!    `Iteration`, `Warning`, etc.) never appear in the output.
//! 2. `surface_op = Append` extends the tail by one message.
//! 3. `surface_op = Replace { start, end, source_event_seqs }`
//!    atomically swaps `surface[start..end]` with the new
//!    message, provided every `source_event_seqs` entry is
//!    `< self.seq` AND `<= previously_max_seq`. A provenance
//!    violation aborts the fold with `FoldError::BadProvenance`.
//! 4. `surface_op = None` (legacy / log-only user messages) is
//!    treated as `Append` for forward compatibility — older
//!    callers did not stamp a surface op on every event.
//! 5. `Compaction` always carries a `Replace`; it is impossible
//!    for an agent loop to log a `Compaction` without a replace
//!    (the deserialiser rejects it, see
//!    `events::tests::compaction_event_requires_surface_op`).
//! 6. The surface is in the order the next LLM pass sees: a user
//!    turn that arrives while a tool call is unanswered waits for
//!    the result that answers it ([`ModelOrder`]). A compaction's
//!    `start` / `end` are positions in *that* order.
//! 7. A `Replace` names the rows it shadows: `source_event_seqs[i]`
//!    is the seq of the surface row at `start + i`. A mismatch is
//!    [`FoldError::SpanMismatch`].
//! 8. A `system` row is a user-kind row: it follows the user row's
//!    deferral rule, so a system row that lands between a `tool_use`
//!    and its `tool_result` waits for the result that answers it.
//!    The rule is stated on the row's *kind*, which every fold reads
//!    the same way, so the typed fold and the raw fold cannot answer
//!    differently about a system row.

use serde_json::Value;

use crate::events::{ReplaceRange, SessionEvent, SurfaceOp};

pub(crate) mod fold;

use fold::{SurfaceFold, SurfaceRow};

/// Error category returned by [`fold_surface`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FoldError {
    /// `SurfaceOp::Replace` cited a `source_event_seqs` entry
    /// that does not satisfy `< self.seq && <=
    /// previous_max_seq`. The fold is aborted at the offending
    /// event and the error names the citation, so the caller can
    /// decide whether to surface a corruption warning to the model
    /// or to fall back to the lenient fold.
    #[error("event {seq} cites source seq {bad_seq}, which must be < {seq}")]
    BadProvenance {
        /// Seq of the offending event.
        seq: u64,
        /// Offending `source_event_seqs` entry.
        bad_seq: u64,
    },
    /// `SurfaceOp::Replace` carried a `[start, end)` range that
    /// does not fit inside the current surface length.
    #[error(
        "event {seq} cites range [{start}, {end}) past surface length {surface_len}"
    )]
    RangeOutOfBounds {
        /// Seq of the offending event.
        seq: u64,
        /// Inclusive start index reported by the op.
        start: usize,
        /// Exclusive end index reported by the op.
        end: usize,
        /// Current surface length at the time of the violation.
        surface_len: usize,
    },
    /// `SurfaceOp::Replace` cited a number of seqs that does not
    /// match the width of the span it replaces.
    ///
    /// The span and the provenance are two descriptions of the same
    /// rows: a span of `n` rows is described by `n` seqs. A
    /// citation of a different width cannot be tied to the rows it
    /// names.
    #[error("event {seq} replaces {width} surface rows but cites {cited} seqs")]
    CitationCount {
        /// Seq of the offending event.
        seq: u64,
        /// Width of the span the op replaces.
        width: usize,
        /// Number of seqs the op cites.
        cited: usize,
    },
    /// `SurfaceOp::Replace` cited a seq that is not the seq of the
    /// surface row at the same offset.
    ///
    /// The range and the provenance disagree: the op names the right
    /// rows in the wrong coordinates, so applying it would shadow
    /// rows it does not cite.
    #[error(
        "event {seq} cites seq {cited} at offset {offset}, but surface row {row} carries seq {expected}"
    )]
    SpanMismatch {
        /// Seq of the offending event.
        seq: u64,
        /// Offset of the citation inside the span.
        offset: usize,
        /// Index of the surface row the citation names.
        row: usize,
        /// The seq the op cited.
        cited: u64,
        /// The seq the surface row actually carries.
        expected: u64,
    },
    /// A `compaction` row met a user turn the fold is still holding
    /// back.
    ///
    /// The surface is not final yet — the held turn joins it when the
    /// open call is answered — so the compaction's range does not
    /// mean what it says. The compaction is refused.
    #[error(
        "event {seq} meets a held-back user turn; the compaction is refused"
    )]
    DeferredTurn {
        /// Seq of the offending event.
        seq: u64,
    },
}

/// Result of [`fold_surface`].
#[derive(Debug, Clone)]
pub struct FoldedSurface {
    /// Messages in the order the next LLM pass sees them. Each
    /// entry is a `serde_json::Value` shaped like
    /// `uva_provider::Message` (the caller projects).
    ///
    /// A user turn that arrived while a tool call was unanswered
    /// waits for the result that answers it: the order is the one
    /// the model sees, not the one the log was written in.
    pub messages: Vec<Value>,
    /// Seqs of the events that produced each surface message.
    /// Parallel to `messages`: `surface_seqs[i]` is the seq of
    /// the event whose `data` payload is in `messages[i]`.
    pub surface_seqs: Vec<u64>,
}

impl FoldedSurface {
    /// Empty fold.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            messages: Vec::new(),
            surface_seqs: Vec::new(),
        }
    }

    /// True when no surface messages were produced.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.messages.is_empty()
    }

    /// Number of surface messages.
    #[must_use]
    pub fn len(&self) -> usize {
        self.messages.len()
    }
}

/// Fold a complete session event log into the model-visible
/// surface.
///
/// Walks `events` in order, applying each surface-eligible
/// variant to `surface` according to its `surface_op`. Log-only
/// variants are ignored. Returns the surface on success or a
/// [`FoldError`] on the first provenance violation.
///
/// # Provenance rule
///
/// `source_event_seqs` for a `Replace` MUST satisfy:
///
/// - `every entry < self.seq` (a replace cannot cite its own
///   seq),
/// - `every entry <= max_seq_of_previous_surface_events` (a
///   replace cannot shadow an event that has not been appended
///   yet), and
/// - `every entry` is the seq of the surface row at the same
///   offset of the replaced span (the citation names the rows the
///   span covers).
///
/// Violations are surface-visible failures: a `SummarizingContextManager`
/// that emits a bad `Compaction` will be rejected at fold time
/// rather than silently corrupting the model-visible history.
pub fn fold_surface(
    events: &[SessionEvent],
) -> Result<FoldedSurface, FoldError> {
    let mut fold = SurfaceFold::<Value>::new();
    for event in events {
        // The role says what this row does on the surface, and the
        // predicate reads the role: a log-only row never reaches it,
        // so the fold keeps no second list of the same fact.
        if !event.is_surface_eligible() {
            continue;
        }
        let Some(data) = event.surface_payload() else {
            // A surface-eligible row that carries no payload: the
            // fold cannot place it, so it says so instead of
            // folding an empty message.
            tracing::warn!(
                target: "uva.session",
                seq = event.seq(),
                "fold_surface: a surface-eligible row carries no payload; skipped"
            );
            continue;
        };
        let seq = event.seq();
        // `None` is treated as `Append` for forward compat —
        // older sessions did not stamp a surface op on every
        // message.
        let op = event
            .surface_op()
            .cloned()
            .unwrap_or_else(SurfaceOp::append);
        let data = data.clone();
        let Some((kind, calls)) = fold::row_view(event, &data) else {
            // A surface-eligible row this fold cannot place: it is
            // skipped with a warning rather than folded as an empty
            // message, so the model never sees a row nobody meant
            // to show it.
            tracing::warn!(
                target: "uva.session",
                seq,
                "fold_surface: a surface-eligible row has no place on the surface; skipped"
            );
            continue;
        };
        fold.apply(SurfaceRow {
            seq,
            kind,
            payload: data,
            op,
            calls,
        })?;
    }
    fold.finish();
    let (messages, surface_seqs) = fold.into_parts();
    Ok(FoldedSurface {
        messages,
        surface_seqs,
    })
}

/// Join a run of [`SessionEvent::AssistantChunk`] events into
/// the assembled assistant text.
///
/// Chunks are ordered by `chunk_seq` ascending — the emitter's
/// monotonic per-turn ordinal — so the log order of `events`
/// does not matter. `data` carries `{"delta": "<text>"}`;
/// non-text chunks (tool-call deltas, usage frames, …)
/// contribute nothing.
///
/// Returns the concatenated text, empty when no chunk carries a
/// textual delta. Pure: no I/O, no shared state.
#[must_use]
pub fn assemble_chunks(events: &[SessionEvent]) -> String {
    let mut chunks: Vec<(u64, &str)> = events
        .iter()
        .filter_map(|event| match event {
            SessionEvent::AssistantChunk {
                chunk_seq, data, ..
            } => {
                let delta = data.get("delta").and_then(Value::as_str)?;
                Some((*chunk_seq, delta))
            }
            _ => None,
        })
        .collect();
    chunks.sort_by_key(|(chunk_seq, _)| *chunk_seq);
    let mut text = String::new();
    for (_, delta) in chunks {
        text.push_str(delta);
    }
    text
}

/// The order the next LLM pass sees.
///
/// A user turn that arrives while a tool call is unanswered waits for
/// the result that answers it: every provider rejects a transcript
/// that interleaves a user turn between a `tool_use` and its
/// `tool_result`. The rule lives here — one implementation — because
/// the durable fold and the live transcript must produce the *same*
/// order. A compaction's span is a position in that order, and a
/// span that names one order while the log holds another shadows the
/// wrong rows.
#[derive(Clone, Debug, Default)]
/// `T` is the row the caller holds: the durable fold keeps a payload
/// and its ordinal, the live transcript keeps a message.
pub struct ModelOrder<T> {
    /// The tool calls an assistant turn asked for that no tool result
    /// has answered yet.
    ///
    /// A handful at most: a call stays open only until its result
    /// lands, so the list is short enough to scan.
    pending_tool_calls: Vec<String>,
    /// The user turns that arrived while a call was pending.
    held: Vec<T>,
}

impl<T> ModelOrder<T> {
    #[must_use]
    /// An order with no rows.
    pub const fn new() -> Self {
        Self {
            pending_tool_calls: Vec::new(),
            held: Vec::new(),
        }
    }

    #[must_use]
    /// Whether a user turn that arrives now has to wait.
    ///
    /// True while a tool call is unanswered: the turn would land
    /// between the call and the result that answers it.
    pub fn holds_back(&self) -> bool {
        !self.pending_tool_calls.is_empty()
    }

    /// A user turn that arrives now.
    ///
    /// `Some(turn)` when the turn may join the surface now; `None`
    /// when it has to wait for the tool result that answers the open
    /// call — the turn is held, and [`Self::answer_calls`] or
    /// [`Self::release_all`] hands it back.
    pub fn admit(&mut self, turn: T) -> Option<T> {
        if self.holds_back() {
            self.held.push(turn);
            None
        } else {
            Some(turn)
        }
    }

    /// An assistant turn asked for these calls.
    pub fn open_calls(&mut self, ids: impl IntoIterator<Item = String>) {
        for id in ids {
            if !self.pending_tool_calls.contains(&id) {
                self.pending_tool_calls.push(id);
            }
        }
    }

    /// A tool row answered these calls.
    ///
    /// The held turns are released when the last open call is
    /// answered: the result comes before the turn it waited for.
    pub fn answer_calls(
        &mut self,
        ids: impl IntoIterator<Item = String>,
    ) -> Vec<T> {
        for id in ids {
            if let Some(index) =
                self.pending_tool_calls.iter().position(|open| open == &id)
            {
                self.pending_tool_calls.remove(index);
            }
        }
        self.release()
    }

    /// The held turns, released because the window closed.
    ///
    /// Empty while a call is still unanswered: the turn may not join
    /// the surface yet.
    pub fn release(&mut self) -> Vec<T> {
        if self.holds_back() {
            return Vec::new();
        }
        std::mem::take(&mut self.held)
    }

    /// The held turns, released whatever the window.
    ///
    /// The end of a surface: a turn whose call was never answered
    /// still belongs to the transcript.
    pub fn release_all(&mut self) -> Vec<T> {
        std::mem::take(&mut self.held)
    }

    #[must_use]
    /// Whether a turn is held right now.
    pub fn is_holding(&self) -> bool {
        !self.held.is_empty()
    }
}

/// Validate a `Replace` op against the previous-log prefix.
///
/// Shared by [`fold_surface`] and [`crate::token_meter::TokenMeter`]
/// so both folds reject the same provenance violations.
///
/// The surface's own seqs are the second half of the check: the op's
/// `source_event_seqs[i]` must be the seq of the surface row at
/// `start + i`. Without that tie a range that names the right rows
/// in the wrong coordinates passes every other check and shadows the
/// wrong rows.
pub(crate) fn validate_replace(
    self_seq: u64,
    range: ReplaceRange<'_>,
    surface_seqs: &[u64],
    max_surface_seq: u64,
) -> Result<(), FoldError> {
    let surface_len = surface_seqs.len();
    // Range check first so out-of-bounds errors take precedence
    // over provenance errors.
    if range.end > surface_len || range.start > range.end {
        return Err(FoldError::RangeOutOfBounds {
            seq: self_seq,
            start: range.start,
            end: range.end,
            surface_len,
        });
    }
    for &cited in range.source_event_seqs {
        if cited >= self_seq || cited > max_surface_seq {
            return Err(FoldError::BadProvenance {
                seq: self_seq,
                bad_seq: cited,
            });
        }
    }
    // The span and the citation are two descriptions of the same
    // rows, so they must agree on how many rows there are.
    let width = range.end - range.start;
    if range.source_event_seqs.len() != width {
        return Err(FoldError::CitationCount {
            seq: self_seq,
            width,
            cited: range.source_event_seqs.len(),
        });
    }
    // And the citation must name the rows the span covers.
    for (offset, &cited) in range.source_event_seqs.iter().enumerate() {
        let row = range.start + offset;
        let expected = surface_seqs[row];
        if cited != expected {
            return Err(FoldError::SpanMismatch {
                seq: self_seq,
                offset,
                row,
                cited,
                expected,
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
