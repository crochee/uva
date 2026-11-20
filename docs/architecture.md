# Architecture

[`AGENTS.md` §3.7](../AGENTS.md) 只列硬不变量；本文是它们的**理由**与仓库全景——agent 在需要判断"为什么这样切"时读它，逐 crate 的 API 细节读 `crates/*/README.md`。

## 1. 两个最高级目标

1. **可作为 lib** — `cargo add uva` 是消费端入口；下游只依赖 `uva` / `uva-*` crates，绝不能反向依赖消费端应用。
2. **乐高式组装** — 七大组件（Provider / Tool / ContextManager / Steering / SessionSink / CancelToken / Agent）每一个都是单独可换的 trait + 默认实现；外部消费者应能从一个空的 `cargo new` 仓库拼出一个完整的 AI agent。

## 2. 工作区全景

31 个 workspace member（`cargo metadata` 为准），全部在 `crates/`。

| 层 | crate | 一句话职责 |
|---|---|---|
| 基座 | `uva-core` | 纯类型 / trait 基座：Clock、IdGen、CancelToken、Spawner、Registry、schema、错误 |
| 基座 | `uva-core-tokio` | `uva-core` 那些 runtime-neutral seam 的 tokio 实现（opt-in） |
| 范式 | `uva-provider` | `ModelProvider` trait + Anthropic / OpenAI adapter（各一 feature） |
| 范式 | `uva-tool` | 工具范式：`Tool` trait、`ToolRegistry`、曝光投影、`workspace` 路径约束、两个合成契约工具 |
| 范式 | `uva-context` | 上下文窗口策略 + 三层记忆（会话 / 工作 / 长期，可选 SQLite） |
| 范式 | `uva-steering` | Guard / Hook / Hint / Tracker / OutputTransformer 与类型化 `HookMap` |
| 范式 | `uva-session` | 会话生命周期、sink、类型化事件流 |
| 审批 | `uva-approval` | 审批词汇、日志推导的决策函数、`approval` 内核插件；harness 冻结面经 re-export 保路径 |
| 范式 | `uva-skill` | 技能注册表：斜杠命令 / prompt / 工具包 |
| 插件 | `uva-tool-plugins` | 工具插件，一模块一 feature：`read` `write` `shell` `todo` `web` `scheduler` `search` `memo` |
| 插件 | `uva-tool-task` | `task` 工具（普通 `Tool`）：解析同伴 agent、开子会话、内联驱动子 run；控制面三工具（`agents_list` / `agents_message` / `agents_interrupt`）管住本会话的子代理；依赖 agent/事件词汇，不依赖 harness |
| 插件 | `uva-policy-plugins` | 策略插件，一模块一 feature：`cache` `resilience` `budget` `cot` `plan-execute` `best-of-n` |
| 插件 | `uva-hook-plugins` | hook 实现族，一模块一 feature：`reflection` `redact` `traffic-light`；内建五 guard 仍住 `uva-steering` |
| 装配 | `uva-kernel` | 插件内核：manifest、依赖序 `mount_all`、可逆 `EffectHandle`、per-plugin `dispose_plugin`、`ui.` 命名空间边界 |
| 装配 | `uva-runtime` | 配置驱动的装配：provider catalogue、tier-1 包装链、agent-loop 工厂 |
| 运行时 | `uva-events` | run 事件词汇（`AgentEvent` / `SystemEvent` / `WarningKind` / `SessionEndReason`）；`uva-harness` 根与 `events` 深路径经 re-export 保路径 |
| 运行时 | `uva-agent` | agent 词汇：`Agent` trait、`AgentInput`、转向通道收件箱（`RunInbox` / `ChannelInbox`）、peer 注册表（`AgentRegistry` / `AgentEntry`）、子会话写入者来源、prompt manifest（`PromptContext` / `PromptSection`）；`AgentCaps` 与 `ReActAgent` 留 harness，harness 根与深路径经 re-export 保路径 |
| 运行时 | `uva-hook` | 拦截契约（`Hook` / `Gate` / 请求与结果 transformer、十二点 `MountPoint`、注册来源、解耦 `HookContext`）、可撤销 `HookRegistry` 与仲裁、发布 `hooks` 服务的 `HookPlugin`；`uva-harness` 根与 `agent::host` 深路径 re-export 保持 |
| 运行时 | `uva-harness` | ReAct 循环、`Agent` / `ReasoningStrategy` seam、`Host`（typed 策略表 + 部署的拦截表） |
| 检索 | `uva-search` | 多域检索引擎：BM25 + 向量混合、可插拔 tokenizer / embedder / reranker |
| 调度 | `uva-scheduler` | runtime-free 的 cron / interval / once 分发 + 原子 JSON 持久化（调用方持有定时器） |
| 评测 | `uva-eval` | 评测框架：suite、同步 / 异步指标、runner、judge + schema 校验、JSON / CSV 导出 |
| 多模态 | `uva-attachment` | 内容寻址的多模态附件存储（图片 / 音频 / 文件字节） |
| 观测 | `uva-telemetry` | console / file logging、Prometheus metrics、OTel 导出；日志与指标的唯一入口 |
| 适配 | `uva-mcp` | MCP 客户端：stdio / HTTP / in-memory 传输、supervisor 重连，把远端工具注册进本机 `ToolRegistry` |
| 宿主 | `uva-app-server` | 应用 crate：`core`（stdio 无头内核）+ `serve`（WebSocket 网关 + 探针 + 静态前端） |
| 宿主 | `uva-tui` | 终端投影：ratatui + crossterm，经 `uva-gateway` 驱动 `serve` 的 `/rpc`；wire 类型（follow 基线、活动投影、schema 树、方法名）直接取自 `uva-app-server`，终端不留副本（ADR-0025） |
| 宿主 | `uva-gateway` | 两宿主共用的网关支撑：进程监管（拉起 / 探活 / 退避 / 回收）+ 网关 JSON-RPC client；公开面 runtime-free |
| 宿主 | `uva-mcp-server` | stdio 上的 MCP 服务器，把插件砖块工具暴露给任意外部 MCP 客户端 |
| 门面 | `uva` | facade：每个部件一个 feature + 精选 `prelude`；只有 `pub use`，零逻辑 |
| 测试 | `uva-test-support` | 跨 crate 的确定性假件：`ModelProviderStub`、`ReplayProvider`、固定时钟 / 序列、golden transcript |

宿主之外还有两个**不属于 root workspace** 的目录：`desktop/`（Tauri 2 壳，独立 workspace）与 `uva-web/`（React / Vite 前端）。

## 3. 不变量与理由

AGENTS.md §3.7 列出全部不变量。下表是"为什么"与"哪道门禁在守"。

| 不变量 | 为什么 | 门禁 |
|---|---|---|
| 依赖方向单向 `uva` → `uva-*` → `uva-core` | 下游能换掉任何一层；反向依赖会把消费端锁死 | `cargo tree -p uva-core` |
| 公开面最小化 + `lib.rs` 显式列名 | `pub use mod::*` 让任何未来新增 `pub` 自动变成对外契约 | `check-pub-surface`、`check-pub-surface-strict` |
| facade 零逻辑 | 逻辑下沉后，只有底层 crate 决定行为，facade 只是目录 | `check-pub-surface`（facade 豁免 glob） |
| 工具实现住在插件族 crate | 一个族 = 一个插件维度；消费方按 feature 自行组装，不存在隐式"默认工具集" | `check-mvp-deps` |
| 插件专属依赖逐插件 optional | "这个工具带什么"是模块内的事 | `check-mvp-deps` |
| 工具边界随工具走 | 描述模型的策略与真正 spawn 的 argv 来自同一个值，不可能漂移 | — |
| harness 与插件分离 | harness 只留核心循环；`task` 是 `uva-tool-task` 的普通工具，注册表分发 | — |
| 公开 API 不出现运行时类型 | 消费方可以选自己的执行器 | `check-public-api-runtime` |
| chrono 唯一时间库 | 墙钟经 `Clock` 注入，测试才能钉住时间 | `check-clock` |
| ULID 默认 ID 生成器 | 同上，ID 经 `IdGen` 注入 | — |
| Registry seam 收口 `uva-core` | 免得每个 crate 各写一份 cursor + limit + envelope | — |
| `uva-kernel` 是框架边界 | 内核不得依赖 agent / UI / 传输层 | `check-kernel-deps` |
| profile HMR 只重挂 diff 的 id | volatile-only 变更不重挂；`hmr = false` 绝不半重载 | — |
| UI 扩展点只传 JSON schema | v1 永不执行远端 JS / CSS | — |
| Optional crates 是 opt-in 扩展 | 默认 `uva` 不拉起任何一个可选 crate | `check-mvp-deps` |

## 4. 依赖与运行时的边界

- **HTTP（`reqwest`）只出现在三处**：`uva-provider/{anthropic,openai}`、工具族的 `web`、telemetry 的 `otlp`。其余成员默认零 HTTP。
- **tokio 是实现细节**：每个 crate 的 `tokio` 行只写自己用到的 feature；仅测试用 tokio 的 crate 放 `[dev-dependencies]`。脱离运行时的接缝只有 `uva_core::spawn::Spawner`。
- **tokio-bound 插件**：`uva-tool-plugins`（`tokio::fs` / `tokio::process`）、provider 的 HTTP adapter、JSONL session sink。消费方在别的运行时上不依赖它们，或自带 `Tool` / `ModelProvider` / `SessionSink`。
- **应用 crate 是豁免的唯一位置**：`uva-app-server` 与 `uva-mcp-server` 的 `main()` 可以出现具体运行时类型。

## 5. 组件替换点

七大组件的 trait、默认实现与"装上你的"边界见 [`SEAMS.md`](../SEAMS.md)；facade 的 feature 与 `prelude` 见 [`crates/uva/README.md`](../crates/uva/README.md)；插件与装配形态见 [`docs/plugins.md`](plugins.md)。
