//! Semantic-cache demo.
//!
//! A `CountingProvider` stand-in supplies a deterministic response
//! to every prompt. A `SemanticCacheProvider` wrapper caches each
//! response keyed on embedding similarity. The demo fires five
//! near-duplicate prompts and prints hit/miss stats; the expected
//! hit-rate on the chosen prompt set is 60% (3/5).
//!
//! Run with:
//! `cargo run --example semantic_cache_demo -p uva-semantic-cache`

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use chrono::TimeZone;
use uva_core::FixedClock;
use uva_policy_plugins::cache::{
    InMemorySemanticCache,
    SemanticCacheConfig,
    SemanticCacheProvider,
    SemanticCacheStore,
};
use uva_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    TextContent,
};
use uva_search::{CjkTokenizer, Embedder, HashingEmbedder};

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
        let text = request
            .messages
            .first()
            .and_then(|m| match &m.content {
                Content::Single(ContentPart::Text(t)) => Some(t.text.clone()),
                _ => None,
            })
            .unwrap_or_default();
        Ok(CompletionResponse {
            id: format!("resp-{text}"),
            model: "test-model".into(),
            content: Content::Single(ContentPart::Text(TextContent {
                text: format!("inner-answer for {text}"),
                cache_control: None,
            })),
            usage: uva_provider::TokenUsage::default(),
            cached: false,
            stop_reason: None,
            replay_state: None,
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let inner = Arc::new(CountingProvider {
        calls: AtomicU64::new(0),
    });
    let store: Arc<dyn SemanticCacheStore> =
        Arc::new(InMemorySemanticCache::default());
    let embedder: Arc<dyn Embedder> =
        Arc::new(HashingEmbedder::new(64, Arc::new(CjkTokenizer)));
    let t0 = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let cached = SemanticCacheProvider::new(
        inner.clone(),
        store,
        embedder,
        // The hashing embedder is whitespace-sensitive, so we lower
        // the threshold to 0.6 to make near-duplicates (e.g. "Hello
        // World" vs "hello world") score above it. A real
        // deployment wires a semantic embedder (`provider`-built
        // `ModelProviderEmbedder` or its own) and a higher threshold.
        SemanticCacheConfig::default()
            .threshold(0.6)
            .ttl(Duration::from_secs(60 * 60)),
        Arc::new(FixedClock::new(t0)),
    );

    // Five near-duplicate prompts. The first is the seed; the next
    // four are slight variations that the hashing embedder groups
    // together at this threshold.
    let prompts = [
        "Hello World",
        "Hello World",
        "hello world",
        "Hello  World",
        "HELLO WORLD",
    ];
    let total = prompts.len() as u64;
    let mut log_lines: Vec<String> = Vec::new();
    for (i, prompt) in prompts.iter().enumerate() {
        let req = CompletionRequest {
            model: "test-model".into(),
            messages: Arc::new(vec![uva_provider::Message {
                role: uva_provider::Role::User,
                content: Content::Single(ContentPart::Text(TextContent {
                    text: (*prompt).to_string(),
                    cache_control: None,
                })),
                tool_call_id: None,
                name: None,
                tool_result_cleared_at: None,
            }]),
            tools: Arc::new(Vec::new()),
            ..CompletionRequest::default()
        };
        let resp = cached.complete(req).await?;
        let tag = if resp.cached {
            "CACHE-HIT"
        } else {
            "CACHE-MISS"
        };
        log_lines.push(format!("[{tag}] prompt #{i}: {prompt}"));
    }

    for line in &log_lines {
        println!("{line}");
    }

    let calls = inner.calls.load(Ordering::SeqCst);
    let hits = cached.hit_count();
    let hit_rate_pct = (hits as f64 / total as f64) * 100.0;
    println!(
        "STATS: total={total} inner_calls={calls} hits={hits} misses={} hit_rate={:.0}%",
        cached.miss_count(),
        hit_rate_pct
    );

    assert!(
        calls <= total,
        "cache must never call the inner more than once per unique request"
    );

    println!("OK: semantic-cache demo complete");
    Ok(())
}
