//! # uva-policy-plugins
//!
//! The shipped **policy plugins** — one module and one feature per
//! policy. A policy wraps the model provider (cache, resilience,
//! budget) or its tools; the seam it plugs into lives in
//! `uva-provider` and `uva-tool`.
//!
//! | feature | module | what the policy does |
//! |---|---|---|
//! | `cache` | [`mod@cache`] | serve a repeated completion from a semantic cache |
//! | `resilience` | [`mod@resilience`] | retry, back off and trip a breaker around a provider |
//! | `budget` | [`mod@budget`] | cap spend, and expose the budget as a tool |
//!
//! ## Composing a policy surface
//!
//! A provider wrapper is a `ModelProvider` implementation: hand it the
//! inner provider and it returns the wrapped one.
//!
//! ## Dependencies travel with their policy
//!
//! A policy's own dependencies are optional and are opened by that
//! policy's feature: `cache` is the only policy that brings the search
//! registry, and `resilience` / `budget` are the ones that bring the
//! tool registry. The default build (no feature) pulls no policy at all.

#[cfg(feature = "budget")]
pub use budget::{
    Budget,
    BudgetExceeded,
    BudgetKind,
    BudgetOutcome,
    BudgetProvider,
    BudgetTool,
    PricingTable,
    StaticPricingTable,
};
#[cfg(feature = "cache")]
pub use cache::{
    CachedCompletion,
    InMemorySemanticCache,
    SemanticCacheConfig,
    SemanticCacheProvider,
    SemanticCacheStats,
    SemanticCacheStore,
};
#[cfg(feature = "resilience")]
pub use resilience::{
    ResiliencePolicy,
    ResilientProvider,
    ResilientTool,
    retry_with_backoff,
};

#[cfg(feature = "budget")]
pub mod budget;
#[cfg(feature = "cache")]
pub mod cache;
#[cfg(feature = "resilience")]
pub mod resilience;
