# 一个核心，多协议适配器：ACP / AG-UI / 云边界

日期：2026-11-25
状态：已实现（ACP 适配层改用官方 SDK；AG-UI 投影落地）
取代：`2026-11-25-acp-headless-harness-design.md`（手写协议层）

---

## 1. 意图与为什么

要求不是「核心用 ACP 就能服务所有客户端」。准确的说法是三层：

1. **核心层**：Harness 只维护一套会话、回合、工具调用、权限请求、取消
   的内部事件流。它不直接说 ACP、AG-UI 或 A2A。
2. **ACP 适配层**：把核心事件流暴露成 ACP，通过 stdio / WebSocket 对外。
   桌面编辑器、TUI、任何 ACP-capable 客户端都能用。
3. **其他客户端边界**：Web 前端在服务端把运行时事件投影成 **AG-UI**；
   云服务 / SaaS 按语义挂 **A2A**（把 Harness 当被调度 Agent）或
   **UHP**（把服务当统一调度平台），而不是让远端系统直接说 ACP。

一句话：**ACP 覆盖「需要持续会话和自动化控制」的客户端，不是所有客户端
的通用语言。**

## 2. 三层的形状

```text
                    ┌───────────────────────────────┐
   编辑器 / TUI ───▶│  ACP 适配层（母语，无头控制面）  │──┐
   (stdio / WS)     └───────────────────────────────┘  │
                                                       │  读同一套事件流
                    ┌───────────────────────────────┐  │
   浏览器      ───▶│  AG-UI 适配层（服务端投影）      │◀─┤
                    └───────────────────────────────┘  │
                                                       │
                    ┌───────────────────────────────┐  │
   云调度器    ───▶│  A2A / UHP 适配层（按语义挂）    │◀─┤
                    └───────────────────────────────┘  │
                                                       │
                    ┌───────────────────────────────┐  │
                    │  核心：一套事件流 + 一套会话日志 │◀─┘
                    └───────────────────────────────┘
```

核心那条线只认 `uva-events` 的 `AgentEvent` / `SystemEvent` 与会话日志。
适配器只做一件事：把核心的词汇读成自己协议的词汇。

## 3. ACP 适配层

### 3.1 协议词汇归协议那一侧

ACP 的消息类型**不再手写**：适配层依赖官方 Rust SDK
`agent-client-protocol`（workspace 依赖，`features = ["stdio"]`）。握手、
`session/new`、`session/load`、`session/prompt`、`session/cancel`、
`session/update`、`session/request_permission` 的线上形状都是 SDK 的
schema 类型，客户端解的就是这个 agent 写的。

> 手写的 `wire.rs`（方法名常量、更新形状、stop reason、权限选项）已删除。
> 仓库自己的映射只剩一处：核心事件 → SDK 的 `SessionUpdate`
> （`adapters/acp/projection.rs`）。

### 3.2 两种传输，一个 agent

| 传输 | 客户端 | 接法 |
|---|---|---|
| stdio | 编辑器 / 终端直接拉起进程 | SDK 的 `Stdio` transport（`adapters/acp/stdio.rs`） |
| WebSocket | 经网关 `/rpc` 连上来的 ACP 客户端 | 核心 JSON-RPC 通道上的 ACP 帧（`adapters/acp/channel.rs`） |

WebSocket 这一条的核心事实：**核心的 JSON-RPC 通道同时承载两套词汇**。
一帧若命中 ACP 的方法名（`initialize` / `authenticate` / `session/new` /
`session/load` / `session/prompt` / `session/cancel`），或者是一帧「有
result 无 method」的应答（那一定是 agent 自己问出去的问题的答案），它
归 ACP 适配层；其余归核心的 router。适配层的出站帧回到同一条通道，
所以一个说 ACP 的客户端和一个说核心 JSON-RPC 的客户端共用一根管子。

网关那一侧补三条路由：

- 客户端的 ACP 请求里带 `sessionId` → 记住「这个连接拥有这个会话」；
- `session/new` 的**应答**里的 `sessionId` → 同样记住；
- 核心发起的请求（`session/request_permission`，有 method 有 id）→ 按
  会话发给拥有它的那个连接，并把 id 记在该连接名下，好让答案原路回来。

### 3.3 参照行为

握手、会话、回合、被held住的风险调用、取消、持久化转录——六件事与
`dsh` / `pi` / `codex` 的 ACP 行为对齐，证据与出处见
[`docs/reference-harnesses.md`](../reference-harnesses.md)。

## 4. AG-UI 适配层

浏览器不说 ACP，也不该说。服务端把同一套核心事件投影成 AG-UI 事件：

| 核心 | AG-UI |
|---|---|
| 回合首个事件 | `RUN_STARTED` |
| 助手文本增量 | `TEXT_MESSAGE_START` + `TEXT_MESSAGE_CONTENT` |
| 推理增量 | `REASONING_MESSAGE_CONTENT` |
| 模型工具调用 | `TOOL_CALL_START` + `TOOL_CALL_ARGS` + `TOOL_CALL_END` |
| 工具结果 | `TOOL_CALL_RESULT` |
| 回合结束 | `RUN_FINISHED` |
| 运行失败 | `RUN_ERROR` |

投影是**有状态**的（哪条消息开着、哪次运行活着），所以它是对象而不是
函数（`adapters/agui/projection.rs`）。事件以 `agui/event` 通知发布，
网关按 `session_id` 投给该会话的订阅者——浏览器多一扇协议形状的门，
ACP 适配层不是浏览器的那扇门。

AG-UI 的规范要求「缺席即缺席」：可选字段一律不发 `null`。词汇表在
`adapters/agui/event.rs`，是这条通知的线上形状的唯一主人。

## 5. 云边界：A2A 与 UHP

云服务 / SaaS 不说 ACP：ACP 是「一个客户端持续控制一个 agent」的协议，
而云侧要的是「把 agent 当作可调度的资源」（A2A）或「把服务当作统一
调度平台」（UHP）。二者按语义挂：

- **A2A**：本 Harness 是被调度的 Agent，任务的输入输出按 A2A 的
  task / message 形状翻译；回合的取消按 A2A 的取消形状翻译。
- **UHP**：本服务是调度平台，跨 agent 的编排、租约与恢复按 UHP 的形状
  翻译。

这两条**本次不实现**：仓内没有 A2A / UHP 的运行时依赖，也没有任何
客户端边界要求它们。它们在这里被声明为「按语义挂适配器」的位置，与
ACP / AG-UI 同一层——核心不需要为它们改一行。

## 6. 门禁

| 门禁 | 守住什么 |
|---|---|
| `check-kernel-deps` | 内核不依赖任何传输层 crate |
| `check-public-api-runtime` | 库 crate 的公开面不出现运行时类型 |
| `make lint-rust` / `make doc-check` | 适配层的公开项有文档、无警告 |
| `tests/acp_stdio.rs` | ACP 适配层的线上行为（握手、回合、held 调用、重载、取消） |
