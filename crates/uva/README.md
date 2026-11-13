# uva

The facade: every piece behind feature flags plus a curated `prelude`,
so a consumer can depend on one crate and assemble from zero.

> 公开 API 与 4 步 MVP guide 见
> [`MINIMAL.md`](../../MINIMAL.md)；7 大组件的可替换点见
> [`SEAMS.md`](../../SEAMS.md)。

## 用法

```toml
[dependencies]
uva = { version = "0.1", features = ["provider-anthropic", "tool-read", "harness"] }
```

```rust
use uva::prelude::*;
```

## CI 契约

- `cargo test -p uva --lib` 绿；
- 零逻辑 facade：`lib.rs` / `*.rs` 只做 `pub use`，不写 `fn` / `impl`
  （`make check-pub-surface` 棘轮化）；
- 默认 features 零 HTTP / DB / OTel 依赖（`make check-mvp-deps`）。

## 插件内核

`kernel` feature 原样重导出 `uva-kernel`：`Plugin` / `PluginManifest`
/ `mount_all` / `MountHandle` / `Context` / `EffectHandle` /
`PluginState`，以及 `ui.` 与核心服务键的命名空间边界。内置插件 crate
（`uva-tool` / `-skill` / `-mcp` / `-eval` / `-scheduler` /
`-attachment` …）各自发布 `Plugin`，消费方用与 app-server 相同的
`mount_all` 装配。

## 装配（runtime）

非默认 `runtime` feature 重导出 `uva-runtime`：配置驱动的
`ModelAdapterServicePlugin`（发布 `llm` catalogue + `llm.default`
tier-1 包装默认句柄）与 `AgentLoopServicePlugin`（apply 期解析
服务后发布 `agentLoop` 工厂），以及 `ProviderCatalogue` /
`wrap_tier1_providers` / `ModelAdapterConfig` / `server_agent_loop`
等装配 API。app-server 的插件脊柱由它承载；开该 feature 会拉起
provider 的 anthropic / openai adapter（reqwest），故不入默认集合。
