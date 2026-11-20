# uva-hook-plugins

hook 实现族 crate：一模块一 feature，每个模块实现
[`uva-hook`](../uva-hook/README.md) 的拦截 trait。默认构建不拉起任何
hook（`default = []`）。

| feature | 模块 | 实现 | 作用 |
|---|---|---|---|
| `reflection` | `reflection` | `Hook` | 定期让便宜的 critic 模型批判轨迹；severity ≥ `Warn` 时把 hint 回灌下一轮 |
| `redact` | `redact` | `ResultTransformer` | 在循环提交工具结果前脱敏出参；默认规则覆盖常见凭据形状，也可整组替换 |
| `traffic-light` | `traffic_light` | `Gate` | 按工具自报的 `Risk` 放行 / 阻断；阻断走 `Block` 短路 |

内建五个 steering guard 不在此族：它们住在 `uva-steering::guards`，
循环启动时经 `Host::default_policy` 装一次——不是可挂载的 hook。

## redact：改写留下溯源行

`RedactTransformer` 改写的是**提交前**的聚合出参：循环随后写的
`tool_result` 行与模型读到的内容一致。循环另外写一条
`tool_result_transformed` 行，`transformers` 列出 `redact`，
`before` / `after` 各是整份出参的 sha256——provenance（谁在提交前改了
模型将读到的结果），不是 reconciliation（不回滚、不并存两份）。

默认规则见 [`src/redact/mod.rs`](src/redact/mod.rs) 的模块文档：
`Bearer`（前缀含一个空格）、`sk-`、`ghp_`、`github_pat_`、`AKIA`、`ASIA`、`xoxb-`，
每条都要求 token 达到最小长度且在 token 边界上。传
`RedactConfig::new([...])` 可整组替换；`RedactConfig::default()` 是
文档化的那组。除文本 part 外，`ToolOutput::metadata` 里的字符串值也会
递归脱敏。

## traffic-light：阻断留下 `hook_block` 行

`TrafficLight` 在 `tool/pre-execute` 上查询工具注册表，调用该工具自己的
`Tool::risk(&arguments)`（因此参数敏感的风险以**本次调用**为准），再与
`TrafficPolicy` 比对。默认策略阻断 `Risk::High`，放行 `Low` 与
`Unknown`；要 fail-closed 就显式把 `Unknown` 也列进阻断集：

```rust
use std::sync::Arc;

use uva_hook::{HookRegistry, MountPoint, RegistrationOrigin};
use uva_hook_plugins::traffic_light::{TrafficLight, TrafficPolicy};
use uva_tool::{Risk, registry::ToolRegistry};

let tools = Arc::new(ToolRegistry::new());
let gate = TrafficLight::with_policy(
    tools,
    TrafficPolicy::blocking([Risk::High, Risk::Unknown]),
);
let registry = HookRegistry::new();
let _registration = registry.register_gate_at(
    MountPoint::ToolPre,
    Arc::new(gate),
    RegistrationOrigin::InProcess,
);
```

注册表判 `Block` 时由 harness 写 durable `hook_block` 行（hook 名、
reason、tool、call_id），重载会话后仍知道谁拦了什么。

## 注册方式

三个模块都经契约 crate 的同一张表注册，不经过本 crate：

- `Hook` → `HookRegistry::register_hook_at`，挂在
  `MountPoint::ToolPost`（`reflection` 的 `after_tool` 观察）；
- `Gate` → `HookRegistry::register_gate_at`，挂在
  `MountPoint::ToolPre`；
- `ResultTransformer` → `HookRegistry::register_result_transformer_at`，
  挂在 `MountPoint::ToolPost`。

每条注册带 `RegistrationOrigin`（仓外插件用 `Plugin`，仓内部署用
`InProcess`）；句柄 drop 即撤回。

## Examples

| Example | 展示 |
|---|---|
| `reflection_demo` | 批判回灌：脚本化 critic 返回 `ERROR` 后 hook 暂存 hint |

跑：`cargo run -p uva-hook-plugins --example reflection_demo --features reflection`。

## CI 契约

- `cargo test -p uva-hook-plugins --all-features` 绿；
- 默认构建零策略：`default = []`，不开 feature 时 lib 不引用任何 hook；
- runtime-free（`make check-no-runtime`）；
- 进插件族文件预算（`make check-plugin-shape`）与可打包顺序
  （`make check-publishable`）。
