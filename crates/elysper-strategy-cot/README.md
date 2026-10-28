# elysper-strategy-cot

`ChainOfThoughtStrategy` — one tool-free pass with a step-format
instruction appended to the system prompt.

The strategy asks the model to reason in numbered steps and then emit a
final `Answer:` line. It offers **no tools**: a tool call inside the
answer is ignored (and warned about), because the call would otherwise
reach the log as an unanswered call.

## Minimal example

```rust
use elysper_strategy_cot::ChainOfThoughtStrategy;

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
`elysper_harness::ReasoningStrategy`, so it is handed to the agent
where the default strategy would go.

```rust
use elysper_harness::ReasoningStrategy;

let strategy = elysper_strategy_cot::ChainOfThoughtStrategy::default();
let boxed: Box<dyn ReasoningStrategy> = Box::new(strategy);
let _ = boxed;
```

## Dependencies

- **Depends on**: `elysper-harness` (the strategy trait and the agent
  runtime), `elysper-provider` (the request / chunk vocabulary),
  `elysper-session` (the step rows), `elysper-core`.
- **Depended on by**: the `elysper` facade's `strategy-cot` feature. No
  other crate in the workspace depends on it — a deployment picks the
  strategy it wants.

## Track

**Library base (opt-in reasoning strategy).** It is not an agent-visible
tool plugin; it replaces *how* the model is asked, not *what* it can
call.

See `crates/elysper-strategy-cot/src/lib.rs` for the full API.
