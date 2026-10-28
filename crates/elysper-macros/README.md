# elysper-macros

Procedural macros for elysper: `#[derive(Tool)]` and
`#[tool(name, description, mode)]` — generate a `elysper_tool::Tool`
impl from a struct + inherent `execute` body.

> 公开 API 见 [`elysper-macros/src/lib.rs`](src/lib.rs)。

## 用法

```rust
use elysper_macros::Tool;

#[derive(Tool, Default)]
#[tool(name = "echo", description = "Echoes the input")]
pub struct Echo;

#[async_trait::async_trait]
impl elysper_tool::Tool for Echo {
    async fn execute(&self, ctx: elysper_tool::ToolContext) -> elysper_tool::ToolResult { /* ... */ }
}
```

## CI 契约

- `cargo test -p elysper-macros --lib` 绿；
- proc-macro crate 不出现在 lib 公共 API 之外。
