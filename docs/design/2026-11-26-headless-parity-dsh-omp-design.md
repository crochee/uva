# 无头 harness 的业界对齐：逆向 dsh 与 omp 的缺口账本

日期：2026-11-26
状态：进行中（本文件是账本；每刀的落地与验收写在自己的小节里）
取代：无
被取代：无

---

## 1. 意图与范围

上一轮（[`2026-11-25-acp-headless-harness-design.md`](2026-11-25-acp-headless-harness-design.md)）
逆向了 **dsh / pi / codex**，把本仓的 ACP 面做到与三者同一台机器上的同一种行为。

本轮把参考集换成 **dsh（DeepSeek Harness）** 与 **omp（oh-my-pi）**，问的是同一个问题：
**本仓的无头 agent harness（核心 JSON-RPC 面 + ACP 面 + 网关面）离业界先进的形态还差什么。**

范围限定在**无头面**：核心事件流、会话持久化、回合机器、审批、取消、委派控制、
控制面协议（帧、游标、订阅、能力协商）。前端（TUI / 浏览器 / 桌面壳）只在
「同一套核心事件流」的意义上出现，不在本轮的对账范围里。

不在范围：模型质量、提示词工程、成本优化、供应商覆盖面。

---

## 2. 逆向方法

两个参考仓都拉到本地，逐行为定位——问「这个 harness 在哪里做 X」，
答案是一个 `path:line`，再读那一段实现：

```bash
git clone --depth 1 https://github.com/deepseek-ai/deepseek-harness /tmp/dsh
git clone --depth 1 https://github.com/can1357/oh-my-pi /tmp/omp
```

dsh 是 Cordis 插件 monorepo（TypeScript），无头控制面在
`packages/bundle/headless`（`src/startup.ts` + `src/index.ts` +
`src/json-stream.ts`）；omp 是 Rust 核心 + TypeScript 产品层（~80k 行 Rust），
无头控制面在 `packages/coding-agent/src/modes/rpc`（13.6k 行）与
`packages/agent/src/agent-loop.ts`。

证据规则：**每条结论都带 `path:line`**；实现是事实，文档只是意图。

---

## 3. 行为对账表

| 维度 | dsh | omp | 本仓 | 判定 |
|---|---|---|---|---|
| 回合机器 | turn → step → 模型 → 工具批（`packages/core/agent-loop/src/agent.ts:203`） | `agent_start` → turn → message → 工具批 → `turn_end`（`packages/agent/src/agent-loop.ts:1153`） | 同形（`crates/uva-harness/src/agent/re_act/loop/mod.rs:300`） | 对齐 |
| 工具批并发 | 默认并行，**只有显式 `true` 才并行**，其余独占（`packages/core/tools/src/pipeline.ts:100-113`） | `shared` / `exclusive`，独占串行（`packages/agent/src/agent-loop.ts:4036-4076`） | 按 `Tool::mode()` 分桶（`crates/uva-harness/src/agent/re_act/loop/step/tools/mod.rs:162`） | 对齐（声明式 vs 声明式） |
| 结果提交序 | 按模型顺序提交，不按完成顺序（`packages/core/tools/src/batch.ts:135-146`） | 同（`agent-loop.ts:4204-4265` 合成结果） | 按 LLM 调用顺序（`…/step/tools/mod.rs:126`） | 对齐 |
| 取消时未跑的调用 | 合成错误结果，日志仍可回放（`packages/core/tools/src/batch.ts:250-260`） | 合成结果，`aborted`/`skipped`/`length` 各有文案（`agent-loop.ts:4204-4265`） | 未开始的调用**不跑**（取消检查在工具流的 drain 里，`…/step/tools/route.rs:168`），已跑完的结果照旧 | 对齐 |
| 日志即真源 | `session.deriveMessages()` 从日志派生（`agent.ts:203`） | `session-entries.ts` 逐条追加 | 审批 / 压缩 / resume 全是日志折叠 | 对齐（本仓更强：单写者） |
| 单写者 | 进程内 + `flock` 跨进程（`packages/session/session-persistence-jsonl/src/lease.ts:1-24`） | 进程租约 `~/.omp/run/session-owners/<id>`，发现别人在写就**换新文件**（`docs/session.md:648-651`） | 进程内单写者 + **`owner.lock` 跨进程所有权**（切 9）：写路径首次追加时 flock 认领、拒绝**失败关闭**（不换文件——本仓的会话 id 就是身份） | 对齐 |
| 崩溃修复 | 断尾截断 + 开回合补closer（`packages/core/session/src/repair.ts:59-105`） | 追加不 fsync，落盘即返回（`packages/coding-agent/src/session/session-storage.ts:66-76`） | `crates/uva-session/src/repair/mod.rs` 合成 `tool_result` / `step_end` / `turn_end` | 对齐 |
| 审批闸门 | `tools/pre-execute` 水瀑，先于任何工具体（`packages/core/tools/src/pipeline.ts:196-215`） | 扩展包装器再查一次**真正要执行的参数**（`packages/coding-agent/src/extensibility/extensions/wrapper.ts:374-389`） | 五级否决流水线（`…/loop/guard.rs:47-85`） | 对齐 |
| 审批**超时** | 工具级 `timeoutMs` → `TOOL_TIMEOUT` 结构化错误（`packages/guard/timeout-policy/src/index.ts:26`） | **没有超时**：要么有人回答，要么回合被 abort（`wrapper.ts:495-500`） | **没有超时**；而 `crates/uva-app-server/src/rpc/mod.rs:29` 却写着「a timeout still defaults to deny」 | **缺口 G1（本轮切）**：文档承诺了代码没有的东西 |
| 审批可分辨的拒绝 | `allowed-once / rejected / cancelled / unavailable` 四态（`packages/interaction/user-approval/src/types.ts:31-34`） | 拒绝文案分工具策略 / 用户策略（`packages/coding-agent/src/tools/approval.ts:591-602`） | `resolved / cancelled / rejected`（`crates/uva-approval/src/approval.rs:305-309`） | 对齐（无「超时」态，随 G1 一起补） |
| 审批策略来源 | `permission-presets` 写 `setApprovalPolicy`（`packages/interaction/permission-presets/src/index.ts:180-250`） | `tools.approvalMode` + 每工具策略 + 每目标授权（`docs/approval-mode.md`） | **行配置被忽略**：永远发布 `ApprovalPolicy::default()`（`crates/uva-app-server/src/composition/kernel/mod.rs:975-979`） | **缺口 G1b（本轮切）** |
| 压缩 | 摘要指令作为**最后一条 user 消息**追加 ⇒ 复用 KV cache（`packages/compaction/compaction-basic/src/summarizer.ts:74-79`） | 缓存友好的裁剪：只裁「省得够多且结果不太小」的（`packages/agent/src/compaction/pruning.ts`） | **同形**（见「切 6」）：`SummaryInput` 携带会话自己的 system prompt 与工具 schema，摘要请求 = `[system, …shadowed…, user(指令)]`——上一个被路由请求的真前缀；裁剪到 `[tool result pruned]` 保持不变 | 对齐 |
| 签名思维块 | 前缀摘要变了就**剥掉**签名思维块，否则整请求被拒（`packages/llm/llm-pi-ai/src/prefix-binding.ts:31-93`） | 同类机制（`prefixDroppedThinkingBlocks` 会话状态） | 压缩后原样回送签名块（`crates/uva-provider/src/anthropic/provider/transform/mod.rs:410-415`） | **缺口 G5**：压缩后 Anthropic 会 400 |
| 原生 replay state | `ReplayEnvelope` 按块对齐，装配丢块就丢信封（`packages/llm/llm/src/assembler.ts:130-152`） | `ProviderPayload` 不透明载荷（`packages/ai/src/types.ts:1494`） | **字段存在但没人写**：`replay_state` 全仓只有 `None`（`crates/uva-provider/src/types/completion/mod.rs:42,76`） | **缺口 G2**：声称已对账的能力是死的 |
| 原始 delta 留存 | 压缩的记录列表，可无损展开（`packages/llm/llm/src/assistant-stream.ts:99-181`） | transcript 存消息，不存 delta | 词汇有 `assistant_chunk`，**没有生产者**；且有测试钉住「每回合一行」（`crates/uva-harness/src/agent/re_act/tests/lifecycle/one_row_per_turn.rs:285`） | 有意不同（记录级 vs 行级），**不算缺口** |
| 插件树 | entry 模型 / group / 按 id diff / 三态审计（`packages/preset/agent-preset-registry/src/mount.ts:115-152`） | — | 同一套（`crates/uva-kernel/src/mount.rs:216-238`） | 对齐 |
| 插件树**审计报告** | `auditRows` → `{failed, pending}` 两张表（`mount.ts:115-152`） | — | **已落**（切 7）：`profile/audit` 返回不能用的行 + 原因（`failed` / `pending` + 缺的 key），dsh 语义 | 对齐 |
| 委派控制面 | `interrupt()` 鉴权到祖先链（`packages/subagent/subagent/src/manager.ts:196-233`） | RPC：`get_subagents` / `cancel_subagent` / `steer_subagent`，子代理记录按字节增量读（`packages/coding-agent/src/modes/rpc/rpc-subagents.ts:74-107`） | 机器有（`crates/uva-tool-task/src/control/mod.rs:389` 的 `kill`、`registry`），**但没有无头门** | **缺口 G8（本轮切）** |
| 控制面协议 | 自有 JSON-RPC stdio server（`packages/sdk/server/src/server.ts:609-667`） | 自有 JSONL：~90 条命令、协议协商、**分块帧**（`wire/commands.ts:35-454`、`rpc-frame.ts:6,9`） | JSON-RPC 2.0 + Content-Length 帧，64 MiB 逻辑上限，超限即错（`crates/uva-app-server/src/rpc/framing.rs:32`） | **对齐（切 10）**：逻辑上限同 omp v2；分块/协商对 Content-Length 不适用（见切 10）；出站背压落盘 |
| 增量读 | — | `get_entries?since` + 分页游标（`docs/rpc.md`） | `sessions/events` **每次整本**返回（`crates/uva-app-server/src/routes/sessions/mod.rs:827-855`） | **缺口 G11（本轮切）** |
| 客户端侧工具 | — | `set_host_tools` + `host_tool_call` 帧（`packages/coding-agent/src/modes/rpc/host-tools.ts:76`） | **fs 委派已落地**（客户端声明 `clientCapabilities.fs` 后，会话的 `read`/`write` 经 ACP `fs/*` 回到客户端，见「切 5」）；`terminal/*` 与每会话 MCP 仍如实缺席 | **半缺口 G12**：terminal 半边留下（本部署没有终端操作可委派） |
| 帧内事件投影 | `--json` 只投影**已提交**事件，单串 8 KiB / 单事件 32 KiB 上限（`packages/bundle/headless/src/json-stream.ts:20-27`） | v1 物理帧 1 MiB，v2 分块到 64 MiB（`rpc-frame.ts:6,9`） | live 事件 + durable 行两路（`crates/uva-app-server/src/routes/chat/stream.rs:1-40`） | 对齐（上限策略不同，见切 10） |
| 退出契约 | 终态 `completed` → 0，其余 → 1（`packages/bundle/headless/src/index.ts:430-440`） | `prompt_result.status`（`docs/rpc.md`） | `chat/turn/event` 的 `end.reason`（`routes/chat/stream.rs:270-280`） | 对齐 |

---

## 4. 缺口清单（按严重度）

| # | 缺口 | 严重度 | 证据 |
|---|---|---|---|
| G1 | **被持有的审批永不超时**：无头客户端不在，审批永远开着，会话永远暂停；文档却写着「a timeout still defaults to deny」 | 高 | `crates/uva-app-server/src/rpc/mod.rs:29`；`crates/uva-approval/src/approval.rs:305-309` |
| G1b | **`approval` 行的 config 是摆设**：发布的一直是 `ApprovalPolicy::default()` | 高 | `crates/uva-app-server/src/composition/kernel/mod.rs:975-979` |
| G2 | ~~`replay_state` 是死字段~~ **已结**（切 8）：字段背后的机制——推理连续性——已由「签名 + 压缩边界规则」结构化交付；两个在仓适配器都没有可铸造的原生状态（OpenAI 走 Chat Completions，Anthropic Messages 无可回放的 response id）。字段保留为 seam，文档如实 | — | 同左 |
| G5 | **压缩后原样回送 Anthropic 签名思维块**：签名绑定请求前缀，压缩改前缀 ⇒ 整请求被 provider 拒 | 高 | `crates/uva-provider/src/anthropic/provider/transform/mod.rs:410-415` |
| G8 | **委派没有无头控制面**：子代理跑飞时，宿主无法列举 / 杀 / 转向 | 高 | `crates/uva-tool-task/src/control/mod.rs:389`；`crates/uva-app-server/src/rpc/methods/names.rs` 无对应方法 |
| G11 | **durable 读没有游标**：整本回传，客户端只能 O(n²) | 中 | `crates/uva-app-server/src/routes/sessions/mod.rs:827-855` |
| G4 | ~~压缩摘要是旁路请求~~ **已落**（切 6）：`SummaryInput` 信封让摘要复用被路由请求的暖前缀 | — | `crates/uva-session/src/compaction/mod.rs`（`SummaryInput`） |
| G7 | ~~并行工具桶不观察取消~~ **已驳回**：取消检查在工具流的 drain 里（`crates/uva-harness/src/agent/re_act/loop/step/tools/route.rs:168`），两个桶都经过它——未开始的调用不跑。补了一个回归钉子（`agent/re_act/tests/parallel_unsafe.rs`） | — | 同左 |
| G9 | ~~无跨进程会话租约~~ **已落**（切 9）：`owner.lock` + flock，append 拒绝失败关闭 | — | `crates/uva-session/src/ownership.rs` |
| G10 | ~~无分块帧 / 协议协商 / 背压落盘~~ **已落（切 10）**：背压落盘 + 64 MiB 逻辑上限（omp v2 同值）；分块帧与协议协商对 Content-Length 传输**不适用**（逐条理由见切 10） | — | 同左 |
| G12 | ~~无客户端侧 fs/terminal 委派~~ **已落**（切 5 的 fs 半边 + 切 11 的 terminal 半边）：`fs/*` 与 `terminal/*` 两套委派都走同一条「客户端拥有」的路 | — | 同左 |
| G6 | ~~插件树没有审计报告面~~ **已落**（切 7）：`profile/audit` | — | `crates/uva-app-server/src/composition/kernel/mod.rs`（`RowAudit` / `plugin_tree_audit`） |

---

## 5. 本轮切刀

### 切 1 — 审批有截止时间（G1 + G1b）

**要什么**：一个被持有的调用在 `approval.timeout_secs` 内没有答案，就按「拒绝」结算，
决定**落进日志**（`approval_decided`），回合**继续**（模型看到一条明确的超时拒绝），
而不是永远挂着。

**怎么落**：

1. `ApprovalPolicy` 加 `timeout_secs: Option<u64>`（策略是部署配置，秒是它能说的粒度）。
2. `approval` 行的 `config` 成为部署策略的真源（mode / ask_unknown / overrides /
   grants / timeout_secs）——行的配置第一次真的生效（G1b）。
3. 控制器在「运行以 `AwaitingApproval` 收尾」时**起一个定时器**；到期后走**同一条**
   resume 路径（派生 token → 构造 `rejected` 决定 → 提交）。定时器到期时先读日志：
   没有开口的审批 ⇒ 有人已经回答过了，什么都不做（幂等）。

**为什么定时器在宿主而不是 harness**：harness 在挂起时就把回合**结束**了
（`crates/uva-app-server/src/adapters/acp/turn.rs:16-22` 记录了这个顺序约束），
等待操作者是宿主的事；宿主也正是持有策略与日志的那一层。

**落点**：
- `crates/uva-approval/src/approval.rs`：`ApprovalPolicy::timeout_secs` +
  `timeout()`；`ResumeStatus::TimedOut`（日志词汇仍是 `denied`，模型可见的
  文案区分「没人回答」与「有人说不」）。
- `crates/uva-app-server/src/config/server/mod.rs`：`[approval]` 配置段
  （`mode` / `ask_unknown` / `timeout_secs` / 逐工具 `overrides`）→
  `ApprovalConfig::policy()`。
- `crates/uva-app-server/src/composition/kernel/mod.rs`：`approval` 行发布
  **部署的**策略（此前发布的是写死的默认值）；`BootContext::with_approval`
  承载它。
- `crates/uva-app-server/src/session/controller/dispatch.rs`：控制器在会话
  处于 `AwaitingApproval` 时**起**截止时间；到期走与人工作答同一条 resume
  路径（派生 token → 一次一个决定 → 提交 → 起跑）。

**验收**：
- 单元：`ApprovalConfig` → 策略（`config/tests.rs`）；`TimedOut` 的日志状态
  与模型文案（`uva-approval`）；策略带期限（`uva-approval`）。
- 行为（`crates/uva-app-server/src/session/controller/tests/approval_deadline.rs`）：
  挂起 → 超时 → 回合以 `TimedOut` 决定继续；同一时刻人工回答 → 决定仍是
  `Resolved`，截止时间不再决定任何事。

### 切 2 — 委派的头控制面（G8）

**要什么**：宿主能列举一个会话的子代理、硬杀一个、给它发一条消息——
omp 的 `get_subagents` / `cancel_subagent` / `steer_subagent`
（`packages/coding-agent/src/modes/rpc/wire/commands.ts:196-232`）。

**怎么落**：机器已经在（`crates/uva-tool-task/src/control/`：`ControlSurface::kill`、
`registry.list`、steer 通道），缺的是**门**：`tool-task` 行把控制面挂到
`BootServices` 的槽位（与 `subagent_writers` / `session_index` 同一手法），
核心 JSON-RPC 面加三条方法：`subagents/list`、`subagents/cancel`、`subagents/steer`。

**落点**：
- `crates/uva-tool-task/src/plugin.rs`：`tool-task` 行把控制面发布为
  `control` 服务（manifest 的 `provides`）。
- `crates/uva-tool-task/src/control/tools.rs`：steer 的投递
  （先落 durable 行，再进 inbox）提成 `deliver_steer`，模型与宿主走**同一
  扇门**。
- `crates/uva-app-server/src/routes/subagents.rs` + `rpc/methods/subagents.rs`：
  三条方法（`subagents/list` / `subagents/cancel` / `subagents/steer`）。

**验收**：`crates/uva-app-server/src/routes/tests/subagents.rs`——名册列出
子代理；取消是终态且**幂等**（第二次答 `cancelled: false`）；steer 送达
运行中的子代理；未知子代理被拒并说明理由。

### 切 3 — durable 读的游标（G11）

**要什么**：`sessions/events` 接受 `since`（一个序号），只回它之后的行，并回一个
`last_seq` 让客户端接着读——omp 的 `get_entries?since`
（`docs/rpc.md`「Read the append-history; with `since`, only entries strictly after
that durable entry id」）。

**怎么落**：行序号是本仓的日志坐标（`LogWriter` 分配，见
`crates/uva-session/src/writer/mod.rs:150-160`），所以游标就是一个序号；
过滤发生在读之后、分类之前。

**落点**：`crates/uva-app-server/src/routes/sessions/mod.rs`
（`list_session_events(state, id, since)` + `TypedEventResponse::last_seq`）
与 `rpc/methods/sessions.rs`（`EventsParams`）。

**验收**：`crates/uva-app-server/src/routes/sessions/tests.rs`——整本读、
带游标的读（只回之后的行）、游标在尾部时读为空且游标不动。

### 切 4 — 压缩作废的签名思维块（G5）

**要什么**：Anthropic 把思维块的签名绑定在**它被生成时的请求前缀**上。
压缩改写了前缀，于是幸存下来的签名**作废**；provider 会因此拒绝**整个请求**
（不是只拒绝那个块）。本仓此前原样回送这些签名——压缩后的第一轮请求就是
一次 400。

**怎么落**：作废的判定放在**知道前缀被改写过的那一层**（harness），而不是
provider：

- 日志是**只追加**的，能改写消息前缀的只有压缩（`SurfaceOp::Replace`）；
- 因此「签名前缀被改写过」等价于「这条消息写在某次压缩之前」；
- harness 在两处清理：开跑前（从日志里的压缩行算出边界）与本轮压缩落盘后
  （边界就是压缩行的序号）。

清理的是**签名**而不是整块思维：签名清空后该 part 变成「无签名」，而
provider 自己的规则（本仓的 Anthropic transform）就会丢掉无签名的思维块——
**模型失去那段推理，回合不失去**。

规则是**粗**的：压缩点**之前**那些其实前缀未变的签名也会被作废。作废一个
有效签名只是让模型少一点上下文；保留一个作废签名会赔上一整个回合。精确
到每条消息的判定需要前缀摘要（= G2 的 `replay_state`），那是下一刀。

**落点**：
- `crates/uva-harness/src/agent/re_act/transcript.rs`：
  `Transcript::void_signatures_before(boundary)`。
- `crates/uva-harness/src/agent/re_act/loop/step/prepare.rs`：开跑前从日志的
  压缩行算出边界并清理。
- `crates/uva-harness/src/agent/re_act/loop/compaction/mod.rs`：本轮压缩落盘
  后按压缩行的序号清理。

**验收**：
- 单元（`transcript.rs`）：边界之前的消息失去签名，边界之后的保留。
- 行为（`agent/re_act/tests/compaction.rs`）：日志里带一次压缩、助手轮带签名
  思维时，发给 provider 的请求里那个 part **无签名**，而助手轮的正文仍在
  请求里（证明作废的是签名，不是整条消息）。

### 切 5 — 客户端侧的文件系统委派（G12 的 fs 半边）

**要什么**：ACP 允许 agent 反向调用客户端的 `fs/*`——一个声明了
`clientCapabilities.fs` 的客户端拥有它开的会话的**文件**：`read` / `write`
工具以客户端的 `cwd` 为根，读写经 `fs/read_text_file` /
`fs/write_text_file` 回到客户端。此前本仓的 ACP 面显式声明「不委派」，编辑器
（Zed、JetBrains）驱动时工具跑在部署的盘上，读写的是部署的文件副本。

**怎么落**：一个 seam，一个注册表，一个 owner：

- **seam**（`uva-tool-plugins::files`）：`FileSystemOwner` ——「这个会话的
  文件在哪里、谁持有字节」。`session_root`（默认 `None` = 部署的工作区）、
  `read_file`、`write_file`。`FileError::Missing` 与 `FileError::Failed`
  分开——「文件不存在」与「读取失败」把工具引向不同的下一步。缺目录的创建
  归 owner：部署的盘能建，客户端（ACP 没有目录操作）不能，写入就按客户端
  自己的回答失败。
- **注册表**（app-server 的 `session::files::SessionFileSystem`）：会话 id →
  客户端 owner。没人认领的会话落到部署自己的盘——与委派存在之前完全一致。
- **owner**（`adapters/acp/files.rs`）：`ClientFileSystem` 持有连接 + 客户端
  的 `cwd` + 它在 `initialize` 里声明的能力；`read_file` /
  `write_file` 把 ACP 请求发给客户端（未声明的能力是**明确的错误**，不是
  静默回退到本地盘）。

**顺序**：`read` / `write` 工具先把路径按 `session_root` 解析并做安全检查
（客户端的会话以客户端的 `cwd` 为根），再交给 owner；工具的渲染
（行号、Created/Updated 文案）不变——**模型看到的形状不因文件在哪台机器
上而变**。

**落点**：
- `crates/uva-tool-plugins/src/files/mod.rs` — the seam + `LocalFileSystem`。
- `crates/uva-tool-plugins/src/{read,write}/mod.rs` — the tools hold the owner。
- `crates/uva/src/tool_files.rs` — the facade module（`tool-files` feature）。
- `crates/uva-app-server/src/session/files.rs` — the registry。
- `crates/uva-app-server/src/composition/kernel/mod.rs` — the `tool-read` /
  `tool-write` rows get the registry as their owner（移到 services-bound
  工厂列表）。
- `crates/uva-app-server/src/adapters/acp/{files,agent}.rs` — the client
  owner + the registration（`initialize` 记能力，`session_new` 认领会话）。

**验收**：
- 单元：本地 owner 读写盘、报告 missing（`files`）；注册表认领 / 释放 /
  未认领回退（`session::files`）。
- 行为（`tests/acp_stdio.rs`）：真二进制 + 真管道——客户端声明
  `fs.readTextFile`，开会话（`cwd = /client`），prompt 触发 `read`；agent 发
  `fs/read_text_file`（path = `/client/notes.txt`——**客户端自己的路径**），
  客户端答 `"from the client"`，模型的工具结果里就是那些字节。

**留下的半边**：`terminal/*` 委派（ACP 允许，本部署没有终端操作可委派）与
每会话 MCP 服务器。这两项在能力里继续如实缺席。

### 切 6 — 缓存友好的压缩（G4）

**要什么**：压缩的摘要请求是一个**旁路**请求——自己的 system prompt、把
span 渲染成一段文字。provider 的前缀缓存对它毫无用处，每轮压缩都是一次
全新计费。dsh 的形状（`compaction-basic/src/summarizer.ts:74-79`）：摘要请求
重放**会话自己的** system prompt 与工具 schema、被遮蔽的消息按 surface 顺序、
指令作为**最后一条 user 消息**——它是上一个被路由请求的**真前缀**，暖缓存
覆盖除指令外的一切。

**怎么落**：`SummaryInput { system: Option<String>, tools }` 走完整条路：

- **harness 的步内压缩**（热路径）：`RunState` 出 system prompt，
  `RunState::projection` 出工具 schema——与下一个请求**逐字节相同**的投影
  （被调用过的 Deferred 工具的晋升也一致）。
- **手动压缩**（`sessions/compact`）：路由只有日志、没有 run，拿不到请求
  信封——`SummaryInput::fresh()` 的独立形状是诚实的回退（span 渲染成文字，
  指令仍是最后一条消息）。
- **工具 schema 上了请求但答复必须是文字**：`ToolChoice::None`。空 schema
  列表保持请求自己的默认（`Auto`）——Anthropic 拒绝带 `tool_choice` 却没有
  tools 的请求，手动路径会因此 400。

**顺手修掉一个潜伏的适配器缺陷**：Anthropic 适配器此前**完全忽略**
canonical `CompletionRequest.tool_choice`——任何调用方设 `None` 都被静默
当作 `auto`。现在映射到线上字段（`none`/`any`/`tool{name}`；`auto` 省略——
Anthropic 默认就是它）。OpenAI 适配器本就处理 `none`，不动。

**落点**：
- `crates/uva-session/src/compaction/mod.rs`：`SummaryInput` +
  `Compactor::summarize(plan, input, cancel)` + 新指令（dsh 的分段骨架）。
- `crates/uva-harness/src/agent/re_act/loop/compaction/mod.rs`：步内压缩
  重放 run 自己的信封。
- `crates/uva-provider/src/anthropic/{types.rs,provider/transform/mod.rs}`：
  `AnthropicToolChoice` 线上类型 + 映射。
- `crates/uva-app-server/src/routes/follow.rs`：手动压缩的回退信封。

**验收**：
- 单元（session）：带信封的摘要请求 = `[system, …shadowed…, user(指令)]`，
  schema 重放且 `tool_choice = None`；无信封 = `[user(render), user(指令)]`
  且 `tool_choice = Auto`（Anthropic 空工具陷阱）。
- 行为（harness）：摘要请求的第一条消息与**主循环被路由的请求**的 system
  prompt 逐字节相同，指令是最后一条 user 消息，`tool_choice = None`。
- 适配器（provider）：四个 `ToolChoice` 变体的线上形状各一条（`Auto` 省略）。

### 切 7 — 插件树的审计报告（G6）

**要什么**：dsh 的 `auditRows`（`mount.ts:115-152`）回答一个问题——「这棵树
有没有不能用的行」。本仓此前只有逐 id 的 `PluginState` 查询，没有报告面：
一个部署跑起来，操作者看不到哪一行在等什么。

**怎么落**：`profile/audit`，dsh 的三态语义——不在报告里的行就是 **usable**，
报告里只有两类：

- `failed`：从未启动，或激活被拒（缺的 key 为空——失败是激活的，不是依赖的）；
- `pending`：在等一个组合不提供的服务，`missing` 逐一列出。

缺口计算复用 mount 自己的不动点（`audit_pending` 的同一套：live 行的
provides 汇聚 + 父作用域已可解析的 key），所以报告说的与 mount 会说的是同一
件事——修好一个依赖后两边同时转绿，不会漂移。

**落点**：
- `crates/uva-app-server/src/composition/kernel/mod.rs`：`RowAudit`（wire
  类型）+ `plugin_tree_audit`（与 `audit_pending` 共享 live_provides /
  parent_service_available 的同一台计算）。
- `Assembly::audit` / `AppState::profile_audit` → RPC 方法 `profile/audit`
  （名字表 + 注册 + 两侧一致性测试随方法表自动覆盖）。

**验收**：健康树报告为空；一行注入不存在的 key → 内核状态 `Pending`，审计
报告 `pending` 且 `missing = [key]`。

### 切 8 — `replay_state` 的诚实结算（G2）

**这个缺口是什么**：`CompletionRequest/Response.replay_state` 全仓只写
`None`，而历史 CHANGELOG 声称「已按 dsh §7 对账」。字段死了，声明没死。

**核实三件事之后的结论**：

1. **两个在仓适配器都没有可铸造的原生状态。** OpenAI 适配器说
   Chat Completions（`openai/provider/request.rs:25`）——`previous_response_id`
   属于 Responses API；Anthropic Messages 没有可回放的 response id
   （响应的 `id` 不被请求接受）。
2. **dsh §7 的 `replayState` 机制，本仓已结构化交付。** dsh 用它做的事是
   推理连续性：Anthropic 的思维块签名绑定请求前缀。本仓的签名随
   `ContentPart::Reasoning` 本身持久化，而切 4 的边界规则把它作废判定
   做对了——**且是精确的，不是近似**：压缩的 span 永远从 surface 头开始
   （`compaction/mod.rs:425`），所以每条 kept-tail 消息的前缀都包含被
   遮蔽的 span，不存在「前缀幸存却仍被作废」的消息。本仓唯一的另一个
   前缀改写者是 steer 的重排，而 `ModelOrder` 把 held-back 的 steer 放
   在工具结果之后、下一条 assistant 之前——生成序与 surface 序对签名
   而言一致。新增回归钉子：`a_signature_in_the_kept_tail_is_voided_too`。
3. **dsh 的逐消息前缀摘要在此不划算。** 它需要把摘要持久化到每条
   assistant 行（行 schema、fold、api-frozen 基线、web types 全动），
   换来的只是在「任意前缀编辑」场景下省一点上下文——而本仓今天没有那种
   改写者。在边界规则已精确的前提下，那是投机性基建。

**落点**：
- `crates/uva-provider/src/types/completion/mod.rs`：两个 `replay_state`
  字段的文档改为如实——在仓适配器不铸造；连续性由「签名 + 压缩边界规则」
  结构化承载；字段是**给会铸造的适配器留的 seam**（Responses-API 适配器的
  `previous_response_id`、逐消息前缀摘要），铸造在响应、回传在请求、校验
  在适配器——seam 两端已存在。
- `crates/uva-harness/src/agent/re_act/tests/compaction.rs`：kept-tail 的
  精确性钉子。

**判定**：G2 以「机制已交付、声明已对齐、seam 留给真实载荷」结案，而不是
以「给一个无人消费的字段再接一层管线」结案。

### 切 9 — 跨进程的会话所有权（G9）

**要什么**：两个进程共享一个 workspace 时，都能打开同一会话并各自追加——两个
写者盖出**碰撞的序号**，而一个序号无法复现的日志是不可读的。dsh 用
`flock(2)` 于 `session.lock`（`lease.ts:1-24,66-88`）：读句柄自由共存，
**无过期**——僵死的持有者锁到进程死为止（过期能撕开日志）。

**怎么落**：`owner.lock` 认领在**首次追加**时（`State::ensure_ownership`），
不是在 sink 构造时——列表会话时每个 durable 会话开一个 sink 只读，读不认领
（dsh 的读句柄规则）。持有到 sink 落地/进程退出：崩溃进程的内核关闭描述符
即释放。**拒绝失败关闭**：第二个写者的 append 得到
`SessionError::OwnedByAnotherProcess { holder: Some(pid) }`——写路径的既有
语义本就是致命错（「洞让后续压缩不可解」），拒绝沿着同一条路走。与 omp 的
差异如实记录：omp 换新兄弟文件（它的会话 id 允许），本仓的会话 id 就是
身份，换文件等于丢身份——拒绝是唯一安全答案。

**落点**：
- `crates/uva-session/src/ownership.rs`：`SessionOwnerLock`（flock
  `LOCK_EX|LOCK_NB`，pid 写进文件供拒绝点名；非 unix 用 O_EXCL + pid，
  僵死文件同样失败关闭——更重，但不出错）。
- `crates/uva-session/src/jsonl/mod.rs`：`append` / `append_many` 的
  closed / 版本门之后、任何字节移动之前认领。
- `crates/uva-session/src/sink.rs`：`SessionError::OwnedByAnotherProcess`。

**验收**：
- 单元（ownership）：同目录第二次认领被拒且点名 pid；drop 后可再认领。
- 行为（jsonl）：同一目录第二个 sink——开与读不认领（读共存）；append 被拒
  （点名 pid）；owner drop 后第二个 sink 写入成功、两行都在日志里。

---

### 切 10 — 出站背压落盘 + 逻辑上限（G10 的可迁移部分）

**要什么**：omp 的 `rpc-frame.ts`：v1 物理帧 1 MiB，v2 分块重组到 64 MiB；
背压时把待发字节**溢写到私有临时文件**，按 64 KiB 块排空
（`rpc.md`：「the server spills pending bytes to a private temporary file
and drains it in 64 KiB blocks」）。本仓的传输是 JSON-RPC 2.0 +
Content-Length 帧——**没有「一行」就没有分块的理由**，但**「读者比作者慢」的
理由完全一样**：出站通道是无界的，慢读者让进程内存无限增长。

**怎么落**：
1. 出站拆成两个任务：**编码器**（把信封通道排空成帧字节）与**写入器**（拥有
   stdout）。编码器急切地编码，慢读者不再在通道里堆 `Value` 图——帧字节比
   信封小一个数量级。
2. 队列 = **内存段**（水位 4 MiB）+ **磁盘段**（spool 临时文件）。溢出之后
   新帧一律进磁盘段，因此**到达顺序**由构造保证：内存段永远排在磁盘段
   之前。排空按 64 KiB 块，内存里只留一块。
3. 入站逻辑上限 8 MiB → **64 MiB**（omp v2 的重组上限）。Content-Length
   没有物理帧上限要分块绕过，唯一可能被撞到的旋钮就是逻辑上限，而 64 MiB
   是行业客户端预期能发的天花板。

**为什么不抄「分块帧」与「协议协商」**：Content-Length 按长度定界，一个帧
就是一个 JSON 值——把它拆开要在 JSON-RPC 层加重组协议（omp 的
`chunk_begin/chunk/chunk_end` 是它自己 JSONL 帧协议的形状），那是一次
**线格式破坏**：所有客户端都要跟着改，而本仓的帧里装的是 JSON-RPC 2.0 的
单一 JSON 值。**协议协商**在只有一个版本的协议里是投机——协商出来的第二
个版本还不存在。本仓的等价旋钮就是帧上限，已对齐 omp 的逻辑天花板。

**落点**：
- `crates/uva-app-server/src/rpc/spool.rs`：`Spool`（私有临时文件，pid + nonce
  命名，64 KiB 块排空，排空完即删；写失败**大声失败**而不是无声丢帧）。
- `crates/uva-app-server/src/rpc/stdio.rs`：`OutboundQueue`（内存段 + 磁盘段，
  到达顺序，内存水位可测）+ 编码器/写入器两个任务。
- `crates/uva-app-server/src/rpc/framing.rs`：`encode_frame`（帧的字节形态，
  编码器用它拼帧）+ `DEFAULT_MAX_FRAME_SIZE` 8 MiB → 64 MiB。

**验收**：
- 单元（spool）：两帧跨块排空，字节与顺序不变；排空后文件随 drop 消失。
- 单元（queue）：10 MiB 压进队列 → 内存段峰值不超过水位（4 MiB），其余在
  磁盘；排空后字节与顺序不变。
- 行为（stdio）：慢读者（64 字节 duplex）下两帧应答都不丢、顺序稳定。


### 切 11 — 客户端侧终端委派（G12 的 terminal 半边）

**要什么**：ACP 的 `terminal/*`（SDK 5.6：`terminal/create` /
`terminal/output` / `terminal/wait_for_exit` / `terminal/kill` /
`terminal/release`）让**客户端拥有终端**：agent 不在自己机器上 spawn
进程，而是请客户端在它自己的窗格里跑——用户看得见、能打断。omp 的
`set_host_tools` 是同一件事的另一种说法（宿主把自己的工具交给 agent）。

**怎么落**：
1. **一条缝**：`uva_tool_plugins::shell::terminal::TerminalOwner`——与
   `FileSystemOwner` 同构的「谁执行」缝：默认的 `LocalTerminals` 谁也不拥有，
   每个会话都在本机跑；客户端拥有终端的会话走客户端。
2. **工具按会话路由**：`ShellTool` 持有 `Arc<dyn TerminalOwner>`；会话被客户
   端拥有时，命令交给客户端，否则走原来的本机路径（策略、沙箱、进程组、超时
   全部不变）。**描述必须诚实**：工具的描述是静态的（一段文本对所有会话），
   所以它同时写明两条路——本机路径讲策略，客户端路径讲「客户端的规则代替策略」。
   一段谎称沙箱在生效、而命令其实跑在用户窗格里的描述，比没有描述更糟。
3. **注册表**：app-server 的 `SessionTerminals` 与 `SessionFileSystem` 同构
   （会话 id → 客户端 owner），挂在 `AppState` 上，`tool-shell` 行拿到它。
4. **ACP 侧**：`ClientTerminal` 按 ACP 的五个方法实现 `TerminalOwner`：
   `create`（`sh -c <command>`，cwd）→ `wait_for_exit`（带我们自己的截止时间，
   超时则 `kill`）→ `output`（窗格的文本）→ `release`。客户端在
   `initialize` 里声明 `terminal: true`，其会话才登记这个 owner；没声明的
   客户端照旧跑在本机。

**为什么恢复会话也要认领**：`session/load` 与 `session/new` 走同一条
`claim_session`——恢复的会话同样是客户端的会话（文件与终端都是）。此前 fs
半边只在 `session/new` 认领，恢复的会话会落回本机磁盘；这一刀把两边一起对齐。

**落点**：
- `crates/uva-tool-plugins/src/shell/terminal.rs`：`TerminalOwner` /
  `TerminalRun` / `TerminalError` / `LocalTerminals`。
- `crates/uva-tool-plugins/src/shell/mod.rs`：`ShellTool::with_terminals` 与
  按会话的路由；`TERMINAL_OWNER_PROMPT`（诚实的描述）。
- `crates/uva-app-server/src/session/terminals.rs`：`SessionTerminals`。
- `crates/uva-app-server/src/adapters/acp/terminal.rs`：`ClientTerminal`。
- `crates/uva-app-server/src/adapters/acp/agent.rs`：`claim_session`（新旧会话
  同一条认领路径）。

**验收**：
- 单元（shell）：被客户端拥有的会话，命令到客户端、输出与退出码按 shell 的
  形状渲染；没被拥有的会话照旧在本机跑，客户端一次也没被问。
- 单元（registry）：认领的会话走客户端；释放后回到本机。
- 行为（ACP，真实协议）：客户端声明 `terminal` 能力 → 开会话 → 部署把该会话
  的终端登记给客户端 → 部署请客户端跑命令 → 客户端的输出与退出码回到部署
  （`terminal/create` → `wait_for_exit` → `output` → `release` 全部走过）。

---

---

## 6. 下一轮候选（不在本轮）

- **G12 之后**：与编辑器对接的下一层是客户端的**扩展工具**（omp 的
  `set_host_tools`：宿主把自己的工具交给 agent 调用）——ACP v1 没有对应的
  原语，这是协议层的缺口，不是本仓能单方面补的。
- **G10 的 JSON-RPC 重组协议**（若真要抄 omp v2 的分块语义）——那是线格式
  破坏：帧里不再是一个 JSON 值，而是一串需要重组的片段，客户端全都要改。
  切 10 已把**可迁移的部分**落完（背压落盘 + 64 MiB 逻辑上限），剩下的只有
  这一条真破坏。
（G7 已驳回——取消检查在工具流的 drain 里。G2 已结——切 8：连续性已
  结构化交付，seam 留给会铸造原生状态的适配器。G4 已落——切 6。
  G6 已落——切 7。G9 已落——切 9。G10 已落——切 10。G12 已落——切 5（fs）+ 切 11
  （terminal）。）

---

## 7. 诚实声明

- 本轮只做**无头面**的对账；前端三端（浏览器 / 终端 / 桌面壳）的界面能力不在账本里。
- dsh 与 omp 的实现是**参考**，不是依赖：本仓不引入它们的代码，也不引入它们的
  运行时依赖（仓库的依赖纪律见 `AGENTS.md` §1.4）。
- 账本里的「对齐」指**行为同形**，不指实现同构：本仓是 Rust、单一事件流、
  单写者日志，dsh 是 Cordis 插件树，omp 是 Rust 核心 + TS 产品层。
