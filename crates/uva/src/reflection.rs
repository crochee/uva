//! Facade re-export for [`uva_hook_plugins::reflection`].
//!
//! Enabled by the `reflection` facade feature.

#[cfg(feature = "reflection")]
pub use uva_hook_plugins::reflection::*;
