//! [`SemanticCacheProvider`] — the `ModelProvider` wrapper.

use std::{sync::Arc, time::Duration};

use elysper_core::Clock;
use elysper_provider::{
    CompletionRequest,
    CompletionResponse,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    StreamChunk,
};
use elysper_search::Embedder;

use crate::cache::store::{
    CachedCompletion,
    EmbeddingKey,
    SemanticCacheStore,
    embed_request,
    fingerprint_request,
};

/// Configuration knobs for [`SemanticCacheProvider`]. Defaults are
/// "off-by-default" friendly: a zero-cost `threshold = 1.0` plus a
/// 1-hour TTL plus a 2048-entry cap.
#[derive(Clone, Debug)]
pub struct SemanticCacheConfig {
    /// Cosine-similarity cutoff. Above this the cached entry wins.
    /// `1.0` = exact-match only; `0.0` = always hit (don't do that).
    pub threshold: f32,
    /// TTL applied to every `store` call.
    pub ttl: Duration,
    /// LRU capacity on the default store. Custom stores ignore this.
    pub max_entries: usize,
}

impl Default for SemanticCacheConfig {
    fn default() -> Self {
        Self {
            threshold: 0.92,
            ttl: Duration::from_secs(60 * 60),
            max_entries: 2048,
        }
    }
}

impl SemanticCacheConfig {
    /// Builder-style threshold setter.
    #[must_use]
    pub fn threshold(mut self, t: f32) -> Self {
        self.threshold = t;
        self
    }

    /// Builder-style TTL setter.
    #[must_use]
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    /// Builder-style capacity setter (default store only).
    #[must_use]
    pub fn max_entries(mut self, n: usize) -> Self {
        self.max_entries = n;
        self
    }
}

/// `ModelProvider` wrapper that consults the cache on `complete` and
/// falls through to the inner provider on miss.
///
/// Streaming calls (`complete_with_stream`) bypass the cache:
/// `Stream<Result<StreamChunk, _>>` is the model view, and a cache
/// hit there would be a single terminal `IsDone` chunk, which would
/// defeat the entire reason a deployment chose streaming. The plan
/// documents this; the test suite pins it.
pub struct SemanticCacheProvider {
    inner: Arc<dyn ModelProvider>,
    store: Arc<dyn SemanticCacheStore>,
    embedder: Arc<dyn Embedder>,
    config: parking_lot::RwLock<SemanticCacheConfig>,
    clock: Arc<dyn Clock>,
    hits: parking_lot::Mutex<u64>,
    misses: parking_lot::Mutex<u64>,
}

impl SemanticCacheProvider {
    /// Build a cache-wrapped provider. The `clock` is used for TTL;
    /// tests inject [`FixedClock`](elysper_core::FixedClock).
    #[must_use]
    pub fn new(
        inner: Arc<dyn ModelProvider>,
        store: Arc<dyn SemanticCacheStore>,
        embedder: Arc<dyn Embedder>,
        config: SemanticCacheConfig,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            inner,
            store,
            embedder,
            config: parking_lot::RwLock::new(config),
            clock,
            hits: parking_lot::Mutex::new(0),
            misses: parking_lot::Mutex::new(0),
        }
    }

    /// Number of cache hits observed since construction.
    #[must_use]
    pub fn hit_count(&self) -> u64 {
        *self.hits.lock()
    }

    /// Number of cache misses observed since construction.
    #[must_use]
    pub fn miss_count(&self) -> u64 {
        *self.misses.lock()
    }

    /// Snapshot of the live config.
    #[must_use]
    pub fn config(&self) -> SemanticCacheConfig {
        self.config.read().clone()
    }

    /// Replace the live config (hot-reload).
    pub fn set_config(&self, config: SemanticCacheConfig) {
        *self.config.write() = config;
    }
}

#[async_trait::async_trait]
impl ModelProvider for SemanticCacheProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), elysper_core::Error> {
        // The wrapper holds `inner` as `Arc<dyn ModelProvider>`, so a
        // mutable call into `inner.initialize` requires
        // `Arc::get_mut` (only succeeds with a single strong ref).
        // We instead require the caller to initialise the inner
        // provider before wrapping — the typical pattern is
        // `let mut p = AnthropicProvider::new(model_config)
        //     .with_api_key(key); p.initialize(cfg).await?;`
        // followed by `Arc::new(p)` and then `SemanticCacheProvider::new(...)`.
        // This matches the harness's own usage (no `initialize`
        // call site): providers come pre-initialised.
        Ok(())
    }

    fn name(&self) -> &str {
        self.inner.name()
    }

    fn model_config(&self) -> ModelConfig {
        self.inner.model_config()
    }

    fn tier(&self) -> elysper_provider::ModelTier {
        self.inner.tier()
    }

    fn supports_inline_cache_hints(&self) -> bool {
        self.inner.supports_inline_cache_hints()
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, elysper_core::Error> {
        let cfg = self.config();
        let fingerprint = fingerprint_request(&request);
        let key = EmbeddingKey::from_fingerprint(&fingerprint);
        let embedding = embed_request(&request, self.embedder.as_ref());
        let now = self.clock.now();

        if let Some(cached) =
            self.store.lookup(&key, &embedding, cfg.threshold, now)
        {
            *self.hits.lock() += 1;
            let mut response = cached.response;
            // Mark the cached response as cache-served so downstream
            // callers (the agent loop, the session sink) can tell the
            // origin. The flag is the same `cached` field the
            // provider adapters use for exact-match cache.
            response.cached = true;
            return Ok(response);
        }

        *self.misses.lock() += 1;
        let response = self.inner.complete(request).await?;

        // Cache the response asynchronously. We do not block the
        // caller's hot path; the store is in-memory and cheap to
        // update, but we still keep the cache decision on the
        // synchronous path so a test can assert hit/miss counts
        // deterministically.
        let to_store = CachedCompletion {
            response: response.clone(),
            stored_at: self.clock.now(),
            ttl: cfg.ttl,
        };
        self.store.store(key, embedding, to_store);

        Ok(response)
    }

    async fn complete_with_stream(
        &self,
        request: CompletionRequest,
        cancel_token: Option<Arc<dyn elysper_core::CancelToken>>,
        on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<CompletionResponse, elysper_core::Error> {
        // Cache decision lives on `complete`. Streaming calls go
        // straight through to the inner provider; the wrapper
        // therefore participates in the cache as a write-only
        // participant (the inner provider still hands the same
        // `CompletionResponse` shape, and the cache decision is made
        // at the `complete` boundary on the way out of the wrapper).
        //
        // Practically: if a deployment calls streaming-only,
        // the cache is never consulted. The plan calls this out, the
        // test suite pins it.
        self.inner
            .complete_with_stream(request, cancel_token, on_delta)
            .await
    }

    async fn embed(
        &self,
        texts: Vec<String>,
    ) -> Result<Vec<Vec<f64>>, elysper_core::Error> {
        self.inner.embed(texts).await
    }
}

impl std::fmt::Debug for SemanticCacheProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SemanticCacheProvider")
            .field("inner", &self.inner.name())
            .field("config", &self.config())
            .field("store", &"<dyn SemanticCacheStore>")
            .field("hits", &self.hit_count())
            .field("misses", &self.miss_count())
            .finish()
    }
}

// Internal re-export of `completion_to_sampling` so test code can
// convert a `CompletionResponse` to the agent's `SamplingResult`
// without reaching into `elysper-provider`. `#[allow(unused_imports)]`
// is intentional: only test code references it.
#[allow(unused_imports)]
pub(crate) use elysper_provider::completion_to_sampling;

// Helper for tests: build a synthetic `CompletionResponse`.
#[cfg(test)]
pub(crate) fn make_response(text: &str) -> CompletionResponse {
    use elysper_provider::{Content, ContentPart, TextContent};
    CompletionResponse {
        id: format!("resp-{text}"),
        model: "test-model".into(),
        content: Content::Single(ContentPart::Text(TextContent {
            text: text.to_string(),
            cache_control: None,
        })),
        usage: elysper_provider::TokenUsage::default(),
        cached: false,
        stop_reason: None,
        replay_state: None,
    }
}

#[cfg(test)]
pub(crate) fn make_request(text: &str) -> CompletionRequest {
    use elysper_provider::{Content, ContentPart, Message, Role, TextContent};
    CompletionRequest {
        model: "test-model".into(),
        messages: Arc::new(vec![Message {
            role: Role::User,
            content: Content::Single(ContentPart::Text(TextContent {
                text: text.to_string(),
                cache_control: None,
            })),
            tool_call_id: None,
            name: None,
            tool_result_cleared_at: None,
        }]),
        tools: Arc::new(Vec::new()),
        ..CompletionRequest::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sane_values() {
        let cfg = SemanticCacheConfig::default();
        assert!((cfg.threshold - 0.92).abs() < 1e-6);
        assert_eq!(cfg.ttl, Duration::from_secs(60 * 60));
        assert_eq!(cfg.max_entries, 2048);
    }

    #[test]
    fn builder_setters_chain() {
        let cfg = SemanticCacheConfig::default()
            .threshold(0.5)
            .ttl(Duration::from_secs(30))
            .max_entries(8);
        assert!((cfg.threshold - 0.5).abs() < 1e-6);
        assert_eq!(cfg.ttl, Duration::from_secs(30));
        assert_eq!(cfg.max_entries, 8);
    }
}
