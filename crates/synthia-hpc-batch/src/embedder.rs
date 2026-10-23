//! [`BatchEmbedder`] — chunked, inflight-bounded wrapper
//! around any [`synthia_search::Embedder`].
//!
//! Switch from a single-call driver to the batched twin
//! with one call:
//!
//! ```ignore
//! engine.with_embedder(Arc::new(BatchEmbedder::new(inner)))
//! ```
//!
//! ## Driver
//!
//! The driver walks the inputs in `[0..N)` blocks of
//! `chunk_size`, hands each block to
//! `futures::stream::iter(...).buffer_unordered(max_inflight)`.
//! The per-chunk work — a sync call into the inner
//! `Embedder::embed_batch` — is offloaded to a fresh
//! `std::thread` so the cap actually bounds real
//! OS-thread concurrency, not just task scheduling.
//!
//! The whole driver is `futures`-based; no tokio dep in
//! the lib, so the public API stays runtime-neutral.

use std::sync::Arc;

use futures::StreamExt;
use synthia_search::Embedder;
use tracing::debug;

use crate::{Error, Result, policy::BatchPolicy, stats::BatchStats};

/// Outcome one chunk hands back to the driver — `Ok` keeps
/// the (start, embs) pair for the order-restoring fold;
/// `Err` short-circuits the rest of the stream.
type ChunkResult = std::result::Result<(usize, Vec<Vec<f32>>), Error>;

/// A batched adapter over any inner [`Embedder`].
pub struct BatchEmbedder {
    inner: Arc<dyn Embedder>,
    policy: BatchPolicy,
    stats: parking_lot::Mutex<BatchStats>,
}

impl BatchEmbedder {
    /// Wrap `inner` with `BatchPolicy::default()` (chunk=32,
    /// inflight=8).
    pub fn new(inner: Arc<dyn Embedder>) -> Self {
        Self::with_policy(inner, BatchPolicy::default())
    }

    /// Wrap `inner` with the supplied [`BatchPolicy`].
    pub fn with_policy(inner: Arc<dyn Embedder>, policy: BatchPolicy) -> Self {
        Self {
            inner,
            policy,
            stats: parking_lot::Mutex::new(BatchStats::default()),
        }
    }

    /// Embed `inputs` end-to-end. Returns embeddings in
    /// the same order as `inputs`. An empty input yields
    /// an empty output without dispatching the inner
    /// driver.
    pub async fn embed_batch(
        &self,
        inputs: Vec<String>,
    ) -> Result<Vec<Vec<f32>>> {
        let n = inputs.len();
        if n == 0 {
            return Ok(Vec::new());
        }
        // Phase 1: claim the lot.
        {
            let mut s = self.stats.lock();
            s.submitted = s.submitted.saturating_add(n);
            s.inflight = s.inflight.saturating_add(n);
            debug!(
                submitted = s.submitted,
                inflight = s.inflight,
                "BatchEmbedder::embed_batch: claimed"
            );
        }
        let chunk = self.policy.chunk_size.max(1);
        let max_inflight = self.policy.max_inflight.max(1);
        let inner = Arc::clone(&self.inner);
        // Per-chunk mirror so the stats live independently
        // of the driver's serial walks.
        let mirror = Arc::new(parking_lot::Mutex::new(self.stats_snapshot()));
        // The stream owns `inputs` by move so each
        // per-chunk future can `to_vec()`-copy its slice
        // without fighting for the same borrow. The
        // collected `Vec<ChunkResult>` holds the outcome
        // for each chunk.
        let inputs_for_driver = inputs;
        let mirror_for_driver = Arc::clone(&mirror);
        let driver =
            futures::stream::iter((0..n).step_by(chunk).map(move |start| {
                let inner = Arc::clone(&inner);
                let mirror = Arc::clone(&mirror_for_driver);
                let chunk_inputs: Vec<String> =
                    inputs_for_driver[start..(start + chunk).min(n)].to_vec();
                let size = chunk_inputs.len();
                async move {
                    let (tx, rx) = std::sync::mpsc::channel::<ChunkResult>();
                    let inner_for_thread = Arc::clone(&inner);
                    let work_inputs = chunk_inputs;
                    let mirror_for_thread = Arc::clone(&mirror);
                    // Offload the (sync) inner call to its
                    // own OS thread so `buffer_unordered`
                    // actually bounds real concurrency.
                    std::thread::spawn(move || {
                        let res = inner_for_thread.embed_batch(work_inputs);
                        let mut s = mirror_for_thread.lock();
                        if res.len() != size {
                            s.failed = s.failed.saturating_add(size);
                            s.inflight = s.inflight.saturating_sub(size);
                            let _ = tx.send(Err(Error::LengthMismatch {
                                expected: size,
                                got: res.len(),
                            }));
                        } else {
                            s.completed = s.completed.saturating_add(size);
                            s.inflight = s.inflight.saturating_sub(size);
                            let _ = tx.send(Ok((start, res)));
                        }
                    });
                    rx.recv().unwrap_or_else(|_| {
                        let mut s = mirror.lock();
                        s.failed = s.failed.saturating_add(size);
                        s.inflight = s.inflight.saturating_sub(size);
                        Err(Error::Item("embedder thread disconnected".into()))
                    })
                }
            }))
            .buffer_unordered(max_inflight)
            .collect::<Vec<ChunkResult>>()
            .await;
        let mut outputs: Vec<Option<Vec<f32>>> = vec![None; n];
        let mut first_err: Option<Error> = None;
        for step in driver {
            match (step, &first_err) {
                (Ok((start, embs)), None) => {
                    for (i, value) in embs.into_iter().enumerate() {
                        outputs[start + i] = Some(value);
                    }
                }
                (Err(e), None) => first_err = Some(e),
                _ => {}
            }
        }
        *self.stats.lock() = *mirror.lock();
        if let Some(e) = first_err {
            return Err(e);
        }
        Ok(outputs.into_iter().map(|o| o.unwrap_or_default()).collect())
    }

    /// Live snapshot of [`BatchStats`].
    pub fn stats(&self) -> BatchStats {
        *self.stats.lock()
    }

    /// Snapshot helper used by the internal driver.
    fn stats_snapshot(&self) -> BatchStats {
        *self.stats.lock()
    }
}
