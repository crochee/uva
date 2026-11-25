# 无头 harness 的 agent harness protocol 面：`acp` 模式设计

日期：2026-11-25
状态：已被取代（协议层改用官方 SDK；适配层的分层见后继文档）
取代：无
被取代：`2026-11-25-protocol-layers-design.md`

---

## 1. 意图与为什么

用户的要求是「逆向 dsh、pi、codex，在使用 agent harness protocol 的情况下达到
同样的无头 harness 行为」。这句话里有三件事：

1. **逆向三个参考 harness**——它们各自的 agent 循环、事件词汇、审批模型、
   取消语义、会话持久化与无头形态；
2. **选定那个协议**——编辑器与 harness 之间真正流通的那一个；
3. **在本仓实现同样的无头行为**——不是模拟，是同一套机器上的同一种行为。

逆向的结论写在 §2，协议的选择写在 §3，行为对账表写在 §4，实现写在 §5，
差异（诚实声明）写在 §6。

## 2. 逆向：三个参考 harness

三个仓库都拉到了本地（`deepseek-ai/deepseek-harness`、`earendil-works/pi`、
`openai/codex`），逐个读了循环、事件、审批、取消、会话与无头入口。

### 2.1 dsh（DeepSeek Harness）

- **形态**：Cordis 插件 monorepo。agent 循环（`packages/core/agent-loop`）、
  事件日志（`packages/core/session`）、工具管线（`packages/core/tools`）、
  审批接缝（`packages/interaction/user-approval`）、持久化接缝
  （`packages/session/session-persistence`）与 **ACP 传输**
  （`packages/acp/acp`）各是一个包。
- **ACP 桥是薄投影**：`packages/acp/acp/src/index.ts` 用
  `@agentclientprotocol/sdk` 1.4.0，把**已提交的** `SessionEvent` 翻译成标准
  `session/update` 通知；它不持有任何 agent 逻辑，`session/prompt` 在真正
  静默后才结算。
- **握手**（`packages/acp/acp/src/index.ts:188-206`）：
  `agentInfo.name = "deepseek-harness-acp"`，
  `agentCapabilities = { mcpCapabilities: {http: true},
  promptCapabilities: {image, audio: false, embeddedContext: false},
  sessionCapabilities: {close: {}, list: {}, resume: {}} }`，`authMethods: []`。
- **权限是一次性机器策略通道**（同文件 `:161-178`）：只给
  `allow-once` / `reject-once` 两个选项，映射为
  `cancelled → cancelled`、`allow-once → allowed-once`、其余 `→ rejected`。
  注释写得很直白：*“The bridge offers one-shot choices only and never infers
  a durable grant from an unknown client response.”*
- **没有 `session/load`**：dsh 的 ACP 面不支持回放。
- **循环无内置预算**（`packages/core/agent-loop/README.md`：
  *“No built-in turn budget”*）：turn → step，step 在「模型不再要工具」时
  结束；turn 的结束理由是 `completed / max-tokens / blocked / aborted /
  error`（`packages/core/session/src/types.ts:216-248`）。
- **ACP 线上只承载已提交的事实**：没有文本/推理增量，一个提交的 assistant
  block 变成一个 `agent_message_chunk`。

### 2.2 pi（earendil-works/pi）

- **没有 ACP**：全仓没有 `session/update`、`session/request_permission` 或
  ACP schema；最接近的是它自己的 RPC JSONL 协议与 durable 的 `beforeTool`
  休眠门。
- **实验性 harness 已被删除**（`packages/agent/CHANGELOG.md:65`：
  *“Removed the experimental harness from `@earendil-works/pi-agent-core`:
  `AgentHarness`, sessions and session storage, the durable runtime…”*）。
  今天的「harness」在 `packages/durable`（先提交后展示）与
  `packages/coding-agent`（产品）里。
- **循环两层**（`packages/agent/src/agent-loop.ts:188-192`）：外层
  `while (true)` 排空 follow-up，内层是工具轮。结束路径：provider 报错或
  中止 → `turn_end` + `agent_end`；`finishTurn`；工具批次终止；自然停止；
  `stopReason === "length"` 时**工具调用失败而不执行**（参数被截断，
  `agent-loop.ts:488-516`）。
- **第一层没有步数上限**；上限在 durable 层（`AgentState.maxTurns`，
  `packages/durable/src/harness/generation.ts:1297-1305`）。

### 2.3 codex（openai/codex）

- **仓内没有 ACP**（`codex-rs/` 里 `agent-client-protocol` 零命中）：它的
  无头协议是自己的 `codex app-server`（stdio 上的 JSON-RPC 2.0）与
  `codex exec`（JSONL）。
- **循环**：Task → Turn → sampling request；没有固定步数上限
  （`codex-rs/core/src/session/turn.rs:553`）。终止于：模型不再要工具且无
  挂起输入、致命错误（`EventMsg::Error`）、中断（`TurnAborted`）、
  预算耗尽、上下文窗口超限（转压缩）。
- **审批是服务端发起的请求/应答**（`codex-rs/core/src/session/mod.rs:3407`，
  oneshot 上阻塞）：`ReviewDecision` =
  `Approved / ApprovedForSession / Denied / TimedOut / Abort`。**拒绝**把
  工具错误交回模型、回合继续；**Abort** 结束回合；**超时**按拒绝处理
  （`codex-rs/core/src/tools/approvals.rs:493`）。客户端断开时 oneshot
  掉落 → `Abort`。
  无头 `codex exec` 直接拒绝所有审批
  （`codex-rs/exec/src/lib.rs:2422`）。
- **取消**：`Op::Interrupt` → `interrupt_task` → 以
  `TurnAbortReason::Interrupted` 中止；app-server 暴露 `turn/interrupt`
  （按 turn id 定位，等终态事件才回话）。
- **转录**：每线程一份只追加的 JSONL rollout（`codex-rs/history/src/lib.rs:424`），
  `thread/resume` 恢复、`thread/fork` 分叉。
- **工具在进程内执行**；只有 dynamic tool 与 `execution: "client"` 的工具
  回传给客户端；exec 输出截断在 1 MiB。

### 2.4 三个的共同形状

| 维度 | dsh | pi | codex | Uva（实现前） |
|---|---|---|---|---|
| 回合结束 | 无工具 / max-tokens / blocked / aborted / error | 无工具 / 结束 / 中止 / length | 无工具 / 错误 / 中断 / 预算 | 迭代上限 / 审批挂起 / 取消 |
| 审批 | 一次性选项（allow-once / reject-once） | durable 的 beforeTool 门 | 服务端请求，Approved/ApprovedForSession/Denied/TimedOut/Abort | **有策略、有行、有 resume，但没有闸门** |
| 取消 | turn aborted | abort signal | interrupt → TurnAborted | `cancel` → run 取消 |
| 转录 | 提交式事件日志 | durable（先提交后展示） | 只追加 JSONL | 只追加 JSONL（`events.jsonl`） |
| 工具执行位置 | 进程内 | 进程内 | 进程内（少数回传客户端） | 进程内 |
| 协议 | **ACP** | 自有 RPC JSONL | 自有 app-server JSON-RPC | 自有 JSON-RPC（+ Anthropic 兼容面） |

最后一行决定了协议的选择。

## 3. 协议：Agent Client Protocol（ACP）v1

三个 harness 里只有 **dsh** 讲编辑器真正流通的那个协议：ACP
（agentclientprotocol.com，v1 为稳定版）。pi 与 codex 各有自有协议，
但它们都能（且已经）通过 ACP 适配器被编辑器驱动——ACP 是这条路上唯一的
共同词汇。因此本仓实现的「agent harness protocol 面」= **ACP v1**。

已实现的子集（能力在 `initialize` 里如实声明）：

| 方法 | 行为 |
|---|---|
| `initialize` | 版本 1、能力（`loadSession`、图片/内嵌上下文）、身份 |
| `authenticate` | 拒绝——本部署没有认证方法 |
| `session/new` | 开一个会话，opaque id |
| `session/load` | 回放转录为 `session/update`，然后应答 |
| `session/prompt` | 跑一个回合：增量以 `session/update` 流回，最后给停止理由 |
| `session/cancel` | 取消在跑的回合 |

服务端发给客户端的请求只有一种：`session/request_permission`（审批问题）。

## 4. 行为对账表

| 行为 | 参考 harness | 本实现 | 证据 |
|---|---|---|---|
| 握手命名能力 | dsh/codex 都声明自己支持什么 | 同 | `acp::server::initialize` |
| 回合增量流回 | codex 逐增量；dsh 只送已提交块 | 逐增量（`agent_message_chunk` / `agent_thought_chunk`） | `acp::updates::updates_for` |
| 工具调用生命周期 | codex `ItemStarted`/`ItemCompleted` | `tool_call`（pending）→ `tool_call_update`（in_progress / completed / failed） | 同上 |
| 审批是一次性选择 | dsh 明确只给 allow-once / reject-once | 同（只给两项） | `acp::wire::permission_options` |
| 拒绝后回合继续 | codex：工具错误交回模型，回合继续 | 同（拒绝写成 is_error 工具结果，模型继续） | `acp::turn::continue_after_permission` |
| 取消 | codex：中断 → 回合中止 | `session/cancel` → run 取消 → 停止理由 `cancelled`；权限问题开着时取消，问题按协议的 `cancelled` 应答，回合立刻以 `cancelled` 结束 | `acp::turn::run` |
| 转录可恢复 | codex `thread/resume`、Uva 自己的 JSONL | `session/load` 回放 | `acp::server::session_load` |
| 工具在进程内跑 | 三个都是 | 同（Uva 的工具注册表） | — |
| 客户端目录 | dsh/codex 在会话目录里跑 | **不同**：工具跑在部署的工作区根 | §6 |

## 5. 实现

新模式 `uva-app-server acp`：同一个二进制，第三个模式。

```text
crates/uva-app-server/src/acp/
  framing.rs   NDJSON 传输（一行一条 JSON-RPC）
  wire.rs      词汇：方法名、update 形状、停止理由、权限选项
  updates.rs   投影：Uva 事件 → ACP update（实时方向与回放方向）
  approval.rs  装上部署的审批闸门
  server.rs    分发：方法、会话、权限问题
  turn.rs      一个回合：从 prompt 到停止理由
```

**为什么是第三个模式而不是 core 里的一组方法**：ACP 的传输（一行一条）、
方法名（`session/prompt` 而不是 `chat/message/send`）与词汇都是它自己的。
把它塞进 core 的 Content-Length 帧与 `domain/verb` 方法表里，会让那张表
同时说两种语言。

**复用的是什么**：回合机器整个是部署自己的——prompt 仍然先落盘成
`parked_prompt` 行，run 仍然在会话的 live 通道上发事件，审批仍然由
harness 的闸门持有、由 durable 行与 `ResumeToken` 结算。只有请求与应答的
形状是 ACP 的。

**关键顺序**：被持有的调用**先结束回合**，然后才问操作者。harness 先写下
审批行、挂起回合、结束 run；控制器在 run 仍活着的时候拒绝 resume，所以
先问会让答案被丢弃、回合永远不继续。ACP 的形状（工具调用 → 权限问题 →
继续）不变，只是问题落在回合结束之后。

**审批闸门**：`approval` 行发布策略，但仓里没有任何东西消费它——策略是个
没人问的服务，于是高风险调用从来不会被持有。ACP 面是「客户端能回答权限
问题」的那个面，所以它装上闸门（`acp::approval::install`）：风险由工具
自己判定（`Tool::risk`），闸门挂在共享的拦截表 `tool/pre-execute` 上。
其余界面保持现状。

**id 命名空间**：本仓发给客户端的请求用字符串 id（`uva-1`…）。客户端也在
同一条会话上给自己的请求编号，共享一个数字计数器会让一边的应答被当成
另一边的。

## 6. 不做的事（诚实声明）

- **客户端目录**：ACP 的 `session/new` 带 `cwd`，本实现记录它，但工具跑在
  **部署的工作区根**——工具注册表是开机时按部署根构造的，一个会话改不了
  它。dsh 与 codex 在会话目录里跑。
- **客户端侧文件系统 / 终端委派**：ACP 允许 agent 反向调用客户端的
  `fs/*` 与 `terminal/*`；本实现不委派（工具在进程内跑，与三个参考
  harness 一致）。
- **每个会话的 MCP 服务器**：`session/new` 里的 `mcpServers` 被记录并
  忽略；本部署用自己的 MCP 配置。
- **会话模式 / 斜杠命令 / 计划 / 子代理**：能力里没声明，客户端不会问。
- **ACP v2**：v1 稳定、v2 是预览；本实现只讲 v1。
- **`session/load` 的分块语义**：回放是整段 transcript 的投影，不是
  v2 的 `compaction_update` 那一套。

## 7. 验证

- `crates/uva-app-server/tests/acp_stdio.rs`——真二进制、真管道：
  握手、未知方法的 `-32601`、坏行的 `-32700`、回合增量与停止理由、
  `session/load` 回放、**被持有的调用发权限问题、拒绝后回合继续且命令
  没有执行**、**权限问题开着时取消 → 回合以 `cancelled` 结束**。
- `crates/uva-app-server/src/acp/{framing,updates,wire}.rs` 的单元测试：
  一行一条消息、坏行被报告、事件到 update 的映射、prompt 块的映射、
  权限应答的映射。

## 8. 附录：rust-anything 的机器分析

`rust-anything` v4.1.0 已装在本机（`~/.cargo/bin/rust-anything`，`doctor`
绿，MCP server 报 22 个工具）。它是「把一个应用逐块搬进 Rust、每一步都
对着原版录下来的行为核对」的工具，所以本轮用它对三个参考仓做了一次
机器分析（`rust-anything analyze <repo>`）：

| 仓库 | 规模 | 单元 | Ready / Caution / Blocked / 留在原语言 |
|---|---|---|---|
| dsh | 426 包，3454 源文件，519915 行 | 2138 | 1155 / 774 / 18 / 191 |
| pi | 70 包，1750 源文件，308966 行 | 641 | 362 / 176 / 6 / 97 |
| codex | 4 包（仓内只有 TS/JS 部分） | 18 | 15 / 3 / 0 / 0 |

分析给出的高风险项（对本仓的实现有直接影响的两条）：

- **字符串偏移**：JS 按 UTF-16 码元，Rust 按 UTF-8 字节。本仓的 ACP 面
  不做偏移换算——文本整段透传（`agent_message_chunk` 带原文），不做切片。
- **动态代码**：dsh 的 loader / cordis host runner / webworker runtime 用
  `eval` 与 computed import，没有 Rust 等价物。本仓不移植这些，只在协议
  面上对齐它们**可观察的行为**。

分析工具本身提醒的也是本轮的做法：行为对齐要对着**录下来的行为**核对，
而不是对着文档——所以本仓的 ACP 面用真二进制的管道测试钉住形状
（`tests/acp_stdio.rs`），而不是只读文档。
