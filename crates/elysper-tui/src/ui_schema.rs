//! The terminal's copy of the deployment's UI tree.
//!
//! The wire contract is the browser's: `ui/schema` carries the declared
//! slots and one schema per `[ui]` row. Slot ids are host-agnostic
//! (D25) — `chat.sidebar` is a DOM region in the browser and a column
//! here — so this module only defines what the terminal needs to read
//! them.
//!
//! Presentation lives in the crate's renderer module, which mirrors the
//! browser's kind registry without sharing a byte of it (D18). The DTOs
//! are this crate's own, not the app server's library types: the
//! terminal is a client of the wire, not of the server crate.

use serde::Deserialize;

/// The slot the terminal renders as a column beside the conversation.
pub const SLOT_CHAT_SIDEBAR: &str = "chat.sidebar";
/// The slot the terminal renders as a block under the transcript.
pub const SLOT_SESSION_EXTENSIONS: &str = "session.detail.extensions";

/// The slots this build places. Everything else lands in the one-line
/// unplaced summary.
pub const PLACED_SLOTS: [&str; 2] =
    [SLOT_CHAT_SIDEBAR, SLOT_SESSION_EXTENSIONS];

/// How many components one slot accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cardinality {
    /// Exactly one.
    One,
    /// Any number, in registration order.
    Many,
}

/// One slot a `[ui]` row declares.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UiSlot {
    /// The host-agnostic slot id, e.g. `chat.sidebar`.
    pub id: String,
    /// How many components the slot accepts.
    pub cardinality: Cardinality,
    /// The declaring `[ui]` row's id.
    pub owner: String,
}

/// One component a `[ui]` row places in a slot.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UiComponent {
    /// Which slot renders it.
    pub slot: String,
    /// The component kind; unknown kinds render as a placeholder.
    pub kind: String,
    /// The component's props.
    #[serde(default = "null_props")]
    pub props: serde_json::Value,
}

/// A component without props answers `null`; the renderers treat that
/// as "nothing to show".
fn null_props() -> serde_json::Value {
    serde_json::Value::Null
}

/// One `[ui]` row's schema.
#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UiSchema {
    /// The schema version.
    pub version: u64,
    /// The row's components, in registration order.
    #[serde(default)]
    pub components: Vec<UiComponent>,
}

/// The `ui/schema` response, as the terminal host reads it.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct UiSchemaResponse {
    /// The declared slots, in declaration order.
    #[serde(default)]
    pub slots: Vec<UiSlot>,
    /// One schema per `[ui]` row, in assembly order.
    #[serde(default)]
    pub schemas: Vec<UiSchema>,
}

impl UiSchemaResponse {
    /// The components delivered into `slot`, in registration order.
    #[must_use]
    pub fn components_in(&self, slot: &str) -> Vec<&UiComponent> {
        self.schemas
            .iter()
            .flat_map(|schema| schema.components.iter())
            .filter(|component| component.slot == slot)
            .collect()
    }

    /// The slot ids that carry components this host does not place.
    #[must_use]
    pub fn unplaced_slots(&self) -> Vec<String> {
        let mut slots: Vec<String> = Vec::new();
        for component in self
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
}

#[cfg(test)]
mod tests;
