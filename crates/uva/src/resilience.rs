//! Facade re-export for [`uva_policy_plugins::resilience`].
//!
//! Enabled by the `resilience` facade feature.

#[cfg(feature = "resilience")]
pub use uva_policy_plugins::resilience::*;
