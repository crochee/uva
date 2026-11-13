//! # uva-policy-plugins — the `budget` module
//!
//! Token / latency / cost budget enforcement for `ModelProvider`
//! and `Tool`.
//!
//! A [`BudgetProvider`] / [`BudgetTool`] wraps the inner and trips a
//! budget when the cumulative tokens, wall-clock latency, or USD
//! cost exceeds the configured limit. The wrapper surfaces a
//! [`BudgetExceeded`] event to the harness, which finalizes the run
//! as soft-finalize (or hard-stop) per [`BudgetOutcome`].
//!
//! ```
//! use std::{sync::Arc, time::Duration};
//!
//! use uva_policy_plugins::budget::{
//!     Budget,
//!     BudgetProvider,
//!     CostBudget,
//!     LatencyBudget,
//!     SharedClock,
//!     StaticPricingTable,
//!     TokenBudget,
//! };
//!
//! // `StaticPricingTable::anthropic_2026()` already hands back an
//! // `Arc`; `CostBudget` wants the trait object.
//! let pricing = StaticPricingTable::anthropic_2026();
//! let budget = Budget::builder()
//!     .tokens(TokenBudget::new(200_000))
//!     .latency(LatencyBudget::new(
//!         Duration::from_secs(120),
//!         Arc::new(SharedClock::system()),
//!     ))
//!     .cost(CostBudget::new(5_000, pricing))
//!     .build();
//! // A test double so the example runs without an HTTP adapter;
//! // wire your own provider here.
//! let provider: Arc<dyn uva_provider::ModelProvider> =
//!     Arc::new(uva_provider::traits_stub::ModelProviderStub::default());
//! let _ = BudgetProvider::new(provider, budget);
//! ```
//!
//! ```no_run
//! use uva_policy_plugins::budget::BudgetProvider;
//! ```

pub mod model;
pub mod pricing;
mod provider;
mod tool;

pub use model::{
    Budget,
    BudgetExceeded,
    BudgetKind,
    BudgetOutcome,
    CostBudget,
    LatencyBudget,
    TokenBudget,
    TokenSource,
};
pub use pricing::{PricingTable, StaticPricingTable};
pub use provider::BudgetProvider;
pub use tool::BudgetTool;
pub use uva_core::{Clock, FixedClock, SharedClock};

/// The README, **compiled**.
///
/// The `rust` fences in [`README.md`](../README.md) are doctests,
/// so a wiring recipe that stops compiling fails
/// `cargo test -p uva-policy-plugins --all-features`.
#[cfg(doctest)]
#[doc = include_str!("README.md")]
pub struct Readme;

#[cfg(test)]
mod tests;
