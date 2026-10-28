//! `simd_bench` — generate 10 000 random 768-dim unit
//! vectors, run a kNN sweep over each VectorStore impl,
//! print per-route microseconds + a speedup figure so the
//! proof line tells a single, honest story.
//!
//! ```bash
//! cargo run --example simd_bench -p elysper-search \
//!   --features simd
//! ```
//!
//! Expected tail:
//!
//! ```text
//! SIMD-BENCH: ok flat_us=… simd_us=… speedup=…x arch=… feature=…
//! ```
//!
//! The reported `arch` and `feature` come from
//! [`elysper_search::vector::dot::route`] — they tell the
//! reader which SIMD lane the bench actually hit (vs
//! "AVX2 expected but you were on a non-AVX2 host and
//! fell through to scalar"). A6 in the plan calls this
//! out: if AVX2 is unavailable, the proof line degrades
//! to `feature=sse2` or `feature=scalar` — this is the
//! only honest answer.

use std::time::Instant;

use elysper_search::vector::{DotRoute, dot, scalar_dot};

/// Deterministic LCG so the bench is reproducible
/// without pulling `rand` into the dep graph.
struct Lcg(u64);

impl Lcg {
    fn next_f32(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 33) as f32 / (u32::MAX >> 1) as f32) - 1.0
    }

    fn unit_vec(&mut self, dim: usize) -> Vec<f32> {
        let mut v: Vec<f32> = (0..dim).map(|_| self.next_f32()).collect();
        let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > f32::EPSILON {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }
}

fn main() {
    use std::hint::black_box;
    // 2 000 × 50 × 768 = ~77 M multiply-adds per pass.
    // Large enough for the signal to dominate timer noise,
    // small enough that `make examples` (debug build) does
    // not spend 40 s in this program.
    let dim = 768;
    let n_vectors = 2_000_usize;
    let n_queries = 50_usize;

    let mut rng = Lcg(0x00c0_ffee_1234_5678);
    let vectors: Vec<Vec<f32>> =
        (0..n_vectors).map(|_| rng.unit_vec(dim)).collect();
    let queries: Vec<Vec<f32>> =
        (0..n_queries).map(|_| rng.unit_vec(dim)).collect();

    // 1) Scalar baseline — 200 queries × 10 000
    // dot products, each 768-dim. `black_box` blocks
    // the optimiser from eliding either loop.
    let scalar_started = Instant::now();
    let mut sink = 0f32;
    for q in &queries {
        for v in &vectors {
            sink += scalar_dot(black_box(q), black_box(v));
        }
    }
    let flat_us = scalar_started.elapsed().as_micros();
    let _ = black_box(sink);

    // 2) SIMD — same workload, this time through
    // `dot::dot` which dispatches to the arch-specific
    // route (AVX2 / SSE2 / NEON / scalar).
    let simd_started = Instant::now();
    let mut sink2 = 0f32;
    for q in &queries {
        for v in &vectors {
            sink2 += dot::dot(black_box(q), black_box(v));
        }
    }
    let simd_us = simd_started.elapsed().as_micros();
    let _ = black_box(sink2);

    // Spot-check the two routes agree on a single pair
    // (the f32-test in `dot.rs` covers the full property;
    // this is the bench's quick proof).
    let mut rng2 = Lcg(0xfeed_beef);
    let a: Vec<f32> = (0..dim).map(|_| rng2.next_f32()).collect();
    let b: Vec<f32> = (0..dim).map(|_| rng2.next_f32()).collect();
    let s_scalar: f32 = scalar_dot(black_box(&a), black_box(&b));
    let s_simd: f32 = dot::dot(black_box(&a), black_box(&b));
    let rel_err = ((s_scalar - s_simd).abs() / s_scalar.abs().max(1e-9)) as f64;
    assert!(
        rel_err < 1e-3,
        "scalar vs simd disagree: scalar={s_scalar}, simd={s_simd}, rel_err={rel_err}"
    );

    let speedup = flat_us as f64 / (simd_us as f64).max(1.0);
    let route: DotRoute = dot::route();
    println!(
        "SIMD-BENCH: ok flat_us={flat_us} simd_us={simd_us} speedup={speedup:.2}x arch={} feature={}",
        std::env::consts::ARCH,
        route.as_tag(),
    );
}
