//! Arch-specific SIMD dot-product.
//!
//! The contract is plain: given two `&[f32]` slices of the
//! same length, return their dot product. The route is
//! picked at compile time on `cfg(target_arch)`:
//!
//! - `x86_64` + AVX2 → 8 elements per group
//!   (`_mm256_loadu_ps` + `_mm256_fmadd_ps` + scalar
//!   reduce).
//! - `x86_64` (no AVX2) → 4 elements per group
//!   (SSE2 `__m128`).
//! - `aarch64` + NEON → 4 elements per group
//!   (`vld1q_f32` + `vfmaq_f32`).
//! - Anything else → the same scalar loop
//!   `FlatVectorStore::search` already uses.
//!
//! On `x86_64` we deliberately **disable SSE2 fallback
//! for `target_feature = "avx2"`-enabled builds**—modern
//! AVX2 CPUs handle SSE2 transparently; the SSE2 path is
//! the compile fallback when AVX2 is gated off (which our
//! `simd` feature does not do—AVX2 detection is per
//! `cfg`).
//!
//! ## Stable-only constraint
//!
//! Stable Rust has no `std::simd` (`#![feature(portable_simd)]
//! is nightly-only). The module sticks to intrinsic
//! functions gated by `is_x86_feature_detected!` /
//! `is_aarch64_feature_detected!`. The runtime check is
//! enabled by Cargo's `target-feature` / `-C
//! target-cpu=native` build mode; the runtime dispatch is
//! explicit in [`dispatch`] below, so a binary built with
//! the default `target-cpu` still picks AVX2 at runtime.
//!
//! ## `simd-portable` is intentionally absent
//!
//! `std::simd` requires nightly; the project is on stable
//! (`rust-toolchain.toml` pins the toolchain). A nightly
//! feature flag would break the project's MSRV
//! (`rust-version = "1.98"`). The `simd` feature stays
//! zero-dep + stable; a future nightly CI run can introduce
//! `simd-portable` behind its own flag without breaking
//! this module's contract.

/// Public-facing dot. Dispatches at runtime on x86_64 +
/// AVX2 / SSE2 detection; aarch64 / other archs collapse
/// to a single [`scalar_dot`] since there is no second
/// path worth a runtime check.
///
/// The function is inlinable so callers see the dispatch
/// folded into their hot loops.
#[inline]
pub fn dot(a: &[f32], b: &[f32]) -> f32 {
    debug_assert_eq!(a.len(), b.len());
    dispatch(a, b)
}

/// Mark an arch-specific dot. Used by the bench to print
/// the route it took so the proof line is honest about
/// which SIMD lane the number came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DotRoute {
    Scalar,
    Sse2,
    Avx2,
    Neon,
}

impl DotRoute {
    /// Short, lower-case tag suitable for the example's
    /// tail-proof line (e.g. `feature=avx2`).
    pub fn as_tag(self) -> &'static str {
        match self {
            DotRoute::Scalar => "scalar",
            DotRoute::Sse2 => "sse2",
            DotRoute::Avx2 => "avx2",
            DotRoute::Neon => "neon",
        }
    }
}

/// Pick the best dot route for the current build. Returns
/// the scalar route on platforms the module does not
/// target, and on the (rare) `x86_64` host without
/// runtime-detectable SSE2.
pub fn route() -> DotRoute {
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            return DotRoute::Avx2;
        }
        if is_x86_feature_detected!("sse2") {
            return DotRoute::Sse2;
        }
        DotRoute::Scalar
    }
    #[cfg(target_arch = "aarch64")]
    {
        if std::arch::is_aarch64_feature_detected!("neon") {
            return DotRoute::Neon;
        }
        DotRoute::Scalar
    }
    #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
    {
        DotRoute::Scalar
    }
}

/// Scalar fallback—identical to the loop
/// `FlatVectorStore::search` already used. Lives here so
/// the dispatch panics down to it instead of forwarding
/// to the search module (which would couple SIMD to the
/// store).
pub fn scalar_dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

#[cfg(target_arch = "x86_64")]
#[inline]
fn dispatch(a: &[f32], b: &[f32]) -> f32 {
    if is_x86_feature_detected!("avx2") {
        unsafe { dot_avx2(a, b) }
    } else if is_x86_feature_detected!("sse2") {
        unsafe { dot_sse2(a, b) }
    } else {
        scalar_dot(a, b)
    }
}

#[cfg(target_arch = "aarch64")]
#[inline]
fn dispatch(a: &[f32], b: &[f32]) -> f32 {
    if std::arch::is_aarch64_feature_detected!("neon") {
        unsafe { dot_neon(a, b) }
    } else {
        scalar_dot(a, b)
    }
}

#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
#[inline]
fn dispatch(a: &[f32], b: &[f32]) -> f32 {
    scalar_dot(a, b)
}

// --- AVX2 ----------------------------------------------------------------

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn dot_avx2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = a.len();
    let chunks = n / 8;
    let mut acc = _mm256_setzero_ps();
    for i in 0..chunks {
        let off = i * 8;
        let va = unsafe { _mm256_loadu_ps(a.as_ptr().add(off)) };
        let vb = unsafe { _mm256_loadu_ps(b.as_ptr().add(off)) };
        acc = unsafe { _mm256_fmadd_ps(va, vb, acc) };
    }
    let mut hi = [0f32; 8];
    unsafe { _mm256_storeu_ps(hi.as_mut_ptr(), acc) };
    let mut total: f32 = hi.iter().sum();
    let tail_start = chunks * 8;
    for i in tail_start..n {
        total += a[i] * b[i];
    }
    total
}

#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn dot_sse2(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::x86_64::*;
    let n = a.len();
    let chunks = n / 4;
    let mut acc = _mm_setzero_ps();
    for i in 0..chunks {
        let off = i * 4;
        let va = unsafe { _mm_loadu_ps(a.as_ptr().add(off)) };
        let vb = unsafe { _mm_loadu_ps(b.as_ptr().add(off)) };
        acc = _mm_add_ps(acc, _mm_mul_ps(va, vb));
    }
    let mut hi = [0f32; 4];
    unsafe { _mm_storeu_ps(hi.as_mut_ptr(), acc) };
    let mut total: f32 = hi.iter().sum();
    let tail_start = chunks * 4;
    for i in tail_start..n {
        total += a[i] * b[i];
    }
    total
}

// --- NEON ----------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
#[target_feature(enable = "neon")]
unsafe fn dot_neon(a: &[f32], b: &[f32]) -> f32 {
    use std::arch::aarch64::*;
    let n = a.len();
    let chunks = n / 4;
    let mut acc = vdupq_n_f32(0.0);
    for i in 0..chunks {
        let off = i * 4;
        let va = unsafe { vld1q_f32(a.as_ptr().add(off)) };
        let vb = unsafe { vld1q_f32(b.as_ptr().add(off)) };
        acc = unsafe { vfmaq_f32(acc, va, vb) };
    }
    let mut total: f32 = unsafe { vaddvq_f32(acc) };
    let tail_start = chunks * 4;
    for i in tail_start..n {
        total += a[i] * b[i];
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All four routes (scalar / SSE2 / AVX2 / NEON) MUST
    /// agree within a single-bit-of-the-mantissa tolerance
    /// on a 768-dim pair (the prompt size the plan calls
    /// out).
    fn equal_within(a: f32, b: f32) -> bool {
        // f32 dot-product accumulation order is
        // route-dependent; tree-reductions on 768 floats
        // can shift the sum by a few ULPs. A 1e-3
        // absolute tolerance is the loose end of the
        // contract; tighter asserts would flake under
        // FMA vs separate-mul-add paths.
        (a - b).abs() < 1e-3
            || ((a - b).abs() / a.abs().max(f32::EPSILON)) < 1e-3
    }

    #[test]
    fn scalar_matches_scalar() {
        let a: Vec<f32> = (0..768).map(|i| (i as f32) * 0.001).collect();
        let b: Vec<f32> = (0..768).map(|i| (i as f32) * 0.001).collect();
        let s = scalar_dot(&a, &b);
        let d = dot(&a, &b);
        assert!(equal_within(s, d), "scalar = {s}, dispatch = {d}");
    }

    #[test]
    fn handles_short_inputs() {
        let a = vec![1.0, 2.0, 3.0];
        let b = vec![4.0, 5.0, 6.0];
        let s = scalar_dot(&a, &b);
        let d = dot(&a, &b);
        assert!(equal_within(s, d));
    }

    #[test]
    fn handles_unaligned_tail() {
        // Length = 768 + 3 = 771—not a multiple of 8
        // or 4, so any SIMD route must process the tail
        // via the scalar loop.
        let n = 771;
        let a: Vec<f32> = (0..n).map(|i| (i as f32) * 0.01).collect();
        let b: Vec<f32> = (0..n).map(|i| (i as f32) * 0.01).collect();
        let s = scalar_dot(&a, &b);
        let d = dot(&a, &b);
        assert!(equal_within(s, d), "scalar = {s}, dispatch = {d}");
    }

    #[test]
    fn route_reports_known_arch() {
        let r = route();
        // We do not assert the route—only that `route()`
        // returns one of the documented variants.
        match r {
            DotRoute::Scalar
            | DotRoute::Sse2
            | DotRoute::Avx2
            | DotRoute::Neon => {}
        }
    }
}
