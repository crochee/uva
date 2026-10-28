# 写者唯一化 — 一个会话一个写者、一个序号源、一次 flush

日期：2026-10-24
状态：待批准（§5.2 已定：(i) harness 写整条消息）
范围：`elysper-session` / `elysper-harness` / `elysper-server` / `elysper-context` / `elysper` facade / `elysper-web`
上游：[`2026-10-23-harness-v2-design.md`](2026-10-23-harness-v2-design.md)（§4.2 / §4.3 / §4.6 / §7）
本轮是那份设计的**第一段落地**：先把「谁写日志、序号从哪来、一次 fsync 付多少钱、恢复后读回什么」
收口，再谈 plugin/、循环从 fold 推导、挂起恢复。

---

## 1. 意图与为什么

`elysper-harness` 已经长出一个批量写器（`crates/elysper-harness/src/log/`），但它**生产上无人使用**；
跑生产的是一条更老、更贵的写法：每写一行 JSONL 就 `fsync` 一次，写入口散在五个地方，
而且**序号有两个来源**。

核实下来是八个互相咬合的事实（§4）：

1. **每个 token 两行、两次 fsync。** provider 的每个文本 delta 既产生一条 server 侧的 durable 行，
   又产生一条 harness 侧的 `AssistantChunk` 行。
2. **恢复后模型看到的历史被打碎。** 上面那两行让一次回复在 resume 时变成 N 条只含碎片的
   assistant 消息——**这是正确性 bug，不是性能问题**。前端看不出来（web 自己会合并文本块），
   但**模型看得到**。
3. **序号有两个来源。** 写者从 `sink.alloc_seq()` 拿号写进行里，折叠器却按文件位置算号；
   并发写一交错，压缩引用的 `source_event_seqs` 就对不上，压缩静默失效。
4. **老路会静默丢行。** `typed_event_sink.rs:162` 的 `let _ = tx.try_send(record);`——队列满就丢，
   洞之后的 provenance 校验会拒绝引用它的压缩记录。
5. **同一件事写两遍。** `request_header` 每 run 两行。
6. **占号与落盘不是原子的**，并发写会让文件顺序 ≠ 序号顺序。
7. **计数器从「行数」续接**，崩溃烧掉的号会在重启后被重用。
8. **写点比想象中多**，且散在 routes、subagent_router、context crate 里。

本轮的目标不是加功能，是**把「一个会话的日志由谁写、序号从哪来、什么时候落盘、恢复后读回什么」
变成一条规则**。

### 竞品坐标

| | pi | dsh | 本设计取用 |
|---|---|---|---|
| 写者 | 应用层的 `SessionManager` | 日志即真源，追加式 | **一个会话一个写者**，写者住在 session crate（§5.1） |
| 序号 | 无（内存 transcript） | 事件日志的序号即 provenance | **行里的 `seq` 是唯一真源**，折叠器读它而不是数位置 |
| 落盘 | — | 追加 + 补齐，从不截断 | 同上；批量 flush，一次 flush 一次 fsync |
| 一次回复落几行 | — | `assistant/message`（整条消息一行） | 同 dsh：一条 assistant 消息一行（§5.2） |
| delta 落不落盘 | — | raw chunk 是 live 事件，durable 的是整条消息 | 同 dsh：delta 不落盘（§5.2） |

---

## 2. 目标 / 非目标 / 反目标

### 目标

1. 一个会话只有**一个**写者对象；所有行（模型可见行、结构行、反馈、关闭标记、压缩行、
   子代理标记、会话 fork、消息编辑）都经它落盘。
2. 序号的唯一真源是**行里的 `seq` 字段**；旧日志（没有 `seq` 的行）继续按位置算，行为不变。
3. **一次回复在日志里是一行**，不是 N 行；resume 后模型看到的是一条 assistant 消息，不是 N 条。
4. 一次 flush = 一次 `append_many`；模型可见的**事实**在广播前落盘，**广播语义不变**。
5. 任何路径都不丢 durable 行：同步回调走 spill，异步路径走背压。
6. 公开面上**删掉**一条会丢行的通道（`TypedEventSink`），不留 shim。

### 非目标

1. 不做「每 step 一次 fsync」——那需要新增一个 step 边界事件并改变 durable 行的广播时机，
   留到下一轮（§10 列出前置条件）。
2. 不动 plugin/、循环从 fold 推导、挂起恢复——那是 v2 设计的 §5 / §4.1 / §6。
3. 不改 `fold` 出来的 surface **内容**（哪些行进 surface、哪些被忽略），只改它**怎么算序号**。
4. 不换 JSONL 格式，不做日志压缩/归档。
5. 不动 compaction 的**判定**逻辑（什么时候压缩、压缩掉哪一段），只保证它的 provenance 能解析。
6. 不给「每轮一行汇总」新增 `SessionEvent` 类型——整条 assistant 消息行已经承载了同样的信息
   （delta 拼起来就是文本），逐 chunk 的边界不值得一个公共类型。

### 反目标（做到了也算失败）

- **为了少一次 fsync 而牺牲「看到的行都在盘上」。** 那是 SSE 断线续传的契约。
- **为了兼容旧签名留 shim。** 按仓库规矩做干净切换。
- **保留两条写路。** 「harness 直写 + host 直写」这种双轨必须归零，否则序号迟早再漂。
- **只把「每 token 一行」变成「每 token 一行、但换个地方写」。**

---

## 3. 约束

继承 `AGENTS.md` 的既有门禁，不新增例外：

- 依赖单向：`elysper` → `elysper-*` → `elysper-core`。**写者下沉到 `elysper-session` 不违反**
  （session 在 harness 下面；harness 本来就依赖 session）。
- 库 crate 只用 `thiserror`；`elysper-server` 是 app crate，用 `anyhow`。
- 墙钟走 `elysper_core::Clock`；ID 走 `elysper_core::IdGen`。
- `max_width = 80`；禁通配符导入；`elysper-harness` 生产代码无函数 >100 行、无嵌套 >4 层。
- `make check-public-api-runtime`：库 crate 的公开 API 不得出现具体运行时类型
  （`elysper-session` 在门禁范围内 → 写者的锁只能是私有字段，且类型必须 runtime-free：
  用 `futures::lock::Mutex`，session crate 已依赖 `futures` ✓）。
- `make check-clock`：`elysper-session` 库代码不得直接 `chrono::Utc::now()` → 写者必须持有
  一个 `elysper_core::Clock`。
- `scripts/harness-docs/baseline` = 56，当前实测 53；只许降不许升。
- `scripts/paradigm-surface/baselines/crates/elysper-session` = 13，当前 13（**正好顶格**）：
  写者要新增 re-export，就必须在同一个提交里抬这条基线。
- `MINIMAL.md` 是**被编译的 doctest**（`make test-guides` ⊂ `make ci`）：它今天用的是
  `Durability` / `LogWriter::new(sink, Durability::StepBoundary)` / `TypedEventSink::channel(64)` /
  `.with_typed_event_sink(...)`——这些 API 全删，**MINIMAL.md 必须同步改写**，
  否则 `make ci` 直接红。

---

## 4. 现状（逐条核实）

| 事实 | 证据 |
|---|---|
| harness 的批量写器生产上无人使用 | server 从不调 `with_log_writer`；`LogWriter` 还出现在 `MINIMAL.md:224,247`、`CHANGELOG.md:22,26` 与 facade 的 `elysper::harness::{LogWriter, Durability}` 公开路径上 |
| server 走的是老路 | `run_task.rs:94` 设 `config.typed_event_sink`；`agent_assembly.rs:84-85` → `with_typed_event_sink` |
| 老路丢行 | `elysper-session/src/typed_event_sink.rs:162` |
| 批量口有生产调用方 | `elysper-server/src/routes/sessions.rs:998` 的 `child.append_many(&inherited)`（会话 fork）；`crates/elysper-harness/src/log/mod.rs:199` |
| `Durability::Batched` 从未构造 | 全仓库无构造点；`log/mod.rs:208` 的 `flush_if_batched` **每次 append 都会被调用**（`:150`），只有它的 `Batched` 分支（`:209-212`）是死的 |
| `Durability` 无生产使用者 | server 不用 `LogWriter` → 四档都没有生产构造点 |
| 折叠器按位置算序号 | `log_surface.rs:484-489`：`seq += 1` 逐事件行递增；模块文档 `:30-33` 明说「seq 是行在日志里的位置」 |
| 账本的种子也是位置序号 | `run_task.rs:189` `ledger.record(index as u64 + 1, row)` |
| 折叠器与写者会不同步 | 见 §4.1 F6 |
| `SessionSink::append_many` 的**默认实现**是逐行 `append` | `elysper-session/src/sink.rs:287-292` → 「一次 flush = 一次 fsync」只对覆写了它的 sink 成立（`jsonl.rs:341`、`in_memory.rs:123`） |

### 4.1 追加核实（写完初稿 + 独立实现者审查后补的八个事实）

**F1 · 每个 token 两行、两次 fsync。**
- provider 每个文本 delta → `StreamChunk::Content(ContentPart::Text)`
  （`elysper-provider/src/assembler.rs:145-151`）→ assembler **为每个 delta 追加一个 part**
  （`append_part`，`:388`）→ harness 的 `publish_parts`（`harness/agent/re_act/stream.rs:99-108`）
  逐 part 发 `AgentEvent::Model(ContentPart::Text(delta))`。
- 该事件 `is_durable()` == true（`harness/src/events/mod.rs:70-79`）→ server **每个 delta 写一行 +
  一次 fsync**（`server/.../persist.rs:188` → `run_log.rs:63` → `jsonl.rs:290`）。
- 同时 harness 为每个 delta 再发一条 typed `AssistantChunk`（`stream.rs:39-66`）→ server 在 run 末尾
  又写一行 + 一次 fsync。
- ⇒ **每个 token 两行、两次 fsync**。这是**上界**：typed 通道容量 64（`typed_event_sink.rs:162`），
  超过就静默丢弃，所以后半段的 chunk 行今天根本没落盘——这正是「丢行」bug 的另一面。

**F2 · 恢复后模型看到的历史被打碎（P0 正确性 bug）。**
- 每个 delta 一行 → 每行是一个只含文本的 assistant 消息（`log_surface.rs:309-317` `classify_part`）。
- 投影层**只**在「上一条 assistant 消息带未闭合的 ToolUse」时合并
  （`elysper-context/src/memory/session.rs:78-97`），**连续两条纯文本 assistant 行不合并**。
- 实测（一次性探针，已删）：3 行 `{"type":"Model","data":{"type":"text",…}}` 经
  `events_to_messages` → **3 条** assistant 消息（`"Hel"` / `"lo "` / `"world"`）。
- ⇒ 前端看不出来（`elysper-web/src/lib/session-to-messages.ts:510` 的 web 侧投影**会**把文本块
  合并进同一条 assistant 轮次），但**模型看得到**：resume 后它自己的上一轮回答被拆成几百条消息。

**F3 · `request_header` 每 run 写两遍。** server 一行（`run_task.rs:293-316`）+ harness 一行
（`prepare.rs:115-120`），两行都在日志里。

**F4 · 写入口不止 4 个。** `subagent_router.rs` 的 `:181`、`:185`、`:202`、`:217`、`:229`
（子会话的 sink）+ `:250+`（父会话的标记行）。

**F5 · 账本的种子是位置序号。** `run_task.rs:189` 用 `index + 1`——与 fold 一致，但与 sink 的号不一致。
fold 改读行里的 `seq` 后，种子必须用**同一个判定**（§5.5），否则两边再次分叉。

**F6 · `alloc_seq` 与 `append` 不是原子的。** `jsonl.rs:249-251` 的计数器是独立的 `AtomicU64`，
文件锁在 `:254` 才拿——「占号」与「落盘」之间可以被并发写插队，于是**文件顺序 ≠ 序号顺序**。

**F7 · 计数器从「行数」续接，不从「最大序号」续接。** `jsonl.rs:219`
`let next_seq = count_lines(&root.join("events.jsonl"));`——开一个已有日志时，计数器 = 行数。
于是：崩溃在「占号」与「落盘」之间（号被烧掉但行没写）时，重启后的计数器会**重用那个烧掉的号**
→ 日志里出现重复序号。

**F8 · 写点比 controller 多得多。**
- `elysper-server/src/routes/sessions.rs`：`:998`（`append_many`，会话 fork 继承行）、`:1128`、`:1164`、
  `:1505`、`:1513`、`:1651`、`:1821`、`:1829`。
- `elysper-server/src/routes/messages/mod.rs:531-532`（种子历史）。
- `elysper-server/src/session/subagent_router.rs`：`:181`、`:185`、`:202`、`:217`、`:229`。
- `elysper-context/src/memory/typed.rs:145-148`（`repair_session`，**生产调用方**：
  `elysper-server/src/state/app_state/sessions.rs:135`，每次建 controller 都调）与
  `elysper-context/src/memory/session.rs:211`（`SessionMemory::append`，公共 API）。
⇒ 「一个会话一个写者」不能只在 controller 里实现：**写者必须挂在会话本身**，
任何拿到 `(user_id, session_id)` 的代码都要能取到同一个写者。
`elysper-session/src/manager.rs:79` 的 `SessionRegistry::sink()` 已经是这个角色（它按会话缓存 sink ✓），
写者应当与它并列：`SessionRegistry::writer(user_id, session_id) -> Arc<LogWriter>`。

---

## 5. 架构决定

### 5.1 写者接缝下沉到 `elysper-session`（推翻 v2 设计 §4.5）

v2 设计写的是「`log/` 是 harness 私有模块，因为『哪些行是 durable』必须与循环的实现同处一个 crate」。
**本轮推翻这一条**，理由：

- 折叠器（`fold_surface.rs`）本来就在 `elysper-session`，而 provenance 校验必须让「写者取的号」
  和「折叠器算的号」是同一个数。把写者放 harness、折叠器放 session，等于把同一个不变量的两端
  分在两个 crate 里，中间靠约定。
- `CompactionCheckpoint`（在 `elysper-session`）的回调是**同步**的：它必须能在不 await 的情况下
  把压缩行交给写者。写者在 harness 时它够不着，只能另开一条通道——那条通道就是今天会丢行的
  `TypedEventSink`。写者在 session 时，同步回调直接走写者的 spill 通道，整条老路可以删。
- 「哪些行 durable」的**判定**并没有因此跑到别的 crate 去：判定仍在 harness
  （`events/mod.rs::is_durable`），写者只负责把判定结果落盘。

依赖方向不变：`elysper-harness` → `elysper-session` 已存在。

### 5.2 delta 与完整消息的界线（**已定：(i)**）

F1/F2 的根因是同一个：**delta 被当成了 durable 事实**。修法必须先定一件事：
「一条 assistant 消息在日志里长什么样、由谁来写这一行」。

**已决定：走 (i) —— harness 写整条消息，一轮一行。**（2026-10-25，用户拍板）

```text
现状（每 token 两行）              目标（每轮一行）
  Model{text:"Hel"}  ← durable       assistant_message{ role: Assistant,
  Model{text:"lo "}  ← durable                       content: [Text("Hello world"),
  Model{text:"world"} ← durable                                     ToolUse(...)] }
  AssistantChunk{"Hel"} ← typed      （一行；delta 只走事件流，不落盘）
  AssistantChunk{"lo "} ← typed
  AssistantChunk{"world"} ← typed
```

**达成的四条**：
1. 一条 assistant 消息在日志里是**一行**（dsh 的 `assistant/message` 同款）。
2. delta **不落盘**（它是流式碎片，不是事实），但**照旧实时广播**（wire 形状不变，流式渲染不变）。
3. 同一轮的文本与 tool_use 在**同一行**（resume 后模型看到的历史与 live 一致）。
4. 工具结果一行一条，且在下一轮模型调用之前已落盘。

#### (i) 的所有权规则（本轮的硬规则）

| 行 | 谁写 | 走什么 |
|---|---|---|
| **assistant 消息**（文本 + 工具调用，整条一行） | **harness** | `writer.append_event(AssistantMessage)` |
| **工具结果** | **harness** | `writer.append_event(ToolResult)` |
| 结构行（step / usage / request_header） | **harness** | `writer.append_event(...)` |
| 用户输入、反馈、关闭标记、压缩行、fork / 消息编辑 | server | `writer.append(...)` |
| delta（每个文本碎片） | 谁都不写 | 只走事件流 |

**为什么工具结果也归 harness**：flush 与 enqueue 必须在同一个任务里保持顺序。
server 在另一个任务里收到 tool_result 事件才入队的话，harness 在下一次模型调用前的 flush
可能抢在入队之前 → 工具结果没落盘就被模型看见 → 契约破。harness 自己写，顺序由它自己保证。

**因此 `AgentEvent::is_durable()` 的含义要改**：
- `Model(Text)` → **false**（delta；内容进了 assistant 消息行）
- `Model(ToolUse)` → **false**（内容进了 assistant 消息行）
- `Model(ToolResult)` → **false**（harness 直接写日志，server 不再从事件里持久化）
- `Model(_)` / `ModelDone` / `System(_)` → false（与今天一致）
- ⇒ **`is_durable()` 变成「这个事件是不是 host 该持久化的 durable 事实」**，而 model 族全部为 false：
  model 可见的事实由 harness 自己写进日志。
  **连带后果：想持久化模型可见事实，就必须给 agent 装一个写者**（今天不装也能靠事件持久化）。

**flush 规则**：
- harness 在**每次调用模型之前** flush 一次（= step 边界，`loop/mod.rs:243` / `finalize.rs:34` 已有）
  → 「模型看得见的事实已落盘」由此成立（工具结果也在这里被刷掉 ✓）。
- server 在**广播一条 durable 事实之前** flush 一次 → 「看到的都在盘上」由此成立。
  两者刷的是同一个写者。

**web 的连带改动**：`elysper-web/src/lib/session-to-messages.ts` 的 `applyHistoryEntry`（`:524`）
今天只认 `UserInput` / `Model`（`:95-96`），要加一个 `assistant_message` 分支。
**流式渲染不动**（delta 的 wire 形状不变），只有历史投影要改。

**被否掉的 (ii)**（沿用 `Model` 行形状、server 只写完整部分）不做的原因：
模型 resume 后会看到**两条** assistant 消息（文本一条、工具调用一条）——一轮仍被拆开；
而且 server 的持久化层得知道「一轮」这个 harness 的概念。

> 其余按本文档执行。

### 5.3 一个会话一个写者

- `LogWriter` 住在 `crates/elysper-session/src/writer.rs`，包一个 `Arc<dyn SessionSink>` 与一个
  `elysper_core::Clock`。
- **一个会话一个实例**，挂在会话本身：`SessionRegistry::writer(user_id, session_id) -> Arc<LogWriter>`
  （与 `SessionRegistry::sink()` 并列，后者已经按会话缓存 sink ✓）。理由见 F8。
- run 期间把写者交给 agent（`ReActAgent::with_log_writer`）；反馈、关闭标记、子代理标记、
  会话 fork、消息编辑、上下文 crate 的写回也都用它。
- 写者是该会话**唯一**的写者。**任何代码路径都不得再直接 `SessionSink::append`**
  （写者自身除外）——这条门禁是**全仓库**的，因此 `elysper-context` 的两个写点
  （`memory/typed.rs:145-148`、`memory/session.rs:211`）必须一起改，它们的公共 API
  （`repair_session(Arc<dyn SessionSink>)`、`SessionMemory::new(sink)`）随之变成拿写者。
- **计数器必须从日志里已有的最大序号续接，不能从行数续接**（F7）：开一个已有日志时，
  下一个号 = `max(日志里出现过的最大 seq)`（旧日志没有 `seq` 字段时退回行数）。
  否则崩溃烧掉的号会在重启后被重用，日志里出现重复序号，折叠器的 provenance 校验随之失效。

### 5.4 序号唯一真源：行里的 `seq`

- 写者给**每一行**插入 `seq` 字段（值来自 `sink.alloc_seq()`）。
- 折叠器（`log_surface.rs`）读行里的 `seq`，不数位置。
- 账本的种子（`run_task.rs:189`）与**账本后续的每一次喂入**都用同一个判定（§5.5）。
- 这条让「文件顺序 == 序号顺序」从**前提**变成**结果**：并发写的行不再破坏 provenance。

### 5.5 折叠器的新读法（兼容旧日志）

序号规则改为**整份日志一次性决定**：

- 若日志里**每一个**事件行都带 `seq` 字段 → 用行里的 `seq`。
- 否则（有任何一行没有 `seq`，即旧日志）→ 全部按位置计数，行为与今天**逐位相同**。

不做「逐行混用」：一半带 `seq` 一半不带会产生重复序号，比位置计数更糟。
判定必须对**整份日志**做一次前置扫描（含 metadata header 的跳过规则）。

**这个判定必须是同一个函数的两个调用点**，不许各写一份：
折叠器（`fold_lenient` / `try_fold_log_surface`）、账本种子、账本的喂入、计数器续接，
四者共用「这份日志是不是全带 `seq`」这一个判断。否则 §5.4 修好的分叉会从别处重新长出来
（这正是混合日志——上线前后的会话都在——最容易出问题的地方）。

### 5.6 批量落盘与 flush 边界

- 写者内部是有界队列 + 一个 spill 缓冲；`flush()` 把两者排空、**按 `seq` 排序**、一次
  `sink.append_many(...)` 落盘 → 一次 `append_many` 覆盖整批。
  （注意：`append_many` 的默认实现是逐行 `append`——「一次 fsync」只对覆写了它的 sink 成立，
  见 §4 表格最后一行。门禁 I7 因此断言的是「写者每次 flush 调一次 `append_many`」，
  而「一次 fsync」要由 sink 自己保证。）
- 按 `seq` 排序是必要的：多个生产者（server 的 run task 与 harness 的 loop task）先后取号、
  先后入队，排序把文件顺序还原成序号顺序。
- **flush 的时机由调用方决定**，写者自己不带节奏：
  - harness：每次调用模型之前（= step 边界，`loop/mod.rs:243` / `finalize.rs:34` 已有）
    → 契约「模型看得见的事实已落盘」由此成立。
  - server：广播一条 durable 事实之前 flush 一次 → 「看到的都在盘上」由此成立。
- 成本核算（一个 step = 1 次模型调用 + 若干工具调用）：
  - 今天：模型可见行 N 次 fsync（N = token 数）+ 结构行每 step 4 次 + chunk 行（≤64）次。
  - 改后：模型可见事实每轮 1 行，结构行搭车 → **每 step 约 1–2 次 fsync**。

### 5.7 同步回调：spill，绝不丢行

- 写者提供同步取号 + 入队：队列满时进 spill 缓冲。spill 无界但只在「同步生产者 + 队列满」时
  增长，下一次 flush 一定排空。
- 供 `CompactionCheckpoint` 的两个同步回调（`record_callback` / `lifecycle_callback`）使用。
- 异步路径用 `append(value).await`：队列满时**背压等待**，不丢。

### 5.8 广播与落盘的关系（不变）

- 今天：durable 行 → fsync → 广播。
- 改后：durable 行 → 取号入队 → **flush** → 广播。
- 用户可见契约不变：「你看到的 durable 事实一定在盘上」。
- **例外且明确**：delta 不再是 durable 事实（§5.2），它们照旧实时广播，但不落盘、不受此契约约束。

### 5.9 删掉的东西（不留 shim）

| 删什么 | 在哪 | 连带 |
|---|---|---|
| `TypedEventSink` / `TypedEventReceiver`（**通道**，不是类型） | `crates/elysper-session/src/typed_event_sink.rs` | 同文件的 `TypedEventRecord`（`:68`）与 `CompactionRecordView`（`:111`）**保留**——后者是 server 的公共 API（`agent_run_config.rs:28,41,49`），且经 facade 的 glob 导出 |
| facade 的 `TypedEventSink` re-export 与文档引用 | `crates/elysper/src/prelude.rs:123`、`src/lib.rs:57,136,251`、`src/session.rs:7-8` | `MINIMAL.md`、`README.md:258,270,275`、`SEAMS.md:21` 同步改写 |
| `EventSink.typed_sink` 字段、`has_typed()` | `crates/elysper-harness/src/agent/sink.rs:31,54` | `EventSink::new` 的参数从 3 个变 2 个 → **11 个调用点、7 个文件、4 个 crate**：`elysper-strategy-cot`、`elysper-strategy-best-of-n`、`elysper-strategy-plan-execute`（含 example）、`elysper-harness` 自身；strategy crate 还在调 `sink.emit_typed(...)`（`cot:138-141`、`best-of-n:288-291`），路由随之变 |
| `EnvCaps.typed_sink` 字段 | `crates/elysper-harness/src/agent/caps.rs:58,106` | — |
| `ReActAgent::with_typed_event_sink` | `crates/elysper-harness/src/agent/re_act/agent.rs:214` | `crates/elysper-harness/examples/assemble_with_builder.rs:33,76,101` 与 `crates/elysper/examples/assemble_from_zero.rs:191-199` 同步改 |
| harness 的 `log/` 模块（`mod.rs` / `queue.rs` / `tests.rs`） | 搬到 session crate | — |
| `Durability` 枚举 | `crates/elysper-harness/src/log/mod.rs:36-56` | 四档全无生产使用者；`Batched` 从未构造 → `flush_if_batched` 的 `Batched` 分支（`:209-212`）随之删 |
| `LogWriter::durability()` 死 getter | `crates/elysper-harness/src/log/mod.rs:115` | — |
| server 与 context crate 对 `SessionSink::append` 的直接调用 | 见 F8 | 全部改走写者 |
| 每 delta 一条 `AssistantChunk` 行 | `crates/elysper-harness/src/agent/re_act/stream.rs:39-66` | 整条 assistant 消息行承载同样的信息（delta 拼起来就是文本），逐 chunk 边界不值得留 |

**不删**：`assemble_chunks`（`crates/elysper-session/src/surface.rs:207`）——它是公共 re-export
（`session/src/lib.rs:130`，facade glob 再导出一次）、有 intra-doc 链接
（`events.rs:153`、`surface.rs:12`，`make doc-check` 用 `-D warnings`，删了直接红）、
还有一个 harness 测试在用（`steering_hooks.rs:118,186`）。留着无害。

`Durability` 一并删掉的理由：它今天没有任何生产构造点，而「什么时候 flush」本来就该由调用方决定
（harness 在 step 边界、server 在广播前）。`elysper::harness::Durability` 这个路径随之消失
（破坏性，已批准）。

---

## 6. API 草案

`crates/elysper-session/src/writer.rs`：

```rust
/// 一个会话的唯一写者。
///
/// 取号来自 sink 的计数器（`SessionSink::alloc_seq`），落盘按批走
/// `SessionSink::append_many`——一次 flush 一次 append_many。
pub struct LogWriter { /* sink, clock, ledger, queue, spill */ }

impl LogWriter {
    /// 建一个写者。`sink` 是这个会话的持久化后端；`clock` 给每一行盖时间戳
    /// （`elysper_core::Clock`——库代码不得直接 `chrono::Utc::now()`）。
    pub fn new(sink: Arc<dyn SessionSink>, clock: Arc<dyn Clock>) -> Self;

    /// 挂上这个会话的账本（压缩检查点用它定位被遮蔽的行）。
    /// 账本与折叠器必须看到同样的 surface 行、同样的顺序。
    pub fn with_ledger(self, ledger: Arc<SurfaceLedger>) -> Self;

    /// 取号 + 入队。队列满时背压等待，绝不丢行。返回这一行拿到的序号。
    pub async fn append(&self, value: Value) -> Result<u64, SessionError>;

    /// 取号 + 入队一个 typed 会话事件（覆盖事件自带的 `seq: 0` 与 `ts`）。
    pub async fn append_event(&self, event: &SessionEvent) -> Result<u64, SessionError>;

    /// 同步取号 + 入队；队列满时进 spill，下一次 flush 一定排空。绝不丢行。
    pub fn append_nowait(&self, value: Value) -> u64;

    /// 同步取号 + 入队一个 typed 事件（压缩检查点的同步回调用）。
    pub fn append_event_nowait(&self, event: &SessionEvent) -> u64;

    /// 落盘承诺。把队列与 spill 排空、按序号排序、一次 `append_many` 写下去。
    pub async fn flush(&self) -> Result<(), SessionError>;

    /// 崩溃后补齐：只追加，绝不截断。补齐行的序号由写者发（不再由调用方算）。
    pub async fn repair(&self, closers: Vec<SessionEvent>) -> Result<(), SessionError>;
}
```

两种行的盖章方式不同，实现时别混：
- **legacy 行**（`{"type":"Model",…}`）：顶层插 `"seq"` 与 `"ts"`（今天 server 在 `run_log.rs:84-90` 这么做）。
- **typed 行**（`#[serde(tag = "type")]`，`events.rs:119-135`）：`seq` 与 `ts` 是 `type` 的**兄弟字段**，
  事件自带的 `seq: 0` 必须被写者覆盖成真号。

`CompactionCheckpoint` 的签名随之改：

```rust
impl CompactionCheckpoint {
    /// 建一个检查点：写者负责落盘，账本负责定位被遮蔽的行。
    pub fn new(writer: Arc<LogWriter>, ledger: Arc<SurfaceLedger>) -> Arc<Self>;
}
```

两个回调（`record_callback` / `lifecycle_callback`）是同步的 → 走 `append_event_nowait` ✓。

`elysper-context` 的两个公共 API 随之改：
- `repair_session(sink: Arc<dyn SessionSink>)` → 拿写者（它自己算的 closers 序号
  `repair.rs:201-240` 的 `state.last_seq + 1` 是**第三个序号来源**，必须去掉）。
- `SessionMemory::new(sink)` → 拿写者（`memory/session.rs:211`）。

按 §5.2 的 (i)：`AgentEvent::is_durable()` 对 model 族全部返回 false——模型可见的事实由 harness
直接写日志，host 不再从事件里持久化它们。`AgentEvent` 枚举本身不变。

---

## 7. 改动清单

### `elysper-session`
- 新增 `src/writer.rs`（写者 + 队列/spill + 账本喂入 + 计数器续接），`src/writer/tests.rs`。
- 删 `src/typed_event_sink.rs` 里的**通道**（`TypedEventSink` / `TypedEventReceiver`），
  保留 `TypedEventRecord` 与 `CompactionRecordView`（公共 API）。
- `src/lib.rs`：去掉通道的 re-export，加 `LogWriter` → **要抬 `scripts/paradigm-surface` 的
  session 基线（13 → 14），同一个提交里改**。
- `src/manager.rs`：`SessionRegistry::writer(user_id, session_id) -> Arc<LogWriter>`（与 `sink()` 并列，
  按会话缓存）。
- `src/compaction_checkpoint.rs`：改持 `Arc<LogWriter>`；同步回调走 `append_event_nowait`；
  `resolve_pending()` 语义不变。
- `src/log_surface.rs`：**序号读法改**（§5.5）；`fold_lenient` / `try_fold_log_surface` / 账本种子 /
  账本喂入共用**同一个**「这份日志全带 seq 吗」的判定函数。
- `src/events.rs`：typed 行的 `seq` / `ts` 由写者盖章（事件自带的 `seq: 0` 被覆盖）。
- `examples/compaction_checkpoint.rs`：改用写者。

### `elysper-harness`
- 删 `src/log/`（`mod.rs` / `queue.rs` / `tests.rs`）；`src/lib.rs` 的 `pub use log::{…}` 去掉。
- `src/agent/sink.rs`：`EventSink` 只留 `log_writer: Option<Arc<LogWriter>>`；删 `typed_sink` 与
  `has_typed()`；`EventSink::new` 少一个参数 → **11 个调用点、4 个 crate**（见 §5.9）。
- `src/agent/caps.rs`：`EnvCaps` 删 `typed_sink`。
- `src/agent/re_act/agent.rs`：删 `with_typed_event_sink`；保留 `with_log_writer`（类型换成 session 的）。
- `src/agent/re_act/stream.rs`：**delta 不再逐个落盘**（`stream.rs:39-66` 那段
  `emit_typed(AssistantChunk)` 删掉）；模型调用结束时，harness 把组装好的整条 assistant 消息
  作为**一行**写进日志（`SessionEvent::AssistantMessage`，含文本与工具调用）。
- `src/agent/re_act/loop/tools/commit.rs`：工具结果行也由 harness 写（§5.2 的所有权规则；
  写者与 flush 在同一个任务里，顺序才成立）。
- `src/agent/re_act/loop/mod.rs`：`flush_log()` 改调 session 的写者（时机不变：调用模型之前）。
- `src/agent/re_act/tests/steering_hooks.rs:118,186`：`assemble_chunks` 保留 → 测试不动 ✓。
- `examples/assemble_with_builder.rs`：改用写者。

### `elysper-server`
- `session/controller/run_log.rs`：`RunLog` 改持 `Arc<LogWriter>`。
- `session/controller/dispatch.rs:320`、`inner.rs:166`、`subagent_router.rs:181,185,202,217,229`、
  `routes/sessions.rs:998,1128,1164,1505,1513,1651,1821,1829`、`routes/messages/mod.rs:531-532`：
  全部改走对应会话的写者（**会话 fork 的继承行要重新盖章**，否则子会话里留着父会话的号）。
- `session/controller/run_task.rs`：建/取写者 → 装到 agent 上（`with_log_writer`）→ 删 typed 通道的
  创建与末尾排空 → 删 `config.typed_event_sink`（`agent_run_config.rs:50`）；
  账本种子改用 §5.5 的判定。
- `session/controller/persist.rs`：模型可见的 durable 事实入队后 **flush 再广播**（语义不变）；
  按 §5.2 的结论决定 delta 是否落盘。
  按 §5.2 的 (i)：server **不再持久化任何 `Model(...)` 事件**（模型可见的事实由 harness 直接写日志），
  这条路径只剩「广播」；server 自己写的行是 host 自己的事实——用户输入、反馈、关闭标记、压缩行。
- `session/subagent_router.rs:212-218`：按 §5.2 的 (i)，子会话的 assistant 轮次由子 harness 自己写，
  路由器不再复制子 agent 的模型事件（只复制它自己写的标记行）。
- `state/app_state/sessions.rs:135`：`repair_session` 改拿写者。

### `elysper-context`
- `src/memory/typed.rs:145-148`：`repair_session` 改拿写者（不再自己算 closers 的序号）。
- `src/memory/session.rs:211`：`SessionMemory::append` 改走写者；`SessionMemory::new(sink)` 的
  公共签名随之改。

### `elysper-strategy-cot` / `elysper-strategy-best-of-n` / `elysper-strategy-plan-execute`
- `EventSink::new` 的参数变化 → 同步改（`cot/src/lib.rs`、`best-of-n/src/lib.rs`、
  `plan-execute/src/strategy.rs`、各自的 example）；它们调 `sink.emit_typed(...)` 的地方随之改。

### `elysper`（facade）
- `src/prelude.rs` / `src/lib.rs` / `src/session.rs`：去掉通道与 `Durability` 的 re-export 与文档引用；
  **写者的导出路径定为 `elysper::session::LogWriter`**（harness 模块没了；`elysper::harness::LogWriter`
  这个路径消失）。
- `examples/assemble_from_zero.rs`、`tests/feature_gating.rs`：更新。
- `MINIMAL.md`（**被编译的 doctest**）、`README.md:258,270,275`、`SEAMS.md:21`：同步改写。

### `elysper-web`
- `src/lib/session-to-messages.ts`：`applyHistoryEntry`（`:524`）要认 `assistant_message` 行
  （今天只认 `UserInput` / `Model`，见 `:95-96`）；文本合并逻辑不变。

---

## 8. 不变量（每条都要有测试）

| # | 不变量 | 断言方式 |
|---|---|---|
| I1 | 一个会话的所有行都经同一个写者 | **全仓库** grep 门禁：除 `LogWriter` 外无 `SessionSink::append` / `append_many` 调用点（覆盖 server、context、strategy crates） |
| I2 | 每一行都带 `seq`，且同一会话内序号唯一、单调 | 写完后读回：所有行的 `seq` 集合 == `{1..n}`（fork 的子会话用自己的号，不继承父会话的） |
| I3 | 折叠器用行里的 `seq` | 并发乱序写（feedback 行插在 run 的行中间）后 `try_fold_log_surface` 仍通过，压缩引用可解析 |
| I4 | 旧日志行为不变 | 没有 `seq` 字段的日志，折叠结果与今天逐位相同（回归测试对比旧断言） |
| I5 | 混合日志不产生重复序号 | 一半带 `seq` 一半不带的日志 → 走位置计数，且不与带 `seq` 的行混用 |
| I6 | 队列满不丢行 | 容量 1 的队列 + 数十行 → 读回的行数 == 写入的行数，无洞 |
| I7 | 同步路径不丢行 | `append_nowait` 在队列满时全部进 spill，flush 后全部落盘 |
| I8 | 一次 flush 一次 `append_many` | 计数 sink：N 行 → `append_many` 被调用 1 次（**不是** `append` N 次——注意 `append_many` 的默认实现是逐行 `append`） |
| I9 | 广播前已落盘 | 广播的 durable 行，在 mock sink 里已经能读到（顺序断言） |
| I10 | 压缩行不会静默消失 | 队列塞满时压缩行仍在日志里，且其 `source_event_seqs` 可解析 |
| I11 | 账本与折叠器看到同一批行 | 账本的 slot 数与折叠出的 surface 长度一致，且顺序一致 |
| **I12** | **一条 assistant 消息 = 一行** | 一个多 token 的回复 → 日志里恰好一行 assistant 事实 |
| **I13** | **resume 后模型看到一条 assistant 消息** | 同上日志 → `events_to_messages` 返回 **1** 条 assistant 消息，内容等于完整回复（今天返回 N 条） |
| **I14** | **同轮的文本与 tool_use 在同一行** | 一个既说文本又调工具的回复 → 日志里一行含两者；resume 后是一条 assistant 消息 |
| **I15** | **重启后不重用序号** | 写一半日志 → 模拟重启（重新开 sink）→ 下一个号 > 日志里已有的最大号 |

---

## 9. 验证

```bash
cargo build --workspace
cargo test -p elysper-session --lib
cargo test -p elysper-harness --lib
cargo test -p elysper-server --lib
cargo test -p elysper-context --lib
cargo +nightly fmt --all
make ci                      # fmt-check + lint-rust + doc-check + test-guides + 各类棘轮
make check-harness-docs      # 注释预算必须 ≤ 56（当前 53）
make check-public-api-runtime
make check-clock
make check-pub-surface-strict
```

针对新行为的测试（不是 wiring 测试）：

1. **I2/I3**：并发写——一个 run 的行与一条 feedback 行交错落盘，`try_fold_log_surface` 通过，
   压缩记录的 `source_event_seqs` 全部可解析（今天会失败的那条）。
2. **I4**：喂一份没有 `seq` 字段的旧日志，断言折叠出的 surface 与旧实现一致。
3. **I5**：混合日志（前半旧、后半新）→ 走位置计数，无重复序号。
4. **I6/I7**：容量 1 的队列；断言无洞、序号连续；spill 里的行最终全部落盘。
5. **I8**：计数 sink 断言 N 行只触发一次 `append_many`。
6. **I9**：mock sink + 广播钩子，断言广播发生的时刻该行已在 sink 里。
7. **I11**：一个含压缩的 run → 账本的 slot 数与折叠出的 surface 长度一致。
8. **I12/I13/I14**：一个多 token 的回复 → 日志里一行 assistant 事实；`events_to_messages` 返回 1 条
   assistant 消息且内容完整；带 tool_use 的回复同样只占一行。
9. **delta 降级**：断言一个多 chunk 的回复在日志里留下**一行** assistant 事实，且不再有 N 行
   `assistant_chunk`；同时断言 harness 的事件流仍然逐块发出 delta（前端流式渲染不受影响）。
10. **F3**：断言一个 run 只留下一行 `request_header`。
11. **I15**：写一半日志 → 重新开 sink → 下一个号大于日志里的最大号。

---

## 10. 风险

| 风险 | 缓解 |
|---|---|
| 折叠器改序号读法影响面大（`log_surface.rs` 是核心文件） | I4/I5 的旧日志与混合日志回归测试先落地；改动只在「取序号」这一处 |
| web 的历史投影要改（认 `assistant_message` 行） | 改动局限在 `applyHistoryEntry` 的一个分支；流式渲染不动 |
| harness 直接写日志 | 写者只有一个（`SessionRegistry::writer`），账本由写者喂 → 账本与折叠器看到同一批行（I11） |
| delta 不落盘 = 逐 chunk 的审计链消失 | 整条 assistant 消息行承载同样的信息（delta 拼起来就是文本）；逐 chunk 边界不值得留（§2 非目标 6） |
| 并发写的顺序不再等于序号顺序 | I2/I3：折叠器读 `seq`，顺序不再是前提 |
| 计数器续接改成「取最大 seq」需要读文件尾 | 开一个已有日志时多做一次尾读；旧日志退回行数（F7 的兼容分支） |
| 公共 API 破坏面比预期大 | `EventSink::new` 的参数变化波及 4 个 crate；`TypedEventSink` 的删除波及 facade、MINIMAL.md、README、SEAMS、三个 example、三个 strategy crate——**一次切干净，不留 shim** |
| `elysper-session` 的 `pub use` 顶格（13/13） | 同一个提交里抬 `scripts/paradigm-surface` 基线 |
| server 多节点（Redis 模式）下两个节点同时写同一会话 | 会话有租约（`cluster/redis_bus.rs`），同一时刻只有一个持有者；`seq` 由 sink 的计数器发，跨节点不成立 → **多节点下同一会话不得并发写**，这是既有约束，不是本轮引入 |
| 「每 step 一次 fsync」没做 | 明确列为非目标；下一轮做（需要新增一个 step 边界事件 + 改 durable 行的广播时机） |

---

## 11. 留给实现者的部分

- `LogWriter` 内部字段名与模块切分（`writer.rs` 一个文件 vs `writer/` + `queue.rs`）。
- spill 缓冲用 `Vec` 还是 `VecDeque`。
- 「这份日志全带 seq 吗」的判定函数放在哪、叫什么（四个调用点共用）。
- `RunLog` 与写者的关系：`RunLog` 持写者，还是 `RunLog` 直接变成写者的一层薄封装。
- 错误类型：写者的失败用 `SessionError` 的哪个变体，还是新加一个。
- 计数器续接时「取最大 seq」的具体读法（读文件尾 vs 扫全文件）。
- harness 写 assistant 消息行与工具结果行的具体调用点。

---

## 12. 与已有文档的关系

- 本文档**推翻** `2026-10-23-harness-v2-design.md` §4.5 的一条决定（原文：「`log/` 是 harness 私有模块，
  不是新 crate」）。理由见 §5.1。v2 文档的 §12（我代做的决定）里对应那一行已同步标注。
- v2 设计的 §4.2（durability 三动词）、§4.3（seq 归属）、§4.4（原子切换边界）在本轮落地；
  §5（插件）、§4.1（循环从 fold 推导）、§6（挂起恢复）仍待后续。
- v2 设计的 §7（精简）里「删除 14 个零外部引用的 `pub` 项」已在 foundation 轮完成，
  本文档追加删 `TypedEventSink`（通道）/ `Durability` / `LogWriter::durability()`。
