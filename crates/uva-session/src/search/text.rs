//! The searchable text of a folded surface message: user and
//! assistant text, reasoning text, and the *names* of tools the
//! assistant called. Tool **results** are deliberately excluded — they
//! are machine output (file contents, command dumps), so indexing
//! them would let a query match a session that merely happened to
//! `cat` the word.

use uva_provider::{ContentPart, Message};

/// The searchable text of one message.
///
/// The message is the **typed** provider message — the same one the
/// surface fold hands the model — so the parts are read through the
/// provider's own types rather than through a hand-written guess at
/// their wire shape.
pub(super) fn message_text(message: &Message) -> String {
    let mut out = String::new();
    for part in message.content.iter() {
        match part {
            ContentPart::Text(text) => {
                out.push_str(&text.text);
                out.push(' ');
            }
            ContentPart::Reasoning(reasoning) => {
                out.push_str(&reasoning.text);
                out.push(' ');
            }
            ContentPart::ToolUse(call) => {
                out.push_str(&call.name);
                out.push(' ');
            }
            // Tool results and images contribute nothing.
            _ => {}
        }
    }
    out
}
