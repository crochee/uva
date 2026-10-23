//! Facade re-export for [`synthia_reflection`].
//!
//! Enabled by the `reflection` facade feature.

#[cfg(feature = "reflection")]
pub use synthia_reflection::*;
