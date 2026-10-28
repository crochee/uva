# elysper-provider

LLM provider abstraction: `ModelProvider` trait plus Anthropic /
OpenAI-compatible adapters. Each adapter is gated behind its own
feature flag — this crate's `anthropic` / `openai` (both on by
default; the facade `elysper` renames them `provider-anthropic` /
`provider-openai`) — and the `ModelProvider` trait itself ships with
no HTTP dependency.

> 详细 wire types 与公开 API 见 [`elysper-provider/src/lib.rs`](src/lib.rs)；
> 7 大组件的可替换点见 [`SEAMS.md`](../../SEAMS.md) §1。

## 用法

```rust
use elysper_provider::{AnthropicProvider, ModelConfig};

let provider = AnthropicProvider::new(ModelConfig {
    name: "claude-sonnet-4-5".to_string(),
    provider: "anthropic".to_string(),
    context_window: 200_000,
    max_output_tokens: 8_192,
    supports_tools: true,
    supports_streaming: true,
    supports_reasoning: false,
})
.with_api_key(&std::env::var("ANTHROPIC_API_KEY")?);
```

## feature 与依赖面

`default = ["anthropic", "openai"]`：默认打开两个出厂 adapter，每个
adapter 都带 `reqwest`、`tokio` 和 `tokio-util`。只要 trait 与消息类型
（自己实现 provider 的消费方）就把默认 feature 关掉：

```toml
elysper-provider = { version = "0.1", default-features = false }
```

harness 的参考消费方（`docs/examples/harness-only-consumer`）就是这么写的，
所以那棵 `cargo tree` 里没有 tokio。

## CI 契约

- `cargo test -p elysper-provider --lib` 绿；
- 默认 feature（`anthropic` / `openai`）才带 `reqwest` / `tokio`；
  `default-features = false` 后本 crate 零 HTTP 依赖 —— facade 的 MVP
  子集同样以这个姿势取 provider，`make check-mvp-deps` 断言它不带
  HTTP；
- HTTP adapter 仅在 `anthropic` / `openai` feature 下出现。
