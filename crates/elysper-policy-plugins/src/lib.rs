//! # elysper-policy-plugins
//!
//! The shipped **policy plugins** — one module and one feature per
//! policy. A policy either wraps the model provider (cache, resilience,
//! budget) or shapes the reasoning strategy (reflection, chain of
//! thought, plan-execute, best-of-N); the seam it plugs into lives in
//! `elysper-provider` and `elysper-harness`.
//!
//! | feature | module | what the policy does |
//! |---|---|---|
//! | `cache` | [`mod@cache`] | serve a repeated completion from a semantic cache |
//! | `resilience` | [`mod@resilience`] | retry, back off and trip a breaker around a provider |
//! | `budget` | [`mod@budget`] | cap spend, and expose the budget as a tool |
//! | `reflection` | [`mod@reflection`] | critique a result and feed the critique back |
//! | `cot` | [`mod@cot`] | chain-of-thought reasoning strategy |
//! | `plan-execute` | [`mod@plan_execute`] | plan, execute, re-plan |
//! | `best-of-n` | [`mod@best_of_n`] | sample N answers and keep the best-scored one |
//!
//! ## Composing a policy surface
//!
//! A provider wrapper is a `ModelProvider` implementation: hand it the
//! inner provider and it returns the wrapped one. A strategy is a
//! `ReasoningStrategy` the harness can be configured with.
//!
//! ```
//! use elysper_policy_plugins::budget::BudgetProvider;
//! ```
//!
//! ## Dependencies travel with their policy
//!
//! A policy's own dependencies are optional and are opened by that
//! policy's feature: `cache` is the only policy that brings the search
//! registry, and the strategies are the only ones that bring the
//! harness. The default build (no feature) pulls no policy at all.

#[cfg(feature = "best-of-n")]
pub use best_of_n::{
    BestOfNStrategy,
    DEFAULT_JUDGE_PROMPT,
    LlmJudgeScorer,
    LongestAnswer,
};
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
#[cfg(feature = "cot")]
pub use cot::ChainOfThoughtStrategy;
#[cfg(feature = "plan-execute")]
pub use plan_execute::{
    DefaultExecutor,
    DefaultPlanGenerator,
    DefaultRePlanner,
    Plan,
    PlanExecuteConfig,
    PlanExecuteStrategy,
    ReplanDecision,
    Step,
    StepOutcome,
};
#[cfg(feature = "reflection")]
pub use reflection::{
    CriticProvider,
    Critique,
    ReflectionHook,
    ReflectiveOutputTransformer,
    ReflectorConfig,
    Severity,
};
#[cfg(feature = "resilience")]
pub use resilience::{
    ResiliencePolicy,
    ResilientProvider,
    ResilientTool,
    retry_with_backoff,
};

#[cfg(feature = "best-of-n")]
pub mod best_of_n;
#[cfg(feature = "budget")]
pub mod budget;
#[cfg(feature = "cache")]
pub mod cache;
#[cfg(feature = "cot")]
pub mod cot;
#[cfg(feature = "plan-execute")]
pub mod plan_execute;
#[cfg(feature = "reflection")]
pub mod reflection;
#[cfg(feature = "resilience")]
pub mod resilience;
