//! The live projection's supersession rule — this host's half of the
//! protocol ADR-0012 settled.
//!
//! The rule is the one the web client answers too, expressed in terms
//! this host can answer it: a durable row retires the live projection
//! when it carries the projection's own content (a content row whose
//! ordinal is past the projection's start position), or when it is a
//! turn boundary and the run is over — nothing else will replace the
//! projection.
//!
//! The read model asks; it does not re-decide.

use uva_session::RowTag;

/// Whether a durable row retires the live projection.
///
/// `seq` is the row's ordinal, absent for a row that carries none.
/// `run_over` is the caller's answer to "is the run over?" — a turn
/// boundary row means it, unless the turn suspended.
pub fn retires_projection(
    tag: RowTag,
    seq: Option<u64>,
    start_seq: u64,
    run_over: bool,
) -> bool {
    if matches!(tag, RowTag::AssistantMessage | RowTag::ToolResult)
        && seq.is_some_and(|seq| seq > start_seq)
    {
        return true;
    }
    matches!(tag, RowTag::Turn) && run_over
}

/// Whether a late live frame may still paint the projection.
///
/// A frame for a projection the durable rows already retired must not
/// come back; a later step's position means a newer projection, and
/// the mark of an older retired one must not silence it.
pub fn live_frame_admitted(start_seq: u64, superseded_start: u64) -> bool {
    superseded_start == 0 || start_seq > superseded_start
}

#[cfg(test)]
mod tests;
