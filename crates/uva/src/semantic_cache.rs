//! Facade re-export for [`uva_policy_plugins::cache`].
//!
//! Enabled by the `semantic-cache` facade feature.

#[cfg(feature = "semantic-cache")]
pub use uva_policy_plugins::cache::*;
