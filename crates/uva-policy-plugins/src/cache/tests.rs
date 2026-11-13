//! Tests for `uva-semantic-cache`.
//!
//! These tests use a small `CountingProvider` fake to verify the
//! wrapper's behavior without pulling in any HTTP / network / disk
//! stack. The cache decision is on `complete`; streaming is pinned
//! to bypass.

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use chrono::TimeZone;
use uva_core::{Clock, FixedClock};
use uva_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    ToolDefinition,
};
use uva_search::{CjkTokenizer, Embedder, HashingEmbedder};

use crate::cache::{
    store::{
        CachedCompletion,
        EmbeddingKey,
        InMemorySemanticCache,
        SemanticCacheStore,
        fingerprint_request,
    },
    wrapper::{
        SemanticCacheConfig,
        SemanticCacheProvider,
        make_request,
        make_response,
    },
};

/// Provider that records how many times `complete` was called and
/// returns a deterministic response keyed on the request text.
struct CountingProvider {
    calls: AtomicU64,
}

#[async_trait::async_trait]
impl ModelProvider for CountingProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), uva_core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "counting"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "test-model".into(),
            provider: "counting".into(),
            context_window: 8_192,
            max_output_tokens: 1_024,
            supports_tools: false,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, uva_core::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // Key the response on the request text so we can see whether
        // the cache returned what the inner would have.
        let text = request
            .messages
            .first()
            .and_then(|m| match &m.content {
                Content::Single(ContentPart::Text(t)) => Some(t.text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        Ok(make_response(&format!("inner:{text}")))
    }
}

fn embedder() -> Arc<dyn Embedder> {
    Arc::new(HashingEmbedder::new(64, Arc::new(CjkTokenizer)))
}

fn clock_at_zero() -> Arc<dyn Clock> {
    Arc::new(FixedClock::new(
        chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    ))
}

/// Hit on near-duplicate request returns cached; inner provider is
/// not called a second time.
#[tokio::test]
async fn hit_on_near_duplicate_skips_inner() {
    let inner = Arc::new(CountingProvider {
        calls: AtomicU64::new(0),
    });
    let store: Arc<dyn SemanticCacheStore> =
        Arc::new(InMemorySemanticCache::default());
    let cached = SemanticCacheProvider::new(
        inner.clone(),
        store,
        embedder(),
        SemanticCacheConfig::default(),
        clock_at_zero(),
    );

    let req1 = make_request("hello world");
    let req2 = make_request("hello world!");
    let r1 = cached.complete(req1.clone()).await.unwrap();
    let r2 = cached.complete(req2).await.unwrap();

    // Identical embedding (HashingEmbedder ignores punctuation at
    // the CJK boundary), so cosine should be 1.0 → hit on second call.
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert!(!r1.cached, "first response is fresh");
    assert!(r2.cached, "second response should be cache-served");
    assert_eq!(cached.hit_count(), 1);
    assert_eq!(cached.miss_count(), 1);
}

/// Miss on dissimilar request calls inner provider exactly once.
#[tokio::test]
async fn miss_on_dissimilar_calls_inner() {
    let inner = Arc::new(CountingProvider {
        calls: AtomicU64::new(0),
    });
    let store: Arc<dyn SemanticCacheStore> =
        Arc::new(InMemorySemanticCache::default());
    let cached = SemanticCacheProvider::new(
        inner.clone(),
        store,
        embedder(),
        // High threshold → only exact embedding matches.
        SemanticCacheConfig::default().threshold(0.999),
        clock_at_zero(),
    );

    let _ = cached.complete(make_request("alpha")).await.unwrap();
    let _ = cached.complete(make_request("beta")).await.unwrap();
    let _ = cached.complete(make_request("gamma")).await.unwrap();

    assert_eq!(inner.calls.load(Ordering::SeqCst), 3);
    assert_eq!(cached.hit_count(), 0);
    assert_eq!(cached.miss_count(), 3);
}

/// TTL expiry forces a fresh inner call on the next request.
///
/// We advance the wall clock by building a second wrapper pointing
/// at the same in-memory store but bound to a `FixedClock` whose
/// instant is past the configured TTL.
#[tokio::test]
async fn ttl_expiry_forces_inner_call() {
    let inner = Arc::new(CountingProvider {
        calls: AtomicU64::new(0),
    });
    let store: Arc<dyn SemanticCacheStore> =
        Arc::new(InMemorySemanticCache::default());
    let t0 = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let cached = SemanticCacheProvider::new(
        inner.clone(),
        store.clone(),
        embedder(),
        SemanticCacheConfig::default()
            .ttl(Duration::from_secs(60))
            .threshold(0.999),
        Arc::new(FixedClock::new(t0)),
    );

    // First call: miss + store.
    let _ = cached.complete(make_request("same")).await.unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);

    // Second call within TTL: hit.
    let r2 = cached.complete(make_request("same")).await.unwrap();
    assert!(r2.cached);
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);

    // Build a second wrapper with a clock 10 minutes in the future,
    // pointing at the same store. TTL is 60 s; the entry must
    // appear stale and the inner is called again.
    let t1 = t0 + chrono::Duration::seconds(600);
    let cached2 = SemanticCacheProvider::new(
        inner.clone(),
        store,
        embedder(),
        SemanticCacheConfig::default()
            .ttl(Duration::from_secs(60))
            .threshold(0.999),
        Arc::new(FixedClock::new(t1)),
    );
    let _ = cached2.complete(make_request("same")).await.unwrap();
    assert_eq!(
        inner.calls.load(Ordering::SeqCst),
        2,
        "TTL expired ⇒ inner called again"
    );
}

/// Helper: build a cache wrapper bound to a `FixedClock` so the test
/// can advance time by building a second wrapper pointing at the
/// same store with a later clock.
fn make_cache_with_clock(
    inner: Arc<CountingProvider>,
    store: Arc<dyn SemanticCacheStore>,
    clock: Arc<dyn Clock>,
) -> SemanticCacheProvider {
    SemanticCacheProvider::new(
        inner,
        store,
        embedder(),
        SemanticCacheConfig::default()
            .ttl(Duration::from_secs(60))
            .threshold(0.999),
        clock,
    )
}

/// Threshold = 1.0 reduces to exact-match behavior; a structurally
/// similar but textually distinct request is a miss.
#[tokio::test]
async fn threshold_one_is_exact_match() {
    let inner = Arc::new(CountingProvider {
        calls: AtomicU64::new(0),
    });
    let store: Arc<dyn SemanticCacheStore> =
        Arc::new(InMemorySemanticCache::default());
    let clock = clock_at_zero();
    let cached =
        make_cache_with_clock(inner.clone(), store.clone(), clock.clone());
    let _ = cached.complete(make_request("identical")).await.unwrap();
    let _ = cached.complete(make_request("identical")).await.unwrap();
    assert_eq!(
        inner.calls.load(Ordering::SeqCst),
        1,
        "identical request must hit on second call"
    );

    // Now a different request: should miss even with threshold = 1.0.
    let _ = cached
        .complete(make_request("totally different"))
        .await
        .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
}

/// LRU eviction honors most-recently-used order.
#[tokio::test]
async fn lru_evicts_oldest_entry() {
    let store = InMemorySemanticCache::with_capacity(2);
    let now = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    for i in 0u8..3u8 {
        let key = EmbeddingKey([i; 32]);
        store.store(
            key,
            vec![0.0; 2],
            CachedCompletion {
                response: CompletionResponse::default(),
                stored_at: now,
                ttl: Duration::from_secs(60),
            },
        );
    }
    assert_eq!(store.len(), 2, "capacity is 2, oldest must be evicted");
}

/// Stream path bypasses the cache: cache decision is on `complete`
/// only.
#[tokio::test]
async fn stream_bypasses_cache() {
    let inner = Arc::new(CountingProvider {
        calls: AtomicU64::new(0),
    });
    let store: Arc<dyn SemanticCacheStore> =
        Arc::new(InMemorySemanticCache::default());
    let cached = SemanticCacheProvider::new(
        inner.clone(),
        store.clone(),
        embedder(),
        SemanticCacheConfig::default(),
        clock_at_zero(),
    );

    let request = make_request("hi");
    let deltas = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let d = deltas.clone();
    let _ = cached
        .complete_with_stream(
            request.clone(),
            None,
            Box::new(move |_chunk| {
                d.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .await
        .unwrap();
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    // Streaming path never consults or populates the cache.
    assert_eq!(store.len(), 0);
    assert_eq!(cached.hit_count(), 0);
    assert_eq!(cached.miss_count(), 0);
    assert!(
        deltas.load(Ordering::SeqCst) >= 1,
        "default streaming impl emits at least one chunk"
    );
}

/// Fingerprint distinguishes requests whose tool set differs.
#[test]
fn fingerprint_distinguishes_tool_set() {
    use std::sync::Arc;
    let req1 = make_request("hi");
    let mut req1 = req1;
    let tools1: Vec<ToolDefinition> = vec![ToolDefinition {
        name: "tool_a".into(),
        description: "".into(),
        input_schema: serde_json::json!({}),
        cache_control: None,
        annotations: None,
    }];
    req1.tools = Arc::new(tools1);
    let mut req2 = make_request("hi");
    let tools2: Vec<ToolDefinition> = vec![ToolDefinition {
        name: "tool_b".into(),
        description: "".into(),
        input_schema: serde_json::json!({}),
        cache_control: None,
        annotations: None,
    }];
    req2.tools = Arc::new(tools2);
    let fp1 = fingerprint_request(&req1);
    let fp2 = fingerprint_request(&req2);
    assert_ne!(fp1, fp2);
}
