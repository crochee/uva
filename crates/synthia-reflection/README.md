# synthia-reflection

Post-execution reflection / self-critique hook.

A `synthia_steering::HookMap` (built with `ReflectionHook::hook_map()`,
installed on `Steering::hooks`) whose `after_tool` handler, after every
`every_n_steps` tool calls, asks a **critic model** (cheap + fast) to
critique the running trajectory. If the critique flags drift (severity ≥
`Warn`), the hook stages a [`Hint`] for the next prompt that nudges the
model back on track. Self-Refine-style.

This crate is **opt-in** (`synthia = { features = ["reflection"] }`
in the facade, or `cargo add synthia-reflection` for direct
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

use synthia_provider::AnthropicProvider;
use synthia_steering::Steering;
use synthia_reflection::{ReflectorConfig, ReflectionHook};

let critic = Arc::new(AnthropicProvider::from_env()?);
let hook = Arc::new(ReflectionHook::new(
    ReflectorConfig::default()
        .critic_provider(critic)
        .every_n_steps(3),
    Arc::new(SystemSharedClock::system()),
));
let steering = Steering::default_policy(root)
    .add_hooks(hook.hook_map());
```

## Demo

```
cargo run --example reflection_demo -p synthia-reflection
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

See `crates/synthia-reflection/src/lib.rs` for the full API.