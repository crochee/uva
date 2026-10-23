//! Arch-specific SIMD dot-product ().
//!
//! Pure forwarding—every name here is a `pub use` of
//! the underlying items in
//! [`synthia_search::vector::dot`].
//!
//! See the module-level docs in that file for the
//! contract—the arch-specific dispatch and the per-arch
//! tolerance live there.

pub use synthia_search::vector::dot::{DotRoute, dot as simd_dot, scalar_dot};
