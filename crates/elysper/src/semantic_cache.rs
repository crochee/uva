//! Facade re-export for [`elysper_policy_plugins::cache`].
//!
//! Enabled by the `semantic-cache` facade feature.

#[cfg(feature = "semantic-cache")]
pub use elysper_policy_plugins::cache::*;
