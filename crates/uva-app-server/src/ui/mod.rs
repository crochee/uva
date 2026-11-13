//! The frontend loader's surface: `[ui]` rows assembled into a slot
//! tree.
//!
//! Three enforcement points keep `[ui]` and the core apart, and they
//! are separate on purpose:
//!
//! 1. The **core loader** rejects any `ui.*` service key
//!    (`crates/uva-app-server/src/profile/mod.rs::build_one` →
//!    [`ProfileError::NamespaceViolation`]).
//! 2. The **UI loader** ([`tree::load_ui_tree`]) rejects a row id
//!    outside `ui.` ([`schema::UiError::NamespaceViolation`]).
//! 3. `profile::model::segment_of` routes every `ui-bundle` row into
//!    the `[ui]` segment, so such a row never reaches the core loader.
//!
//! Slot ids (`chat.sidebar`) are not service keys: they carry no `ui.`
//! prefix, never enter the kernel registry, and are host-agnostic —
//! the browser renders them as DOM, the terminal as ratatui widgets.
//!
//! [`ProfileError::NamespaceViolation`]: crate::profile::ProfileError::NamespaceViolation

pub mod schema;
pub mod tree;
