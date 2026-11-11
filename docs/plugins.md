# 插件目录

> 每个插件 = **一个模块 + 一个 feature**。crate 是插件的**族**（一个维度一组），
> 不是插件本身。本文是唯一的索引：想装什么、看哪个 feature、依赖带什么。

## 0. 怎么读

| 列 | 含义 |
|---|---|
| **facade feature** | 消费方写在 `elysper = { features = [...] }` 里的名字（**一字不改**，迁移前后一致） |
| **族 crate** | 插件住在哪个 crate（直接依赖该 crate 时用它） |
| **模块** | 族内的模块路径（`use` 路径） |
| **接缝** | 插件实现的是哪个 trait / 契约 |
| **专属依赖** | 只有打开这个 feature 才会进入依赖树的依赖 |

直接依赖族 crate 时，feature 名相同：`elysper-tool-plugins = { features = ["read"] }`。
facade 之所以多一层，是为了让**可选性**与**依赖隔离**留在一个入口上。

## 1. 工具插件（`elysper-tool-plugins`）

接缝统一是 `elysper_tool::Tool`（`name` / `description` / `parameters` / `call`）；
每个工具另有一个 kernel 插件包装（`…ToolPlugin`），在 `apply` 里把自己注册进注入的
`tools` 注册表，卸载时精确撤回那一条注册。

| 插件 | facade feature | 模块 | 主要类型 | 专属依赖 | 用途 |
|---|---|---|---|---|---|
| read | `tool-read` | `read` | `ReadTool` / `ReadToolPlugin` | — | 读工作区文件，可按 1 基行号区间 |
| write | `tool-write` | `write` | `WriteTool` / `WriteToolPlugin` | — | 写工作区文件（覆写 / 追加） |
| shell | `tool-shell` | `shell` | `ShellTool` / `ShellToolPlugin` / `shell::sandbox::*` | `libc` | 跑进程；**唯一**自带 OS 执行策略的工具（`ExecutionPolicy` / `SandboxBackend` / `BwrapBackend`），无后端时 fail-closed |
| todo | `tool-todo` | `todo` | `TodoWriteTool` / `TodoWriteToolPlugin` | — | 模型维护的结构化任务清单 |
| web | `tool-web` | `web` | `WebFetchTool` / `WebFetchToolPlugin` | `reqwest`, `url`, `elysper-core` | 取 URL；**唯一**联网的工具 |
| task | `tool-task` | `task` | `SubagentPlugin` / `TaskDelegator` | `elysper-harness`, `elysper-provider`, `elysper-core` | 把任务委派给已注册的同伴 agent（gate 校验 + worktree 隔离 + `@agent:` 路由） |
| scheduler | `tool-scheduler` | `scheduler` | `ScheduleToolPlugin` / `SchedulerTool` / `register_scheduler_tool` | `elysper-scheduler`, `elysper-core` | 在 `elysper-scheduler::ScheduleStore` 上建 / 查 / 停定时任务；`cron` 解析是 opt-in |
| search | `tool-search` | `search` | `SearchTool` / `register_search_tool` | `elysper-search` | 跨域检索宿主构建的 `elysper-search::Registry`；冷启动只广告名字，首次调用后才补全 schema |
| memo | `tool-memo` | `memo` | `MemoizedTool` / `InMemoryMemoStore` / `MemoConfig` / `MemoScope` | `blake3`, `elysper-core` | 给任意 `Tool` 套一层结果记忆（LRU + TTL + single-flight） |

## 2. 策略插件（`elysper-policy-plugins`）

接缝有两个：`elysper_provider::ModelProvider`（包装一层 provider）与
`elysper_harness::agent::ReasoningStrategy`（换一种推理策略）。

| 插件 | facade feature | 模块 | 主要类型 | 专属依赖 | 用途 |
|---|---|---|---|---|---|
| cache | `semantic-cache` | `cache` | `SemanticCacheProvider` / `SemanticCacheConfig` | `elysper-search` | 语义缓存：重复的补全直接从缓存出 |
| resilience | `resilience` | `resilience` | `ResilientProvider` / `ResilientTool` / `retry_with_backoff` | `elysper-tool` | 重试、退避、熔断；失败分类决定重试还是放弃 |
| budget | `budget` | `budget` | `BudgetProvider` / `BudgetTool` / `PricingTable` | `elysper-tool` | 封顶：token / 成本 / 时延三条预算，超限按策略处理 |
| reflection | `reflection` | `reflection` | `ReflectionHook` / `ReflectiveOutputTransformer` / `CriticProvider` | `elysper-tool`, `elysper-harness`, `elysper-context`, `elysper-steering` | 批判结果并把批判回灌到下一轮 |
| cot | `strategy-cot` | `cot` | `ChainOfThoughtStrategy` | `elysper-harness`, `elysper-session` | 思维链推理策略 |
| plan-execute | `strategy-plan-execute` | `plan_execute` | `PlanExecuteStrategy` / `DefaultPlanGenerator` / `DefaultExecutor` / `DefaultRePlanner` | `elysper-tool`, `elysper-harness`, `elysper-context`, `elysper-steering` | 计划—执行—重规划 |
| best-of-n | `strategy-best-of-n` | `best_of_n` | `BestOfNStrategy` / `LlmJudgeScorer` | `elysper-harness`, `elysper-session` | 采 N 份答案，按评分挑一份 |

## 3. 插件通道（`elysper-plugin-runtime`）

一条通道的三个断面：词汇、说词汇的人、不用进程也能产出工具的第三条路。

| 插件 | facade feature | 模块 | 主要类型 | 专属依赖 | 用途 |
|---|---|---|---|---|---|
| protocol | `plugin-protocol` | `protocol` | `methods` / `wire::*` | —（serde-only） | 线上契约：方法名、能力握手、事件与裁决词汇、错误码。任何语言的插件作者都能只读这份 |
| host | `plugin-host` | `host` | `RemotePlugin` / `ProcessPluginsPlugin` / `PluginRegistry` | `elysper-harness`, `elysper-tool`, `elysper-kernel`, `elysper-core`, `elysper-steering` | 把插件**进程**接成 harness 上的注册项；agent 循环不知道插件存在 |
| wasm | `wasm` | `wasm` | `WasmToolPlugin` / `WasmSandbox` / `WasmLimits` | `wasmtime`, `sha2`, `elysper-kernel`, `elysper-tool`, **`elysper-tool-plugins/shell`** | 加载 Wasm 组件（`describe` / `execute`），用 shell 插件的沙箱接缝限制它 |

## 4. 我要做 X

### 组装一个自己的 agent

1. 装 `elysper`（或直接装需要的族 crate），打开你要的 feature；
2. 自己构造 `ModelProvider`（`elysper-provider` 的 adapter，或你自己的实现）；
3. 建 `ToolRegistry`，把要的插件注册进去（或注册你自己的 `Tool`）；
4. 把 registry、provider、context manager、session sink 交给 `ReActAgent`。

最小可运行版本见 [`MINIMAL.md`](../MINIMAL.md)；
三个独立 consumer proof 在 [`docs/examples/`](examples/)。

### 开发一个自己的工具插件

1. 依赖：`elysper`（`default-features = false`，features `["tool", "kernel"]`）——
   `Tool` trait + 注册表 + kernel 插件契约，**一个入口**；
2. 实现 `Tool`（`#[derive(Tool)]` 从 `elysper-macros` 生成，或手写四个方法）；
3. 可选：写一个 kernel 插件包装，在 `apply` 里 `registry.register_entry`，
   效果句柄在卸载时撤回那一条注册；
4. 挂载：`mount_all` 与工具注册表插件一起装配。

可运行的最小样例：[`docs/examples/plugin-author/`](examples/plugin-author/)。

### 开发一个自己的策略插件

包装 `ModelProvider`（重试 / 缓存 / 预算 / 批判）或实现 `ReasoningStrategy`
（换一种推理）：依赖 `elysper` 的 `provider` 或 `harness` feature，
照 `elysper-policy-plugins` 里任一模块的形状写。

### 开发一个自己的进程插件 / Wasm 插件

照 `elysper-plugin-runtime` 的 `protocol` 模块写方法与词汇（JSON-RPC over stdio），
宿主侧用 `host` 模块的 `RemotePlugin` 接进来；若是 Wasm 组件，导出
`describe() -> string` 与 `execute(input: string) -> string` 两个函数即可。

## 5. 依赖隔离一览

| 组合 | 依赖树里**没有** |
|---|---|
| 工具族八个离线 feature（`read` / `write` / `shell` / `todo` / `task` / `scheduler` / `search` / `memo`） | `reqwest` / `hyper` / `rustls` / `h2` / `tower` / OTel / DB |
| 工具族 + `web` | 其它一切不变，只有 `reqwest` 进来 |
| 策略族全 feature | `reqwest` / `hyper` / `rustls` / `h2` / `tower` / OTel / DB |
| 插件通道默认（仅 `protocol`） | `wasmtime` / `tokio` |
| facade 的七 feature 最小子集 | 任何 HTTP 栈 |

门禁：[`Makefile`](../Makefile) 的 `check-mvp-deps` 逐条断言（按 **feature 组合**，
并反向断言 `web` 确实是打开 `reqwest` 的那个开关）。

## 6. 对账

本文每一行的 feature 名都能在族 crate 的 `[features]` 表与 facade 的
`[features]` 表里逐条核到；改了任一侧，跑：

```sh
make check-mvp-deps     # 依赖隔离（按 feature 组合）
make check-publishable  # 29 个 crate 逐个打包
```
