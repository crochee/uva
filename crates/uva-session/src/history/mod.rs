//! Model-history projection — replay the durable session log
//! into the provider history the next request sees.
//!
//! [`derive_history`] is the read end of the log: the agent loop
//! no longer owns the transcript, it asks the log what the model
//! has seen. Rows are projected by [`crate::try_fold_log_surface`],
//! the crate's single raw-log projection — it skips the sink's
//! metadata header ([`crate::jsonl::is_metadata_header_row`]),
//! recognises both the typed rows and the legacy envelopes the
//! repo still writes, numbers rows the way the rest of the system
//! does, and applies the `Replace` a durable compaction wrote.
//!
//! The projection is pure: the same rows always produce the same
//! history.
//!
//! It is also the one place that knows **which row each message came
//! from**: [`ProvenancedHistory`] carries the history and its
//! ordinals together, and [`derive_history`] is that projection with
//! the ordinals dropped. Two callers cannot derive one list and
//! number it by another.
//!
//! It is also **pairing-aware**, but the pairing rule itself is
//! not its own: the log records a turn's tool call before the tool
//! runs and its result after, so a message that lands in between —
//! a steer, appended while the batch is in flight — would sit
//! between an assistant's `tool_use` and the result that answers
//! it. Every provider rejects that transcript, and the message
//! would break the very run it steers. The **fold** holds such a
//! user message back until the pending tool calls are answered
//! ([`crate::surface::ModelOrder`]), so the surface this projection
//! reads is already in the order the next LLM pass sees; the
//! projection only decodes it.

use serde_json::Value;
use uva_provider::{Content, ContentPart, Message, Role};

use crate::{log_surface::try_fold_log_surface, surface::FoldError};

/// Error returned by [`derive_history`].
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    /// The log does not fold into a model surface — a `Replace`
    /// cites a span or a seq the fold cannot prove.
    #[error("session log does not fold into a model surface: {0}")]
    Fold(#[from] FoldError),
    /// A folded surface payload is not a provider message.
    #[error("session log surface payload is not a message: {0}")]
    Decode(#[source] serde_json::Error),
}

/// Derive the model history from a raw session log.
///
/// The messages alone: [`ProvenancedHistory::from_log`] is the same
/// projection and keeps the ordinal each message was written under,
/// so an ordinary caller gets plain messages while a caller that
/// cites provenance gets the pairing.
///
/// Rows are folded by [`try_fold_log_surface`], which accepts the
/// typed rows and the legacy envelopes alike, skips the sink's
/// metadata header
/// ([`is_metadata_header_row`](crate::jsonl::is_metadata_header_row)),
/// numbers rows by the log's own ordinals and applies the
/// `Replace` a durable compaction wrote. Each folded surface
/// payload is then decoded into a [`Message`]: a message row is
/// the message it carries, and a compaction row — whose payload is
/// its record shape (`{"summary": …}`) — becomes the assistant
/// turn that replaced the span.
///
/// The fold is strict: a rewrite whose provenance cannot be proven
/// is an error rather than silently replaying the span it
/// shadowed. A resumed run must not continue on a history the log
/// does not support.
///
/// # Errors
///
/// Returns [`HistoryError::Fold`] when the log's surface rewrites
/// do not validate and [`HistoryError::Decode`] when a folded
/// payload is neither a message nor a compaction summary.
pub fn derive_history(rows: &[Value]) -> Result<Vec<Message>, HistoryError> {
    Ok(ProvenancedHistory::from_log(rows)?.into_messages())
}

/// The model-visible history of a session log, together with the
/// ordinal of the row each message was written under.
///
/// [`derive_history`] is this projection with the ordinals dropped.
/// Both come from the one fold, so the two lists cannot disagree:
/// entry `i` of [`Self::ordinals`] is always the row entry `i` of
/// [`Self::messages`] was read from. The fold already put the
/// history in the model-visible order — a steer that lands between a
/// `tool_use` and its `tool_result` is released after the result,
/// and carries its own ordinal to its new position — so a caller
/// that cites rows (a compaction's `Replace`) names the rows it
/// really shadows.
#[derive(Debug, Clone)]
pub struct ProvenancedHistory {
    // The messages, in the order the model sees them.
    messages: Vec<Message>,
    // The log ordinal of each message, index-parallel with
    // `messages`.
    ordinals: Vec<u64>,
    // Whether each message is exactly one folded surface row.
    one_message_per_row: bool,
}

impl ProvenancedHistory {
    /// Project a raw session log onto the model-visible history and
    /// the ordinal each message was written under.
    ///
    /// The fold, the decoding and the pairing rule are exactly
    /// [`derive_history`]'s, because both are this function: rows the
    /// fold cannot prove are an error, and a payload that is neither a
    /// message nor a compaction summary fails the whole projection.
    ///
    /// # Errors
    ///
    /// Returns [`HistoryError::Fold`] when the log's surface rewrites
    /// do not validate and [`HistoryError::Decode`] when a folded
    /// payload is neither a message nor a compaction summary.
    pub fn from_log(rows: &[Value]) -> Result<Self, HistoryError> {
        let surface = try_fold_log_surface(rows)?;
        let mut projection = SurfaceProjection::default();
        for (payload, seq) in surface.messages.iter().zip(&surface.surface_seqs)
        {
            projection.push(decode_message(payload)?, *seq);
        }
        let one_message_per_row = !projection.merged;
        let (messages, ordinals) = projection.into_parts();
        Ok(Self {
            messages,
            ordinals,
            one_message_per_row,
        })
    }

    #[must_use]
    /// The messages, in the order the model sees them.
    pub fn messages(&self) -> &[Message] {
        &self.messages
    }

    #[must_use]
    /// The log ordinal of each message, index-parallel with
    /// [`Self::messages`].
    pub fn ordinals(&self) -> &[u64] {
        &self.ordinals
    }

    #[must_use]
    /// Whether each message is exactly one row of the folded surface.
    ///
    /// `false` when the log carries the legacy one-part-per-row
    /// dialect, where one assistant turn is several rows that the
    /// projection merges into the one message a provider accepts. The
    /// pairing stays right — the merged message carries the ordinal of
    /// the row that opened the turn — but the surface then has more
    /// rows than messages, so a *position* in this history is no longer
    /// the position of the row it came from. A caller that plans a
    /// positional rewrite, such as a compaction's `Replace` span, must
    /// not cite such a surface.
    pub fn is_one_message_per_row(&self) -> bool {
        self.one_message_per_row
    }

    #[must_use]
    /// The messages alone, dropping the ordinals.
    pub fn into_messages(self) -> Vec<Message> {
        self.messages
    }
}

#[must_use]
/// Project folded surface payloads into provider messages.
///
/// The lenient counterpart of [`derive_history`]: a payload that is
/// neither a message nor a compaction summary is dropped with a
/// warning instead of failing the whole projection, because one bad
/// row must not wedge a long-lived session's transcript.
///
/// The payloads are a **folded** surface, already in the order the
/// next LLM pass sees: the fold holds a user turn back until the
/// tool result that answers the open call, so this projection only
/// decodes.
///
/// Both entry points share one payload decoder and one coalescing
/// rule, so the harness and the server's transcript projections
/// cannot disagree about a row's meaning or about a turn's wire
/// shape.
pub fn surface_to_messages(payloads: &[Value]) -> Vec<Message> {
    let mut projection = SurfaceProjection::default();
    for payload in payloads {
        match decode_message(payload) {
            // In-memory payloads stand for no log row; the ordinal is
            // required by the one push path and dropped with the rest.
            Ok(message) => projection.push(message, 0),
            Err(error) => tracing::warn!(
                target: "uva.session",
                %error,
                "surface projection: dropping a row that is not a message"
            ),
        }
    }
    projection.finish()
}

#[must_use]
/// Decode one folded surface payload into the message it carries.
///
/// The index-preserving counterpart of [`surface_to_messages`]:
/// callers that map a position back to a log ordinal (a compaction's
/// `Replace` range) need one message per surface row, in fold order.
/// A payload that is neither a message nor a compaction summary is
/// `None` — the caller must then decline to map positions at all.
pub fn surface_message(payload: &Value) -> Option<Message> {
    decode_message(payload).ok()
}

/// Normalise one folded surface payload into a [`Message`].
///
/// Message rows carry a `Message`-shaped payload; a compaction row
/// carries `{"summary": …}`, which the model saw as the assistant
/// turn replacing the batch.
fn decode_message(payload: &Value) -> Result<Message, HistoryError> {
    match serde_json::from_value::<Message>(payload.clone()) {
        Ok(message) => Ok(message),
        Err(error) => match payload.get("summary").and_then(Value::as_str) {
            Some(summary) => Ok(Message::assistant(summary)),
            None => Err(HistoryError::Decode(error)),
        },
    }
}

/// The shared projection of folded surface payloads into provider
/// messages.
///
/// One rule set, one place: the assistant-part coalescing and the
/// pairing of each message with the ordinal it was written under
/// live here, so [`derive_history`], [`ProvenancedHistory`] and
/// [`surface_to_messages`] cannot disagree about a transcript's shape
/// or about which row a message came from. The *order* is not this
/// projection's: the fold already put the surface in the order the
/// next LLM pass sees.
#[derive(Default)]
struct SurfaceProjection {
    /// The projected transcript, in the order the model sees it.
    messages: Vec<Message>,
    /// The log ordinal of each message, index-parallel with
    /// `messages`.
    ordinals: Vec<u64>,
    /// Whether the legacy one-part-per-row dialect forced a merge.
    ///
    /// A merge is the only way this projection produces fewer messages
    /// than the surface rows it folded: [`Self::push_assistant`] joins a
    /// legacy turn's parts onto the message that opened it. A deferral
    /// moves a message without changing the count, so it must not read
    /// as a merge. This is what [`ProvenancedHistory::is_one_message_per_row`]
    /// reports, rather than comparing lengths (which a still-deferred
    /// tail fools).
    merged: bool,
}

impl SurfaceProjection {
    /// Append one projected message, applying the projection's
    /// shape rules.
    fn push(&mut self, message: Message, seq: u64) {
        if message.role == Role::Assistant {
            self.push_assistant(message, seq);
        } else {
            self.append(message, seq);
        }
    }

    /// Append one message and the ordinal it was written under.
    ///
    /// The one growth path for both lists: a message can never enter
    /// the projection without its ordinal, or with someone else's.
    fn append(&mut self, message: Message, seq: u64) {
        self.messages.push(message);
        self.ordinals.push(seq);
    }

    /// Append an assistant turn, coalescing its parts.
    ///
    /// A legacy `"Model"` log carries ONE part per row, so an
    /// assistant turn that asked for several tools in parallel is
    /// several `tool_use` rows. Projected as separate messages that
    /// breaks the contract every provider enforces: a message
    /// carrying `tool_calls` must be followed immediately by its
    /// tool results, and here the first one is followed by another
    /// assistant message instead — OpenAI rejects it with
    /// "insufficient tool messages following tool_calls message".
    /// Merging the parts keeps the turn one message, so the tool
    /// results that follow answer it directly. The merged message
    /// keeps the ordinal of the row that opened the turn; it has no
    /// single row of its own.
    ///
    /// The merge is deliberately narrower than "adjacent assistant
    /// rows": it applies only while the message being extended holds
    /// a pending `ToolUse`. Two text-only assistant messages are
    /// genuinely separate turns (a compaction summary following the
    /// text it replaced), and merging those would rewrite the
    /// transcript's meaning rather than repair its wire shape.
    fn push_assistant(&mut self, message: Message, seq: u64) {
        if let Some(last) = self.messages.last_mut()
            && last.role == Role::Assistant
            && last
                .content
                .iter()
                .any(|part| matches!(part, ContentPart::ToolUse(_)))
        {
            merge_assistant_parts(last, message);
            // Two surface rows became one message: the legacy dialect,
            // and the one thing that makes a position in this history a
            // position in a different list.
            self.merged = true;
        } else {
            self.append(message, seq);
        }
    }

    /// The projected transcript and the ordinal of each message.
    fn into_parts(self) -> (Vec<Message>, Vec<u64>) {
        (self.messages, self.ordinals)
    }

    /// The projected transcript, without its ordinals.
    fn finish(self) -> Vec<Message> {
        self.into_parts().0
    }
}

/// Append one assistant message's parts onto `into`.
///
/// The parts are carried in the wire-neutral `Content` the two
/// messages hold, so this stays a content merge rather than a
/// re-encode: a single-part `Content::Single` is expanded into a
/// `Multi` only when the other side has something to join it to.
fn merge_assistant_parts(into: &mut Message, from: Message) {
    let mut parts: Vec<ContentPart> = into.content.iter().cloned().collect();
    parts.extend(from.content.iter().cloned());
    into.content = Content::parts(parts);
    // A `tool_call_id` belongs to a tool message; two assistant
    // messages never carry one, but keep the first if either did.
    if into.tool_call_id.is_none() {
        into.tool_call_id = from.tool_call_id;
    }
}

#[cfg(test)]
mod tests;
