# elysper-policy-plugins

The shipped **policy plugins** — one module and one feature per policy:

| feature | module | what the policy does |
|---|---|---|
| `cache` | [`cache`](src/cache/README.md) | serve a repeated completion from a semantic cache |
| `resilience` | [`resilience`](src/resilience/mod.rs) | retry, back off and trip a breaker around a provider |
| `budget` | [`budget`](src/budget/README.md) | cap spend, and expose the budget as a tool |
| `reflection` | [`reflection`](src/reflection/mod.rs) | critique a result and feed the critique back |
| `cot` | [`cot`](src/cot/README.md) | chain-of-thought reasoning strategy |
| `plan-execute` | [`plan_execute`](src/plan_execute/mod.rs) | plan, execute, re-plan |
| `best-of-n` | [`best_of_n`](src/best_of_n/README.md) | sample N answers and keep the best-scored one |

A policy either wraps the model provider (cache, resilience, budget) or
shapes the reasoning strategy (reflection, chain of thought,
plan-execute, best-of-N). The seam it plugs into lives in
`elysper-provider` and `elysper-harness`; this crate ships the
implementations.

> Strategy seam: [`SEAMS.md`](../../SEAMS.md). Each policy's full API
> is in its module docs (`src/<policy>/mod.rs`).

## Usage

```toml
[dependencies]
elysper-policy-plugins = { version = "0.1", features = ["resilience"] }
```

```rust
use elysper_policy_plugins::resilience::ResilientProvider;

// A provider wrapper: hand it the inner provider and it returns the
// wrapped one.
let provider = ResilientProvider::new(inner);
```

Through the facade, the same policies are one feature away:

```toml
elysper = { version = "0.1", features = ["resilience", "budget"] }
```

## Dependencies travel with their policy

A policy's own dependencies are optional and are opened by that
policy's feature: `cache` is the only policy that brings the search
registry, and the strategies are the only ones that bring the harness.
The default build (no feature) pulls no policy at all.

## Examples

| Example | Shows |
| :--- | :--- |
| `semantic_cache_demo` | 语义缓存命中与统计 |
| `resilience_demo` | 重试、退避与熔断 |
| `budget_demo` | 预算封顶与超限处理 |
| `reflection_demo` | 批判结果并回灌 |
| `chain_of_thought` | 思维链策略 |
| `plan_execute_demo` | 计划—执行—重规划 |
| `best_of_n_demo` | 多路采样择优 |

跑： `cargo run --example <name> -p elysper-policy-plugins --features <policy>`。

## CI 契约

- 离线（无 HTTP / DB / OTel）：`make check-mvp-deps` 断言全组合；
- `cargo test -p elysper-policy-plugins --all-features` 绿；
- `RUSTDOCFLAGS="-D warnings" cargo doc --no-deps -p elysper-policy-plugins
  --all-features` 绿（门禁见 `AGENTS.md` §3.6）。
