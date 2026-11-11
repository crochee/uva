//! Facade re-export for [`elysper_policy_plugins::plan_execute`].
//!
//! Enabled by the `strategy-plan-execute` facade feature.

#[cfg(feature = "strategy-plan-execute")]
pub use elysper_policy_plugins::plan_execute::*;
