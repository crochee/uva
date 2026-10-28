//! Facade re-export for [`elysper_semantic_cache`].
//!
//! Enabled by the `semantic-cache` facade feature.

#[cfg(feature = "semantic-cache")]
pub use elysper_semantic_cache::*;
