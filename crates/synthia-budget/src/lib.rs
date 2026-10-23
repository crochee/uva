//! # synthia-budget
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
//! use synthia_budget::{
//!     Budget,
//!     BudgetProvider,
//!     CostBudget,
//!     LatencyBudget,
//!     StaticPricingTable,
//!     TokenBudget,
//! };
//!
//! let pricing = Arc::new(StaticPricingTable::anthropic_2026());
//! let budget = Budget::builder()
//!     .tokens(TokenBudget::new(200_000))
//!     .latency(LatencyBudget::new(Duration::from_secs(120)))
//!     .cost(CostBudget::new(50_00, pricing))
//!     .build();
//! # let _: Arc<dyn synthia_provider::ModelProvider> = Arc::new(
//! #     synthia_provider::traits_stub::ModelProviderStub::default(),
//! # );
//! let _ = BudgetProvider::new(
//!     Arc::new(synthia_provider::traits_stub::ModelProviderStub::default()),
//!     budget,
//! );
//! ```

pub mod budget;
pub mod pricing;
mod provider;
mod tool;

pub use budget::{
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
pub use synthia_core::{Clock, FixedClock, SharedClock};
pub use tool::BudgetTool;

#[cfg(test)]
mod tests;
