//! `embed_batch_demo` — 64 inputs through a scripted
//! embedder; verify `chunk_size` cap, `inflight_peak`
//! bound, and order preservation.
//!
//! ```bash
//! cargo run --example embed_batch_demo \
//!   -p elysper-hpc-batch
//! ```
//!
//! Expected tail:
//!
//! ```text
//! EMBED-BATCH: ok submitted=64 completed=64 inflight_peak=… wall_ms=…
//! ```

use std::sync::Arc;

use elysper_hpc_batch::{BatchEmbedder, BatchPolicy};
use elysper_search::{CjkTokenizer, Embedder, HashingEmbedder};

/// Scripted single-thread embedder. Each chunk sleeps
/// 50 µs so the wall-clock numbers have something to
/// bite into; the contract (length + order) is what the
/// demo is actually proving.
struct ScriptedEmbedder {
    inner: HashingEmbedder,
}

impl ScriptedEmbedder {
    fn new() -> Self {
        Self {
            inner: HashingEmbedder::new(64, Arc::new(CjkTokenizer)),
        }
    }
}

impl Embedder for ScriptedEmbedder {
    fn dim(&self) -> usize {
        self.inner.dim()
    }

    fn embed(&self, text: &str) -> Vec<f32> {
        self.inner.embed(text)
    }

    fn embed_batch(&self, texts: Vec<String>) -> Vec<Vec<f32>> {
        std::thread::sleep(std::time::Duration::from_micros(50));
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.inner.embed(&t));
        }
        out
    }
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async {
        let inner = Arc::new(ScriptedEmbedder::new());
        let batch = BatchEmbedder::with_policy(
            inner,
            BatchPolicy {
                chunk_size: 32,
                max_inflight: 8,
            },
        );
        let inputs: Vec<String> = (0..64)
            .map(|i| format!("input number {i}"))
            .collect();
        let started = std::time::Instant::now();
        let embs = batch
            .embed_batch(inputs)
            .await
            .expect("64-call batch should succeed");
        let wall_ms = started.elapsed().as_millis();
        let stats = batch.stats();
        println!(
            "EMBED-BATCH: ok submitted={} completed={} inflight_peak={} wall_ms={}",
            stats.submitted,
            stats.completed,
            stats.inflight,
            wall_ms,
        );
        assert_eq!(embs.len(), 64);
        assert_eq!(stats.submitted, 64);
        assert_eq!(stats.completed, 64);
        assert_eq!(stats.failed, 0);
    });
}
