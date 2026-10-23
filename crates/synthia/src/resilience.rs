//! Facade re-export for [`synthia_resilience`].
//!
//! Enabled by the `resilience` facade feature.

#[cfg(feature = "resilience")]
pub use synthia_resilience::*;
