# 插件目录

> 每个插件 = **一个模块 + 一个 feature**。crate 是插件的**族**（一个维度一组），
> 不是插件本身。本文是唯一的索引：想装什么、看哪个 feature、依赖带什么。

## 0. 怎么读

| 列 | 含义 |
|---|---|
| **facade feature** | 消费方写在 `uva = { features = [...] }` 里的名字（**一字不改**，迁移前后一致） |
| **族 crate** | 插件住在哪个 crate（直接依赖该 crate 时用它） |
| **模块** | 族内的模块路径（`use` 路径） |
| **接缝** | 插件实现的是哪个 trait / 契约 |
| **专属依赖** | 只有打开这个 feature 才会进入依赖树的依赖 |

直接依赖族 crate 时，feature 名相同：`uva-tool-plugins = { features = ["read"] }`。
facade 之所以多一层，是为了让**可选性**与**依赖隔离**留在一个入口上。

## 1. 工具插件（`uva-tool-plugins` / `uva-tool-task`）

接缝统一是 `uva_tool::Tool`（`name` / `description` / `parameters` / `call`）；
每个工具另有一个 kernel 插件包装（`…ToolPlugin`），在 `apply` 里把自己注册进注入的
`tools` 注册表，卸载时精确撤回那一条注册。

| 插件 | facade feature | 模块 | 主要类型 | 专属依赖 | 用途 |
|---|---|---|---|---|---|
| read | `tool-read` | `read` | `ReadTool` / `ReadToolPlugin` | — | 读工作区文件，可按 1 基行号区间 |
| write | `tool-write` | `write` | `WriteTool` / `WriteToolPlugin` | — | 写工作区文件（覆写 / 追加） |
| shell | `tool-shell` | `shell` | `ShellTool` / `ShellToolPlugin` / `shell::sandbox::*` | `libc` | 跑进程；**唯一**自带 OS 执行策略的工具（`ExecutionPolicy` / `SandboxBackend` / `BwrapBackend`），无后端时 fail-closed |
| todo | `tool-todo` | `todo` | `TodoWriteTool` / `TodoWriteToolPlugin` | — | 模型维护的结构化任务清单 |
| web | `tool-web` | `web` | `WebFetchTool` / `WebFetchToolPlugin` | `reqwest`, `url`, `uva-core` | 取 URL；**唯一**联网的工具 |
| task | `tool-task` | `uva-tool-task`（crate 根） | `TaskToolPlugin` / `TaskTool` / 控制面 `agents_list` · `agents_message` · `agents_interrupt` | `uva-agent`, `uva-events`, `uva-tool`, `uva-core`（crate 无 feature，依赖固定） | 把任务派给已注册的同伴 agent（gate 校验 + worktree 隔离）+ 子代理控制面；普通 `Tool`，注册表分发，不依赖 harness |
| scheduler | `tool-scheduler` | `scheduler` | `ScheduleToolPlugin` / `SchedulerTool` / `register_scheduler_tool` | `uva-scheduler`, `uva-core` | 在 `uva-scheduler::ScheduleStore` 上建 / 查 / 停定时任务；`cron` 解析是 opt-in |
| search | `tool-search` | `search` | `SearchTool` / `register_search_tool` | `uva-search` | 跨域检索宿主构建的 `uva-search::Registry`；冷启动只广告名字，首次调用后才补全 schema |
| memo | `tool-memo` | `memo` | `MemoizedTool` / `InMemoryMemoStore` / `MemoConfig` / `MemoScope` | `blake3`, `uva-core` | 给任意 `Tool` 套一层结果记忆（LRU + TTL + single-flight） |

## 2. 策略插件（`uva-policy-plugins`）

接缝有两个：`uva_provider::ModelProvider`（包装一层 provider）与
`uva_harness::agent::ReasoningStrategy`（换一种推理策略）。

| 插件 | facade feature | 模块 | 主要类型 | 专属依赖 | 用途 |
|---|---|---|---|---|---|
| cache | `semantic-cache` | `cache` | `SemanticCacheProvider` / `SemanticCacheConfig` | `uva-search` | 语义缓存：重复的补全直接从缓存出 |
| resilience | `resilience` | `resilience` | `ResilientProvider` / `ResilientTool` / `retry_with_backoff` | `uva-tool` | 重试、退避、熔断；失败分类决定重试还是放弃 |
| budget | `budget` | `budget` | `BudgetProvider` / `BudgetTool` / `PricingTable` | `uva-tool` | 封顶：token / 成本 / 时延三条预算，超限按策略处理 |
| cot | `strategy-cot` | `cot` | `ChainOfThoughtStrategy` | `uva-harness`, `uva-session` | 思维链推理策略 |
| plan-execute | `strategy-plan-execute` | `plan_execute` | `PlanExecuteStrategy` / `DefaultPlanGenerator` / `DefaultExecutor` / `DefaultRePlanner` | `uva-tool`, `uva-harness`, `uva-context`, `uva-steering` | 计划—执行—重规划 |
| best-of-n | `strategy-best-of-n` | `best_of_n` | `BestOfNStrategy` / `LlmJudgeScorer` | `uva-harness`, `uva-session` | 采 N 份答案，按评分挑一份 |

## 3. hook 插件（`uva-hook-plugins`）

接缝是 [`uva-hook`](../crates/uva-hook/README.md) 的四个拦截 trait；
每个插件经 `HookRegistry` 或 harness `Host` 注册，不经过本族 crate。
默认构建（`default = []`）不拉起任何 hook。

| 插件 | facade feature | 模块 | 主要类型 | 专属依赖 | 用途 |
|---|---|---|---|---|---|
| reflection | `reflection` | `reflection` | `ReflectionHook` / `ReflectiveOutputTransformer` / `CriticProvider` | `uva-provider`, `uva-context`, `uva-steering` | 定期让 critic 模型批判轨迹，severity ≥ `Warn` 时把 hint 回灌下一轮 |
| redact | `redact` | `redact` | `RedactTransformer` / `RedactConfig` / `RedactionRule` | `uva-tool`, `serde_json` | `ResultTransformer`：提交前脱敏工具出参；循环写 `tool_result_transformed` 溯源行（`redact` + 前后摘要） |
| traffic-light | `traffic-light` | `traffic_light` | `TrafficLight` / `TrafficPolicy` | `uva-tool` | `Gate`：按工具自报的 `Risk` 放行 / 阻断；阻断由 harness 写 `hook_block` 行 |

内建五个 steering guard（工作区约束、循环检测等）不在这个族：
它们住在 `uva-steering::guards`，循环启动时经 `Host::default_policy`
装一次，不是可挂载 hook。

## 4. 第三方工具：走 MCP（`uva-mcp`）

**进程插件 / Wasm 插件那条路已经删掉了**：没有"插件通道"，没有 `protocol` /
`host` / `wasm` 三个断面，也没有对应的 facade feature。今天把第三方工具接进
agent 只有一条路——**MCP server**。

1. 写一个 MCP server（Node / Python 都有官方 SDK），它只说 MCP，不说 Uva；
2. 在部署布局的 `mcp.json` 里声明它：`mcpServers` 映射，一个条目给
   `command`（要起的程序）与 `args`（它的参数），可选 `env` / `enabled`：

   ```json
   {
     "mcpServers": {
       "records": {
         "command": "python3",
         "args": ["/srv/mcp/records_server.py"]
       }
     }
   }
   ```

3. 布局有两层：`<workspace>/.agents/mcp.json` 与 `~/.agents/mcp.json`
   （插件包里的 `mcp.json` 以 `<plugin>/<server>` 命名空间落地）；
4. 宿主用 **stdio** 把它起起来、跑 MCP 握手，把它广告的工具注册进 agent
   用的**同一个工具注册表**（`mcp__<server>__<tool>`），所以模型直接就能
   调用——第三方工具上线不用重编宿主。

## 5. 我要做 X

### 组装一个自己的 agent

1. 装 `uva`（或直接装需要的族 crate），打开你要的 feature；
2. 自己构造 `ModelProvider`（`uva-provider` 的 adapter，或你自己的实现）；
3. 建 `ToolRegistry`，把要的插件注册进去（或注册你自己的 `Tool`）；
4. 把 registry、provider、context manager、session sink 交给 `ReActAgent`。

最小可运行版本见 [`MINIMAL.md`](../MINIMAL.md)；
四个独立 consumer 样板在 [`docs/examples/`](examples/)。

### 开发一个自己的工具插件

1. 依赖：`uva`（`default-features = false`，features `["tool", "kernel"]`）——
   `Tool` trait + 注册表 + kernel 插件契约，**一个入口**；
2. 实现 `Tool`（手写五个方法：`name` / `description` / `parameters` / `output_definition` / `annotations` / `call`）；
3. 可选：写一个 kernel 插件包装，在 `apply` 里 `registry.register_entry`，
   效果句柄在卸载时撤回那一条注册；
4. 挂载：`mount_all` 与工具注册表插件一起装配。

可运行的最小样例：[`docs/examples/plugin-author/`](examples/plugin-author/)。

### 开发一个自己的策略插件

包装 `ModelProvider`（重试 / 缓存 / 预算 / 批判）或实现 `ReasoningStrategy`
（换一种推理）：依赖 `uva` 的 `provider` 或 `harness` feature，
照 `uva-policy-plugins` 里任一模块的形状写。

### 接一个第三方工具

第三方工具现在是 **MCP server**：写一个 MCP server（Node / Python 都有官方
SDK），在部署布局的 `mcp.json` 里用 `command` + `args` 声明它，宿主用 stdio
把它起起来，它广告的工具就落进 agent 用的同一个工具注册表。细节见 §4。

（旧的"进程插件 / Wasm 插件"通道已经删掉，不再有对应的 crate 与 feature。）

## 6. 依赖隔离一览

| 组合 | 依赖树里**没有** |
|---|---|
| 工具族七个离线 feature（`read` / `write` / `shell` / `todo` / `scheduler` / `search` / `memo`），加无 feature 的 `uva-tool-task` | `reqwest` / `hyper` / `rustls` / `h2` / `tower` / OTel / DB |
| 工具族 + `web` | 其它一切不变，只有 `reqwest` 进来 |
| 策略族全 feature | `reqwest` / `hyper` / `rustls` / `h2` / `tower` / OTel / DB |
| hook 族全 feature | `reqwest` / `hyper` / `rustls` / `h2` / `tower` / OTel / DB |
| facade 的七 feature 最小子集 | 任何 HTTP 栈 |

门禁：[`Makefile`](../Makefile) 的 `check-mvp-deps` 逐条断言（按 **feature 组合**，
并反向断言 `web` 确实是打开 `reqwest` 的那个开关）。

## 7. 对账

本文每一行的 feature 名都能在族 crate 的 `[features]` 表与 facade 的
`[features]` 表里逐条核到；改了任一侧，跑：

```sh
make check-mvp-deps     # 依赖隔离（按 feature 组合）
make check-publishable  # 31 个 crate 逐个打包
```
