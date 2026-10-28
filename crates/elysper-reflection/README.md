# elysper-reflection

Post-execution reflection / self-critique hook.

A `elysper_steering::HookMap` (built with `ReflectionHook::hook_map()`,
installed on `Steering::hooks`) whose `after_tool` handler, after every
`every_n_steps` tool calls, asks a **critic model** (cheap + fast) to
critique the running trajectory. If the critique flags drift (severity ≥
`Warn`), the hook stages a [`Hint`] for the next prompt that nudges the
model back on track. Self-Refine-style.

This crate is **opt-in** (`elysper = { features = ["reflection"] }`
in the facade, or `cargo add elysper-reflection` for direct
consumers). It stays in the offline tier — no HTTP, no DB, no OTel
— and is covered by `make check-mvp-deps`.

## Surface

- `ReflectorConfig { critic_provider, every_n_steps,
  max_reflection_tokens }`.
- `ReflectionHook { config, clock, last_reflected_step }` produces a
  `HookMap` via `hook_map()`.
- `Critique { severity: Severity, reason, suggested_hint }` returned
  by the critic.
- `ReflectiveOutputTransformer` rewrites the final answer into a
  "self-critique + revised answer" pair.

## Wiring

```rust
use std::sync::Arc;

use elysper_core::SharedClock;
use elysper_provider::ModelProvider;
use elysper_reflection::{ReflectorConfig, ReflectionHook};
use elysper_steering::Steering;

// The critic is the consumer's own provider: this crate takes the
// `ModelProvider` trait (`elysper-provider` with
// `default-features = false`), not one of its HTTP adapters.
let critic: Arc<dyn ModelProvider> = /* your cheap, fast critic model */;
let hook = Arc::new(ReflectionHook::new(
    ReflectorConfig::default()
        .critic_provider(critic)
        .every_n_steps(3),
    Arc::new(SharedClock::system()),
));
let steering = Steering::default_policy(root)
    .add_hooks(hook.hook_map());
```

## Demo

```bash
cargo run --example reflection_demo -p elysper-reflection
```

Expected tail: `OK: drift caught at step 2`.

## Design notes

- The `after_tool` handler runs on the agent loop's hook dispatch; the
  critic call is async via `ModelProvider::complete`. Cancellation
  propagates through the agent's `CancelToken`.
- `every_n_steps = 1` invokes on every tool call; bounded history
  snapshot of the last 4 tool results keeps the critic prompt
  small.
- `last_reflected_step` is an `AtomicU32`; tests inject
  `FixedClock` for deterministic steps.

See `crates/elysper-reflection/src/lib.rs` for the full API.
