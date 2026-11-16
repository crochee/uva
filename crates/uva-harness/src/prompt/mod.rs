//! System-prompt assembly, harness side: the transcript the sampling
//! paths start from and the volatile runtime-context snapshot.
//!
//! The manifest itself — [`PromptContext`], its [`PromptSection`]
//! blocks and the [`DEFAULT_SYSTEM_PROMPT`] text — lives in
//! [`uva_agent`], where a crate that only drives a run can name it
//! without the loop. This module re-exports those names so the
//! crate-root facade keeps them, and keeps the two pieces the loop
//! alone needs.

mod snapshot;

pub(crate) use snapshot::RuntimeContext;
pub use uva_agent::{DEFAULT_SYSTEM_PROMPT, PromptContext};
use uva_provider::Message;

/// The conversation every sampling path starts from: the assembled
/// system prompt (skipped when empty — a placeholder system message
/// would evict the provider's prefix cache for no content), the
/// caller's history, and the turn's user message when the caller
/// owns one.
///
/// A writer-backed run passes `None`: the host already logged this
/// turn's user message, so it arrives inside `history` (as the fold's
/// tail) and appending `input.content` would duplicate it.
pub(crate) fn base_transcript(
    assembled_system: &str,
    history: &[Message],
    user: Option<Message>,
) -> Vec<Message> {
    let mut messages = Vec::with_capacity(history.len() + 2);
    if !assembled_system.is_empty() {
        messages.push(Message::system(assembled_system));
    }
    messages.extend(history.iter().cloned());
    messages.extend(user);
    messages
}

#[cfg(test)]
mod tests;
