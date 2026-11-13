# uva-policy-plugins — the chain-of-thought strategy

`ChainOfThoughtStrategy` — one tool-free pass with a step-format
instruction appended to the system prompt.

The strategy asks the model to reason in numbered steps and then emit a
final `Answer:` line. It offers **no tools**: a tool call inside the
answer is ignored (and warned about), because the call would otherwise
reach the log as an unanswered call.

## Minimal example

```rust
use uva_policy_plugins::cot::ChainOfThoughtStrategy;

let strategy = ChainOfThoughtStrategy::new(5);
let instruction = strategy.instruction();
assert!(instruction.contains("Step N:"));

// The labelled steps in an answer, in order.
let steps = ChainOfThoughtStrategy::parse_steps(
    "Step 1: read the file\nStep 2: summarise it\nAnswer: done",
);
assert_eq!(steps.len(), 2);
assert!(steps[0].contains("read the file"));
```

Wiring it into a run: the strategy implements
`uva_harness::ReasoningStrategy`, so it is handed to the agent
where the default strategy would go.

```rust
use uva_harness::ReasoningStrategy;

let strategy = uva_policy_plugins::cot::ChainOfThoughtStrategy::default();
let boxed: Box<dyn ReasoningStrategy> = Box::new(strategy);
let _ = boxed;
```

## Dependencies

- **Depends on**: `uva-harness` (the strategy trait and the agent
  runtime), `uva-provider` (the request / chunk vocabulary),
  `uva-session` (the step rows), `uva-core`.
- **Depended on by**: the `uva` facade's `strategy-cot` feature. No
  other crate in the workspace depends on it — a deployment picks the
  strategy it wants.

## Track

**Library base (opt-in reasoning strategy).** It is not an agent-visible
tool plugin; it replaces *how* the model is asked, not *what* it can
call.

See `crates/uva-policy-plugins/src/cot/mod.rs` for the full API.
