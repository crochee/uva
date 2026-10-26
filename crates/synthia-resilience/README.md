# synthia-resilience

Circuit breaker + adaptive backoff + jitter per provider or tool.

A single seam that wraps any [`ModelProvider`] or [`Tool`] with a
`classify → retry → breaker` pipeline. Coexists with
`synthia-provider`'s transport-level retry by lifting the policy to
the user-visible surface — per-target, hot-reloadable, runtime-neutral.

## Surface

- `ResiliencePolicy` — `max_attempts`, `initial_backoff`,
  `max_backoff`, `jitter`, `breaker`, `classify`.
- `Jitter::None | Full | Decorrelated` (matches `aws-sdk` patterns).
- `BreakerConfig { failure_threshold, half_open_after,
  success_threshold_in_half_open }`.
- `ResilientProvider { inner: Arc<dyn ModelProvider>, policy: Arc<RwLock<
  ResiliencePolicy>>, breaker: Arc<Mutex<BreakerState>>, clock, sleeper }`.
- `ResilientTool` mirrors the wrap for `Arc<dyn Tool>`.
- `BreakerState` lives in `synthia-core::breaker` (≤120 LOC, runtime
  -neutral).
- The retry backoff waits through `synthia-core`'s `Sleeper` seam
  (`with_sleeper`); the default waits on a std thread, so the crate
  names no runtime.
- Default classifier maps `ProviderError::RateLimited` /
  `Overloaded` / `Http(5xx)` → `Retry`, `ProviderError::Auth` →
  `GiveUp`.

## Wiring

```rust
use std::sync::Arc;
use std::time::Duration;

use synthia_provider::AnthropicProvider;
use synthia_resilience::{
    BreakerConfig, Jitter, ResiliencePolicy, ResilientProvider,
};

let provider = Arc::new(AnthropicProvider::from_env()?);
let resilient = ResilientProvider::new(
    provider,
    ResiliencePolicy::default()
        .max_attempts(5)
        .breaker(BreakerConfig {
            failure_threshold: 5,
            half_open_after: Duration::from_secs(15),
            ..Default::default()
        }),
    Arc::new(synthia_core::SystemClock),
);
// On a tokio host, install that runtime's sleeper instead of the
// default std-thread wait:
//   .with_sleeper(Arc::new(synthia_harness_tokio::TokioSleeper))
```

## Demo

```
cargo run --example resilience_demo -p synthia-resilience
```

Expected tail: `OK: recovered after 5 retries`.

## Design notes

- Order matters at composition time: `memo_inside_resilient` so the
  breaker sees real failures, not cache hits.
- Per-tool breaker state keyed by tool name — different tools do not
  share state.
- The retry delay goes through `synthia-core`'s `Sleeper` seam: the
  default waits on a std thread, so the crate works under any
  executor; a host on tokio installs `TokioSleeper`.
- `RwLock<ResiliencePolicy>` for hot-reload; the next call observes
  the new limits.
- `FixedClock` for tests.

[`ModelProvider`]: https://docs.rs/synthia-provider/latest/synthia_provider/traits/trait.ModelProvider.html
[`Tool`]: https://docs.rs/synthia-tool/latest/synthia_tool/traits/trait.Tool.html

See `crates/synthia-resilience/src/lib.rs` for the full API.