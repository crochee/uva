//! Facade re-export for [`elysper_resilience`].
//!
//! Enabled by the `resilience` facade feature.

#[cfg(feature = "resilience")]
pub use elysper_resilience::*;
