//! The `[ui]` segment as JSON schema the web front-end renders.
//!
//! A UI row is a description, never code: `UiBundle` carries the
//! `version` and the component list a registered renderer draws. v1
//! executes no remote JavaScript and applies no remote stylesheet, so
//! a schema naming one is rejected at load time.
//!
//! This module owns the wire types and the error vocabulary; the
//! loader that assembles them lives in [`super::tree`].
//!
//! # Namespace
//!
//! UI ids carry the kernel's [`UI_PREFIX`] (`ui.`). The core loader
//! rejects a core manifest declaring a `ui.` key
//! ([`ProfileError::NamespaceViolation`]); [`load_ui_tree`] is the
//! other half of the same boundary and rejects an entry outside it.
//!
//! [`ProfileError::NamespaceViolation`]: crate::profile::ProfileError::NamespaceViolation
//! [`load_ui_tree`]: super::tree::load_ui_tree

use elysper::kernel::UI_PREFIX;
use serde::Serialize;
use serde_json::Value;

/// One UI row's rendered description.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UiSchema {
    /// The schema version the front-end renders. v1 is the current one.
    pub version: u32,
    /// The panels the row contributes, in assembly order.
    pub components: Vec<UiComponent>,
}

/// One component a UI row places in a slot.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UiComponent {
    /// Which slot renders it, e.g. `chat.sidebar`.
    pub slot: String,
    /// The registered component kind. An unknown kind is carried
    /// through unchanged and rendered as the fallback placeholder.
    pub kind: String,
    /// The component's props, a JSON object.
    pub props: Value,
}

/// Why a `[ui]` row could not be projected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UiError {
    /// The id is not in the `ui.` namespace.
    #[error(
        "entry `{id}` is not in the `ui.` namespace; UI entries must \
         start with `ui.`"
    )]
    NamespaceViolation {
        /// The rejected id.
        id: String,
    },
    /// The row names a runtime the UI loader does not read.
    #[error(
        "entry `{id}` selects runtime `{runtime}`, but the UI loader \
         reads `ui-bundle` rows only"
    )]
    WrongRuntime {
        /// The row's id.
        id: String,
        /// The row's runtime wire name.
        runtime: &'static str,
    },
    /// The row's config is not a renderable schema.
    #[error("entry `{id}` config is not a valid UI schema: {message}")]
    InvalidConfig {
        /// The row's id.
        id: String,
        /// The failing field and reason.
        message: String,
    },
    /// The row names executable content, which v1 never runs.
    #[error(
        "entry `{id}` declares `{field}`; v1 UI bundles are JSON schema \
         and never execute remote JavaScript or CSS"
    )]
    ExecutablePayload {
        /// The row's id.
        id: String,
        /// The rejected top-level field.
        field: String,
    },
    /// A row declared a slot that is already declared.
    #[error(
        "entry `{id}` declares slot `{slot}`, already declared by `{owner}`"
    )]
    SlotOccupied {
        /// The declaring row's id.
        id: String,
        /// The contested slot id.
        slot: String,
        /// The row that declared it first.
        owner: String,
    },
    /// A component addressed a slot no row declares.
    #[error("entry `{id}` registers into slot `{slot}`, which no row declares")]
    UnknownSlot {
        /// The registering row's id.
        id: String,
        /// The undeclared slot id.
        slot: String,
    },
    /// A `one`-cardinality slot received a second registration.
    #[error(
        "entry `{id}` registers into `one` slot `{slot}`, already holding \
         a component from `{owner}`"
    )]
    CardinalityExceeded {
        /// The registering row's id.
        id: String,
        /// The full slot.
        slot: String,
        /// The row whose component holds it.
        owner: String,
    },
}

/// The `ui.` prefix the loader requires, re-exported for callers that
/// build entry ids.
pub const UI_NAMESPACE: &str = UI_PREFIX;
