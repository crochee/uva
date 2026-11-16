//! Raw-log surface projection: fold a JSONL session log — typed
//! events and the legacy envelopes the server has always written —
//! into the ordered list of model-visible messages, applying
//! compaction replacements on the way.
//!
//! [`fold_surface`] folds *typed*
//! [`SessionEvent`]s. A production log is a mix: structural rows are
//! typed, message rows are the legacy `UserInput` / `Model`
//! envelopes the controller persisted before the
//! typed layer existed, and compaction checkpoints are typed rows on
//! top of both. The resume path that rebuilds a session's message
//! list reads that mixture as one stream, so this module is that
//! stream's single projection:
//!
//! - [`fold_log_surface`] walks the raw rows in order, normalises
//!   every message-producing row (typed or legacy) to the
//!   `uva_provider::Message` wire shape, stamps each with its
//!   **log ordinal** as its seq, and applies the
//!   [`SurfaceOp::Replace`] op carried by a `compaction` row. The
//!   surface it returns is in the **model-visible** order: a user
//!   turn that lands while a tool call is unanswered waits for the
//!   result that answers it (see
//!   [`crate::surface::ModelOrder`]). A compaction's `start` /
//!   `end` are positions in that order, and the fold refuses a
//!   replace whose citation does not name the rows its span
//!   covers.
//! - [`SurfaceLedger`] is the incremental twin of that projection:
//!   the run's writer records every row it appends, and a compaction
//!   checkpoint asks the ledger where the rows it is about to shadow
//!   sit and which seqs identify them.
//!
//! Both share `classify_row`'s vocabulary, so the fold and the
//! ledger cannot disagree about what the surface is.
//!
//! ## Provenance in the raw domain
//!
//! A seq is a row's ordinal in the log. A writer stamps that ordinal
//! into the row (`seq`), and the reader reproduces it from that field,
//! so a `Replace { source_event_seqs }` written by a checkpoint cites
//! real earlier rows — no sequence numbers are invented on either
//! side.
//!
//! The citation is also tied to the span: `source_event_seqs[i]` is
//! the seq of the surface row at `start + i`. A range that names the
//! right rows in the wrong coordinates would otherwise pass every
//! other check and shadow the wrong rows.
//!
//! One log, one numbering: a log whose every event row carries a
//! `seq` is read by those values ([`log_is_stamped`] decides, once
//! per log), and a log that carries none — an old one — falls back to
//! counting rows, exactly as it did before. Deciding row by row
//! would mix the two schemes and produce duplicate ordinals.
//!
//! ## Failure policy
//!
//! A `Replace` whose range or provenance cannot be validated is
//! *ignored* by [`fold_log_surface`] (with a warning) so a corrupt
//! checkpoint degrades to "replay the shadowed span" instead of
//! aborting a resume. [`try_fold_log_surface`] is the strict
//! variant for consumers that want the violation surfaced (the
//! typed projection does). A compaction that meets a user turn the
//! fold still holds back is refused the same way: the surface is not
//! final yet, so its positions do not mean what the range says.

use std::sync::Arc;

use parking_lot::Mutex;
use serde_json::Value;
use uva_provider::{Content, ContentPart, Message, Role};

use crate::{
    events::{SessionEvent, SurfaceOp},
    ledger_key,
    surface::{
        FoldError,
        FoldedSurface,
        fold::{RowKind, SurfaceFold, SurfaceRow, calls_of},
        fold_surface,
    },
};

/// One raw log row projected onto the surface vocabulary.
enum LogRow {
    /// A surface-contributing row: the normalised payload it carries,
    /// the typed variant it projects to, its durable key (the call it
    /// makes or the call it answers; `None` for a row that carries
    /// neither), and how it joins the surface.
    Surface {
        kind: RowKind,
        payload: Value,
        key: Option<String>,
        op: SurfaceOp,
    },
    /// Log-only, malformed, or unrecognised — no surface
    /// contribution.
    Ignored,
}

/// Classify one raw row.
///
/// Typed message tags (`user_message` / `assistant_message` /
/// `tool_result`) carry a `uva_provider::Message`-shaped `data`
/// payload; the legacy envelopes (`Message` / `UserInput` /
/// `Model`) carry the shapes the pre-typed controller wrote. Rows
/// outside the surface vocabulary (iteration / step / usage / …)
/// are log-only.
fn classify_row(row: &Value) -> LogRow {
    let Some(tag) = row.get("type").and_then(Value::as_str) else {
        return LogRow::Ignored;
    };
    match tag {
        "compaction" => classify_compaction(row),
        "user_message" | "assistant_message" | "tool_result" => {
            classify_typed_message(row)
        }
        "Message" => classify_message_envelope(row),
        "UserInput" => classify_user_input(row),
        "Model" => classify_model_part(row),
        _ => LogRow::Ignored,
    }
}

/// The op a row asks for: its own `surface_op` when it carries one,
/// `append` otherwise (older callers did not stamp one).
fn row_op(row: &Value) -> SurfaceOp {
    row.get("surface_op")
        .and_then(|value| {
            serde_json::from_value::<SurfaceOp>(value.clone()).ok()
        })
        .unwrap_or_else(SurfaceOp::append)
}

/// Typed message row: `data` is the message payload.
fn classify_typed_message(row: &Value) -> LogRow {
    let data = row.get("data").cloned().unwrap_or(Value::Null);
    match decode_message(&data) {
        Some((message, key)) => {
            let kind = kind_of(message.role);
            message_row(message, kind, key, row_op(row))
        }
        // A typed row whose payload is not a full `Message` (e.g. a
        // repair-synthesised tool result) still names a real row.
        None => classify_fallback_payload(row, &data),
    }
}

/// Decode a message-bearing payload.
///
/// Accepts the exact `Message` wire form and the looser
/// `{"role": …, "content": [<ContentPart>, …]}` form callers write
/// by hand; the returned key is the row's durable identity (see
/// [`ledger_key::key_of`]) when the payload carries one.
fn decode_message(data: &Value) -> Option<(Message, Option<String>)> {
    if let Ok(message) = serde_json::from_value::<Message>(data.clone()) {
        let key = ledger_key::key_of(&message);
        return Some((message, key));
    }
    let role = role_of(data.get("role").and_then(Value::as_str)?)?;
    let parts: Vec<ContentPart> = data
        .get("content")?
        .as_array()?
        .iter()
        .filter_map(|part| {
            serde_json::from_value::<ContentPart>(part.clone()).ok()
        })
        .collect();
    if parts.is_empty() {
        return None;
    }
    let mut key = data
        .get("tool_call_id")
        .and_then(Value::as_str)
        .map(str::to_string);
    if key.is_none() && role == Role::Tool {
        key = parts.iter().find_map(|part| match part {
            ContentPart::ToolResult(result) => Some(result.tool_use_id.clone()),
            _ => None,
        });
    }
    let message = Message {
        role,
        content: Content::parts(parts),
        tool_call_id: key,
        ..Message::default()
    };
    let key = ledger_key::key_of(&message);
    Some((message, key))
}

/// The provider role a wire tag names.
fn role_of(tag: &str) -> Option<Role> {
    match tag {
        "user" => Some(Role::User),
        "assistant" => Some(Role::Assistant),
        "tool" => Some(Role::Tool),
        "system" => Some(Role::System),
        _ => None,
    }
}

/// Map a provider role onto the typed surface variant.
fn kind_of(role: Role) -> RowKind {
    match role {
        Role::User | Role::System => RowKind::User,
        Role::Tool => RowKind::Tool,
        _ => RowKind::Assistant,
    }
}

/// The lossless `SessionMemory` envelope: the legacy `Message` tag
/// with a whole provider [`Message`] as its payload.
fn classify_message_envelope(row: &Value) -> LogRow {
    let Some(data) = row.get("data").cloned() else {
        return LogRow::Ignored;
    };
    match serde_json::from_value::<Message>(data) {
        Ok(message) => {
            let key = ledger_key::key_of(&message);
            let kind = kind_of(message.role);
            message_row(message, kind, key, row_op(row))
        }
        Err(error) => {
            tracing::warn!(
                target: "uva.session",
                %error,
                "log surface: dropping malformed Message envelope"
            );
            LogRow::Ignored
        }
    }
}

/// The controller's user-prompt envelope: the legacy `UserInput` tag
/// with the turn's text under `data`.
fn classify_user_input(row: &Value) -> LogRow {
    match row
        .get("data")
        .and_then(|data| data.get("text"))
        .and_then(Value::as_str)
    {
        Some(text) => {
            message_row(Message::user(text), RowKind::User, None, row_op(row))
        }
        None => {
            tracing::warn!(
                target: "uva.session",
                "log surface: dropping malformed UserInput envelope"
            );
            LogRow::Ignored
        }
    }
}

/// `{"type":"Model","data":<ContentPart>}` — one streamed model
/// part per row.
fn classify_model_part(row: &Value) -> LogRow {
    let Some(data) = row.get("data").cloned() else {
        return LogRow::Ignored;
    };
    match serde_json::from_value::<ContentPart>(data) {
        Ok(part) => classify_part(part, row_op(row)),
        Err(error) => {
            tracing::warn!(
                target: "uva.session",
                %error,
                "log surface: dropping malformed Model envelope"
            );
            LogRow::Ignored
        }
    }
}

/// Project one legacy `ContentPart` onto a message row.
///
/// Every row that can be shadowed carries a durable identity — the
/// call it makes or the call it answers — so the compaction manager
/// can name it without trusting an index. The identity is the
/// message's own ([`ledger_key::key_of`]): a tool-result row carries
/// the call id on its message, exactly as the typed row does.
fn classify_part(part: ContentPart, op: SurfaceOp) -> LogRow {
    match part {
        ContentPart::Text(text) => {
            let message = Message {
                role: Role::Assistant,
                content: Content::Single(ContentPart::Text(text)),
                ..Message::default()
            };
            let key = ledger_key::key_of(&message);
            message_row(message, RowKind::Assistant, key, op)
        }
        ContentPart::ToolUse(tool_use) => {
            let message = Message {
                role: Role::Assistant,
                content: Content::Single(ContentPart::ToolUse(tool_use)),
                ..Message::default()
            };
            let key = ledger_key::key_of(&message);
            message_row(message, RowKind::Assistant, key, op)
        }
        ContentPart::ToolResult(tool_result) => {
            let call_id = tool_result.tool_use_id.clone();
            let message = Message {
                role: Role::Tool,
                content: Content::Single(ContentPart::ToolResult(tool_result)),
                // The row's identity travels on the message, as it
                // does for the typed row: the call this result
                // answers.
                tool_call_id: Some(call_id),
                ..Message::default()
            };
            let key = ledger_key::key_of(&message);
            message_row(message, RowKind::Tool, key, op)
        }
        ContentPart::Image(_)
        | ContentPart::Audio(_)
        | ContentPart::Reasoning(_)
        | ContentPart::Resource(_) => LogRow::Ignored,
    }
}

/// Last-resort normalisation: a payload that does not decode as a
/// `Message` but still carries text or a call id.
fn classify_fallback_payload(row: &Value, data: &Value) -> LogRow {
    let op = row_op(row);
    if let Some(text) = data.get("text").and_then(Value::as_str) {
        return message_row(Message::user(text), RowKind::User, None, op);
    }
    let key = data
        .get("call_id")
        .or_else(|| data.get("tool_call_id"))
        .and_then(Value::as_str);
    match key {
        Some(key) => {
            let message = Message::tool(Content::text(data.to_string()), key);
            let key = ledger_key::key_of(&message);
            message_row(message, RowKind::Tool, key, op)
        }
        None => LogRow::Ignored,
    }
}

/// A `compaction` row: its `surface_op` must be a `Replace` (the
/// event schema makes any other shape unrepresentable) and its
/// summary becomes the replacement message.
fn classify_compaction(row: &Value) -> LogRow {
    let Some(op) = compaction_op(row) else {
        return LogRow::Ignored;
    };
    let payload = compaction_payload(row);
    LogRow::Surface {
        kind: RowKind::Compaction,
        payload,
        key: None,
        op,
    }
}

/// Validate that `row` carries a `Replace` `surface_op`. Any other
/// shape is unrepresentable by the event schema, so a malformed
/// compaction row is dropped with a warning — the resume then
/// replays the shadowed span instead of breaking the fold.
fn compaction_op(row: &Value) -> Option<SurfaceOp> {
    let op_value = compaction_op_value(row)?;
    parse_replace_op(op_value)
}

/// Pull the raw `surface_op` JSON value off a compaction row,
/// warning when the field is missing.
fn compaction_op_value(row: &Value) -> Option<&Value> {
    let Some(op_value) = row.get("surface_op") else {
        tracing::warn!(
            target: "uva.session",
            "log surface: compaction row carries no surface_op"
        );
        return None;
    };
    Some(op_value)
}

/// Decode a `surface_op` JSON value as a [`SurfaceOp`] and verify
/// it is a `Replace`. Anything else drops the row with a warning —
/// the same recovery path the missing-field case uses.
fn parse_replace_op(op_value: &Value) -> Option<SurfaceOp> {
    let op = match serde_json::from_value::<SurfaceOp>(op_value.clone()) {
        Ok(op) => op,
        Err(_) => {
            tracing::warn!(
                target: "uva.session",
                "log surface: compaction row does not carry a replace op"
            );
            return None;
        }
    };
    if !matches!(op, SurfaceOp::Replace { .. }) {
        tracing::warn!(
            target: "uva.session",
            "log surface: compaction row does not carry a replace op"
        );
        return None;
    }
    Some(op)
}

/// The surface row carries the compaction's own payload. The
/// message decoders normalise `{"summary": …}` into an assistant
/// message (see `uva_context::events_to_messages`), so the
/// raw and typed folds agree on the payload.
fn compaction_payload(row: &Value) -> Value {
    match row.get("data").cloned() {
        Some(payload) if payload.is_object() => payload,
        Some(payload) => serde_json::json!({ "summary": payload.to_string() }),
        None => serde_json::json!({}),
    }
}

/// Serialize a normalised message into a surface row.
fn message_row(
    message: Message,
    kind: RowKind,
    key: Option<String>,
    op: SurfaceOp,
) -> LogRow {
    match serde_json::to_value(&message) {
        Ok(payload) => LogRow::Surface {
            kind,
            payload,
            key,
            op,
        },
        Err(error) => {
            tracing::warn!(
                target: "uva.session",
                %error,
                "log surface: dropping unserialisable message row"
            );
            LogRow::Ignored
        }
    }
}

/// Whether every event row of this log carries a `seq` field.
///
/// The fold, the ledger's seed, the ledger's feed, and the writer's
/// counter continuation all share this one judgement. Numbering one
/// log's rows by two different schemes — some by the `seq` they
/// carry, the rest by their position — produces duplicate ordinals,
/// which is worse than counting positions throughout.
///
/// The sink's metadata header is not an event row and is skipped,
/// exactly as the fold skips it. A log with no event rows is
/// vacuously stamped: there is nothing to renumber.
#[must_use]
pub fn log_is_stamped(rows: &[Value]) -> bool {
    rows.iter()
        .filter(|row| !crate::jsonl::is_metadata_header_row(row))
        .all(|row| row.get("seq").and_then(Value::as_u64).is_some())
}

/// The ordinal a row carries.
///
/// A stamped log's row carries the ordinal its writer reserved for
/// it, and that is the number every reader must reproduce: the
/// file's position stops being the ordinal as soon as two writers
/// interleave their rows. An unstamped log — an old one, or a mixed
/// one — falls back to the positional count.
///
/// Shared with the writer's ledger seed, so the ledger and the fold
/// number a log's rows the same way.
pub(crate) fn row_seq(row: &Value, stamped: bool, positional: u64) -> u64 {
    if stamped {
        row.get("seq").and_then(Value::as_u64).unwrap_or(positional)
    } else {
        positional
    }
}

/// The ordinal of a log's last row.
///
/// The fold, the ledger and every reader that caches by log tail must
/// number rows the same way, so this is the one implementation: a
/// stamped log uses each row's own `seq`, an unstamped one counts
/// event rows, and the sink's metadata header is not a row. An empty
/// log has tail `0`.
#[must_use]
pub fn log_tail_seq(rows: &[Value]) -> u64 {
    let stamped = log_is_stamped(rows);
    let mut tail = 0;
    let mut positional: u64 = 0;
    for row in rows {
        if crate::jsonl::is_metadata_header_row(row) {
            continue;
        }
        positional += 1;
        tail = row_seq(row, stamped, positional);
    }
    tail
}

/// Walk `rows` in order and return the folded surface, skipping any
/// row that does not validate.
fn fold_lenient(rows: &[Value]) -> FoldedSurface {
    let mut fold = SurfaceFold::<Value>::new();
    // One log, one numbering: a stamped log reads each row's own
    // `seq`, an unstamped one counts positions. Deciding per row
    // would mix the two schemes and produce duplicate ordinals.
    let stamped = log_is_stamped(rows);
    // The positional fallback counts **event** rows, not slice
    // positions: the sink's metadata header is the first line of a
    // fresh log but is not a row, and the sink's own sequence skips
    // it. Counting it here would put every folded seq one ahead of
    // the `stream_index` the rest of the system speaks.
    let mut positional: u64 = 0;
    for row in rows {
        if crate::jsonl::is_metadata_header_row(row) {
            continue;
        }
        positional += 1;
        let seq = row_seq(row, stamped, positional);
        let LogRow::Surface {
            kind, payload, op, ..
        } = classify_row(row)
        else {
            continue;
        };
        let calls = calls_of(kind, &payload);
        if let Err(error) = fold.apply(SurfaceRow {
            seq,
            kind,
            payload,
            op,
            calls,
        }) {
            tracing::warn!(
                target: "uva.session",
                seq,
                %error,
                "log surface: ignoring an unresolvable row; the shadowed \
                 span is replayed"
            );
        }
    }
    fold.finish();
    let (messages, surface_seqs) = fold.into_parts();
    FoldedSurface {
        messages,
        surface_seqs,
    }
}

/// Fold a raw session log into the model-visible surface.
///
/// Every message-producing row — typed or legacy — becomes one
/// surface row; the row's seq is the ordinal the log gives it. In a
/// stamped log — every event row carries a `seq`, which
/// [`log_is_stamped`] decides once per log — that is the row's own
/// `seq`; in an unstamped one it is the row's 1-based position among
/// the **event** rows. The sink's metadata header
/// ([`is_metadata_header_row`](crate::jsonl::is_metadata_header_row))
/// is not a row: it is skipped and consumes no ordinal, so the
/// header never shifts the fold's numbering.
/// A `compaction` row replaces the surface span it cites. A
/// replacement that cannot be validated is skipped with a warning,
/// so the caller always gets a usable surface.
#[must_use]
pub fn fold_log_surface(rows: &[Value]) -> FoldedSurface {
    fold_lenient(rows)
}

/// Strict variant of [`fold_log_surface`]: returns the first
/// provenance or range violation instead of skipping the op.
///
/// # Errors
///
/// Returns [`FoldError::BadProvenance`] when a `compaction` row
/// cites a seq outside the previously-folded surface and
/// [`FoldError::RangeOutOfBounds`] when its `[start, end)` does not
/// fit.
pub fn try_fold_log_surface(
    rows: &[Value],
) -> Result<FoldedSurface, FoldError> {
    fold_surface(&surface_events_from_log(rows))
}

/// Project a raw session log onto the typed surface stream.
///
/// Each message-producing row becomes the matching
/// [`SessionEvent::UserMessage`] / [`SessionEvent::AssistantMessage`]
/// / [`SessionEvent::ToolResult`] carrying the normalised payload,
/// and each `compaction` row becomes a
/// [`SessionEvent::Compaction`]; every event is stamped with the
/// ordinal the log gives its row as `seq` — the row's own `seq` in a
/// stamped log, its 1-based position among the **event** rows
/// otherwise (the sink's metadata header is skipped and consumes no
/// ordinal, matching [`fold_log_surface`]). Feeding the result to
/// [`fold_surface`] yields the same surface as
/// [`fold_log_surface`], with provenance violations surfaced instead
/// of skipped.
///
/// This is a projection, not a round-trip: the returned events carry
/// the surface payload, not the row's original wire envelope.
/// Unrecognised rows contribute nothing.
///
/// Private on purpose: the strict fold is the only caller, so a
/// consumer cannot fold a projection whose numbering it would have to
/// keep in step with [`fold_log_surface`] by hand. The public fold is
/// the surface.
fn surface_events_from_log(rows: &[Value]) -> Vec<SessionEvent> {
    let mut events = Vec::with_capacity(rows.len());
    // Same numbering as [`fold_lenient`] so the strict and lenient
    // folds agree on every seq: a stamped log reads each row's own
    // `seq`, an unstamped one counts event rows (the metadata header
    // is skipped and consumes no ordinal — see that function for
    // why).
    let stamped = log_is_stamped(rows);
    let mut positional: u64 = 0;
    for row in rows {
        if crate::jsonl::is_metadata_header_row(row) {
            continue;
        }
        positional += 1;
        let seq = row_seq(row, stamped, positional);
        let ts = row
            .get("ts")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let LogRow::Surface {
            kind, payload, op, ..
        } = classify_row(row)
        else {
            continue;
        };
        events.push(match kind {
            // `source` is a row annotation, not part of the model
            // surface: the fold projects what the model saw, and
            // what the model saw is the message.
            RowKind::User => SessionEvent::UserMessage {
                seq,
                ts,
                data: payload,
                source: None,
                surface_op: Some(op),
                // The projection carries the surface payload, not
                // the row's envelope annotations: attachment refs
                // ride the log row for a transcript reader, and the
                // model never saw them.
                attachments: Vec::new(),
            },
            RowKind::Assistant => SessionEvent::AssistantMessage {
                seq,
                ts,
                data: payload,
                surface_op: Some(op),
            },
            RowKind::Tool => SessionEvent::ToolResult {
                seq,
                ts,
                data: payload,
                // The fold projects the message, and the message
                // carries no failure flag: the surface a resumed
                // run reads back is the transcript, not the row's
                // metadata.
                is_error: false,
                tool_call_id: None,
                surface_op: Some(op),
            },
            RowKind::Compaction => SessionEvent::Compaction {
                seq,
                ts,
                data: payload,
                surface_op: op,
            },
        });
    }
    events
}

/// Where one compaction's shadowed rows sit, and which seqs
/// identify them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedReplace {
    /// Inclusive surface index of the first shadowed row.
    pub start: usize,
    /// Exclusive surface index of the shadowed span.
    pub end: usize,
    /// Seqs of the shadowed rows, in surface order.
    pub source_event_seqs: Vec<u64>,
}

/// Why a compaction's shadowed rows could not be mapped onto the
/// durable log.
///
/// Every variant names the gap so the checkpoint can log exactly
/// what was missing instead of writing provenance it cannot back.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MappingGap {
    /// The record's span size disagrees with the number of keys it
    /// carries.
    #[error(
        "compaction claims {expected} shadowed rows but carries {found} keys"
    )]
    CountMismatch {
        /// Rows the record says it replaced (`end - start`).
        expected: usize,
        /// Keys the record carries.
        found: usize,
    },
    /// The message at `position` has no durable identity, so no log
    /// row can be proven to be it.
    #[error("shadowed row {position} has no durable key")]
    MissingKey {
        /// Offset of the keyless row inside the record.
        position: usize,
    },
    /// No surface row carries the key — the row was never persisted
    /// (yet), so citing it would invent provenance.
    #[error("no durable row carries key {key:?}")]
    UnknownKey {
        /// The key that did not resolve.
        key: String,
    },
    /// More than one surface row carries the key; the shadowed row
    /// is ambiguous.
    #[error("key {key:?} matches more than one durable row")]
    AmbiguousKey {
        /// The ambiguous key.
        key: String,
    },
    /// The keys resolved to rows that are not adjacent in the
    /// surface, so a single `[start, end)` cannot cover them.
    #[error("shadowed rows [{first}, {last}] are not contiguous")]
    NotContiguous {
        /// Surface index of the first resolved row.
        first: usize,
        /// Surface index of the last resolved row.
        last: usize,
    },
}

/// Incremental index of the folded surface over a raw log.
///
/// The run's writer [`record`](Self::record)s every row it appends,
/// in order, with that row's 1-based log ordinal. The ledger keeps
/// one slot — `(seq, key)` — per surface row and applies
/// `compaction` rows exactly like [`fold_log_surface`], so
/// [`resolve`](Self::resolve) answers with a span and seqs the
/// durable fold will reproduce.
///
/// The tail is provisional until the run ends. The ledger never
/// releases a user turn the order is still holding back — a tool call
/// that was never answered — while the durable fold releases it when
/// the log ends. So the ledger holds one row fewer than the fold
/// does for such a turn, and [`resolve`](Self::resolve) cannot map
/// it: the answer it gives for a turn the fold would have released
/// is [`MappingGap::UnknownKey`]. A caller that needs the fold's own
/// answer must fold the log, not ask the ledger for its tail.
///
/// Interior mutability (`Mutex`) because the writer, the checkpoint,
/// and the flush share one ledger behind an `Arc`.
#[derive(Debug, Default)]
pub struct SurfaceLedger {
    state: Mutex<LedgerState>,
}

#[derive(Debug, Default)]
struct LedgerState {
    /// The surface the ledger indexes: one slot per row, in the
    /// order the next LLM pass sees them.
    fold: SurfaceFold<Option<String>>,
}

impl SurfaceLedger {
    /// Empty ledger.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one appended log row. `seq` is the row's 1-based
    /// ordinal in the session log; `row` is the exact value that was
    /// appended.
    ///
    /// Log-only and unrecognised rows contribute nothing. A
    /// `compaction` row whose op does not validate is ignored the
    /// same way [`fold_log_surface`] ignores it, keeping the ledger
    /// and the fold in step.
    pub fn record(&self, seq: u64, row: &Value) {
        let mut state = self.state.lock();
        let LogRow::Surface {
            kind,
            payload,
            key,
            op,
        } = classify_row(row)
        else {
            return;
        };
        let calls = calls_of(kind, &payload);
        if let Err(error) = state.fold.apply(SurfaceRow {
            seq,
            kind,
            payload: key,
            op,
            calls,
        }) {
            tracing::warn!(
                target: "uva.session",
                seq,
                %error,
                "surface ledger: ignoring an unresolvable row"
            );
        }
    }

    /// Map a compaction's shadowed-row keys onto the current
    /// surface.
    ///
    /// Every key must resolve to exactly one row, the rows must be
    /// adjacent, and they must appear in the record's order; the
    /// returned span is the one a `Replace` may cite.
    ///
    /// # Errors
    ///
    /// Returns the first [`MappingGap`] that explains why the span
    /// cannot be proven.
    pub fn resolve(
        &self,
        keys: &[Option<String>],
    ) -> Result<ResolvedReplace, MappingGap> {
        let state = self.state.lock();
        let Some((first_key, rest)) = keys.split_first() else {
            return Err(MappingGap::CountMismatch {
                expected: 1,
                found: 0,
            });
        };
        let mut positions = Vec::with_capacity(keys.len());
        let mut seqs = Vec::with_capacity(keys.len());
        for (position, key) in
            std::iter::once(first_key).chain(rest).enumerate()
        {
            let Some(key) = key.as_deref() else {
                return Err(MappingGap::MissingKey { position });
            };
            let mut matches = state
                .fold
                .rows()
                .iter()
                .enumerate()
                .filter(|(_, slot_key)| slot_key.as_deref() == Some(key));
            let Some((index, _)) = matches.next() else {
                return Err(MappingGap::UnknownKey {
                    key: key.to_string(),
                });
            };
            if matches.next().is_some() {
                return Err(MappingGap::AmbiguousKey {
                    key: key.to_string(),
                });
            }
            positions.push(index);
            seqs.push(state.fold.seqs()[index]);
        }
        let (Some(&start), Some(&last)) = (positions.first(), positions.last())
        else {
            return Err(MappingGap::CountMismatch {
                expected: 1,
                found: 0,
            });
        };
        for window in positions.windows(2) {
            if window[1] != window[0] + 1 {
                return Err(MappingGap::NotContiguous { first: start, last });
            }
        }
        Ok(ResolvedReplace {
            start,
            end: start + positions.len(),
            source_event_seqs: seqs,
        })
    }

    /// Number of surface rows currently indexed.
    #[must_use]
    pub fn len(&self) -> usize {
        self.state.lock().fold.rows().len()
    }

    /// `true` when no surface row has been recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Shared handle to a ledger.
pub type SharedSurfaceLedger = Arc<SurfaceLedger>;

#[cfg(test)]
mod tests;
