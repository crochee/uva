//! HNSW benchmark: generate 10 000 random 256-dim unit vectors,
//! build one `FlatVectorStore` and one `HnswStore`, run 100
//! nearest-neighbour queries against each, and print the
//! speed-up. The gate is `speedup >= 10.0`.
//!
//! Run with `cargo run --example hnsw_bench -p synthia-search
//! --features hnsw`.

use std::time::Instant;

use synthia_search::vector::{
    FlatVectorStore,
    HnswParams,
    HnswStore,
    VectorStore,
};

/// Small deterministic LCG so the benchmark is reproducible
/// without pulling `rand` into the default dep set.
struct Lcg(u64);

impl Lcg {
    fn next_f32(&mut self) -> f32 {
        // Numerical Recipes constants.
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
    let n = 10_000usize;
    let dim = 256usize;
    let queries = 100usize;
    let top_k = 10usize;

    let mut rng = Lcg(0x5EED_5EED_5EED_5EED);
    let vectors: Vec<Vec<f32>> = (0..n).map(|_| rng.unit_vec(dim)).collect();
    let query_vecs: Vec<Vec<f32>> =
        (0..queries).map(|_| rng.unit_vec(dim)).collect();

    // -- FlatVectorStore ------------------------------------------------
    let mut flat = FlatVectorStore::new(dim);
    let started = Instant::now();
    for v in &vectors {
        flat.add(v.clone()).expect("flat add");
    }
    let flat_build = started.elapsed();

    let started = Instant::now();
    let mut flat_hits_total = 0usize;
    for q in &query_vecs {
        let hits = flat.search(q, top_k);
        flat_hits_total += hits.len();
    }
    let flat_query = started.elapsed();
    let flat_us = flat_query.as_secs_f64() * 1e6 / queries as f64;

    // Lower `ef_construction` keeps the (debug-mode) build under a
    // minute; `ef_search = 16` with `top_k = 10` still leaves the
    // graph enough candidates to return full results while cutting
    // the per-query work — that is where the ≥10× speed-up over the
    // flat O(n·dim) scan comes from.
    let mut hnsw = HnswStore::new(
        dim,
        HnswParams {
            ef_construction: 32,
            ef_search: 16,
            ..Default::default()
        },
    );
    let started = Instant::now();
    for v in &vectors {
        hnsw.add(v.clone()).expect("hnsw add");
    }
    let hnsw_build = started.elapsed();

    let started = Instant::now();
    let mut hnsw_hits_total = 0usize;
    for q in &query_vecs {
        let hits = hnsw.search(q, top_k);
        hnsw_hits_total += hits.len();
    }
    let hnsw_query = started.elapsed();
    let hnsw_us = hnsw_query.as_secs_f64() * 1e6 / queries as f64;

    let speedup = flat_us / hnsw_us.max(f64::EPSILON);

    println!("HNSW-BENCH: ok n={n} dim={dim} queries={queries} top_k={top_k}");
    println!("  build: flat={:?} hnsw={:?}", flat_build, hnsw_build);
    println!(
        "  query: flat_us={flat_us:.2} hnsw_us={hnsw_us:.2} speedup={speedup:.2}x"
    );
    println!("  hits: flat={flat_hits_total} hnsw={hnsw_hits_total}");

    // Sanity: both returned the full top-k for every query.
    assert_eq!(flat_hits_total, queries * top_k);
    assert_eq!(hnsw_hits_total, queries * top_k);

    // Gate: HNSW must be at least 10× faster per query at this
    // scale. Fail loudly so CI catches a regression.
    assert!(
        speedup >= 10.0,
        "HNSW speedup {speedup:.2}x is below the 10x gate"
    );
    println!("  gate: speedup >= 10x PASSED ({speedup:.2}x)");
}
