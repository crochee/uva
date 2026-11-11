//! Facade re-export for [`elysper_policy_plugins::resilience`].
//!
//! Enabled by the `resilience` facade feature.

#[cfg(feature = "resilience")]
pub use elysper_policy_plugins::resilience::*;
