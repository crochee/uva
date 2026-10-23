//! Facade re-export for [`synthia_strategy_plan_execute`].
//!
//! Enabled by the `strategy-plan-execute` facade feature.

#[cfg(feature = "strategy-plan-execute")]
pub use synthia_strategy_plan_execute::*;
