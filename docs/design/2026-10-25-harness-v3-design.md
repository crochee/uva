# Harness V3 — 薄循环、日志真源、按需审批

日期：2026-10-25
状态：待批准
关系：接续 [`2026-10-23-harness-v2-design.md`](2026-10-23-harness-v2-design.md) 未落地的 §5–§7；
写者唯一化已由 [`2026-10-25-writer-unification-plan.md`](2026-10-25-writer-unification-plan.md) 落地。
本文取代该文档 §10.1 的第 4–7 步，并新增 server 的验收面与教程交付物。

---

## 1. 意图与为什么

`uva-harness` 要同时满足三个目标，缺一不可：

1. **可作为社区基座库**：第三方 crate 装进来加 tool / hook / 策略，不改 harness 一行。
2. **崩溃后可精确恢复**：日志是唯一真源，resume 不重复、不缺口、不伪造历史。
3. **上下文可长期运行**：模型历史是日志的投影，压缩是带 provenance 的替换。

今天这三个目标各自只完成了一半：写者与 repair 在 `uva-session`，循环仍以内存
`RunState.messages` 为真源；`Steering` 的 provider hook 只观察；插件挂载是编译期 builder；
`uva-app-server` 有 SSE、队列、取消、fork、导出，但没有审批、挂起、重连带基线、压缩可见性。

**为什么现在做**：写者唯一化刚落地，日志的序号与 flush 语义已经稳定；此时把“日志真源 → 历史投影
→ 事件接缝”接起来，不需要第二次重写。`uva-app-server` 作为第一个宿主和教程，能在真实对话里
暴露接缝是否够用。

---

## 2. 已由对话确定

| 决定 | 内容 |
|---|---|
| 三条线都要 | 插件深度、崩溃/resume、长会话上下文效率，不是三选一 |
| 首个宿主 | `uva-app-server` 端到端跑通，并作为范式与教程 |
| 验收面 | 完整的 AI agent 对话体验（见 `docs/research/agent-conversation-experience-2026-10-25.md`） |
| 审批范围 | **只在高风险工具上需要**，不是每次工具调用都问 |
| 运行中纠偏 | 纳入本轮：queue（轮间）与 steer（step 边界注入）两种语义都要 |
| 循环形态 | 保留一个薄的具体循环；不把循环本身做成插件 |
| 依赖 | 零运行时依赖；harness 无 `[features]`；公开 API 不出现具体运行时类型 |
| 允许破坏性改动 | 不留 shim / 别名；调用方同一次切换 |
| 顺序 | 先完成 harness 的日志/投影/接缝，再接 server；多节点无状态化后置 |

---

## 3. 目标 / 非目标 / 反目标

### 目标

1. 第三方通过 `install(&mut Host)` 注册 tool / observer / gate / transformer / prompt section /
   delegator，所有注册返回可撤销句柄；不修改 harness。
2. 模型历史由 `fold(log)` 派生；循环不再持有可写的内存历史。
3. resume 前先 `repair`，只追加不截断；未闭合的 turn / step / tool 调用被显式关闭。
4. 压缩是可选能力（seam），摘要以 replace 操作落盘，记录被遮蔽的 seq；UI 可见、可手动触发。
5. 高风险工具在派发前挂起，等待批准/拒绝；恢复幂等，跨刷新与进程重启成立。
6. `uva-app-server` 满足完整对话验收剧本，并产出可照做的教程。
7. 删除零引用公开面、重复实现与失真的注释；门禁全绿。
8. 运行中可注入纠偏（steer）；注入以日志为准，只在 step 边界生效，不打断在途模型调用。

### 非目标

1. 运行时动态加载（`.so` / WASM / 热重载）；本仓不做，未来可由独立 crate 提供。
2. harness 增加任何 cargo feature；可选能力一律用 trait + 宿主组装。
3. 第一阶段的多节点/无状态化；它依赖日志与写者契约稳定。
4. 浏览器/桌面两半的插件协议、A2A、计费、向量检索。
5. 新增模型 provider；沿用现有 adapter。
6. 为兼容旧签名保留 shim。

### 反目标（做到也算失败）

- 插件能改 system prompt 本体或已落盘历史。
- 队列满时丢行；`try_send` 的返回值被吞掉。
- 为了“精简”砍掉可观测性；或用 `dead_code` 抑制警告。
- 公开 API 出现 `tokio::*` / `async_std::*` / `smol::*` 类型。
- “一切皆插件”到循环本身，导致每个热路径过 trait object。
- 审批默认覆盖所有工具，把正常对话变成确认疲劳。

---

## 4. 约束

继承 `AGENTS.md`：

- 库 crate 用 `thiserror`；`uva-app-server` 用 `anyhow`。
- `max_width = 80`；禁通配符导入；harness 生产代码函数 ≤100 行、嵌套 ≤4 层。
- 墙钟走 `uva_core::Clock`，ID 走 `uva_core::IdGen`。
- 依赖单向 `uva` → `uva-*` → `uva-core`。
- `make check-public-api-runtime` / `check-no-runtime` / `check-harness-features` /
  `check-harness-shape` / `check-clock` / `check-harness-docs` 保持绿。
- 注释预算只许降不许升；新模块的非显然约束与删除项在同一次提交内完成。

---

## 5. 架构

### 5.1 数据流

```text
                 ┌─────────────────────────────────────────────┐
                 │                ReActLoop                    │
                 │  derive(log) → hooks → sample → dispatch    │
                 └──────┬───────────────────────────┬──────────┘
                        │ append（不丢行）           │ AgentEvent（live，可丢）
                        ▼                           ▼
                 ╔══════════════════╗         ┌───────────────┐
                 ║   Session log    ║         │  SSE / 前端    │
                 ║   append-only    ║         └───────────────┘
                 ╚════════╤═════════╝
                          │ flush（step 边界）
                          ▼
                 ┌──────────────────┐
                 │ JSONL / backend  │
                 └────────┬─────────┘
                          │ resume
                          ▼
                 ┌──────────────────────────────┐
                 │ repair（追加 closers）        │
                 │ fold → surface → messages    │
                 └──────────────────────────────┘
```

两条通道的命运不同：

- **durable**：`SessionEvent` 行；writer 受理即有序，flush 才承诺活过崩溃；永不因队列满丢弃。
- **ephemeral**：`AgentEvent`；只服务实时 UI，可丢；不作为任何恢复的真源。

### 5.2 crate 职责

| crate | 拥有 | 不拥有 |
|---|---|---|
| `uva-session` | 日志行、`LogWriter`、`SessionSink`、`fold`、repair、compaction 检查点、token meter | 循环决策、provider 调用 |
| `uva-harness` | `ReActLoop`、`Agent`、`ReasoningStrategy`、`HookEvent` 派发、`Host` 注册表、`Suspension` | 具体工具、HTTP、存储后端 |
| `uva-steering` | hook 词表与 guard/hint/transformer | 运行时安装（由 harness 的 Host 调它） |
| `uva-tool` | `Tool` / `ToolRegistry` / `ToolAnnotations` / 派发 | 审批策略的具体值 |
| `uva-tool-*` | 单个工具及其风险声明 | 全局策略 |
| `uva-app-server` | 路由、SSE、审批 decision、配置、教程宿主 | agent 语义、日志格式 |

**不新建 `uva-plugin` crate。** 插件的语义就是 harness 的接缝（能观察/改写哪些点），拆出去会
出现“插件 crate 依赖 harness 类型、harness 又依赖插件 crate”的环。插件实现放在独立 crate；
宿主是 harness 的私有模块 + 窄公开面。

### 5.3 seam 规则

沿用 `uva-harness/src/lib.rs` 的判据：

- **`dyn`**：下游必须在运行时安装——`Tool`、`Hook`、`Checkpointer`、`ContextManager`、
  `SessionSink`、`Clock`、`IdGen`、`CancelToken`、`Spawner`、`Compactor`。
- **泛型 / 具体**：纯函数与编译期已知的组合——`fold`、`derive_history`、
  `prepare_messages_for_shot`、`ToolProjectionCache`。
- **不做**：把所有东西塞进一个 `Box<dyn Plugin>`；那会把类型能力退化成字符串约定。

---

## 6. 日志契约

### 6.1 三个动词（已落地，本文把它变成公开契约）

| 操作 | 保证 | 失败语义 |
|---|---|---|
| `stamp()` / `append()` | 分配单调序号；受理、有序、对读可见 | 队列满时等待 flush，**不丢弃**；spill 兜底同步路径 |
| `flush()` | 唯一持久化屏障；一次 `append_many` | 失败则行仍欠账，下次 flush 重试并上报 |
| `repair()` | 崩溃后**追加**补齐行 | 不截断、不重写已落盘行 |

### 6.2 resume 协议

```text
open(writer)
  → rows = read()
  → repair(rows)        // 追加 TOOL_OUTCOME_UNKNOWN / turn_end{interrupted} / step_end
  → history = fold(rows + closers)
  → loop.run(history)
```

`Checkpointer` 只作为可选加速：命中时仍需验证日志尾号不落后于快照；未命中走 fold。
**循环不得把 checkpointer 当唯一真源。**

`AgentInput.history` 与日志的关系：装了 `log_writer` 时，历史以 fold 为准；`history` 必须为空
或与 fold 相等，否则以 `SessionEndReason::Error` 结束。没有 writer 的纯库调用仍可用
`history`——此时不存在第二真源。恢复入口是 `AgentInput::resume_decisions(ResumeToken, [ResumeDecision])`
（`resume_approving_all` 只是按 token 的未决集合写满 `Resolved` 决定的便捷封装），不是再传一遍历史。

### 6.3 不变量

1. 一个 session 一个写者；第二个写打开失败。
2. 序号连续且单调；不得重用烧掉的号。
3. 已 commit 的行永不改写；只有未 resolve 的撕裂尾可丢弃。
4. durable 行不得因背压被丢弃；ephemeral 事件可以丢。
5. 模型可见的事实必须能从日志重建；新增模型可见输入必须新增 `SessionEvent`。

### 6.4 请求快照与配置漂移

`request_header` 的哈希只能做漂移检测，不能回答“模型看到了什么”。新增 log-only
`request_snapshot` 行，记录：

- provider / model / 采样参数；
- tool 名 + 每个 schema 的哈希；
- system prompt 的哈希；
- prompt section 的 id 与顺序；
- approval policy 的指纹。

不存完整 prompt 文本（体积与敏感信息）。resume 时重算当前 Host 的指纹并与快照比较：

- 一致：正常继续；
- 不一致：以 `SessionEndReason::ConfigDrift` 结束，server 返回可诊断差异；
  只有显式 `allow_drift` 才继续，并在日志追加 `config_drift` 行。

这把“精确恢复”从愿望变成可验证契约：恢复的是**同一配置下的同一日志**。

---

## 7. 历史投影与压缩

### 7.1 投影

```rust
pub fn derive_history(rows: &[Value]) -> Vec<Message>;
```

纯函数；同一日志两次调用结果相同。循环在每个 step 开始时取投影（带 seq 水位缓存），
`RunState.messages` 退化为只读缓存或删除。

`ContextManager` 的角色改为**投影后的策略**：输入是派生历史与预算，输出是这次请求要发的消息；
它不再拥有“历史”本身。

### 7.2 压缩 seam

```rust
pub trait Compactor: Send + Sync {
    /// 判断是否需要压缩；返回 None 表示不动。
    fn plan(&self, history: &[Message], budget: TokenBudget) -> Option<CompactionPlan>;
    /// 执行摘要；实现可以调用模型。
    fn summarize(
        &self,
        plan: &CompactionPlan,
        cancel: Arc<dyn CancelToken>,
    ) -> BoxFuture<'static, Result<CompactionOutcome, CompactionError>>;
}
```

- 默认实现：`uva-session` 的摘要压缩（现有 `CompactionSettings`）。
- 摘要调用可设超时：`SummaryCompactor::with_timeout`。harness 运行时无关，不自己
  武装定时器；`uva-app-server` 在手动压缩上安装 `DEFAULT_SUMMARY_TIMEOUT`（120s）。
  超时产生 `CompactionError::Timeout`，审计行记为 `Failed`，turn 继续而不是挂死。
- 摘要以 replace 落盘：`user/message` + `surface_op: { op: "replace", start, end }`，
  同时记录 `shadowed_seqs`、`shadowed_token_count`、provider/model/usage。
- 摘要前先跑 tool-result pruner；压缩边界必须保证 tool-call/result 配对。
- 手动与自动走同一路径；`compaction/start` / `summary` / `end` 是 log-only 审计行。
- 压缩是可选能力：不安装 `Compactor` 时循环不做压缩。

### 7.3 可见性

- `AgentEvent` 增加压缩开始/结束的 live 事件（含 reason、被遮蔽范围摘要）。
- server 的 follow 基线带 `context: { used_tokens, window, compacted_before_seq }`。
- UI 在时间线插入“已压缩”标记；点击可看摘要与遮蔽范围。

**并发规则**：压缩只在 idle 或 step 边界运行；streaming 或 `AwaitingApproval` 期间
`POST /compact` 返回 `409 busy`。压缩若遮蔽了某个未决 `ResumeToken` 引用的 seq，
该 token 立即失效，resume 返回 `stale_resume`，由客户端重新取基线。

---

## 8. 事件与插件接缝

### 8.1 核心事件是封闭枚举

沿用 `uva_steering::HookEvent`，不新造第二套词表。v1 的决策点：

| 点 | 时机 | 允许的决策 |
|---|---|---|
| `BeforeRun` | run 开始前 | `Terminate` |
| `BeforeStep` | 每次模型调用前 | `Terminate` |
| `BeforeRequest` | 请求组装后、发送前 | 只读观察 + `ReplaceParams`（T2） |
| `AfterResponse` | 响应完成后 | 只读观察 |
| `BeforeTool` | 工具派发前 | `Allow` / `Block` / **`Ask`** |
| `AfterTool` | 工具返回后 | `Terminate`；结果改写走 `ResultTransformer`，不经 `HookDecision` |
| `AfterStep` | step 结束 | `Terminate` |
| `OnCompaction` | 压缩前后 | 只读观察 |

`HookDecision` 扩展为：

```rust
pub enum HookDecision {
    Allow,
    Block { reason: String },
    Ask { reason: String },
    ReplaceParams(RequestParams),
    Terminate { reason: String },
}
```

每个变体只对允许它的点有意义；派发层对不支持的变体返回 `Allow` 并记一条诊断，不 panic。

**最终形态（以此为准）**：`Ask` 携带 `reason: String`，不是 `ApprovalRequest`——审批请求由
循环在 gate 阶段自己构造（`id` / `call_id` / `arguments_hash` / `risk`），hook 无法伪造一个。
`ReplaceParams(RequestParams)` 仍在 steering 的词汇里，由 harness 的 `RequestTransformer`
在 `BeforeRequest` 应用（`RequestParams` 定义在 steering，`RequestTransformer` 定义在
harness）。结果改写不在 `HookDecision` 里：它由 harness 的 `ResultTransformer`
（`Host::result_transformer`，`AfterTool`）完成。因此早期词表里的
`ReplaceResult(ToolOutput)` 已废弃，最终实现中不存在；`ReplaceParams` 仍然存在。

### 8.2 插件事件是逃生通道

关闭的核心枚举无法容纳第三方领域事件。新增：

```rust
pub struct PluginEvent {
    pub kind: Cow<'static, str>,
    pub payload: serde_json::Value,
}
```

`PluginEvent` **只观察、不参与循环决策**；它用于 UI 扩展、审计、指标。
需要 durable 的插件事件追加为 `SessionEvent::Plugin { kind, payload }`；
不需要 durable 的走 live 通道。

**为什么不做泛型事件词表**：`Agent<S, E>` 会让 `Agent` trait、`ReasoningStrategy`、server、
全部 strategy crate 都多一个类型参数；收益只是编译期检查插件事件名，代价是整条链路的签名污染。
本仓选择“封闭核心 + JSON 逃生通道”，把泛型留给真正需要零成本的值路径。

### 8.3 Host 与注册

```rust
pub struct Host { /* observers, gates, transformers, sections, tools, delegators */ }

impl Host {
    pub fn observe(&mut self, hook: Arc<dyn Hook>) -> Registration;
    pub fn gate(&mut self, gate: Arc<dyn Gate>) -> Registration;
    pub fn request_transformer(&mut self, t: Arc<dyn RequestTransformer>) -> Registration;
    pub fn result_transformer(&mut self, t: Arc<dyn ResultTransformer>) -> Registration;
    pub fn prompt_section(&mut self, s: PromptSection) -> Registration;
    pub fn tool(&mut self, tool: Arc<dyn Tool>) -> Registration;
    pub fn delegator(&mut self, d: Arc<dyn DelegatingTool>) -> Registration;
}

pub trait Plugin: Send + Sync {
    fn name(&self) -> &str;
    fn install(&self, host: &mut Host) -> PluginHandle;
}
```

- `Host::install(&mut self, plugin: &dyn Plugin) -> PluginHandle`：句柄持有该插件全部
  `Registration`，drop 或 `uninstall()` 一并撤销。单个 `add_*` 仍返回 `Registration`。
- 注册按安装顺序执行；`Registration` drop 或 `unregister()` 撤销。
- 卸载只影响后续 dispatch，不影响正在进行的 dispatch（pi 的同款语义）。
- **仲裁顺序**：`Block` / `Terminate` 立即短路并获胜；`Ask` 不短路，收集到 gate 阶段末尾，
  与其它 `Ask` 合并为一次 suspension；`ReplaceParams` / `ReplaceResult` 链式传给下一个
  transformer。
- **失败策略**：gate 返回不支持/错误或 panic → 视为 `Block`（fail-closed），记诊断；
  observer / transformer panic → 记诊断并禁用该注册，后续 dispatch 不再调用它（fail-open
  只用于非安全决策）。
- **安全边界**：审批由循环内的 `ApprovalPolicy` 强制执行，不依赖某个 gate 被安装；
  gate 只能**增加** `Ask`，不能把 `Ask` 降级为 `Allow`。
- **per-agent 选择**：`ReActAgent::with_host(Host)`；server 为每个 agent 构建自己的 Host。
  全局单例不提供。
- 按名注册沿用 `StrategyRegistry` 的 `NameTable` 路子，供配置字符串解析；不做动态加载。

### 8.4 分级授权

| 档 | 能力 | 类型保证 |
|---|---|---|
| T1 观察 | 看事件、写指标/审计 | 只能拿到 `&` 视图 |
| T2 改写 | 改请求参数、改工具结果、注入临时 tail | 拿不到 `messages[0]` 的可写引用；没有 `system` 字段 |
| T3 注入 | 追加 system prompt 片段 | 只有 `add`，没有 `remove`/`replace` |
| T4 不可及 | 改 system prompt 本体、删历史、改日志行、关会话、给自己提权 | 方法不存在 |

**T2 的请求视图**沿用旧设计 §5.2：`RequestParams` 只含 model/temperature/max_tokens/tool_choice/tools；
`messages` 是 `&[Message]`，没有 `system` 字段。这样“改不了 system prompt”由签名保证，不靠文档。

### 8.5 委派

`SyntheticTool` 升级为 `DelegatingTool`（独立于 T1–T4）：

```rust
pub trait DelegatingTool: Send + Sync {
    fn definitions(&self) -> Vec<ToolDefinition>;
    fn mode(&self) -> ExecutionMode { ExecutionMode::Sequential }
    fn annotations(&self) -> Option<ToolAnnotations> { None }
    fn execute(&self, call: DelegationCall<'_>) -> BoxFuture<'static, ToolOutput>;
}
```

`DelegationCall` 提供 `call`、`emit`、`cancel`、`depth`、`ToolContext`。派发从
`claims(name)` 线性扫描改为按名查表。`uva-tool-task` 在同一次切换中迁移。

**子 agent 的审批**：策略在每个 agent 上独立生效，子 agent 不能借用父级的 `Never`。
子 agent 挂起时，`DelegationCall::emit` 把 `Suspended` 事件转发给父 run；父 run 以
组合 token 挂起，token 记录子会话 id 与 interrupt id。审批行落在**执行该工具的子会话**，
父会话追加一条 `subagent_suspended` 引用。resume 由父会话路由到子会话；未实现这条
之前，委派工具中的高风险调用一律 `Block`，不得静默放行。

### 8.6 运行中注入（steer）

两种用户输入，语义不同：

| 类型 | 时机 | 落点 |
|---|---|---|
| Queue（默认） | 当前 run 结算后 | 新 turn 的第一条 `user/message` |
| Steer | 当前工具批次完成、下一次模型调用前 | 当前 turn 的下一个 step 的 `user/message` |

- 输入走同一个 `POST /messages`，body 带 `delivery: "queue" | "steer"`；默认 queue。
- **日志是收件箱**：server 在返回响应前把 steer 追加为 durable `user/message`
  （`source: "steer"`，带 turn/step 坐标），并 `flush`。循环在每个 step 边界按 seq 水位
  重新 derive，看到新行就纳入下一次请求。
- **不打断在途模型调用**：steer 只在 step 边界生效；不会把一次 tool batch 拆开，也不会在
  streaming 中途改历史。
- **不丢**：如果 run 在边界前已经结算，server 把 steer 转成 queued turn；响应里说明它落在
  哪个 turn。`Stop` 把尚未注入的 steer 退回队列，不静默丢弃。
- **不做内存队列真源**：不复活已计划删除的 `RunInbox`；只保留一个 seq 水位。需要唤醒时由
  server 的 notify 触发，即使丢失唤醒，下一次 step 边界仍会从日志看到。
- **UI 状态**：`accepted`（已落盘）→ `injected step N`（已进入某次请求）→ `consumed`；
  若转 queue，显示 `next turn`。用户能看见自己的纠偏影响了哪一步。

---

## 9. 挂起与高风险审批

### 9.1 风险分类（按调用，不按工具）

注解给出的是工具级上界；同一次 `shell` 里 `ls` 与 `rm -rf` 不能同样对待。因此分类接口
按**调用**：

```rust
pub trait Tool {
    // ...
    /// 默认从 annotations 推导；需要看参数的工具有状态地覆盖。
    fn risk(&self, input: &serde_json::Value) -> Risk {
        Risk::from_annotations(self.annotations())
    }
}

pub enum Risk { Low, High, Unknown }
```

- `read_only_hint == true` → `Low`，永不询问。
- `destructive_hint == true` 或 `open_world_hint == true` → 默认 `High`；
  `ShellTool` 覆盖：只读 argv（`ls` / `cat` / `git status` …）→ `Low`，
  写文件、重定向、删除、提权、网络 → `High`。
- 无注解 → `Unknown`，按 `ask_unknown`（默认 false）决定。
- 注解是提示，不是安全边界；`ExecutionPolicy` / sandbox 仍是最后一道。

策略：

```rust
pub struct ApprovalPolicy {
    pub mode: ApprovalMode,          // AskHighRisk（默认）| Always | Never
    pub ask_unknown: bool,           // 默认 false
    pub overrides: BTreeMap<String, ApprovalMode>,
    pub grants: BTreeSet<GrantKey>,  // 宿主预授权
}
pub enum ApprovalMode { AskHighRisk, Always, Never }
```

**“永久”的范围**：`ApprovalGrant { tool, workspace_hash, scope }`，`scope` 只允许
`Session` 或 `Deployment`。`Session` 授权由审批卡的“批准本会话”产生（决策 payload
`{ "scope": "session" }`），resume 在同一个 flush 里追加 `approval_granted` 行，并把
grant 加进本次 run 的策略；下一次 run 在 setup 时从日志重放这些行。`Deployment` 只能
来自 server 配置，不能由一次点击产生。**没有全局无限期授权。**

### 9.2 挂起

不改变 `Agent::run` 的返回类型（那会击穿全部 strategy crate）。用事件表达：

```rust
pub enum SystemEvent {
    // ...
    Suspended(Suspension),
}

pub enum Suspension {
    AwaitingApproval {
        interrupts: Vec<ApprovalRequest>,
        resume: ResumeToken,
    },
    Paused { resume: ResumeToken },
}
```

**日志语义**：挂起不是崩溃。循环在结束前追加：

```text
approval/asked { id, call_id, tool, arguments_hash, risk, agent }
turn/suspended { resume_token, pending: [call_id] }
step/end { status: "suspended" }
turn/end { reason: "suspended" }
```

`repair` 认出 `turn/end{suspended}` 后**不再**补 `TOOL_OUTCOME_UNKNOWN`，也不会把它
当 interrupted turn。挂起期间所有未决调用保持未执行；已完成的低风险调用照常落盘。

**恢复入口**：`AgentInput::resume_decisions(token, decisions)`。空列表不是通配符：每个未决
interrupt 都必须有一条按 id 对齐的决定，否则以 `ResumeCoverageError::Missing` 拒绝。
`resume_approving_all(token)` 只是为 token 的每个未决 call 写一条 `Resolved` 决定的封装。
循环校验 token：

1. token 的最后 seq 等于日志尾（否则 `stale_resume`）；
2. `pending` 集合与日志里的 `approval/asked` 未决集合一致；
3. 当前 `request_snapshot` 指纹一致（否则 `ConfigDrift`，除非 `allow_drift`）。

校验通过后，循环**自己**派发已批准的调用、追加结果，再 `turn/start` 继续；server 不执行工具。
`ResumeToken` 由最后 durable seq + 未决集合摘要派生，跨进程与多节点可用。
`SessionEndReason` 增加 `Suspended` 与 `ConfigDrift`；`stale_resume` 作为 resume 错误返回，
不必成为 end reason。

### 9.3 审批流程与持久化屏障

```text
BeforeTool batch
  → 对每个 call 求 risk
  → 低风险：可执行
  → 高风险：暂停，不发车
  → append approval/asked（每条）
  → append turn/suspended
  → flush(Sync)          // 审批行必须活过崩溃
  → emit Suspended
  → run 结束
  → POST /resume
  → append approval/decided（每条）
  → flush(Sync)
  → 批准：append tool_dispatch{call_id} → flush(Sync) → 执行
  → 拒绝：写入 is_error 的工具结果，模型可见
  → 全部未决决定后才继续；不支持的 resume 明确报错
```

**持久化屏障**：

- `approval/asked`、`approval/decided`、`tool_dispatch` 三条是 `Sync` flush，不等 step 边界。
  审批是低频事件，fsync 成本可接受。
- `tool_dispatch` 是“副作用可能已发生”的意图记录。若进程在副作用之后、结果行之前崩溃，
  repair 写 `TOOL_OUTCOME_UNKNOWN`；**恢复不自动重跑** `Risk::High` 调用。
  只有 `idempotent_hint == true` 且 `Risk::Low` 的调用允许重试。
- `approval/asked` / `decided` / `tool_dispatch` 都是 log-only，不进模型历史。
  `approval/granted` 在本波没有生产者（会话级授权延后），只有类型与 builder。

**批量语义**：一个 step 可以有多个高风险调用。gate 阶段先跑完所有调用，低风险调用可执行，
高风险调用一起挂起；resume 必须覆盖全部未决 interrupt（AG-UI 规则）。挂起期间到达的
用户消息进入队列，不启动新 run。

**会话状态**：`SessionState` 增加 `AwaitingApproval`，由日志推导而不是内存标志；
`maybe_start_run` 在挂起期间拒绝启动。`cancel` 把全部未决审批结算为 `cancelled` 并关闭 turn。
默认不设过期；需要 TTL 的部署可在 `ApprovalRequest` 上带 `expires_at`，过期 resume 报错。

### 9.4 server 线协议与重连

借鉴 AG-UI 的语义，不实现完整协议：

- `GET /api/v1/sessions/{id}/follow?from=` 的首帧是**完整基线**：
  `{ header, rows (last durable window), cursor, live, context, pending_interrupts, request_snapshot_hash }`。
  `live` 是当前未结算 turn 的临时投影（已收到的文本、工具调用、思考），
  来自进程内缓冲，不落盘。
- 客户端收到首帧后**整窗替换**本地状态，再从 `cursor + 1` 应用增量。
- 增量帧带 `turn_id`、`seq`（durable）或 `live_seq`（transient）。
  同一 `turn_id` 的 durable 行到达时**替换**该 turn 的 live 投影；`live_seq <= 基线` 的增量丢弃。
- 进程在生成中途崩溃：`live` 丢失，durable 里没有半条 assistant 行；UI 显示中断，
  下次 run 从 `turn/start` 重来。这是“一轮一行”的既定取舍，不伪造部分历史。
- `POST /api/v1/sessions/{id}/resume`：`{ interrupts: [{ id, status, payload }], resume_token }`。
- `POST /api/v1/sessions/{id}/compact`：手动压缩；busy 时 409。
- `GET /api/v1/sessions/{id}/usage`：会话级 token/成本。

现有的 `/messages/stream` 在同一次切换中替换为 `follow`，前端同步改；不留别名。

---

## 10. 清理与性能

- 删除旧设计 §7 列的零外部引用 `pub`，删除前逐个复验。
- 合并重复实现：resume key、会话组装、token 记账、“调用没跑”的文本。
- 修 `sink` 注释漂移；注释只记非显然约束。
- 性能：
  - `commit.rs` 每工具结果 3 次 clone、`route.rs` 每次深 clone input；
  - server 的每条 append 一次 fsync（写者已支持批量，接上 `flush`）；
  - 投影缓存按 seq 水位失效，避免每 step 全量重算。

---

## 11. 验证

### 11.1 单元不变量

1. 队列容量 1 + 数十行：一行不丢、seq 连续、`fold` 的 provenance 校验通过。
2. 烧号后重启：下一个号大于日志最大号，不重用。
3. 混合日志（旧行无 `seq`）：整份按位置编号，不逐行混用。
4. fork：子会话从 1 重新编号；父会话不变。
5. repair：只追加 closers，原行字节不变。
6. fold 确定性：同一日志两次结果相同。
7. 压缩：摘要 replace 的 `shadowed_seqs` 正确；tool 配对不断裂。
8. 插件卸载：只影响后续 dispatch；正在进行的调用不受影响。
9. 风险分类：read-only 不问、destructive/open-world 必问、无注解按策略。
10. 审批幂等：同一 resume 重放不产生第二次工具执行。
11. 挂起重启：`turn/end{suspended}` 不被 repair 当作中断；pending 审批仍在。
12. Sync 屏障：`approval/asked` / `decided` / `tool_dispatch` 在返回前已落盘。
13. 不重跑高风险：副作用后崩溃 → repair 写 `TOOL_OUTCOME_UNKNOWN`，工具执行次数仍为 1。
14. 配置漂移：改 tool schema / prompt section 后 resume → `ConfigDrift`；`allow_drift` 才继续。
15. 批量审批：三个高风险调用产生三个 interrupt；只决定两个 → 拒绝 resume。
16. per-call 风险：`ls` 为 Low、`rm -rf` 为 High。
17. 压缩与挂起互斥：挂起期间 compact 返回 409；遮蔽 pending seq 后 resume 返回 stale。
18. follow 基线：中途重连后整窗替换，不产生重复文本；durable 行替换 live 投影。
19. steer 只在 step 边界生效，不拆分 tool batch、不改在途请求。
20. steer 行在返回响应前 durable；内存队列不是真源。
21. run 先结算时 steer 转 queued turn，不丢；被取消时未注入的 steer 留在日志里，成为下一轮的第一条 user 消息。
22. 同一 steer 在 derive 后只出现一次，且位置在下一个 step 的 `user/message`。

### 11.2 端到端验收

按 `docs/research/agent-conversation-experience-2026-10-25.md` 的剧本执行：

- A 基本回合 + 工具；
- B 中途纠偏：
  - queue：工具执行期间发送，当前 run 结算后才成为新 turn 的第一条消息；
  - steer：工具执行期间发送，落在下一次模型请求的历史里，且不拆分当前 tool batch；
  - run 先结算：steer 转 next turn，不丢；`Stop` 退回未注入项；
- C 高风险审批（批准/拒绝/永久）；
- D 崩溃重启；
- E 断线重连；
- F 压缩可见；
- G fork / 导出 / 会话级用量。

每条都要求可观察断言，不以“接口返回 200”代替。

### 11.3 门禁

`cargo +nightly fmt --all`、`cargo clippy --all-targets --all-features --tests --all`、
`make ci`、`make check-web`、`make test-guides`。

---

## 12. 实施顺序（每步工作区可编译）

| 步 | 内容 | 破坏性 |
|---|---|---|
| 1 | 删零引用 `pub`、合并重复实现、修注释（腾出预算） | 无（纯删） |
| 2 | `derive_history` + 循环改读投影；`Checkpointer` 退为加速 | 是（harness 内） |
| 3 | 运行中注入：`delivery: queue / steer`、step 边界重 derive、UI 状态 | 是（server + web） |
| 4 | Host + Hook 决策扩展 + `Registration`；`Steering` 并入统一派发 | 是（steering/harness） |
| 5 | `DelegatingTool`；`uva-tool-task` 迁移 | 是（跨 crate） |
| 6 | `Suspension` + 审批策略 + log rows；`AgentEvent` / `SystemEvent` 扩展 | 是（事件词表） |
| 7 | server：`follow` / `resume` / `compact` / `usage`；web 同步切 | 是（路由） |
| 8 | 教程与验收脚本；`make` 入口 | 无 |

第 2、4、6 步必须各自一次切换：第 2 步两侧同时读历史会产生两套真源；第 4 步两套 hook 派发
会重复执行；第 6 步事件词表变更会击穿全部 exhaustiveness match。第 3 步的输入路径与第 7 步的
follow 路由也要在同一次提交内换掉前端，不留双写。

---

## 13. 我代做的决定（可否决）

| 决定 | 理由 |
|---|---|
| 不新建 `uva-plugin` crate | 避免 harness ↔ plugin 的依赖环；插件实现可独立成 crate |
| 核心事件封闭 + `PluginEvent` JSON 逃生通道 | 不做泛型事件词表，避免签名污染；插件领域事件不需要参与循环决策 |
| 不改变 `Agent::run` 返回类型，用 `SystemEvent::Suspended` + `SessionEndReason::Suspended` | 旧设计的返回类型变更会击穿全部 strategy crate；事件表扩展更小 |
| 审批只问 `destructive` / `open_world`，无注解默认放行 | 用户明确“只在高风险工具”；`ask_unknown` 可由运维打开 |
| 审批决策用 `HookDecision::Ask`，不新造 gate trait | 复用现有 `BeforeTool` 点，避免第二套工具拦截 |
| `follow` 替换 `/messages/stream`，不留别名 | 仓库规矩：干净切换 |
| 多节点无状态化不进第一阶段 | 先让日志/挂起契约稳定；`ResumeToken` 已按跨节点可接手设计 |
| 挂起用显式 `turn/end{suspended}`，不把 turn 留开 | 否则 repair 会把有意挂起误判成崩溃中断 |
| 审批三行 `Sync` flush；高风险副作用后崩溃不重跑 | 审批低频，fsync 可接受；未知结果交给模型，不重复副作用 |
| 风险按调用分类，`ShellTool` 覆盖 | 否则 `ls` 也会问，违背“只在高风险” |
| 永久授权只到 Session；Deployment 只能来自配置 | 防止一次点击产生全局无限期授权 |
| gate 仲裁：Block/Terminate 短路、Ask 合并、gate 失败 fail-closed | 安全决策不能因为插件缺省而放行 |
| 审批由循环内的 `ApprovalPolicy` 强制，不靠 Host gate | 未安装 gate 不能成为绕过审批的路径 |
| 配置漂移默认阻断，`allow_drift` 显式放行 | 恢复的是同一配置下的同一日志，不是“尽量像” |
| 压缩与挂起互斥；遮蔽 pending seq 使 token 失效 | 审批引用的是 seq，压缩换 surface 后引用不再成立 |
| follow 首帧带 live 投影与 durable 基线，整窗替换 + `turn_id` 去重 | 中途重连不能靠“拼接”本地缓存与服务端历史 |
| 子 agent 审批独立、挂起向父 run 传播 | 否则委派成为审批绕过通道 |
| steer 以日志为收件箱，不做内存队列真源 | 唤醒可以丢，日志不能丢；这也避免复活已计划删除的 `RunInbox` |
| steer 只在 step 边界生效 | 不拆分 tool batch、不在 streaming 中途改历史 |
| queue / steer 同一路由，`delivery` 区分 | 一种输入资源，两种投递语义；前端只多一个动作 |
| run 先结算时 steer 转 queued turn | 用户输入不因为时序而消失 |

## 14. 留给实现者的部分

- 模块与文件切分（`log/`、`plugin/`、`approval/` 的内部布局）。
- 投影缓存的键与失效策略（seq 水位 vs 内容哈希）。
- `ContextManager` 是否直接改为 `Compactor` 的适配器，或并存一个版本。
- writer 内部通道的具体容量与 flush 批大小。
- SSE 帧的具体字段名与前端事件映射。
- 测试夹具与 fake provider 的组织。

---

## 15. 验收定义

“完成”指：

1. 上述七步全部落地，`make ci` / `make check-web` / `make test-guides` 绿。
2. 验收剧本 A–G 在真实 `uva-app-server` + `uva-web` 上跑通，有可复现命令与观察记录。
3. 一个外部最小消费方（不依赖 server）能安装一个自有插件并跑通一轮；证明接缝不是 server 专用。
4. 教程 `docs/tutorial/complete-agent.md` 可从空环境照做，不需要读源码。
