# synthia-budget

Token / latency / cost budget enforcement.

A `ModelProvider` / `Tool` wrapper that trips a **budget** when the
cumulative tokens, wall-clock latency, or USD cost exceeds the
configured limit. The wrapper surfaces `BudgetExceeded` events to
the caller; the harness treats them as soft-finalize (or
hard-stop) by configuration.

This crate is **opt-in** (`synthia = { features = ["budget"] }` in
the facade, or `cargo add synthia-budget` for direct consumers).
It stays in the offline tier — no HTTP, no DB, no OTel.

## Surface

- `Budget { tokens: TokenBudget, latency: LatencyBudget, cost:
  CostBudget }`. Each budget has a `used` counter and a `limit`.
- `TokenSource` trait — `synthia_session::TokenMeter` plugs in
  directly; a custom impl works too.
- `PricingTable` trait + `StaticPricingTable` fixture (Anthropic /
  OpenAI 2026 rates). Unpriced models log `model_unpriced` and
  the cost gate is skipped.
- `BudgetProvider { inner, budget, on_exceeded }`.
- `BudgetTool` mirrors the wrap.
- `BudgetExceeded { kind, used, limit }` event published via the
  event sink.

## Wiring

```rust
use std::sync::Arc;

use synthia_budget::{
    Budget, CostBudget, LatencyBudget, PricingTable, StaticPricingTable,
    TokenBudget, BudgetProvider,
};
use synthia_provider::AnthropicProvider;

let pricing = Arc::new(StaticPricingTable::anthropic_2026());
let budget = Budget::builder()
    .tokens(TokenBudget::new(200_000))
    .latency(LatencyBudget::new(Duration::from_secs(120)))
    .cost(CostBudget::new(50_00, pricing)) // 50.00 USD
    .build();
let provider = BudgetProvider::new(inner_provider, budget);
```

## Demo

```
cargo run --example budget_demo -p synthia-budget
```

Expected tail: `OK: budget trip respected`.

## Design notes

- `AtomicU64` for hot counters; `RwLock<Budget>` only for config
  mutation.
- `LatencyBudget` uses `synthia_core::Clock` — never
  `chrono::Utc::now()` directly (passes `make check-clock`).
- Token accumulation hooks into `synthia-session::TokenMeter`
  via the `TokenSource` trait. The default `TokenSource::Meter`
  impl reads from a meter instance.
- The provider wrapper adds zero allocation on the hot path —
  counters are `AtomicU64::fetch_add`.
- Unpriced models: cost gate is skipped with a tracing warning,
  tokens and latency gates still bind.

See `crates/synthia-budget/src/lib.rs` for the full API.