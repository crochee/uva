//! Surface fold — replay a [`SessionEvent`] log into the
//! ordered list of model-visible messages.
//!
//! Mirrors `deepseek-harness`'s `core/session/src/surface.ts`:
//! `fold_surface` is a pure function that walks the typed event
//! log and emits a `Vec<serde_json::Value>` in the order the next
//! LLM pass sees. It validates `SurfaceOp::Replace`
//! `source_event_seqs` against the previous-log prefix so an
//! out-of-band rewrite cannot shadow an event the model has
//! never seen.
//!
//! The module also carries [`assemble_chunks`], the join that
//! rebuilds a turn's assistant text from the log-only
//! `AssistantChunk` stream records.
//!
//! ## Invariants
//!
//! 1. Only surface-eligible events (`UserMessage`,
//!    `AssistantMessage`, `ToolResult`, `Compaction`) project to
//!    the surface. Log-only events (`ToolCall`, `Step`, `Turn`,
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

use serde_json::Value;

use crate::events::{ReplaceRange, SessionEvent, SurfaceOp};

/// Error category returned by [`fold_surface`].
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FoldError {
    /// `SurfaceOp::Replace` cited a `source_event_seqs` entry
    /// that does not satisfy `< self.seq && <=
    /// previous_max_seq`. The fold is aborted at the offending
    /// event; the partial surface up to but not including it is
    /// returned via `partial` so callers can decide whether to
    /// surface a corruption warning to the model.
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
}

/// Result of [`fold_surface`].
#[derive(Debug, Clone)]
pub struct FoldedSurface {
    /// Messages in the order the next LLM pass sees them. Each
    /// entry is a `serde_json::Value` shaped like
    /// `uva_provider::Message` (the caller projects).
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
///   seq), and
/// - `every entry <= max_seq_of_previous_surface_events` (a
///   replace cannot shadow an event that has not been appended
///   yet).
///
/// Violations are surface-visible failures: a `SummarizingContextManager`
/// that emits a bad `Compaction` will be rejected at fold time
/// rather than silently corrupting the model-visible history.
pub fn fold_surface(
    events: &[SessionEvent],
) -> Result<FoldedSurface, FoldError> {
    let mut surface = FoldedSurface::empty();
    let mut max_surface_seq: u64 = 0;
    for event in events {
        if !event.is_surface_eligible() {
            continue;
        }
        let seq = event.seq();
        // `None` is treated as `Append` for forward compat —
        // older sessions did not stamp a surface op on every
        // message.
        let op = event
            .surface_op()
            .cloned()
            .unwrap_or_else(SurfaceOp::append);
        let data = match event {
            SessionEvent::UserMessage { data, .. }
            | SessionEvent::AssistantMessage { data, .. }
            | SessionEvent::ToolResult { data, .. }
            | SessionEvent::Compaction { data, .. } => data.clone(),
            _ => unreachable!("is_surface_eligible narrows to these four"),
        };
        match op {
            SurfaceOp::AppendString(_) => {
                surface.messages.push(data);
                surface.surface_seqs.push(seq);
                max_surface_seq = max_surface_seq.max(seq);
            }
            SurfaceOp::Replace {
                start,
                end,
                source_event_seqs,
            } => {
                validate_replace(
                    seq,
                    ReplaceRange {
                        start,
                        end,
                        source_event_seqs: &source_event_seqs,
                    },
                    surface.len(),
                    max_surface_seq,
                )?;
                // Atomic replacement.
                if end > surface.messages.len() || start > end {
                    return Err(FoldError::RangeOutOfBounds {
                        seq,
                        start,
                        end,
                        surface_len: surface.messages.len(),
                    });
                }
                let removed =
                    surface.messages.splice(start..end, [data]).count();
                debug_assert_eq!(removed, end - start);
                surface.surface_seqs.splice(start..end, [seq]);
                max_surface_seq = max_surface_seq.max(seq);
            }
        }
    }
    Ok(surface)
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

/// Validate a `Replace` op against the previous-log prefix.
///
/// Shared by [`fold_surface`] and [`crate::token_meter::TokenMeter`]
/// so both folds reject the same provenance violations.
pub(crate) fn validate_replace(
    self_seq: u64,
    range: ReplaceRange<'_>,
    surface_len: usize,
    max_surface_seq: u64,
) -> Result<(), FoldError> {
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
    Ok(())
}

#[cfg(test)]
mod tests;
