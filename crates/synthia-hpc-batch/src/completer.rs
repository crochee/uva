//! [`BatchCompleter`] — chunked, inflight-bounded wrapper
//! around any [`synthia_provider::ModelProvider`].
//!
//! Forwarding is one trait call: `inner.complete(req)` for
//! each item in the chunk. The wrapper owns no state
//! beyond the inner Arc + the policy + a
//! `parking_lot::Mutex<BatchStats>`; downstream providers
//! keep their per-call semantics.

use std::sync::Arc;

use futures::StreamExt;
use synthia_provider::{CompletionRequest, CompletionResponse, ModelProvider};
use tracing::debug;

use crate::{Error, Result, policy::BatchPolicy, stats::BatchStats};

/// Outcome one chunk hands back to the driver.
type ChunkResult = std::result::Result<(usize, Vec<CompletionResponse>), Error>;

/// A batched adapter over any inner [`ModelProvider`].
pub struct BatchCompleter {
    inner: Arc<dyn ModelProvider>,
    policy: BatchPolicy,
    stats: parking_lot::Mutex<BatchStats>,
}

impl BatchCompleter {
    /// Wrap `inner` with `BatchPolicy::default()`.
    pub fn new(inner: Arc<dyn ModelProvider>) -> Self {
        Self::with_policy(inner, BatchPolicy::default())
    }

    /// Wrap `inner` with the supplied [`BatchPolicy`].
    pub fn with_policy(
        inner: Arc<dyn ModelProvider>,
        policy: BatchPolicy,
    ) -> Self {
        Self {
            inner,
            policy,
            stats: parking_lot::Mutex::new(BatchStats::default()),
        }
    }

    /// Run `requests` end-to-end. Returns responses in the
    /// same order as `requests`. An empty input yields an
    /// empty output without dispatching the inner driver.
    ///
    /// The driver's chunk_size + max_inflight caps work
    /// exactly like [`crate::BatchEmbedder::embed_batch`]
    /// — for the HTTP adapters this turns into a queue
    /// that respects reqwest's per-host idle limit; for the
    /// scripted provider it stays on the configured
    /// scheduler.
    pub async fn complete_batch(
        &self,
        requests: Vec<CompletionRequest>,
    ) -> Result<Vec<CompletionResponse>> {
        let n = requests.len();
        if n == 0 {
            return Ok(Vec::new());
        }
        {
            let mut s = self.stats.lock();
            s.submitted = s.submitted.saturating_add(n);
            s.inflight = s.inflight.saturating_add(n);
            debug!(
                submitted = s.submitted,
                inflight = s.inflight,
                "BatchCompleter::complete_batch: claimed"
            );
        }
        let chunk = self.policy.chunk_size.max(1);
        let max_inflight = self.policy.max_inflight.max(1);
        let inner = Arc::clone(&self.inner);
        let mirror = Arc::new(parking_lot::Mutex::new(self.stats_snapshot()));
        let requests = Arc::new(requests);
        let mirror_for_driver = Arc::clone(&mirror);
        let driver =
            futures::stream::iter((0..n).step_by(chunk).map(move |start| {
                let inner = Arc::clone(&inner);
                let mirror = Arc::clone(&mirror_for_driver);
                let requests = Arc::clone(&requests);
                let size = (start + chunk).min(n) - start;
                async move {
                    let end = start + size;
                    let slice = requests[start..end].to_vec();
                    let mut local: Vec<CompletionResponse> =
                        Vec::with_capacity(slice.len());
                    for req in slice {
                        match inner.complete(req).await {
                            Ok(resp) => local.push(resp),
                            Err(e) => {
                                let mut s = mirror.lock();
                                s.failed = s.failed.saturating_add(size);
                                s.inflight = s.inflight.saturating_sub(size);
                                return Err(Error::Item(e.to_string()));
                            }
                        }
                    }
                    let mut s = mirror.lock();
                    s.completed = s.completed.saturating_add(local.len());
                    s.inflight = s.inflight.saturating_sub(local.len());
                    Ok((start, local))
                }
            }))
            .buffer_unordered(max_inflight)
            .collect::<Vec<ChunkResult>>()
            .await;
        let mut outputs: Vec<Option<CompletionResponse>> = vec![None; n];
        let mut first_err: Option<Error> = None;
        for step in driver {
            match (step, &first_err) {
                (Ok((start, items)), None) => {
                    for (i, value) in items.into_iter().enumerate() {
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
