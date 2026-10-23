//! Facade re-export for [`synthia_semantic_cache`].
//!
//! Enabled by the `semantic-cache` facade feature.

#[cfg(feature = "semantic-cache")]
pub use synthia_semantic_cache::*;
