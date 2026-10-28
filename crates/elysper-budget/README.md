# elysper-budget

Token / latency / cost budget enforcement.

A `ModelProvider` / `Tool` wrapper that trips a **budget** when the
cumulative tokens, wall-clock latency, or USD cost exceeds the
configured limit. The wrapper surfaces `BudgetExceeded` events to
the caller; the harness treats them as soft-finalize (or
hard-stop) by configuration.

This crate is **opt-in** (`elysper = { features = ["budget"] }` in
the facade, or `cargo add elysper-budget` for direct consumers).
It stays in the offline tier — no HTTP, no DB, no OTel.

## Surface

- `Budget { tokens: Option<TokenBudget>, latency:
  Option<LatencyBudget>, cost: Option<CostBudget>,
  on_exceeded: BudgetOutcome }`. Each budget has a `used` counter
  and a `limit`.
- `TokenSource` trait — `elysper_session::TokenMeter` plugs in
  directly; a custom impl works too.
- `PricingTable` trait + `StaticPricingTable` fixture (Anthropic /
  OpenAI 2026 rates). Unpriced models log `model_unpriced` and
  the cost gate is skipped.
- `BudgetProvider { inner, budget }` (+ `trip_count()`,
  `set_budget()`); `on_exceeded` lives on `Budget`, not the wrapper.
- `BudgetTool` mirrors the wrap.
- `BudgetExceeded { kind, used, limit }` event published via the
  event sink.

## Wiring

```rust
use std::sync::Arc;
use std::time::Duration;

use elysper_budget::{
    Budget,
    BudgetProvider,
    CostBudget,
    LatencyBudget,
    PricingTable,
    SharedClock,
    StaticPricingTable,
    TokenBudget,
};
use elysper_provider::ModelProvider;

/// Wrap the provider you already talk to. The wrapper forwards
/// every call to `inner_provider` unchanged and only counts
/// tokens, latency and cost.
fn with_budget(
    inner_provider: Arc<dyn ModelProvider>,
) -> BudgetProvider {
    // `anthropic_2026()` already returns an `Arc`; `CostBudget`
    // takes the trait object.
    let pricing: Arc<dyn PricingTable> =
        StaticPricingTable::anthropic_2026();
    let budget = Budget::builder()
        .tokens(TokenBudget::new(200_000))
        .latency(LatencyBudget::new(
            Duration::from_secs(120),
            Arc::new(SharedClock::system()),
        ))
        .cost(CostBudget::new(5_000, pricing)) // 5_000 cents = 50.00 USD
        .build();
    BudgetProvider::new(inner_provider, budget)
}
```

`inner_provider` is the provider you already hold — anything that
implements `ModelProvider`, e.g. `AnthropicProvider::new(model_config)`
from `elysper-provider` (enable its `anthropic` feature).

## Demo

```console
cargo run --example budget_demo -p elysper-budget
```

Expected tail: `OK: budget trip respected`.

## Design notes

- `AtomicU64` for hot counters; `RwLock<Budget>` only for config
  mutation.
- `LatencyBudget` uses `elysper_core::Clock` — never
  `chrono::Utc::now()` directly (passes `make check-clock`).
- Token accumulation hooks into `elysper-session::TokenMeter`
  via the `TokenSource` trait. The default `TokenSource::Meter`
  impl reads from a meter instance.
- The provider wrapper adds zero allocation on the hot path —
  counters are `AtomicU64::fetch_add`.
- Unpriced models: cost gate is skipped with a tracing warning,
  tokens and latency gates still bind.

See `crates/elysper-budget/src/lib.rs` for the full API.
