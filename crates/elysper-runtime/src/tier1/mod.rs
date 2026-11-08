//! The tier-1 provider wrapping chain (semantic cache → resilience →
//! budget), applied in inner-to-outer order on the default provider.
//! Each wrap is opt-in and reads its own config section.

use std::sync::Arc;

use elysper_budget::{
    Budget,
    BudgetProvider,
    CostBudget,
    LatencyBudget,
    PricingTable,
    StaticPricingTable,
    TokenBudget,
};
use elysper_core::{Clock, SharedClock, sleeper::Sleeper};
use elysper_provider::traits::ModelProvider;
use elysper_resilience::{
    BreakerConfig,
    Jitter,
    ResiliencePolicy,
    ResilientProvider,
};
use elysper_search::{CjkTokenizer, Embedder, HashingEmbedder};
use elysper_semantic_cache::{
    InMemorySemanticCache,
    SemanticCacheConfig,
    SemanticCacheProvider,
    SemanticCacheStore,
};

use crate::model_adapter::ModelAdapterConfig;

/// Compose the tier-1 wraps in inner-to-outer order on a provider
/// handle. Each wrap is opt-in and reads its own config section; the
/// function is a no-op when none of the sections are enabled.
///
/// The sleeper is injected — a tokio host passes its runtime's
/// sleeper so a retry backoff costs a timer entry, not the OS thread
/// the core `ThreadSleeper` default spends.
pub fn wrap_tier1_providers(
    inner: Arc<dyn ModelProvider>,
    config: &ModelAdapterConfig,
    clock: SharedClock,
    sleeper: Arc<dyn Sleeper>,
) -> Arc<dyn ModelProvider> {
    // 1. Semantic cache (innermost, so the breaker sees real errors).
    let cache = config.cache.as_ref().filter(|section| section.enabled);
    let mut next: Arc<dyn ModelProvider> = match cache {
        Some(cache) => {
            let store: Arc<dyn SemanticCacheStore> =
                Arc::new(InMemorySemanticCache::default());
            let embedder: Arc<dyn Embedder> =
                Arc::new(HashingEmbedder::new(64, Arc::new(CjkTokenizer)));
            Arc::new(SemanticCacheProvider::new(
                inner.clone(),
                store,
                embedder,
                SemanticCacheConfig::default()
                    .threshold(cache.threshold)
                    .ttl(std::time::Duration::from_secs(cache.ttl_secs)),
                clock_arc(&clock),
            ))
        }
        None => Arc::clone(&inner),
    };

    // 2. Resilience (breaker + adaptive backoff).
    if let Some(resilience) =
        config.resilience.as_ref().filter(|section| section.enabled)
    {
        next = Arc::new(
            ResilientProvider::new(
                next,
                ResiliencePolicy::default()
                    .max_attempts(resilience.max_attempts)
                    .breaker(BreakerConfig {
                        failure_threshold: resilience.failure_threshold,
                        half_open_after: std::time::Duration::from_secs(
                            resilience.half_open_after_secs,
                        ),
                        success_threshold_in_half_open: 1,
                    })
                    .jitter(Jitter::Full),
                clock_arc(&clock),
            )
            .with_sleeper(Arc::clone(&sleeper)),
        );
    }

    // 3. Budget (last gate; trips on token / cost / latency).
    if let Some(budget) =
        config.budget.as_ref().filter(|section| section.enabled)
    {
        let pricing: Arc<dyn PricingTable> =
            StaticPricingTable::anthropic_2026();
        let budget = Budget::builder()
            .tokens(TokenBudget::new(budget.tokens_limit))
            .latency(LatencyBudget::new(
                std::time::Duration::from_secs(budget.latency_secs),
                clock_arc(&clock),
            ))
            .cost(CostBudget::new(budget.cost_cents_limit, pricing))
            .build();
        next = Arc::new(BudgetProvider::new(next, budget));
    }

    next
}

fn clock_arc(clock: &SharedClock) -> Arc<dyn Clock> {
    Arc::new(clock.clone()) as Arc<dyn Clock>
}

#[cfg(test)]
mod tests;
