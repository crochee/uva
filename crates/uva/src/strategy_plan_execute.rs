//! Facade re-export for [`uva_policy_plugins::plan_execute`].
//!
//! Enabled by the `strategy-plan-execute` facade feature.

#[cfg(feature = "strategy-plan-execute")]
pub use uva_policy_plugins::plan_execute::*;
