//! The terminal's reading of the deployment's UI tree.
//!
//! The wire contract is the server's: `ui/schema` carries the declared
//! slots and one schema per `[ui]` row, and the answer is the server's
//! own type ([`UiSchemaResponse`]) — the terminal decodes it in place
//! rather than transcribing it (ADR-0025). What is left here is what
//! the **terminal** knows: which slots this build places, and which
//! slots it therefore does not.
//!
//! Slot ids are host-agnostic (D25) — `chat.sidebar` is a DOM region
//! in the browser and a column here — so this module only says what
//! the terminal needs to read them.
//!
//! Presentation lives in the crate's renderer module, which mirrors
//! the browser's kind registry without sharing a byte of it (D18).

use uva_app_server::routes::ui::UiSchemaResponse;

/// The slot the terminal renders as a column beside the conversation.
pub const SLOT_CHAT_SIDEBAR: &str = "chat.sidebar";
/// The slot the terminal renders as a block under the transcript.
pub const SLOT_SESSION_EXTENSIONS: &str = "session.detail.extensions";

/// The slots this build places. Everything else lands in the one-line
/// unplaced summary.
pub const PLACED_SLOTS: [&str; 2] =
    [SLOT_CHAT_SIDEBAR, SLOT_SESSION_EXTENSIONS];

/// The slot ids that carry components this host does not place.
///
/// The answer is the server's shape; the question is the terminal's:
/// which slots this build renders. A slot the terminal does not place
/// is named in one line rather than hidden.
#[must_use]
pub fn unplaced_slots(tree: &UiSchemaResponse) -> Vec<String> {
    let mut slots: Vec<String> = Vec::new();
    for component in tree
        .schemas
        .iter()
        .flat_map(|schema| schema.components.iter())
    {
        if !PLACED_SLOTS.contains(&component.slot.as_str())
            && !slots.iter().any(|slot| slot == &component.slot)
        {
            slots.push(component.slot.clone());
        }
    }
    slots
}

#[cfg(test)]
mod tests;
