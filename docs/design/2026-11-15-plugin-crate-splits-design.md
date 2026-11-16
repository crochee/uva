# 插件面下沉：审批 / hook 挂载点 / 子代理 / 流式输出的 crate 拆分设计

日期：2026-11-15
状态：设计定稿——§11 全部 14 项裁定已落（2026-11-15，含两轮拷问）；待批准后按 §9 分期实施（未写任何产品代码）
配套调研：[`docs/research/2026-11-15-plugin-crate-splits-dsh-omp.md`](../research/2026-11-15-plugin-crate-splits-dsh-omp.md)（92 条证据，证据 id 形如 `[E001]`）
取代：无（这是 `AGENTS.md` §3.7 插件原则的下一段，不是补丁）
配套原型：[`docs/prototypes/plugin-splits-b-double-prime.prototype.html`](../prototypes/plugin-splits-b-double-prime.prototype.html)
（单文件双击即开：架构 / 界面 / 三条时序 / 可把玩的状态机；截图 11–15 同目录）

---

## 1. 意图与为什么

用户提了四件事：**授权审批插件独立成一个 crate**、**hook 也抽出来作为一个插件 crate**、
**hook 的那些挂载点能不能作为对外提供插件的挂载点**、**tool 的流式实时输出与子代理分发逻辑也独立出来**。

调研之后，这四件事的**性质并不相同**，混在一个变更里做会互相拖累：

| 诉求 | 调研结论 | 性质 |
|---|---|---|
| 审批独立成 crate | 词汇表 785 行，零 harness 内部类型；唯一被咬住的是生产上从未安装的 `ApprovalGate` `[E028][E029]` | **换包不换形**，机械 |
| 子代理分发独立 | 已是 seam（harness）+ 实现（工具插件族）+ 会话行（session）三段式 `[E035]`；拷问后升级为**档三**：seam 删除、task 变普通 Tool、词汇下沉 | **档三**：删 seam + 词汇下沉（Q1/Q6a 裁定） |
| hook 独立成插件 crate | 三层拦截栈（词汇/注册表/机制）与 7 个挂载点键名**都已存在** `[E019][E021][E031]`；载荷 `HookEvent` 本身也在 harness 之下（`uva-steering`），**真正卡住仓外 crate 的是 trait 与 `HookContext` 住在 harness 里**（后者还带着 harness 私有的 `EventSink`）`[E070][E065]` | **真设计问题**：契约必须下沉到 harness 之下 |
| tool 流式实时输出独立 | seam 已在 `uva-tool`（`Tool::stream` 是主入口）`[E022][E038]`；真问题是**零生产者** + app-server 把第一个 progress 当终态 `[E039][E080]` | **不是拆分问题**：是语义修复 + 一个行 tag + 一个生产者 |

所以本设计的主张是：

1. **只有 hook 那一件需要动结构**。其余两件是搬家，两件是补功能。
2. **搬家的正确形态是"路径保持不变"**：`uva-harness` 保留显式 `pub use`，冻结 API 基线一行不改 `[E041]`。
3. **dsh 的教训是"契约归契约包，实现归实现包"**：dsh 的 `core/agent`（公开 Agent 契约）与
   `core/agent-loop`（具体驱动）分属两包，扩展插件只依赖前者，循环因此可换 `[E001][E003]`；
   dsh 的 `interaction/user-approval` 则是一个**独立能力包**，自带闭合的 fail-closed 结果词表与审计对 `[E006][E007]`；
   dsh 的 `subagent/` 是**seam + provider + tool 三段** `[E008][E052]`。
4. **两个参考实现在同一处意见相反，且与 uva 现状都不同**：omp 允许插件改写工具入参并按改写后的入参重算审批 `[E087]`；
   dsh 明令禁止，理由是"历史、审计、UI、执行必须一致" `[E025][E058]`；而 uva 的 `HookDecision`
   根本没有改写这一档 `[E074]`。**这是选型题，不是移植题**——本设计给出裁决并写进 ADR。

---

## 2. 目标 / 非目标 / 反目标

### 2.1 目标

- **G1**：`uva-approval` 成为独立 crate，承载审批词汇表、决策函数、策略服务与内核插件。
- **G2**：`uva-hook` 成为独立 crate，承载**拦截 trait（`Hook`/`Gate`/transformer）+ 解耦后的 `HookContext` + 挂载点键 + 注册表与仲裁规则**；
  `uva-harness` 退化为分发器。**结果是：一个只依赖 `uva-hook` 的仓外 crate 能实现 handler、拦截一次工具调用。**
- **G3**：`uva-hook-plugins` 成为插件族 crate（一模块一 feature），首个模块由现有 `uva-policy-plugins::reflection` 迁入。
- **G4**：子代理轴档三——seam 删除、task 变普通 `Tool`；`uva-events`/`uva-agent` 下沉后，
  `uva-tool-task` 成为**不依赖 `uva-harness`** 的独立 crate（delegator/gate/worktree/schema + `control`
  模块的 list/message/interrupt）。
- **G5**：tool 流式输出**真的能流**：live 投影区分 partial 与 final、`shell` 产出增量块、增量有 durable 行。
- **G6**：子代理获得 omp 式生命周期（预分配 id、工件集、`running|idle|parked|aborted` 四态、空闲 TTL 停放、
  **冷唤醒**）`[E015][E016]`，以及模型控制面（§6.8）与三层可见性（§6.7）。
- **G7**：每个新 crate 都进得去门禁：冻结基线、pub-surface 棘轮、插件族文件预算、publishable 顺序、
  runtime-free 名单、MVP 禁依赖集、facade feature、`docs/architecture.md` crate 表、`AGENTS.md` §1.1 `[E034][E041]`。

### 2.2 非目标

- **N1**：不做"仓外进程插件"通道。第三方扩展仍是 MCP server（`AGENTS.md` §3.7 现行规则）`[E075]`。
- **N2**：不引入 WASM / 动态组件 / 插件脚本运行时。
- **N3**：不改 provider 协议、不改 session 日志格式版本、不改 JSON-RPC 方法名。
- **N4**：不把 `uva-kernel` 变成 agent 层（`check-kernel-deps` 现行约束）`[E033]`。
- **N5**：不在本设计里实现子代理名册 UI（TUI/web 的呈现另开一份）。
- **N6**：不动 `uva-tool` 的 `Tool` trait 方法集；`stream` 已经是主入口 `[E022]`，只补语义。

### 2.3 反目标（做到即失败）

- **A1**：**为了拆 crate 而拆 crate**——新增一个 crate 却只是把一个类型搬过去，不改变任何可观察行为。
- **A2**：**冻结面无提示地断裂**——`uva_harness::ApprovalPolicy` 之类名字必须继续可导入；破坏必须显式、显式记录。
- **A3**：**插件挂载点名义公开但实际不可用**——G2 的验收是"仓外插件 crate 的依赖树里没有 `uva-harness`，且 handler 真的拦到了一次工具调用"，不是"键名是 pub 的"。
- **A4**：**两套并行的仲裁规则**——外部插件与内部 hook/gate 不得各走一套短路语义。
- **A5**：**用"以后再说"掩盖已知的行缺失**——外部插件 `Block` 而不写 `hook_block` 行的已知缺陷必须在本次收口 `[E032]`。

---

## 3. 约束

### 3.1 仓内硬约束（`AGENTS.md` §1.4 / §3.7 / §3.10）

| 约束 | 对本设计的影响 |
|---|---|
| 依赖单向 `uva` → `uva-*` → `uva-core`，永不回指 | 新 crate 只能被上层依赖；`uva-approval` 不得依赖 `uva-harness` |
| 库 crate 公开面不得出现 tokio/async-std/smol | 新 crate 一律 runtime-free；并发限流必须自实现（§6.5） |
| 每个 `uva-*` 的 `lib.rs` 是公开面的唯一决定处，禁止 `pub use <mod>::*` | 新 crate 用显式名字列表；harness 侧保留**一条**显式 `pub use` 承接冻结名 `[E041]` |
| `check-frozen-api` 抽的是**名字**不是路径 | 搬 crate 不必改冻结基线；只有名字离开 crate root 才要改 `[E041]` |
| `check-pub-surface-strict` 只在 `pub use` **行数**增长时失败 | `uva-harness` 的语句数已是 5 / 上限 5，所以新名字必须**合并进现有 use 树**而不是新加一条（§6.1）`[E041]` |
| `check-plugin-shape`：插件族 crate 的生产文件 ≤ 1086 行，扫描 `src/**` 递归 | 新插件族 crate 必须进 `PLUGIN_CRATE_ROOTS`；拆文件时注意预算 |
| `check-harness-docs`：harness 私有项 `///` 注释预算 = 0 | harness 变瘦时不要顺手加私有文档注释 |
| `check-harness-features`：harness 必须零 `[features]` | 新的可选行为不得进 harness 的 feature 表 |
| `check-single-writer`：会话行只由该会话的 writer 写 | 新增 `tool_output` 行必须走 `crates/uva-session/src/writer` `[E081]` |
| `check-web-contract`：`uva-web/src/api/types.ts` 必须等于服务端重新生成结果 | 动 wire 类型必须同变更重生成并提交 |
| 默认清理切换：迁移所有调用方，删除被取代的旧路径，不留 shim | `ApprovalGate` 若保留必须仍是**唯一**实现，不得新旧两份 |

### 3.2 两个参考实现的约束（取其结构，不取其具体值）

- dsh：`扩展插件依赖 Service Definition，绝不依赖具体 provider` `[E002]`；
  `core/agent` 与 `core/agent-loop` 分包，循环可换 `[E001][E003]`；
  审批是独立能力包、结果词表闭合且 fail-closed `[E006]`；
  工具拦截是 pre-execute / execute / post-execute 三段 waterfall，**最终 guard 只有拒绝、没有放行**，
  所以监听顺序无法把拒绝翻回放行 `[E004]`；
  委派是 seam + provider + tool 三段 `[E008]`。
- omp：扩展是"一个模块导出一个工厂 + `pi.on()` + `pi.registerTool()`" `[E012]`；
  工具拦截成对出现：`tool_call`（前）/ `tool_result`（后）`[E011]`；
  子代理是"策略 + `SpawnRun` + 进程级注册表 + 会话级信号量 + 工件集 + 四态生命周期" `[E015][E016]`；
  审批是自带 `read|write|exec` 闭合档位、未知工具默认 `exec` 的 fail-closed 解析器 `[E042]`。
- **两家的规模数字不移植**（并发 32、停放 7 分钟、输出 500 KB 是 omp 当前默认值）`[E068]`。

---

## 4. 方案与取舍

### 方案 A「只搬不改」（否决）

新增 `uva-approval`、`uva-tool-task` 两 crate，seam 留 harness，流式只修语义。便宜，但不回答
"挂载点能否对外"，也不回答"harness 为什么要背子代理代码"。

### 方案 B「hook 契约下沉」（hook 轴采纳）

`uva-hook`（拦截 trait + 解耦 context + 键 + 注册表）+ `uva-hook-plugins`。回答挂载点那一问；
harness 保留循环与 `Seams` 分发器。

### 方案 C「词汇全下沉 + 删 seam」（子代理轴采纳）

`uva-events` + `uva-agent` 下沉，**删除 `DelegatingTool` seam**，task 变普通 Tool。
两轴分别取 B 与 C 的形状：hook 轴不下沉 `Seams`/`Host`，子代理轴彻底出 harness。

### 选择：B″（两轮拷问定稿）

hook 轴走 B + 子代理轴走 C（档三）+ 词汇下沉 + 模型控制面。关键裁定：**Q1=档三**（harness 子代理
代码归零，task 走 `ToolRegistry` 现成的定义/曝光/restriction/mode 机制，"委派名在 registry 之外
并列广播"的特例消失）；**Q6(a)=词汇下沉**（`uva-tool-task` 与第三方委派者不依赖 harness，与
`uva-hook-plugins` 同层）。**代价实测**：`AgentEvent`/`SystemEvent` 有 55 个 harness 外消费文件，
但"路径保持"手法（新 crate + harness 合并 use 树 re-export）令其**零 import 改动**、冻结名全保——
初稿把 C 标"高风险"是估重了。**Q12=模型控制面**为需求（§6.8）。

---

## 5. 目标架构

```text
                              uva (facade)
                                   │
      ┌───────────────┬────────────┼─────────────────┬──────────────┐
      │               │            │                 │              │
 uva-app-server   uva-tui   uva-hook-plugins   uva-tool-task   (第三方委派者)
      │               │            │                 │              │
      │               │      ┌─────┴─────┐           │              │
      │               │      │           │           │              │
      │          uva-hook    │           │           │              │
      │        (拦截契约)     │           │           │              │
      │               │      │           │           │              │
      └──────┬────────┴──────┘           │           │              │
             │                          │           │              │
        uva-harness ────────────────────┤           │              │
        （纯循环：ReAct + Seams 分发器）  │           │              │
             │                          │           │              │
      ┌──────┴──────────┐               │           │              │
      │                 │               │           │              │
  uva-agent ──────▶ uva-events ◀────────┴───────────┴──────────────┘
  (Agent/AgentInput/   (AgentEvent/SystemEvent/AgentMeta)
   AgentRegistry/
   SubagentWriters)
      │                 │
  ┌───┴─────┐     ┌─────┴─────┐
  │         │     │           │
uva-core  uva-tool uva-session uva-approval
```

要点：**harness 之下没有子代理代码**；`uva-tool-task` 只依赖 `uva-agent`+`uva-events`
（+`uva-tool` 的 Tool trait 与 `uva-core`），不依赖 harness；
`DelegatingTool`/`DelegationCall`/`Delegators` 从此不存在。

---

## 6. 各部分设计

### 6.1 `uva-approval`（新 crate）

**移入**：`crates/uva-harness/src/agent/approval.rs`（785 行词汇表+决策函数）→ `src/approval.rs`；
`approval_plugin.rs` 的插件半边（`ApprovalPlugin`/`KEY_APPROVAL`/`RiskOf`）→ `src/plugin.rs`；
`re_act/tests/lifecycle/approval{,_dispatch,_resume}.rs` → `tests/`。

**`ApprovalGate` 分两步**：P1 暂留 harness（`Gate` 的 `HookContext` 此刻还带私有 `EventSink`），
改为薄适配器；P2 `Gate` 下沉后整体迁入——`uva-approval` 因此依赖 `uva-hook`，两者层次定死。

**依赖**：`uva-core`/`uva-tool`（`Risk`）/`uva-session`（三行事件）/`uva-kernel`（Plugin）/
serde/schemars/sha2/hex/thiserror。**不得**依赖 `uva-harness`。

**路径保持**（关键手法，实测过）：`uva-harness/src/lib.rs` 把现有 `pub use approval_plugin::{…};`
改成一条合并 use 树：

```rust
// 与 agent::{…} / events / input / prompt 的语句合并，crate-root 语句数保持 5
pub use {
    approval_plugin::ApprovalGate,
    uva_approval::{
        ApprovalMode, ApprovalPlugin, ApprovalPolicy, ApprovalRequest, GrantKey,
        GrantScope, KEY_APPROVAL, ResumeCoverageError, ResumeDecision,
        ResumeStatus, ResumeToken, RiskOf, Suspension, align_decisions,
        check_coverage, latest_request_snapshot_hash, may_retry, open_requests,
        suspended_segment, workspace_hash,
    },
};
```

冻结名共 **21 个**（`RiskOf` 在基线第 106 行，初稿漏数）；P1 搬 20 个，`ApprovalGate` P2 补搬；
`check-frozen-api` 抽名字不抽路径，**基线一行不改**；`check-pub-surface-strict` 语句数 5/5 不增。
不搬：`InterruptDecision`（app-server 线类型）；三行归 `uva-session`。
已知耦合：开放集/`ResumeToken` 从 `SessionEvent` 行推导 → 依赖 `uva-session` 是 uva 独有形状，不为对齐 dsh 改。

### 6.2 `uva-hook`（新 crate，契约层）

**职责**：对外提供插件的挂载点。核心判断：**载荷不新造**——`HookEvent`（`uva-steering`，已在
harness 之下）本身就是完整载荷（`BeforeTool{tool_name,call_id,arguments}` 等）。

**拥有**：

1. **7 个挂载点键**（自 `seams/mod.rs:86-125`）：`STEP_PRE`=agent/step-pre、`STEP_POST`、`TOOL_PRE`
   =tool/pre-execute、`TOOL_POST`、`LLM_REQUEST`、`LLM_RESULT`、`PROMPT_SECTIONS`。re-export 写在
   `pub mod seams` **模块内部**（深路径 `uva_harness::seams::STEP_PRE` 保持；模块内 `pub use`
   不算 crate-root 冻结名、不占 lib.rs 语句预算）。**不要**提升到 crate root（7 个 NEW 名直接红门禁）。
2. **拦截 trait**：`Hook`/`Gate`/`RequestTransformer`/`ResultTransformer`（自 `host/mod.rs:168,186,235,256`）。
3. **解耦 `HookContext`**：`{ state: &AgentState, cancel: &Arc<dyn CancelToken> }`——**删 `events`
   字段**（全仓仅 harness 自己的 `record_hook_block` 用过；删后 sink 改显式传参，
   `host.seam_context()` 本就握着）。破坏性变更记 `docs/api-frozen.md`。
4. **裁决词汇不新造**：插件返回现有 `HookDecision`（五档含 `ReplaceParams`）；改写走
   `RequestTransformer`/`ResultTransformer`（`&mut` 原位）。`SeamRuling` 留 harness 内部
   （waterfall 短路值）；`HookPayload`（`HookEvent`+`HookContext`）替代私有 `SeamPass`。
5. **`HookRegistry`**：按键持有外部注册，仲裁见 6.2.1，注册返回可撤销句柄。
6. **`HookPlugin`**（内核插件包装，`provides=["hooks"]`）。**接线走 `subagents` 模式**：
   harness 的 seam 链住在 `SeamChains::new()` 私有 context 里（`seams/mod.rs:306-318`），与
   app-server 的 mount context 不同——正确路径是 `AgentLoopServicePlugin::resolve` 用
   `ctx.get` **机会式**解析进 `AgentLoopDefaults`，循环穿进 `Seams`。
   **不要**把它加成 `AGENT_LOOP_INJECTS` 的边：kernel 对 `inject` 键做静态校验、缺 provider
   直接拒绝整组，那样等于把 `hooks` 行变成硬依赖——写在本行之前的 profile 文档、以及把
   行设为 `Off` 的部署都会在 boot 期失败且没有自救路径。原生 profile 的 `hooks` 行仍排在
   `agent-loop` 之前，让它在场时先挂载。

**依赖集**：`uva-steering`/`uva-context`/`uva-core`/`uva-tool`（`ResultTransformer` 收
`&mut ToolOutput`）/`uva-kernel`。**无 `uva-harness`**。

**机械判据**：样板 `docs/examples/hook-only-consumer/`（独立 crate，照抄 harness-only-consumer
先例；`uva-hook` 是唯一 normal 依赖，harness 只进 dev-deps）；新门禁 `scripts/check-hook-external.sh`
在其中跑 `cargo tree -e normal` 断言无 harness（**不能**拿 `cargo tree -p uva-hook` 当判据——必然为真）。

**harness 侧**：`Seams` 退化为"构造 `HookPayload`→跑注册表→翻译裁决"；`Host` 保留（内部 typed
注册表），其 `Hook`/`Gate`/transformer 改为 `uva_hook` 的 trait；`Host::waterfall` 内部化。

#### 6.2.1 仲裁规则（dsh 单调拒绝）

- `Block` 先到先决、立即短路；`Ask` 收集全部理由（沿 `Arbitration::asks`）；
  `Terminate` 合法性**沿用 `HookEvent::accepts` 五点表**（`event.rs:147-154`），内外同一张表。
- `tool/post-execute` 结果改写逐字段合并，后到覆盖先到、缺省不抹除。
- 注册表最后跑一道 **deny-only** guard（无放行结果，顺序无法翻回）。
- **行由 harness 落**：外部插件 `Block` 时 harness 写 `hook_block` 行——收口现存
  "外部 middleware 短路不写行"缺陷。

#### 6.2.2 裁决：入参不可改写、出参可改写（已裁定）

入参侧采 dsh（历史/审计/UI/执行必须一致；uva 的 `tool_dispatch` 哈希、`tool_call` 展示、审批哈希
三处会分叉）。出参侧是**公开能力**：`ResultTransformer` 改写聚合输出，溯源由现有
`tool_result_transformed` 行承载（`transformers`+`before`/`after` 哈希，`guard.rs:186-196`，
语义 "provenance, not reconciliation"）。将来开入参改写须另开 ADR 并同改三处。

### 6.3 `uva-hook-plugins`（新 crate，实现族）

照抄 `uva-policy-plugins` 一模块一 feature：`reflection`（自 policy-plugins 迁入，已是 `Hook`
实现）+ `redact`（出参脱敏，6.2.2 的第一个消费者）+ `traffic-light`（按 `Risk` 分级放行/阻断；
内建五 guard 住在 `uva-steering::guards`、boot 时 `Host::default_policy` 装一次，不在此族）。
门禁：`PLUGIN_CRATE_ROOTS` 加本 crate，重算 `plugin-shape/baseline`。

### 6.4 子代理轴（档三，最大改动）——P4′ 五步

**Step 1 `uva-events`**：`harness/src/events/mod.rs`（473 行）→ `crates/uva-events`。依赖
`uva-core`/`uva-provider`/`uva-tool`/`uva-session`/`uva-approval`（`Suspended(Suspension)`）。
harness 一条 `pub use uva_events::{…}` 并入 use 树——55 个消费文件零 import 改动，
`AgentEvent`/`SystemEvent`/`WarningKind` 冻结名保路径。**例外**：`AgentEvent::Agent(AgentMeta, …)`
变体删除（§6.7）。

**Step 2 `uva-agent`**：下沉 `Agent` trait（`agent/mod.rs:49-56`）、`AgentInput`+`ChannelInbox`/
`RunInbox`、`AgentRegistry`/`AgentEntry`（Q4：peer 目录随词汇走）、`SubagentWriters`。冻结名
全 re-export 保路径；`AgentCaps`/`ReActAgent` 留 harness。

**Step 3 删 seam**：删 `delegation.rs`（371 行）、`ReActAgent::with_delegator`、`Host::delegator`、
`ToolCaps::delegators`、`EnvCaps::subagent_writers`、`RunConfig::{delegators,subagent_writers}`、
`route.rs::try_delegation`、`AgentInput.subagent_*`、`AgentEvent::Agent`/`AgentMeta`、
app-server `persist.rs:27-35` 事件拦截、`SubagentRouter` 的**路由半边**（**写者半边保留**：
`ChildSessionWriters` 继续开子档，`subagent_enter(child,parent,depth,agent)` 照写，归属不丢）。
kernel `subagent-backend` 行 → 普通 `tool-task` 工具行；`AGENT_LOOP_INJECTS` 删 `"subagents"`。
冻结名 `DelegatingTool`/`DelegationCall`/`AgentMeta` 离开——记 `docs/api-frozen.md`
（本设计唯一一次名字离开，有裁定背书）。

**Step 4 `uva-tool-task`**：`uva-tool-plugins::task` 全量迁入（2 816 行）。task 是普通 `Tool`：
注册进 `ToolRegistry`；执行时查 `AgentRegistry` 解析 peer、`SubagentWriters` 开子档、构造
`AgentInput`、内联驱动 `agent.run()`。**取消（Q7a）**：`uva_tool::Context` 加
`pub cancel: Arc<dyn CancelToken>`（`Context::new` 默认未取消 token，加 `with_cancel`；
记 api-frozen）——dsh 称工具 `exec.signal` 为 "the canonical cancellation channel both before
and after startup"，omp 同路径，交叉验证。**并发**：一个 `task` 调用起一个子代理；工具声明
`ExecutionMode::Sequential`，批次调度器把同一批里的 `task` 调用串行化（委派起整个 run，
批次内并发会改动取消与批次语义），因此不设每会话信号量。**深度**：`MAX_SUBAGENT_DEPTH=3`
工具自查。**control 模块**见 §6.8。

**Step 5 收口**：§7/§8 清单。

### 6.5 子代理生命周期（P6）

| 项 | 裁定/事实 |
|---|---|
| 预分配 id | 铸造点从 runner 上提到工具（派发前 mint），产物/名册/控制面共用 |
| 工件集 | `artifacts/<id>.{md,jsonl,patch}` 落会话目录；约定写进 `docs/log-format.md`；保留期 P6 定 |
| 四态+停放 | `running\|idle\|parked\|aborted`，TTL 5 分钟，主会话永不停放 |
| 冷唤醒 | **不依赖停放**：读子日志头取 `agent_name` → 解析 peer → 子会话 writer 再跑一轮（多轮会话是 app-server 日常路径）；**唤醒即 `running`**（新起一轮，非停于 idle——原型验证时修正，与 dsh "an idle target starts a turn" 一致） |
| 名册数据源 | P6 注册表；P4′ 期间 `list` 用日志扫描兜底（`tasks/list` 逻辑下沉复用） |

### 6.6 tool 流式实时输出（P5，不拆 crate）

1. **修 live 语义**：`LiveTurn::apply` 收 `ToolProgress` 走 `finish_call`→`settle_call` 直接结算、
   `output` 换当前块文本（`live.rs:78-120,183-196`），`signature()` 的 `output_bytes` 是已结算长度之和
   （`live.rs:145-160`）——两段同长度分片既不推进 `live_seq` 也提前标完成。改为**追加不结算**、
   状态保持 `running`（词表 `running`/`completed`/`failed`，无 `ok`），指纹覆盖累积内容。
2. **durable 行** `tool_output`（partial）：32 个现有 tag 无此项；触点 = session 事件枚举、builder、
   `type_tag`、`surface_role`、`empty_event_of`、app-server `SessionEventType` 并集、重生成
   `types.ts`、web 行处理表、TUI applied/ignored；行经 `session/writer`（`check-single-writer`）；
   落库点补 `agent/sink.rs`（新追加路径）与 `route.rs:229-235`（现在只发 live）。
3. **`shell` 产出增量**：覆写 `Tool::stream`，`spawn_reader` 的 8 KiB 块逐块产出 `Progress`。
4. **task 的 Progress（L1）**：载荷 `{child_id, status, 当前活动一行}`，**先按 ~150 ms 合并**再产出
   （`EventSink` 1024 事件软上限；omp 同窗口）。

### 6.7 子代理 live 可见性（Q11 定稿，三层）

调研：`docs/research/2026-11-15-subagent-live-visibility.md`。两参考 + Claude Code 第三方生态收敛于
同一分层，**无一家把子事件灌进父流**：

| 层 | 内容 | 现状/归属 |
|---|---|---|
| L1 工具块进度 | task 的 `Progress` 汇报子代理摘要（带 child id，顺带解决发现性） | 传输线已在（ToolProgress→live 帧）；P5 后即用 |
| L2 子会话钻取 | 全保真 = follow 子会话（子会话是真源） | **今天就能用**：`sessions/follow` 按 id 订阅任意会话、清单带 `parent_session_id`、web 委派卡已链接 `/chat/<childId>` |
| L3 名册 | 四态注册表 + 状态推送（omp Hub / dsh catalog+registry 等价物） | 没有；P6 数据源 / P7 UI |

取舍（已认）：L1 只承载摘要，父页面不看子代理流式文本，全保真点进子会话；L1 重放依赖 P5 的
`tool_output` 行。父侧 durable 记录 = `tool_result` 行带 child id（dsh `subagent-settled` notice
的等价物），无 enter/exit 成对行。

### 6.8 模型控制面（Q12 需求 / Q13b / Q14a）

`uva-tool-task` 内 `control` 模块，三个模型工具随 Step 4 落地：

| 工具 | 行为 | 依赖 |
|---|---|---|
| `agents/list` | 列本会话子代理（id/名/状态/当前活动）；P4′ 扫描日志，P6 切注册表 | 扫描→注册表 |
| `agents/message` | running→steer（写其 `ChannelInbox` 发送端，机制已有：`AgentInput.inbox` 即 "the run's steer channel"）；idle 在内存→追问；**已结束→冷唤醒**（Q13b，见 §6.5） | 工具保留每子 steer 发送端 + writers |
| `agents/interrupt` | 打断指定子代理当前 run | 工具保留每子 cancel token |

对照：dsh `list_agents`/`send_message`/`interrupt_agent`；omp `write agent://<id>`/`proc://<id>/kill`。
冷唤醒实例的所有权：control 模块持 per-session 句柄表，P6 停放/唤醒接管内存生命周期。

---

## 7. 文件迁移清单

### 7.1 `uva-approval`

```
新建  crates/uva-approval/{src/{lib,approval,plugin}.rs, tests/, README.md}
移出  crates/uva-harness/src/agent/approval.rs
      crates/uva-harness/src/approval_plugin.rs 的插件半边
      crates/uva-harness/src/agent/re_act/tests/lifecycle/approval{,_dispatch,_resume}.rs
改    crates/uva-harness/src/lib.rs（合并 use 树；删 pub mod approval）
      crates/uva-harness/src/agent/mod.rs, agent/caps.rs
      crates/uva-harness/src/agent/re_act/{state,agent}.rs
      crates/uva-harness/src/agent/re_act/loop/{mod,guard,resume}.rs
      crates/uva-harness/src/agent/re_act/loop/step/prepare.rs
      crates/uva-harness/src/agent/re_act/loop/compaction/mod.rs
      crates/uva-harness/src/agent/re_act/loop/step/mod.rs
      crates/uva-harness/src/agent/re_act/loop/step/tools/（mod/route/commit）
      crates/uva-harness/src/input.rs, events/mod.rs, agent/sink.rs
      crates/uva-harness/tests/approval.rs, examples/durable_hitl.rs
      crates/uva-app-server/src/composition/kernel/mod.rs（工厂改指）, api/contract.rs（import 路径）
      crates/uva/src/lib.rs, harness.rs, Cargo.toml（facade）+ 根 Cargo.toml
```

### 7.2 `uva-hook`

```
新建  crates/uva-hook/{src/{lib,context,traits,seam,registry,plugin}.rs, README.md}
      docs/examples/hook-only-consumer/（独立 crate；uva-hook 唯一 normal 依赖，
                                      uva-harness 仅 dev-deps，自带测试驱动真实工具调用）
      scripts/check-hook-external.sh（样板内 cargo tree -e normal 断言无 harness；
        按 AGENTS §3.6 同变更同步四处：Makefile check-*、make ci、lint-sh、shellcheck.yml）
移出  crates/uva-harness/src/agent/host/mod.rs:152-296（HookContext/Hook/Gate/transformer）
      crates/uva-harness/src/seams/mod.rs:139-152,194-280（SeamRuling/SeamPass 对应物）
改    crates/uva-harness/src/agent/host/mod.rs（record_hook_block 改显式传参）
      crates/uva-harness/src/seams/mod.rs（适配器化；键在模块内 re-export）
      crates/uva-harness/src/lib.rs（trait 名并入 use 树，语句数 5）
      crates/uva-harness/src/agent/sink.rs（append_hook_block 调用方改显式传参）
      crates/uva-policy-plugins/src/reflection/（改指 uva_hook::Hook）
      crates/uva-runtime/src/agent_loop/mod.rs（`resolve` 机会式解析 hooks，不加注入边）
      crates/uva-app-server/src/composition/kernel/mod.rs（hooks 行）
      docs/api-frozen.md（HookContext 删字段）
```

### 7.3 `uva-hook-plugins`

```
新建  crates/uva-hook-plugins/{src/{lib,reflection,redact,traffic_light}/…, README.md}
移出  crates/uva-policy-plugins/src/reflection/
改    crates/uva-policy-plugins/Cargo.toml（去 reflection feature）, src/lib.rs, README.md
      crates/uva/Cargo.toml（facade feature 重定向）
      scripts/check-plugin-shape.sh（PLUGIN_CRATE_ROOTS）+ plugin-shape/baseline 重算
```

### 7.4 流式（无新 crate）

```
改    crates/uva-app-server/src/session/controller/live.rs（追加不结算；指纹覆盖累积）
      crates/uva-harness/src/events/mod.rs + events/tests/{wire_tags,helpers}.rs
      crates/uva-session/src/events/mod.rs, typed_event_builders.rs
      crates/uva-session/src/{surface/fold.rs,repair/mod.rs,log_surface/tests.rs}
      crates/uva-harness/src/agent/sink.rs（durable tool_output 追加路径）
      crates/uva-harness/src/agent/re_act/loop/step/tools/route.rs（落库点）
      crates/uva-app-server/src/api/v1/session_view.rs, api/contract.rs
      uva-web/src/api/types.ts（重生成提交；web 在仓根 uva-web/，不在 crates/ 下）
      uva-web/src/lib/session-to-messages.ts, api/follow-frames.ts, pages/ChatPage.tsx
      crates/uva-tui/src/app.rs（applied/ignored 分类）
      crates/uva-tool-plugins/src/shell/mod.rs（覆写 stream 逐块产出）
```

### 7.5 `uva-events`

```
新建  crates/uva-events/ ← crates/uva-harness/src/events/mod.rs 整体（473 行）+ events/tests/
改    crates/uva-harness/src/lib.rs（pub use uva_events::{…} 并入 use 树）
      harness 内直接引用处（agent/re_act/{stream,projection}.rs, agent/sink.rs 等）改 import
      其余 55 个消费文件零改动（路径保持）
```

### 7.6 `uva-agent`

```
新建  crates/uva-agent/
移出  crates/uva-harness/src/agent/mod.rs:49-56（Agent trait）
      crates/uva-harness/src/input.rs（AgentInput/ChannelInbox/RunInbox；subagent_* 字段删）
      crates/uva-harness/src/input.rs:78-90（SubagentWriters）
      crates/uva-harness/src/agent/registry/（AgentRegistry/AgentEntry）
改    crates/uva-harness/src/lib.rs, agent/re_act/**（re-export 保路径 + import）
      crates/uva-app-server/src/state/app_state/boot.rs 等（registry 构造改指）
```

### 7.7 删 seam + `uva-tool-task`

```
删    crates/uva-harness/src/agent/re_act/delegation.rs
      crates/uva-harness/src/agent/re_act/loop/step/tools/route.rs 的 try_delegation
      crates/uva-app-server/src/session/controller/persist.rs:27-35（事件拦截）
      crates/uva-app-server/src/session/subagent_router.rs 的路由半边
移出  crates/uva-tool-plugins/src/task/** → crates/uva-tool-task/（2 816 行 + tests/examples）
改    crates/uva-tool/src/types.rs（Context 加 cancel + with_cancel）
      crates/uva-app-server/src/composition/kernel/mod.rs（subagent-backend→tool-task 行）
      crates/uva-runtime/src/agent_loop/mod.rs（去 "subagents" 边；删两字段）
      crates/uva/src/tool_task.rs, crates/uva/Cargo.toml
      Makefile（check-mvp-deps 的 FAMILY_TOOL_* 改每 crate 一份 feature 列表）
      uva-web/src/lib/session-to-messages.ts 等（subagent_enter/exit 处理随 L3 重估）
      docs/api-frozen.md（DelegatingTool/DelegationCall/AgentMeta 离开）
```

---

## 8. 门禁与文档同步清单

| 项 | 动作 |
|---|---|
| `scripts/frozen-api/baseline` | **不改**（所有路径保持；唯一例外是 7.7 删的三个名字离开时**也不改 baseline**——删名会报 REMOVED，须同变更把三行移出 baseline 并记 api-frozen） |
| `docs/api-frozen.md` | 三处记录：`HookContext` 删字段、`uva_tool::Context` 加字段、`DelegatingTool`/`DelegationCall`/`AgentMeta` 离开 |
| `paradigm-surface/baselines/crates/<新crate>` + `PARADIGM_CRATES` | 五个新 crate 各加基线与名单 |
| `check-plugin-shape.sh` `PLUGIN_CRATE_ROOTS` + baseline | `uva-hook-plugins`、`uva-tool-task` 加入；重算 |
| `check-hook-external.sh`（新增） | 样板 crate 内断言；同步四处（AGENTS §3.6） |
| `RUNTIME_FREE` | 五个新 crate 全加入（实测 `cargo tree -p uva-tool -e normal` 0 tokio；`uva-tool-task` 若 dev-dep 用 tokio 照 `uva-tool` 惯例办） |
| `check-publishable.sh` `ORDER` | `uva-approval`（session/tool 后）→`uva-hook`（steering/context 后）→`uva-events`→`uva-agent`→均先于 `uva-harness`；`uva-tool-task`/`uva-hook-plugins` 在 harness 后 |
| `check-mvp-deps` | `FAMILY_TOOL_*` 改每 crate 一份 feature 列表（`task` 离开 `uva-tool-plugins`） |
| facade（`crates/uva`） | 五个 feature/模块；根 `Cargo.toml` members/`[workspace.dependencies]` |
| `docs/architecture.md` crate 表 + `AGENTS.md` §1.1 分层表 | 加五行；§3.7 "委派工具走 DelegatingTool seam" 条款改为"task 是普通 Tool，词汇在 `uva-agent`" |
| `crates/<新crate>/README.md` | 五份（markdownlint 范围扩到 30 个 crate README） |
| ADR ×2 | ①挂载点契约下沉判据；②入参不可改写/出参可改写 |

---

## 9. 分期落地（定稿）

| 期 | 内容 | 风险 |
|---|---|---|
| **P0** | ADR ×2 + `make ci` 基线 | 无 |
| **P1** | `uva-approval` | 低 |
| **P2** | `uva-hook`（含删字段、门禁、样板） | 中 |
| **P3** | `uva-hook-plugins` | 低 |
| **P4′** | S1 `uva-events` → S2 `uva-agent` → S3 删 seam → S4 `uva-tool-task`+control → S5 收口 | 中高（五步各自可合可回滚） |
| **P5** | 流式（含 task 的 Progress = L1） | 中 |
| **P6** | 生命周期 + 注册表（`list` 切真源）+ 冷唤醒句柄表 | 中 |
| **P7** | 名册 UI（父子树、唤醒） | 低（另开设计） |

依赖：P2←P1；P3←P2；P4′←P2（共动 harness lib.rs use 树，串行）；P5 与 P4′ 文件面可并行，
但 task 的 Progress 汇报等 S4；P6←P4′；P7←P6。

---

## 10. 验证方案

| 目标 | 验证 |
|---|---|
| G1 审批 | `cargo test -p uva-approval`/`-p uva-harness`；`make check-frozen-api` 绿且基线 diff 为空 |
| G2 挂载点对外 | 样板 crate `cargo tree -e normal` 无 `uva-harness`（新门禁）；样板测试驱动真实工具调用，断言 handler 见到工具名/入参、`Block` 短路、注册可撤销 |
| G3 hook 家族 | `cargo test -p uva-hook-plugins --all-features` |
| G4/G8 子代理独立 | `cargo test -p uva-tool-task --all-features`；`cargo tree -p uva-tool-task -e normal` 无 `uva-harness`；task-only 样板 |
| G5 流式 | live：`output` 随分片增长、`status` 保持 `running`、`live_seq` 推进；durable：多条 `tool_output` 经 writer；终态 `tool_result` 结算 |
| G6 生命周期 | 跑完→`idle`→TTL→`parked`→唤醒→`idle` 续聊 |
| G7 门禁 | `make ci` 全绿；publishable crate 数 +5；`check-web` 绿 |
| G9 seam 真删 | `grep -r "DelegatingTool\|DelegationCall\|AgentEvent::Agent" crates/` 零命中 |
| G10 取消传播 | 父 run 取消 → 子 run 终止（Context.cancel 传递） |
| G11 控制面 | spawn→`list` 见→`message` steer（子日志现 steer 行）→`interrupt` 停→结束后 `message` 冷唤醒（子日志再起一轮） |
| G12 可见性 | L1 父页工具块出摘要带 child id；L2 点入子会话见全程；重放由 `tool_output` 行重建 |

---

## 11. 裁定记录（全部已裁定，2026-11-15，两轮拷问）

| # | 裁定 |
|---|---|
| 前三问 | 入参不可改写、出参可改写（溯源=现有 `tool_result_transformed`）；一次 `task` 调用一个子代理、批次内串行（`Sequential`，无每会话信号量）；产物目录 `artifacts/<id>.*` |
| Q1 | 子代理轴**档三**：删 seam、task 变普通 Tool |
| Q2/Q6 | 词汇下沉（`uva-events`+`uva-agent`）；`uva-tool-task` 不依赖 harness |
| Q3 | 两个 crate（词汇层 + 实现层） |
| Q4 | `AgentRegistry` 随词汇 crate |
| Q5/Q10 | 排期 P0→P1→P2→P4′(五步)→P5→P6/P7；hook 先于词汇下沉 |
| Q7 | 取消走 `Context` 加 `cancel` 字段（dsh/omp 交叉验证） |
| Q8/Q11 | 可见性三层（L1 P5 / L2 已有 / L3 P6-P7）；删 `AgentEvent::Agent`/`AgentMeta`；路由半边删、写者半边留 |
| Q9 | 父侧无 enter/exit 行；`tool_result` 带 child id 即结算记录；子会话 `subagent_enter` 照写 |
| Q12 | 模型控制面为需求：list/message/interrupt |
| Q13 | `message` 含冷唤醒（不依赖 P6 停放） |
| Q14 | control 模块随 `uva-tool-task`（P4′ S4）；`list` 先扫描后注册表 |

## 附录 A：证据索引

完整证据表见 [`docs/research/2026-11-15-plugin-crate-splits-dsh-omp.md`](../research/2026-11-15-plugin-crate-splits-dsh-omp.md#evidence-table)。
本设计用到的关键证据：

| 主题 | 证据 |
|---|---|
| dsh 契约/实现分包、循环可换 | `[E001][E003][E051]` |
| dsh 扩展点三域与"新行为去哪"表 | `[E012][E010][E045]` |
| dsh 审批是独立能力包、闭合 fail-closed 词表 | `[E006][E007][E042]` |
| dsh 委派 seam + provider + tool 三段 | `[E008][E052][E044]` |
| dsh 单调拒绝 guard | `[E004][E048]` |
| dsh 无独立 hooks 包 | `[E020][E088][E049]` |
| omp 扩展通道与两段式工具拦截 | `[E011][E012][E043]` |
| omp 子代理机制（信号量/工件/四态/名册） | `[E015][E016][E017][E068][E069]` |
| omp 审批档位与 fail-closed | `[E042]` |
| omp 与 dsh 关于改写入参的分歧 | `[E025][E087][E074]` |
| uva 审批词汇与耦合面 | `[E028][E029][E030][E060]` |
| uva 拦截三层栈与 7 个挂载点 | `[E019][E021][E031][E070][E065]` |
| uva 流式现状（零生产者、live 结算） | `[E022][E038][E039][E080]` |
| uva 行词汇与 12 个协同触点 | `[E040][E081]` |
| uva 委派现状与双实例漂移 | `[E035][E036][E090][E061]` |
| 子代理 live 可见性分层（第二轮调研） | 见 [`2026-11-15-subagent-live-visibility.md`](../research/2026-11-15-subagent-live-visibility.md)（25 条证据） |
| 门禁共改清单与"路径保持"手法 | `[E034][E041][E077]` |

## 附录 B：评审核对（builder check）

成稿后由一名独立评审对全部承重论断做了核对（冻结名抽取器行为、合并 use 树语法、
trait/键/`record_hook_block`/`live.rs` 的行号引用、task 模块 2 816 行、`uva-core` 已有 `Semaphore`——全部属实），
并返回 12 条更正，均已并入上文。要点：

| # | 更正 |
|---|---|
| 1 | `HookRegistry` 的接线必须走 `AgentLoopServicePlugin::resolve` 的 `ctx.get` 机会式解析（`subagents` 模式），因为 harness 的 seam 链住在私有 kernel context 里；**不能**走 `AGENT_LOOP_INJECTS`——注入键是硬依赖，缺行会让 boot 失败（§6.2 第 6 条） |
| 2 | 不发明 `HookRuling`；插件返回现有 `HookDecision`，改写走 `RequestTransformer`/`ResultTransformer`（§6.2 第 4 条） |
| 3 | 键常量的 re-export 写在 `seams` 模块内部（深路径），不上 crate root；hook trait 名并入唯一那条 use 树（§6.2 / §6.1） |
| 4 | P6 依赖 P4（§9） |
| 5 | "外部插件"判据不能拿 `cargo tree -p uva-hook` 当断言（必然为真）；样板放 `docs/examples/hook-only-consumer/` 独立 crate，harness 只进 dev-dependencies（§6.2 / §10 G2） |
| 6 | `uva-hook` 的依赖集漏了 `uva-tool`（`ResultTransformer` 的 `&mut ToolOutput`）；`uva-tool-task` 的 RUNTIME_FREE 判断实测有误（本仓 provider 是 `default-features = false`，`cargo tree -p uva-tool -e normal` 0 个 tokio）（§6.2 / §8） |
| 7 | 文件清单里的路径笔误已修（`uva-web/` 在仓根、`loop/step/prepare.rs`、`loop/compaction/mod.rs`、`agent/caps.rs`）；行 tag 计数 32 不是 34 |
| 8 | G5 的状态词表是 `running`/`completed`/`failed`（无 `ok`）；live 缓冲与 durable 行是两个面；§7.5 补上 `agent/sink.rs` 与 `route.rs` 两个落库点 |
| 9 | `RiskOf` 在基线第 106 行被冻结——审批冻结名是 **21** 个不是 20（§6.1） |
| 10 | 预分配 id 需要把 `Ulid::generate()` 从 runner 上提到 delegator（§6.5） |
| 11 | P2 之后 `ApprovalGate` 应整体搬进 `uva-approval`（它的 `check` 本来就忽略 ctx）；`record_hook_block` 的 sink 改显式传参（§6.1 / §6.2 / §7.2） |
| 12 | `TaskDelegator` 收口裁定：kernel 行存活、run factory 删除、`uva-runtime` 的 `subagents` 注入边保留（§6.4） |

**注**：附录 B 的 12 条更正针对「方案 B 版」成稿；其后两轮拷问（Q1–Q14）把子代理轴升级为档三并新增
词汇下沉与控制面，§6.4–§6.8 与 §7.5–§7.7 为其后的定稿内容——其中第 12 条的「kernel 行存活」已随档三
改写为「`subagent-backend` 行改为普通 `tool-task` 工具行、注入边删除」。

## 附录 C：原型验证（2026-11-15）

原型：`docs/prototypes/plugin-splits-b-double-prime.prototype.html`（六个页签：架构 / 界面 / 派生与可见性 / hook 仲裁与审批 / 控制面 / 自由把玩）。

**它回答的问题**：共识落地后系统长什么样、交互顺序是否自洽。**验证结论**：

1. 生命周期全链路自洽：派生 → 推进 → 完成(idle) → 追问(running) → 停放(parked) → 冷唤醒 —— 每步落行侧正确（父侧永远只有 `tool_call/tool_output/tool_result`，子会话是真源）。
2. 非法操作被拒而非挂起：对 `aborted` 终态 `message` → 错误结果；对非 running `step`/`interrupt` → 拒绝。
3. **修正一处**：冷唤醒应直接进入 `running`（新起一轮），而非停于 `idle`——已同步 §6.5。

原型本身按仓例留在 `docs/prototypes/`（一次性，标注 `.prototype.html`）；其纯逻辑模块（`Subagents` reducer 与 `HookArbiter.arbitrate`）可在 P6/`uva-hook` 实现时整体搬入。
