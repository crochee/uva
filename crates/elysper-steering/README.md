# elysper-steering

Steering layer: `Guard` / `HookMap` / `Hint` / `Tracker` /
`OutputTransformer`. A `Steering` is a container that the harness
consults each turn; you compose it with `Steering::noop()` +
`.add_guard(g)` + `.add_hooks(m)` + ….

> 详细 seam 与公开 API 见 [`elysper-steering/src/lib.rs`](src/lib.rs)；
> 7 大组件的可替换点见 [`SEAMS.md`](../../SEAMS.md) §1。

## 用法

```rust
use elysper_steering::{Steering, Guard};

let steering = Steering::default_policy(root)
    .add_guard(Guard::max_tool_calls(20))
    .add_hint("Always cite file paths as workspace-relative.");
```

## CI 契约

- `cargo test -p elysper-steering --lib` 绿；
- 默认 features 零运行时依赖（`make check-no-runtime`）。
