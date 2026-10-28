# elysper-eval

Evaluation framework for elysper agents: suites, sync / async metrics,
runner, judge + schema validation, JSON / CSV export.

> 公开 API 见 [`elysper-eval/src/lib.rs`](src/lib.rs)。

## 用法

```rust
use elysper_eval::{Suite, Metric, Runner};

let suite = Suite::new("smoke")
    .add_case("greet", case)
    .add_metric(Metric::exact_match("answer"));

let report = Runner::new(suite).run(&provider).await?;
report.write_json("/tmp/report.json")?;
```

## CI 契约

- `cargo test -p elysper-eval --lib` 绿；
- 默认 features 零外部依赖（`make check-mvp-deps`）。
