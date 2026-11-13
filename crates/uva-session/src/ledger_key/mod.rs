//! Durable identity of a surface row.
//!
//! A compaction shadows rows: it replaces a span of the surface with
//! a summary. To write that replacement the checkpoint must prove
//! *which* rows it shadowed, and the proof is a key — an identity
//! that survives the copy from the run's in-memory message list into
//! the durable log.
//!
//! A row's ordinal does not survive that copy: the log numbers its
//! rows and a message list numbers its messages, so the same message
//! carries two different numbers in the two places. The key is
//! therefore derived from the message's own content — the call it
//! makes, or the call it answers.
//!
//! ## The scheme
//!
//! - a tool row is named by the call it answers: `tool:<call id>`;
//! - an assistant row is named by the first call it makes:
//!   `assistant:<call id>`;
//! - every other row carries no identity.
//!
//! The prefix is what keeps a turn and its result apart: both name
//! the same call, and without the prefix they would collide into one
//! ambiguous key — the ledger would find two rows for one key and
//! refuse to prove the span.

use uva_provider::{Message, Role};

use crate::tool_pairing::first_tool_use_id;

/// The durable identity of a message, or `None` when it carries none.
///
/// The identity must survive a copy into another log — a row's
/// ordinal does not (a copy remaps it) — so it is derived from the
/// message's own content.
///
/// A tool message is named by the call it answers (`tool:<id>`); an
/// assistant message is named by the first call it makes
/// (`assistant:<id>`). Every other message carries no identity: it
/// names no call, so there is nothing durable to cite — and a row
/// without an identity cannot be proven to be the row a compaction
/// meant.
pub fn key_of(message: &Message) -> Option<String> {
    match message.role {
        Role::Tool => message.tool_call_id.as_deref().map(tool_key),
        Role::Assistant => first_tool_use_id(message).map(assistant_key),
        _ => None,
    }
}

/// The identity of a tool row: the call its result answers.
fn tool_key(call_id: &str) -> String {
    format!("tool:{call_id}")
}

/// The identity of an assistant row: the first call it makes.
fn assistant_key(call_id: &str) -> String {
    format!("assistant:{call_id}")
}

#[cfg(test)]
mod tests;
