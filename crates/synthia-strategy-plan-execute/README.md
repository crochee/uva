# synthia-strategy-plan-execute

Plan-and-Execute reasoning strategy.

A [`ReasoningStrategy`] that decomposes the user prompt into
numbered steps, runs each step through the model's tool loop, then
re-prompts the model between iterations to either continue or
finalize. Reduces tool-call churn on long-horizon tasks vs pure
ReAct.

## Surface

- `PlanExecuteStrategy { planner, executor, re_planner, max_steps,
  reflect_every }` — each role is a small trait so callers can plug
  custom implementations.
- `PlanGenerator::generate(input, ctx) -> Plan { steps: Vec<Step> }`
  — default uses the same provider as the host agent with a fixed
  plan-extraction prompt.
- `Executor::execute(step, runtime) -> StepOutcome` — default drives
  the host provider with the step's tool subset and runs the tool
  loop until the model emits a final answer or the step's iteration
  cap is reached.
- `RePlanner::replan(plan_so_far, outcomes, ctx) -> ReplanDecision
  { Continue, Finalize, Abort }`.

## Wiring

```rust
use std::sync::Arc;

use synthia_harness::ReActAgent;
use synthia_strategy_plan_execute::{
    PlanExecuteConfig, PlanExecuteStrategy,
};
use synthia_tool::ToolRegistry;

let strategy = PlanExecuteStrategy::new(PlanExecuteConfig::default()
    .max_steps(20)
    .reflect_every(5)
    .tool_subset(["read", "shell", "write"]));
let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
    .with_strategy(Arc::new(strategy));
```

## Demo

```
cargo run --example plan_execute_demo -p synthia-strategy-plan-execute
```

Expected tail: `OK: 3/3 steps succeeded`.

## Design notes

- Concurrency: each step is a sequential tool loop; the strategy
  emits `SystemEvent::Progress { step, total }` for step
  boundaries and `SessionEnded` for the terminal event.
- Step pool is bounded by `max_steps` (default 20) so memory is
  bounded.
- Cancellation propagates through `AgentRuntime::cancel` between
  steps.
- `ToolRestriction` (existing) is reused to enforce the step's tool
  subset.

[`ReasoningStrategy`]: https://docs.rs/synthia-harness/latest/synthia_harness/agent/strategy/trait.ReasoningStrategy.html

See `crates/synthia-strategy-plan-execute/src/lib.rs` for the full API.