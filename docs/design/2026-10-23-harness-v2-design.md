# Harness V2 — 日志即真源，插件即一等公民

日期：2026-10-23
状态：已落地（第一段）——「日志即真源」的第一段由
[`2026-10-25-writer-unification-plan.md`](2026-10-25-writer-unification-plan.md)
实施：写者唯一化、序号以行里的 `seq` 为准、一轮一行。插件与循环从 fold 推导的那几段
尚未开始。
取代：无（这是 `elysper-harness` 的下一次形态，不是补丁）

---

## 1. 意图与为什么

`crates/elysper-harness` 要成为社区级 AI agent 基座库：第三方 crate 装进来就能加 tool 和
plugin，不改 harness 一行；外围应用组装它就得到一个完整 agent。今天它做不到后者——
插件挂载是编译期的，循环的真源在内存里，两者互不相干。

本设计把三件事一起换掉，因为它们互为前提：

1. **日志成为唯一真源。** 循环的每一步都从日志推导，不从内存对象推导。
2. **插件变成运行时挂载物。** 能观察、能改写普通请求与工具结果、能追加 system prompt；
   但碰不到 system prompt 本体和已落盘的历史。
3. **写入器归 harness 所有。** 「什么是 durable、崩了怎么补」是循环的语义，不该由
   server 事后兜底。

三者缺一，另外两个都不成立。只做 2 会得到「插件能改但改了什么没人知道」；只做 1 会得到
「日志很完整但没人能插手」。

### 现状（已核实）

- `elysper-session` 已经持有 dsh 那套机制：22 种 `SessionEvent` 变体、
  `SurfaceOp::Replace { source_event_seqs }` provenance 校验、`fold_surface`（`log_surface.rs`,
  957 行）、`repair.rs`（820 行，`interrupted_turn_closers`）、`token_meter.rs`（977 行）。
  `events.rs` 文件头注明 *"Adopted from `deepseek-harness`'s `core/session/src/{types, surface,
  repair}.ts`"*。**本设计不重造这些。**
  `fold_surface` 已有生产调用方（`elysper-context/src/memory/typed.rs:99` 的
  `try_fold_log_surface`），压缩路径不是死的。
- 断的是接线：`elysper-harness` 的 `EventSink` 只发结构事件（step / turn / usage），
  用户消息、assistant 消息、工具结果一行不进日志；写这些的是 server
  （`elysper-server/src/session/controller/run_task.rs`）。
- `AgentCaps` 四平面（Model / Tool / Control / Env）共 22 个 `with_*` builder，全部
  `mut self -> Self`，构造后不可变。
- `SyntheticTool`（`agent/re_act/synthetic.rs:23`）是唯一的「跨切面 tool」接缝：
  输出一次性 `BoxFuture<'static, ToolOutput>`（不可流式）、拿不到 `Tool` 的 `Context`、
  没有 `mode()`（一律串行）、没有 `annotations()` / `truncate()`、靠 `claims(name)`
  线性扫全部实例（`loop/tools/route.rs:120-125`）、`ToolExposure` / `ToolRestriction`
  未接入。
- `Steering` 的 `BeforeProviderCall` / `AfterProviderCall` 只观测，**决定被丢弃**。
- `TypedEventSink::record` 用 `try_send` 并丢弃返回值
  （`elysper-session/src/typed_event_sink.rs:157`）。队列满时丢行，于是
  `run_task.rs:535` 现盖的 seq 出现空洞，`fold_surface` 的 provenance 校验此后拒绝
  引用空洞之后的行——compact 静默失效，无报错。
- `elysper-harness` 的公开 API 有 14 个 `pub` 项在 `crates/elysper-harness/src/` 之外
  零引用（`with_run_inbox` / `MpscInbox` 一族占 1,009 行测试、`with_clock`、
  `with_tool_registry`、`with_model_hint`、`AgentRegistry::is_empty`、
  `Checkpointer::list_keys` 等）。注：`with_clock` 在 `elysper-tool-scheduler` 等处有同名
  命中，但那是 `SchedulerTool::with_clock` 这一**另一个类型**的方法；`with_tool_registry`
  的唯一外部命中是 `elysper-tool/src/restriction.rs:166` 的一句文档提及。两者对
  `ReActAgent` 而言仍是死代码。
- `sink.rs:95` 的文档注释称 *"elysper-harness holds `Arc<dyn SessionSink>`"*，
  而字段是 `typed_sink: Option<TypedEventSink>`。代码与注释不一致。

### 竞品基线

| | pi (`badlogic/pi-mono`, MIT) | dsh (`deepseek-ai/deepseek-harness`, MIT) | 本设计取用 |
|---|---|---|---|
| 真源 | JSONL 树（`id`/`parentId`） | append-only `SessionEvent` 日志 | dsh 的 append-only + pi 的树形父子 |
| 循环 | 外层续跑 × 内层 `hasMoreToolCalls \|\| pendingMessages` | turn × step，`deriveMessages()` | step 边界（harness 已有 `Step` 枚举） |
| 压缩 | 超 `contextWindow − 16384` 自动，保 20k | surface ops + provenance | 已有 `fold_surface`，只补 harness 侧调用方 |
| 扩展 | `ExtensionAPI`: `pi.on()` / `registerTool` / `registerProvider`，自带 exposure | Cordis 插件树，waterfall 挂载点 | 静态 + 按名注册；T1–T4 分级 |
| 挂起 | `AbortController` | `agent.cancel(cause)` 保留已投递文本 + 待批状态 | **从零建**，见 §6 |
| 崩溃恢复 | read-and-resume | append 补齐 `interruptedTurnClosers`，从不截断 | 三者中取 dsh 的补齐式 |

arXiv 2608.23953（2026-08）对比 deepagents / pi / dsh，结论之一是
*"thinner in coaching, thicker in durability"*——各家在引导模型上花心思，在崩溃正确性上
越做越厚。本设计取后半句。

---

## 2. 目标 / 非目标 / 反目标

### 目标

1. 第三方 crate 通过一个 `install(&mut Host)` 加 tool、plugin、hook、策略，**不改 harness**。
2. 循环的每一步都能回答「模型为什么看到这个」——日志是权威记录，改写留痕。
3. 崩溃后 resume 精确：已落盘的行一条不少，未完成的步骤被显式补齐。
4. 稳态零运行时依赖：公开 API 不出现 `tokio` / `async_std` / `smol` 类型；无 `[features]`。
5. 零 feature 门：可选能力用 trait + 宿主组装表达，不用 cargo feature。
6. 库面精简：删掉零引用的 `pub`，删掉重复实现，注释只记非显然约束。

### 非目标

1. **不做运行时动态加载。** 不引入 `libloading` / Rhai / Lua / WASM。理由见 §8。
2. 不做 GUI、不做多租户计费、不做向量检索（那属于 `elysper-search`）。
3. 不替换 `elysper-provider` 的 HTTP adapter 层。
4. 不为兼容旧签名保留 shim——按仓库既有规矩做干净切换。

### 反目标（做到了也算失败）

- **为了「精简」把可观测性砍掉。** 每个 step 边界、每次改写都必须可追溯。
- **插件能改 system prompt 本体或已落盘历史。** 见 §5 的 T4。
- **引入静默失败。** 今天的 `let _ = tx.try_send(...)` 吞掉返回值，是本设计要消灭的原型。
- **公开 API 里出现具体运行时类型**，或 harness crate 长出 `[features]` 表。

---

## 3. 约束

继承 `AGENTS.md` §3.6 / §3.7 / §3.8 的既有门禁，不新增例外：

- 依赖单向 `elysper` → `elysper-*` → `elysper-core`。
- 库 crate 只用 `thiserror`；app crate 只用 `anyhow`。
- 墙钟走 `elysper_core::Clock`，ID 走 `elysper_core::IdGen`；生产代码禁 `chrono::Utc::now()`。
- `max_width = 80`，禁通配符导入，函数 ≤100 行、嵌套 ≤4 层（harness 专项）。
- `scripts/harness-docs/baseline` 棘轮只许降不许升。
- `make check-public-api-runtime`、`make check-clock`、`make check-no-runtime`、
  `make check-harness-shape`、`make check-harness-features` 保持绿。

---

## 4. 架构

### 4.1 数据流

```text
        ┌──────────────────────────────────────────────┐
        │              AgentLoop（无 RunState）          │
        │  derive() → request → sample → dispatch → …    │
        └───────┬──────────────────────────┬───────────┘
                │ append（永不阻塞）        │ AgentEvent
                ▼                          ▼
      ╔═══════════════════╗          ┌──────────────┐
      ║  SessionLog       ║          │ 前端 / SSE   │
      ║  append-only      ║          └──────────────┘
      ║  唯一真源          ║
      ╚═══════╤═══════════╝
              │ flush（step 边界，一次 fsync）
              ▼
        ┌──────────┐
        │  JSONL   │
        └──────────┘
              │ resume
              ▼
      ┌───────────────────────┐
      │ repair：追加补齐       │  从不截断
      │ fold → surface 投影   │
      └───────────────────────┘
```

### 4.2 写入器：三个动词

```rust
pub enum Durability {
    /// 默认。step 边界一次 flush。
    StepBoundary,
    /// 每 flush_rows 条或 flush_every 毫秒攒批一次。
    Batched { flush_rows: usize, flush_every: Duration },
    /// 每条 append 都 fsync。调试与审计用。
    Sync,
    /// 纯内存，不落盘。测试用。
    Off,
}
```

契约（取自 dsh 的 `SessionPersistence`，逐条对应）：

| 动词 | 保证 | 谁在等 |
|---|---|---|
| `append` | 已被接受、有序、对读可见。**永不丢弃**：队列满则挂起生产者（背压）。 | 只等入队 |
| `flush` | **唯一承诺落盘的操作。** 一次 fsync 覆盖整个批次。 | flush 边界 |
| `repair` | 崩溃后**追加**补齐行：`TOOL_OUTCOME_UNKNOWN`、`turn_end{interrupted}`、缺失的 `step_end`。 | resume 时 |

**为什么默认档选 step 边界**：一个 step 内可能有 500 个 `assistant_chunk`，按 50ms 时间窗
flush 是 ~800 次 fsync；step 边界是 N 次（N = 步骤数）。一次模型调用 1–30 秒，一次 fsync
0.1–10 毫秒，step 边界方案的开销 < 0.01%。而且它给出时间窗给不了的语义：**工具结果在
下一轮 LLM 调用之前已落盘**，resume 能说「跑完了，结果在这里」，而不是让模型猜要不要重跑
（重跑一个写操作是有代价的）。

**`flush` 在热路径上，但只有 step 边界这一次。** 这一点必须说清楚，否则「append 永不阻塞」
与「工具结果在下次调用前已落盘」会显得矛盾。循环的形状是：

```text
derive() → sample() → dispatch() → commit() → [flush] → 下一轮
                                                    ↑
                                    唯一一次等待 fsync 的地方
                                    此时模型本来就在等网络
```

若 `Durability` 取 `StepBoundary` 或 `Sync`，则 flush 在等待之列；取 `Off` 则不等待。
**`StepBoundary` 是唯一既满足 §10 测试 2、又不给 `append` 加背压之外的阻塞的档位。**

**现有 `SessionSink` 已提供 `append_many`（单次 fsync，实测 ~200× 于逐行）与
`append_with_key`（幂等），`flush` 层直接建在 `append_many` 上。`repair` 层的
`interrupted_turn_closers` 已存在，缺的是 harness 侧调用方。**

**Codex 的教训**（[issue #16644](https://github.com/openai/codex/issues/16644)）：三层异步
持久化同时有在途写入，进程终止时线程历史静默丢失。修复方案是「重试失败的 flush 并上报
持久化失败」，不是「更聪明地丢弃」。本设计的 `append` 因此没有「丢弃」分支。

### 4.3 seq 的归属：编号上移（已决定）

**问题**：今天 `SessionSink::append` 由 sink 在落盘时分配 seq 并返回给调用方
（`elysper-session/src/sink.rs:109-114`）。server 的 `RunLog::append` 拿这个 seq 喂给
`SurfaceLedger`；`persist_and_broadcast` 还把它当 SSE 游标返回给
`routes/chat.rs::resume_stream_to_sse`。

若改成异步队列，**生产者在 append 时拿不到自己的 seq**——而
`Compaction.source_event_seqs` 必须引用已落盘的 seq 做 provenance 校验。拿不到 seq，
压缩静默降级为「重放被遮蔽的区间」。

**决定（用户选定 A：编号上移）。seq 属于日志，不属于 sink。**

```rust
pub trait LogWriter {
    /// 分配单调 seq。同步，不阻塞。
    fn stamp(&self) -> Seq;
    /// 按已分配的 seq 入队。队列满则挂起生产者（背压）。
    fn append(&self, row: StampedRow) -> impl Future<Output = ()>;
    /// 落盘承诺。step 边界调用一次。
    async fn flush(&self);
}
```

`LogWriter` 持一个 `AtomicU64`。调用方先 `stamp()` 拿到号，再 `append()`——因此
provenance 引用当场就能填，不必等 flush。

**破坏性变更**：`SessionSink::append` 的 `-> Result<u64, SessionError>` 改为
`-> Result<(), SessionError>`。sink 不再编号，只负责「按序把这几行写下去」。
`append_with_key` 与 `append_many` 同步调整。
**附带收益**：`SessionSink` 退化为纯粹的可插拔持久化后端，换 Redis / 远程对象存储时
不必重新实现编号语义。

**SSE 游标**：`persist_and_broadcast` 现在把 sink 返回的 seq 当流游标
（`routes/chat.rs::resume_stream_to_sse`）。改为使用 `stamp()` 的号，
`SessionSnapshot.last_stream_index` 语义不变。

### 4.4 原子切换边界

harness 与 server **必须在同一次提交里换掉写入者**。理由：两侧同时写会产生重复行，
provenance 立即失效。切换顺序为：先加 `LogWriter` 与新 sink 实现（无行为变化），
再一次性把 server 的 `RunLog::append` 与 harness 的 `EventSink::emit_typed` 指向它，
最后删旧路径。

### 4.5 模块划分

> **已被 [`2026-10-24-writer-unification-design.md`](2026-10-24-writer-unification-design.md) §5.1 推翻。**
> 写者（`LogWriter`）下沉到 `elysper-session`：折叠器本来就在那里，而 provenance 要求写者取的号与
> 折叠器算的号是同一个数；写者留在 harness 会让 `elysper-session` 里的 `CompactionCheckpoint`
> 够不着它，只能另开一条会丢行的通道。下文的 `log/` 只保留「durable / ephemeral 分类」的职责，
> 写者本身归 session crate。

```text
crates/elysper-harness/src/
├── log/                    # 循环的日志视图（harness 私有，不是新 crate）
│   ├── mod.rs              # ~~LogWriter：stamp / append / flush / repair~~ → 下沉到 session
│   ├── surface.rs          # fold 出来的只读模型视图
│   └── events.rs           # durable / ephemeral 分类
├── agent/
│   ├── mod.rs              # Agent trait（极简）
│   ├── caps.rs             # 保留但瘦身为不可变配置
│   ├── loop/               # 状态机：derive → sample → dispatch → commit
│   └── run.rs              # 一次 run = 一个 turn
├── plugin/                 # ★ 新增：运行时挂载体系
│   ├── mod.rs
│   ├── host.rs             # Host：插件唯一的注册入口
│   ├── tier.rs             # T1–T3 授权等级
│   ├── delegate.rs         # 委派接缝（§5.3），独立于 T1–T4
│   └── registry.rs         # 按名解析
└── ...
```

`log/` 是 harness **私有模块**，不是新 crate——因为「哪些行是 durable」必须与循环的实现
同处一个 crate，否则又变成 server 的事。

### 4.6 注释预算：当前零余量

`scripts/harness-docs/baseline` 是 **56**，`check-harness-docs.sh` 实测当前**正好 56**——
余量为零。而 `log/` 与 `plugin/` 需要为非显然约束写私有项文档（seq 归属规则、
durable/ephemeral 判据、T2 只读视图的意图），正是这个棘轮想保留的那类注释。

因此 §7 的删除项**必须在引入新模块的同一次提交里完成或先行**，否则门禁立刻变红。
`AgentCaps::defaults` 不删，改私有（它有两次 crate 内调用）。

---

## 5. 插件体系

### 5.1 发现与装配

组合两条 Rust 生态惯例（用户已确认）：

- **静态安装**：宿主 crate 显式调用 `plugin.install(&mut host)?`。
- **按名注册**：插件导出 `const NAME: &str` 与 `fn plugin() -> Box<dyn Plugin>`，
  宿主按配置字符串解析（沿用 `StrategyRegistry` 的 `NameTable` 路子）。

Rust 无运行时反射，故不做 `libloading`。§8 记录了这条路和 WASM 路线的取舍。

### 5.2 分级授权

| 档 | 能力 | API 形状 |
|---|---|---|
| **T1 观察** | 看得见动不了。遥测、审计、计费。 | `on_request(&Request) -> Report` |
| **T2 改写** | 改写**普通请求**与**工具结果**，改完模型看得见。 | `rewrite_request(&mut Request) -> Rewrite`、`rewrite_result(&mut ToolOutput) -> Rewrite` |
| **T3 注入** | 向 system prompt **追加**片段。原始 prompt 字节不变。 | `contribute(&mut PromptContext) -> Injection`（只有 add，无 remove） |
| **T4 不可及** | **类型上不存在**：无 `set_system_prompt`、无 `drop_history`、无 `rewrite_log_row`、无 `close_session`、无 `grant_t3_to_peer`。 | — |

T4 是关键：权限靠约定维持会被绕过，靠类型维持不会。缺席的 `grant_t3_to_peer` 防止
T3 插件自我提权——否则分级退化成「有或没有」。

**T2 拿到的 `Request` 不是 `CompletionRequest`（重要）。**
`elysper_provider::CompletionRequest.messages` 是 `Arc<Vec<Message>>`，且 `messages[0]`
就是 system prompt。若把这个类型交给 T2，插件改一下 `messages[0]` 就绕过了 T3/T4——
分级形同虚设，而只断言「`set_system_prompt` 不存在」的测试抓不到这种绕过。

因此 T2 拿到的是一个**结构上就够不着那些东西的视图**：

```rust
/// T2 能改的：采样参数与工具集。
pub struct RequestParams<'a> {
    pub model: &'a mut String,
    pub temperature: Option<f32>,
    pub max_tokens: Option<usize>,
    pub tool_choice: &'a mut ToolChoice,
    pub tools: &'a mut Vec<ToolDefinition>,
}
/// T2 能追加的：本轮临时内容，不落盘、不进历史。
pub struct TransientTail(Vec<ContentPart>);
/// T2 只能读的：全部消息（含 system prompt），只读。
pub messages: &'a [Message],
```

`messages` 是 `&[Message]` 而非 `&mut`，且**没有 `system` 字段**。
于是「插件改不了 system prompt 和历史」由签名保证，不由文档约定保证。

**改写留痕（用户已确认「留」）**：T2 改写工具结果时写入
`tool_result_rewritten { by, original_hash, content }`。默认只存原文哈希而非原文，插件
可显式要 `retain_original` 才重复存。审计链因此能算出「改前是什么」并定位到「哪个插件
改的」，代价可控。

**副作用**：T4 使「日志只增不减」成为类型保证，`source_event_seqs` 的 provenance 校验
永远成立。

### 5.3 委派不是第四档，是独立能力

`SyntheticTool` 需要的能力**不在 T1–T4 任何一档里**——T1–T4 管的是「你能对**内容**做什么」，
而 `TaskDelegator` 需要的是「**执行**一个跑在循环内部的东西」（派生子 agent、转发事件、
限制深度）。把执行塞进「改写」这一档是本设计初稿的错误。

因此委派单列：

```rust
pub struct DelegationCall<'a> {
    pub call: &'a ToolUse,
    pub emit: mpsc::UnboundedSender<AgentEvent>,  // 转发子 agent 的事件
    pub cancel: Arc<dyn CancelToken>,
    pub depth: usize,                              // 子 agent 在 depth + 1
    pub context: &'a ToolContext,                  // workspace_root 等
}

/// 拿到 emit / cancel / depth / context 的插件。
pub trait DelegatingTool: Send + Sync {
    fn definitions(&self) -> Vec<ToolDefinition>;
    fn mode(&self) -> ExecutionMode { ExecutionMode::Sequential }
    fn annotations(&self) -> Option<ToolAnnotations> { None }
    fn execute(
        &self,
        call: DelegationCall<'_>,
    ) -> BoxFuture<'static, ToolOutput>;
}
```

与现有 `SyntheticTool` 相比**补齐了权力差距**：拿到 `context`、可声明 `mode()`（并行/串行）、
可声明 `annotations()`、并接入 `ToolExposure` 与 `ToolRestriction`。
`elysper-tool-task` 的 `TaskDelegator` 迁到此处；`server/agent_assembly.rs`、
`run_stream.rs`、`tests/delegation.rs` 随之改。

派发从「线性扫全部实例找 `claims()`」改为按名查表，沿用 `ToolProjectionCache` 已有的
version-keyed 缓存模式。

**授权边界**：委派是「能跑子 agent」这一独立能力，安装时显式授予；它**不**带来 T4 那批
禁止项——跑子 agent 不等于能改 system prompt。

---

## 6. 挂起与恢复

harness 目前**没有中途挂起原语**：`Checkpointer` 只能整轮快照，`Steering` 的 hook 只有
同步 `Block` / `Terminate`（`elysper-steering/src/hook_map.rs:115`）。这是 dsh / pi /
Claude SDK 都有、elysper 独缺的能力。

新增：

```rust
pub enum Suspension {
    /// 已投递的文本保留，未派发的调用标记 ABORTED_BEFORE_DISPATCH。
    AwaitingApproval { tool_call: ToolUse, resume_token: ResumeToken },
    /// 人离开了，稍后接着跑。
    Paused { resume_token: ResumeToken },
}
```

`ResumeToken` 由日志派生（最后 durable seq + 未闭合行的摘要），因此 resume 不依赖
进程内存——多节点部署下任意计算节点都能接手。

---

## 7. 精简

- **删除 14 个零外部引用的 `pub` 项**：`with_run_inbox` / `MpscInbox` 一族（连带 1,009 行
  `inbox_tests.rs`）、`with_clock`、`with_tool_registry`、`with_model_hint`、
  `AgentRegistry::is_empty`、`Checkpointer::list_keys`、`InMemoryCheckpointer::new`、
  `CheckpointSnapshot::at`、`StrategyRegistry::names`、`AgentInput::to_message`、
  `SessionEndReason::as_str`、`AgentCaps::defaults`、`ReActAgent::run_inbox`。
  删除前逐个复验外部引用（可能随 §5 的重写而变）。
- **合并重复实现**：
  - resume key 格式在 `prepare.rs:81` 造、`finalize.rs:127` 解——提为共享构造器。
  - 会话组装在 `sample.rs:47` 与 `strategy/mod.rs:88` 各写一遍——合并为一处。
  - token 记账有三个 sink（`stream.rs:134`、`sample.rs:321/322`），且 stream 与 response
    记的是两次调用的量——收敛为一个。
  - 「这个调用没跑」的文本构造有三份——收为一处。
- **修文档漂移**：`sink.rs:95` 关于 `Arc<dyn SessionSink>` 的注释与字段不符。
- **注释**：遵守 `check-harness-docs` 棘轮，只记非显然约束，删除复述签名的注释。
- **性能**：修 `run_task.rs` 现有每条 append 一次 fsync 的热路径；清理
  `commit.rs:130-136` 每工具结果 3 次 clone、`route.rs:39` 每次调用深 clone `input`。

---

## 8. 关于运行时动态加载（已决定不做）

dsh 的 Cordis 与 pi 的 `ExtensionAPI` 都能「扫描到就注册」，因为它们是动态语言。
Rust 侧的等价物是 `libloading` 加载 `.so`，代价是：ABI 不稳定（插件一改就崩宿主）、
需要沙箱与权限边界（与 §5 的 T1–T4 重叠且更弱）、错误跨 FFI 边界。

取舍：动态加载带来的能力（配置改完不重编译）**不足以**抵消它对「社区库」这个目标的
伤害——一个能加载任意 `.so` 的 agent 基座，本身就是攻击面。

**留的口子**：本 harness 不含此能力，但 `Plugin` trait 的形状（按名注册 + 显式 install）
不排斥未来由独立 crate 提供 WASM 宿主。若将来需要，`elysper-harness` 不必改。

---

## 9. 留给实现者的部分

实现者需要自己决定、且猜错也不要紧的：

- `LogWriter` 内部用 `futures::channel::mpsc` 还是 `tokio::sync::mpsc`（harness 内只能是
  futures；宿主若要 tokio 需过 `Spawner`）。
- `fold` 的结果缓存策略（现在 `ToolProjectionCache` 是每次比较 `last_snapshot`）。
- `Rewrite` 枚举的具体变体集（至少 `Keep` / `Replace` / `Fail`）。
- 目录与文件名（`log/` 下的切分）。

---

## 10. 验证

| 目标 | 命令 |
|---|---|
| 编译 | `cargo build -p elysper-harness` |
| 单测 | `cargo test -p elysper-harness --lib` |
| 公开 API 无运行时类型 | `make check-public-api-runtime` |
| 无 tokio 依赖 | `make check-no-runtime` |
| harness 无 features 表 | `make check-harness-shape`、`make check-harness-features` |
| 时钟棘轮 | `make check-clock` |
| 注释预算未增长 | `make check-harness-docs` |
| 全量 | `make ci` |

**新行为的针对性验证**（不是 wiring 测试）：

1. 队列塞满时 durable 行一条不丢，seq 连续——构造容量 1 的队列 + 生产数十行，
   断言 `fold_surface` 成功且 provenance 校验通过。**这是修复 #16644 那类静默损坏的
   回归测试。**
2. step 边界 flush：工具结果在下一轮 LLM 请求前已落盘——用计数 fake provider 断言
   第二次请求发出时 `SessionSink::snapshot().last_event_seq` 已含该结果。
3. T2 改写留痕：`tool_result_rewritten` 行含正确的 `by` 与 `original_hash`，且
   `original_hash` 确为原文哈希。
4. T4 类型不可及：编译失败测试（doctest `compile_fail`）断言
   `set_system_prompt` / `grant_t3_to_peer` 不存在于 `Plugin` 的可用方法中。
   **并且**断言 T2 视图本身够不着：`RequestParams` 无 `system` 字段、
   `messages` 是 `&[Message]` 而非 `&mut [Message]`（纯编译期，不需要 trybuild）。
5. crash 恢复：写一半日志 → 模拟 resume → 断言 `repair` **追加**了
   `TOOL_OUTCOME_UNKNOWN` 且原有行字节不变（对比崩溃前后 `read()` 输出）。
6. seq 上移：`SessionSink` 后端不再编号——断言同一 sink 先 `stamp()` 出
   `{1,2,3}` 再任意顺序 `append`，`read()` 出来的行序仍按号排列。

### 10.1 可编译的落地顺序

这是破坏性重写，每一步后工作区必须能编译：

| 步 | 内容 | 破坏性 |
|---|---|---|
| 1 | 补 §7 的删除项，腾出注释预算（§4.6 的前置条件） | 无（纯删） |
| 2 | 加 `LogWriter` + 新 `SessionSink` 形态，`sink.rs` 保留旧实现并适配 | 无（新旧并存） |
| 3 | **原子切换**：server `RunLog::append` 与 harness `EventSink::emit_typed` 同时指向新写入者；同一次提交内删旧路径 | 是 |
| 4 | 加 `plugin/` 模块（T1–T3 + `DelegatingTool`），不改现有循环 | 无（旁挂） |
| 5 | 循环改从 `fold` 出的 surface 推导；删 `RunState` 的可变字段 | 是（仅 harness 内） |
| 6 | `TaskDelegator` 迁到 `DelegatingTool`，改 `server/agent_assembly.rs` / `run_stream.rs` | 是 |
| 7 | 加 `Suspension` / `ResumeToken`；`Agent::run` 返回类型变更，**连带全部 `elysper-strategy-*` crate** | 是（跨 crate） |

第 3 步与第 7 步是仅有的两处原子切换：前者两侧同时写会产生重复行、破坏 provenance；
后者 `Agent::run` 的签名变更会击穿所有 strategy crate 与 server 的调用点。
**两者都不得拆成多次提交。**

---

## 11. 已由对话确定、非我代决的决定

| 决定 | 依据 |
|---|---|
| 本轮唯一优先级是「第三方 crate 装插件不改 harness」 | 用户选择 ③ |
| dsh 那种（运行时挂载 + 日志真源）与 pi 那种（exposure / 压缩 / 会话树）都要 | 用户明确 |
| 精简与性能要，可暂时忽略一些毛病 | 用户明确 |
| 现有 API 允许破坏性改动，只要最终目的达成 | 用户明确 |
| 日志异步追平、循环走内存 | 用户选择 |
| 两类事件两种命运（durable 背压 / ephemeral 可丢） | 用户确认我的猜测 |
| flush 攒批，且批次边界比纯时间窗更合理 | 用户选择 |
| 默认档 step 边界 | 我的建议（用户选择攒批后提出，未反对） |
| 分级授权 T1–T4，T4 类型上不存在 | 用户选择 |
| 改写留痕（存原文哈希 + 改后内容） | 用户选择「留」 |
| 静态安装 + 按名注册，不做动态加载 | 用户选择 |
| seq 编号上移到 `LogWriter`，`SessionSink::append` 改为 `Result<(), _>` | 用户选择 A |

## 12. 我代做的决定（供你否决）

| 决定 | 理由 |
|---|---|
| `Durability` 枚举四档，默认 `StepBoundary` | 语义沿用 LangGraph `durability` / PG `synchronous_commit` / Redis `appendfsync`，但默认档选在最省的位置 |
| ~~`log/` 是 harness 私有模块而非新 crate~~ **（2026-10-24 推翻）** | 原理由：「什么行是 durable」须与循环实现同 crate，否则重蹈 server 兜底的覆辙。推翻后：写者下沉到 `elysper-session`（折叠器在那里，两端必须同 crate 才能保证 provenance 的号一致）——见 [`2026-10-24-writer-unification-design.md`](2026-10-24-writer-unification-design.md) §5.1 |
| 设计文档放 `docs/design/` 而非 `docs/superpowers/specs/` | 后者在本仓库 `.gitignore:42`，AGENTS.md 明定为 agent 草稿区 |
| `SyntheticTool` **改造**为 `DelegatingTool` 而非删除 | 实现者审查发现：把执行能力塞进 T2「改写」档是错的。本能题成「执行循环内子 agent」的插件无处落地。改造保留其 `emit`/`cancel`/`depth`，补齐 `context`/`mode()`/`annotations()` |
| T2 拿只读 `RequestParams` 视图而非 `CompletionRequest` | 后者的 `messages[0]` 就是 system prompt，交给插件等于绕过 T3/T4 |
| 委派是独立能力、不占用 T1–T4 档位 | 档位管「对内容能做什么」，委派管「能否执行」，两者正交 |
| 挂起原语（§6）纳入本轮而非后续 | 无挂起则 `AwaitingApproval` 类插件无处落地，§5 的分级授权形同虚设 |
| §10.1 的 7 步顺序，其中第 3、7 步为原子切换 | 实现者审查问「能否排序落地」；第 3 步两侧同写会破坏 provenance，第 7 步会击穿全部 strategy crate |

## 13. 实现者审查已解决

派了独立 subagent 当实现者读本文档，其提出的问题与处置：

| # | 问题 | 处置 |
|---|---|---|
| 1 | T2 的 `Request` 是否含 system prompt 与历史？若是，分级形同虚设 | §5.2 改为只读 `RequestParams` 视图 |
| 2 | seq 归属与原子切换边界未定义 | §4.3 编号上移；§4.4 原子切换；§10.1 落地顺序 |
| 3 | `SyntheticTool` 的**执行**能力在 T1–T4 中无对应 | §5.3 新增 `DelegatingTool` 独立能力 |
| 4 | `flush` 是否在热路径上 | §4.2 写明：仅 step 边界一次，在等待之列 |
| 5 | 注释预算零余量（baseline 56 / 实测 56） | §4.6 列为硬约束；删除项须先行 |
| — | 其称「`fold_surface` 零生产调用方」 | **该结论有误**：`elysper-context/src/memory/typed.rs:99` 在用。已在 §1 更正 |
