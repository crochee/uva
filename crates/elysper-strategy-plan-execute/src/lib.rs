//! # elysper-strategy-plan-execute
//!
//! Plan-and-Execute reasoning strategy.
//!
//! A [`ReasoningStrategy`] that decomposes the user prompt into
//! numbered steps, runs each step through the model's tool loop,
//! then re-prompts the model between iterations to either continue
//! or finalize. Reduces tool-call churn on long-horizon tasks vs
//! pure ReAct.
//!
//! ```
//! use std::sync::Arc;
//!
//! use elysper_strategy_plan_execute::{PlanExecuteConfig, PlanExecuteStrategy};
//!
//! let strategy = PlanExecuteStrategy::new(PlanExecuteConfig::default()
//!     .max_steps(20)
//!     .tool_subset(["read", "shell", "write"]));
//! # let _: Arc<dyn elysper_harness::agent::ReasoningStrategy> = Arc::new(strategy);
//! ```
//!
//! ## Layering
//!
//! ```text
//!   ReActAgent::run
//!     └─ PlanExecuteStrategy::run
//!          ├─ PlanGenerator    ← prompt → numbered steps
//!          ├─ Executor::execute(step)   ← per-step tool loop
//!          └─ RePlanner::replan(...)     ← Continue | Finalize | Abort
//! ```
//!
//! The strategy owns the orchestration; the underlying provider /
//! tool registry / cancel token / spawner all come from the
//! [`AgentRuntime`](elysper_harness::AgentRuntime) the harness hands it.

mod defaults;
mod strategy;

pub use defaults::{DefaultExecutor, DefaultPlanGenerator, DefaultRePlanner};
pub use strategy::{
    Plan,
    PlanExecuteConfig,
    PlanExecuteStrategy,
    ReplanDecision,
    Step,
    StepOutcome,
};
pub use elysper_harness::agent::ReasoningStrategy;

#[cfg(test)]
mod tests;
