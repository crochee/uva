# 写者唯一化 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 一个会话一个写者、序号只有行里的 `seq` 一个来源、一次 flush 一次 `append_many`；assistant 一轮在日志里是一行，模型 resume 后看到的是一条完整的 assistant 消息。

**Architecture:** 写者接缝从 `elysper-harness` 下沉到 `elysper-session`（折叠器本来就在那里，写者与折叠器必须能共用同一个序号判定）。server 通过 `SessionRegistry::writer(user_id, session_id)` 拿到该会话唯一的写者，harness 通过 `with_log_writer` 拿到同一个写者；两者都只经它落盘。折叠器改读行里的 `seq`（旧日志没有该字段时按位置计数，行为不变）。harness 写模型可见的事实（整条 assistant 消息一行、工具结果一行），delta 只走事件流不落盘。

**Tech Stack:** Rust 1.98、edition 2024、`futures::channel::mpsc` + `futures::lock::Mutex`（runtime-free）、`thiserror`、`serde_json`。不新增依赖。不新增 cargo feature。

**Spec:** `docs/design/2026-10-24-writer-unification-design.md`（§5.2 已定 (i)；§8 的 15 条不变量；§9 的 11 条测试）。本计划是那份设计的落地。

## Global Constraints

- 库 crate 只用 `thiserror`；`elysper-server` 是 app crate，用 `anyhow`。
- 依赖版本一律 `major.minor`；新依赖只能来自根 `Cargo.toml` 的 `[workspace.dependencies]`，crate 内写 `dep = { workspace = true }`。
- `max_width = 80`；禁通配符导入；`elysper-harness` 生产代码无函数 >100 行、无嵌套 >4 层。
- 库代码不得直接 `chrono::Utc::now()`——墙钟走 `elysper_core::Clock`。
- 库 crate 的公开 API 不得出现具体运行时类型（`tokio::` 等）——写者的内部锁用 `futures::lock::Mutex`。
- `elysper-harness` 不得长出 `[features]` 表，不得依赖 tokio。
- `scripts/harness-docs/baseline` = 56（当前 53）：私有项的 `///` 行数只许降不许升。
- `scripts/paradigm-surface/baselines/crates/elysper-session` = 13（当前 13）：新增 `pub use` 行必须在**同一个提交**里抬基线。
- `MINIMAL.md` 是被编译的 doctest（`make test-guides` ⊂ `make ci`）：改了公开 API 就必须同提交改它。
- 破坏性 API 一次切干净，不留 shim、不留别名。
- 每个任务结束时：工作区能编译、`cargo +nightly fmt --all` 干净、该 crate 的测试绿。
- 提交信息用英文祈使句（与仓库既有历史一致）。

## Review Focus

规格暗示、但任何单个任务的测试都不会自然覆盖的五类失败模式。每条都配了归属任务与测试。

1. **队列塞满时的压缩行**——压缩检查点的回调是**同步**的，不能 await；一个合理的实现会把「塞满」处理成「丢掉」。期望：压缩行进 spill，下一次 flush 一定落盘，且它的 `source_event_seqs` 仍可解析。→ Task 3 的 `compaction_row_survives_a_full_queue`。
2. **崩溃在占号与落盘之间**——号被烧掉但行没写；重启后计数器若从「行数」续接就会重用那个号，日志里出现重复序号，折叠器的 provenance 随之失效。期望：下一个号 > 日志里已有的最大号。→ Task 2 的 `restart_does_not_reuse_a_burned_ordinal`。
3. **混合日志**（上线前的老行没有 `seq`，上线后的新行有）——一个合理的实现会逐行混用两套编号，产生重复序号。期望：整份日志一次性判定，全带 `seq` 才用 `seq`，否则全部按位置。→ Task 1 的 `a_mixed_log_falls_back_to_positional_ordinals`。
4. **会话 fork 后的子会话**——继承行带着父会话的号；一个合理的实现会原样复制。期望：子会话的序号自成一套，从 1 开始。→ Task 5 的 `a_forked_session_restamps_its_inherited_rows`。
5. **resume 后模型看到的历史**——一个合理的实现会把一轮拆成多行。期望：一次多 token 的回复在日志里是一行，resume 后是一条 assistant 消息，内容完整。→ Task 6 的 `a_streamed_reply_is_one_row_and_one_message_after_resume`。

---

## File Structure

**新建：**

- `crates/elysper-session/src/writer/mod.rs` — `LogWriter`：取号、入队（背压）、spill（同步路径）、批量 flush、账本喂入、计数器续接。
- `crates/elysper-session/src/writer/tests.rs` — 写者的单元测试（`#[cfg(test)] mod tests;` 声明在 `writer/mod.rs`）。
- `crates/elysper-harness/src/agent/re_act/tests/lifecycle/one_row_per_turn.rs` — 「一轮一行」的生命周期测试（并入 `lifecycle/mod.rs` 的 `mod` 列表）。

**删除：**

- `crates/elysper-harness/src/log/mod.rs`、`log/queue.rs`、`log/tests.rs`（写者下沉到 session crate）。
- `crates/elysper-session/src/typed_event_sink.rs` 里的**通道**（`TypedEventSink` / `TypedEventReceiver`）；同文件的 `TypedEventRecord` 与 `CompactionRecordView` **保留**（公共 API）。

**修改（生产代码）：**

- `crates/elysper-session/src/log_surface.rs` — 折叠器读行里的 `seq`；新增共用的判定函数 `log_is_stamped`；账本的 `record` 不变。
- `crates/elysper-session/src/manager.rs` — `SessionRegistry::writer(user_id, session_id) -> Arc<LogWriter>`。
- `crates/elysper-session/src/compaction_checkpoint.rs` — 改持 `Arc<LogWriter>`；两个同步回调走 `append_event_nowait`。
- `crates/elysper-session/src/jsonl.rs`、`in_memory.rs` — 计数器从「日志里已有的最大 `seq`」续接（旧的按行数）。
- `crates/elysper-session/src/lib.rs` — 去掉通道的 re-export，加 `LogWriter`（→ 抬 paradigm-surface 基线）。
- `crates/elysper-harness/src/agent/sink.rs` — `EventSink` 只留写者；`EventSink::new` 从 3 参变 2 参。
- `crates/elysper-harness/src/agent/caps.rs` — `EnvCaps` 删 `typed_sink`。
- `crates/elysper-harness/src/agent/re_act/agent.rs` — 删 `with_typed_event_sink`；`with_log_writer` 换成 session 的类型。
- `crates/elysper-harness/src/agent/re_act/stream.rs` — delta 不再逐个落盘；模型调用结束时写整条 assistant 消息一行。
- `crates/elysper-harness/src/agent/re_act/loop/tools/commit.rs` — 工具结果行由 harness 写。
- `crates/elysper-harness/src/agent/re_act/loop/mod.rs`、`finalize.rs` — `flush_log()` 调 session 的写者。
- `crates/elysper-harness/src/events/mod.rs` — `AgentEvent::is_durable()`：model 族全部 false。
- `crates/elysper-harness/src/lib.rs` — 去掉 `pub use log::{…}`。
- `crates/elysper-harness/src/agent/re_act/tests/lifecycle/log_writer.rs` — 写者测试改用 session 的类型。
- `crates/elysper-harness/examples/assemble_with_builder.rs` — 改用写者。
- `crates/elysper-strategy-cot/src/lib.rs`、`elysper-strategy-best-of-n/src/lib.rs`、`elysper-strategy-plan-execute/src/strategy.rs`（+ 各自 example）— `EventSink::new` 的调用点与 `emit_typed` 的路由。
- `crates/elysper-server/src/session/controller/{run_log,run_task,persist,dispatch,inner}.rs`、`session/subagent_router.rs`、`routes/sessions.rs`、`routes/messages/mod.rs` — 全部改走写者。
- `crates/elysper-server/src/agent_run_config.rs` — 删 `typed_event_sink` 字段。
- `crates/elysper-context/src/memory/{typed,session}.rs` — 两个写点改拿写者。
- `crates/elysper/src/{prelude,lib,session}.rs` — re-export 与文档。
- `elysper-web/src/lib/session-to-messages.ts` — 历史投影认 `assistant_message` 行。

**修改（文档 / 教程）：**

- `MINIMAL.md`、`README.md`、`SEAMS.md`、`crates/elysper/examples/assemble_from_zero.rs`、
  `crates/elysper-harness/examples/assemble_with_builder.rs`、`crates/elysper-session/examples/compaction_checkpoint.rs`、
  `crates/elysper/tests/feature_gating.rs`、`crates/elysper-harness/src/agent/strategy/tests.rs`。

---

### Task 1: 折叠器与账本用行里的 `seq`

**Files:**
- Modify: `crates/elysper-session/src/log_surface.rs`
- Test: `crates/elysper-session/src/log_surface.rs`（内联测试块当前 126 行，尚有余量）

**Interfaces:**
- Consumes: 无（起点任务）。
- Produces:
  ```rust
  // crates/elysper-session/src/log_surface.rs
  /// 这份日志里的每一个事件行是否都带 `seq` 字段。
  ///
  /// 折叠器、账本、计数器续接共用这一个判断：逐行混用两套编号
  /// 会产生重复序号，比全部按位置计数更糟。
  pub fn log_is_stamped(rows: &[Value]) -> bool;
  ```
  `fold_lenient` 与 `try_fold_log_surface` 改为：整份日志带 `seq` 时用行里的 `seq`，
  否则按位置计数（与今天逐位相同）。

- [ ] **Step 1: 写失败的测试**

在 `crates/elysper-session/src/log_surface.rs` 的内联测试块里加：

```rust
#[test]
fn a_stamped_log_uses_the_rows_own_ordinals() {
    let rows = vec![
        json!({"type": "Model", "seq": 7, "data": {"type": "text", "text": "a"}}),
        json!({"type": "Model", "seq": 9, "data": {"type": "text", "text": "b"}}),
    ];
    let folded = fold_log_surface(&rows);
    assert_eq!(folded.surface_seqs, vec![7, 9]);
}

#[test]
fn an_unstamped_log_keeps_positional_ordinals() {
    let rows = vec![
        json!({"type": "Model", "data": {"type": "text", "text": "a"}}),
        json!({"type": "Model", "data": {"type": "text", "text": "b"}}),
    ];
    let folded = fold_log_surface(&rows);
    assert_eq!(folded.surface_seqs, vec![1, 2]);
}

#[test]
fn a_mixed_log_falls_back_to_positional_ordinals() {
    let rows = vec![
        json!({"type": "Model", "seq": 7, "data": {"type": "text", "text": "a"}}),
        json!({"type": "Model", "data": {"type": "text", "text": "b"}}),
    ];
    let folded = fold_log_surface(&rows);
    assert_eq!(folded.surface_seqs, vec![1, 2], "no mixing: one log, one numbering");
}
```

`fold_log_surface` 的 `surface_seqs` 字段名与类型以 `crates/elysper-session/src/surface.rs` 里
`FoldedSurface` 的现状为准（`surface_seqs: Vec<u64>`，与 `messages` 平行）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p elysper-session --lib log_surface -- --nocapture`
Expected: FAIL —— `log_is_stamped` 不存在，且 `a_stamped_log_uses_the_rows_own_ordinals`
拿到的是 `[1, 2]` 而不是 `[7, 9]`。

- [ ] **Step 3: 实现判定函数与折叠器改动**

在 `crates/elysper-session/src/log_surface.rs`：

```rust
/// 这份日志里的每一个事件行是否都带 `seq` 字段。
pub fn log_is_stamped(rows: &[Value]) -> bool {
    rows.iter()
        .filter(|row| !crate::jsonl::is_metadata_header_row(row))
        .all(|row| row.get("seq").and_then(Value::as_u64).is_some())
}
```

折叠器里把 `let mut seq: u64 = 0; ... seq += 1;` 的位置计数换成：
先 `let stamped = log_is_stamped(rows);`，然后每行的序号取
`if stamped { row.get("seq").and_then(Value::as_u64).unwrap_or(positional) } else { positional }`
（`positional` 保持今天的「逐事件行递增、跳过 metadata header」语义）。

`try_fold_log_surface`（严格折叠）走同一套逻辑。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p elysper-session --lib`
Expected: PASS（新增三条 + 既有全绿）。

- [ ] **Step 5: 提交**

```bash
git add crates/elysper-session/src/log_surface.rs
git commit -m "feat(session): the fold reads each row's own ordinal"
```

---

### Task 2: `LogWriter`——写者接缝搬到 `elysper-session`

**Files:**
- Create: `crates/elysper-session/src/writer/mod.rs`
- Create: `crates/elysper-session/src/writer/tests.rs`
- Modify: `crates/elysper-session/src/lib.rs`（`mod writer;` + `pub use writer::LogWriter;` → 抬 paradigm-surface 基线）
- Modify: `crates/elysper-session/src/jsonl.rs`、`in_memory.rs`（计数器续接）
- Modify: `scripts/paradigm-surface/baselines/crates/elysper-session`（13 → 14，同提交）

**Interfaces:**
- Consumes: `elysper_session::{SessionSink, SessionError, SessionEvent, SurfaceLedger, log_surface::log_is_stamped}`、`elysper_core::Clock`。
- Produces:
  ```rust
  // crates/elysper-session/src/writer/mod.rs
  /// 一个会话的唯一写者。
  ///
  /// 取号来自 sink 的计数器（`SessionSink::alloc_seq`），落盘按批走
  /// `SessionSink::append_many`——一次 flush 一次 append_many。
  pub struct LogWriter { /* sink, clock, ledger, queue, spill */ }

  impl LogWriter {
      pub fn new(sink: Arc<dyn SessionSink>, clock: Arc<dyn Clock>) -> Self;
      /// 同上，但队列容量由调用方给（测试用来逼出背压路径）。
      pub fn with_capacity(sink: Arc<dyn SessionSink>, clock: Arc<dyn Clock>, capacity: usize) -> Self;
      /// 挂上这个会话的账本（压缩检查点用它定位被遮蔽的行）。
      pub fn with_ledger(self, ledger: Arc<SurfaceLedger>) -> Self;
      /// 把历史行喂进账本（resume 用）——与折叠器同一套序号判定。
      pub fn seed_ledger(&self, rows: &[Value]);
      /// 取号 + 入队。队列满时背压等待，绝不丢行。返回这一行拿到的序号。
      pub async fn append(&self, value: Value) -> Result<u64, SessionError>;
      /// 取号 + 入队一个 typed 会话事件（覆盖事件自带的 seq / ts）。
      pub async fn append_event(&self, event: &SessionEvent) -> Result<u64, SessionError>;
      /// 同步取号 + 入队；队列满时进 spill，下一次 flush 一定排空。绝不丢行。
      pub fn append_nowait(&self, value: Value) -> u64;
      /// 同步取号 + 入队一个 typed 事件（压缩检查点的同步回调用）。
      pub fn append_event_nowait(&self, event: &SessionEvent) -> u64;
      /// 落盘承诺。排空队列与 spill、按序号排序、一次 append_many 写下去。
      pub async fn flush(&self) -> Result<(), SessionError>;
      /// 崩溃后补齐：只追加，绝不截断。补齐行的序号由写者发。
      pub async fn repair(&self, closers: Vec<SessionEvent>) -> Result<(), SessionError>;
  }
  ```

- [ ] **Step 1: 写失败的测试**

`crates/elysper-session/src/writer/tests.rs`：

```rust
#[tokio::test]
async fn a_saturated_queue_never_drops_a_row() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = LogWriter::with_capacity(sink.clone(), Arc::new(SystemClock), 1);
    for _ in 0..64 {
        writer.append(json!({"type": "Model", "data": {"type": "text", "text": "x"}})).await.unwrap();
    }
    writer.flush().await.unwrap();
    let rows = sink.read().await.unwrap();
    assert_eq!(rows.len(), 64, "a durable row is never dropped");
    let seqs: Vec<u64> = rows.iter().filter_map(|r| r["seq"].as_u64()).collect();
    assert_eq!(seqs, (1..=64).collect::<Vec<_>>(), "ordinals are gap-free");
}

#[tokio::test]
async fn one_flush_is_one_append_many() {
    let sink = Arc::new(CountingSink::new());
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));
    for _ in 0..10 {
        writer.append(json!({"type": "Model"})).await.unwrap();
    }
    writer.flush().await.unwrap();
    assert_eq!(sink.append_many_calls(), 1);
    assert_eq!(sink.append_calls(), 0);
}

#[tokio::test]
async fn a_sync_append_never_drops_a_row() {
    // append_nowait 在队列满时进 spill；flush 后全部落盘。
}

#[tokio::test]
async fn restart_does_not_reuse_a_burned_ordinal() {
    // 写一行、占一个号但不写（烧掉）、重开 sink → 下一个号 > 已有的最大号。
}
```

`CountingSink` 是测试里的一个 `SessionSink` 实现，计数 `append` / `append_many` 的调用次数。
`InMemorySessionSink` 已存在（`crates/elysper-session/src/in_memory.rs`）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p elysper-session --lib writer -- --nocapture`
Expected: FAIL —— `LogWriter` 不存在。

- [ ] **Step 3: 实现 `writer/mod.rs`**

核心结构：

```rust
pub struct LogWriter {
    sink: Arc<dyn SessionSink>,
    clock: Arc<dyn Clock>,
    ledger: Option<Arc<SurfaceLedger>>,
    queue: futures::channel::mpsc::Sender<StampedRow>,
    queue_rx: futures::channel::mpsc::Receiver<StampedRow>,  // 见下方说明
    spill: parking_lot::Mutex<Vec<StampedRow>>,
}
```

实现要点（签名已定，体由实现者写）：

- **取号**：`self.sink.alloc_seq()`。
- **盖章**：legacy 行在顶层插 `"seq"` 与 `"ts"`；typed 行序列化后覆盖顶层的 `"seq"` 与 `"ts"`
  （typed 事件的 `seq`/`ts` 是 `type` 的兄弟字段，见 `events.rs:119-135`）。
  `ts` 取 `self.clock.now().to_rfc3339()`。
- **入队**：有界 `futures::channel::mpsc`（容量由构造参数给，默认 256）。
  `append` 用 `Sender::send().await`（队列满时背压等待，**不丢**）；
  `append_nowait` 用 `try_send`，满了就 `spill.lock().push(row)`。
  **`futures::channel::mpsc::Sender::send` 取 `&mut self`**——写者不能把 `Sender` 直接塞进
  一个 `Mutex` 再跨 `.await` 持有它（`parking_lot::MutexGuard` 不是 `Send`）。
  正确写法：写者持一个 `Sender`，每次发送先 `let mut tx = self.tx.clone();`（futures 的
  `Sender::clone` 只是 Arc 计数），在**局部**可变借用上 `send().await`；
  接收端 `Receiver` 放在 `parking_lot::Mutex` 里，`flush` 取行时短暂持锁、**不跨 await**。
- **队列满时的处理**：`append` 发现队列满时，**先 `flush()` 腾出空间**再发
  （今天的 harness 写器就是这么做的：`crates/elysper-harness/src/log/mod.rs:148-152`）。
  没有排空方，满了就会永久等待。
- **flush**：把 spill 与队列里的行收齐 → 按 `seq` 排序 → 一次 `sink.append_many(&values)`。
  `Durability` 不存在（本轮删除），flush 的节奏由调用方决定。
- **账本喂入**：每写一行，若挂了账本就 `ledger.record(seq, &value)`。
  `seed_ledger(rows)` 用 `log_is_stamped` 的同一套判定把历史行喂进账本。
- **flush 的接收端**：`LogWriter` 需要一个自己能从 `append` 侧推入、又能从 `flush` 侧排空的队列。
  `futures::channel::mpsc` 的 `Sender`/`Receiver` 可以拆开持有（`Sender` 可 clone），
  写者内部同时持有二者即可；`flush` 用 `Receiver::try_next` 排空到 `None`/空。

- [ ] **Step 4: 实现计数器续接（`jsonl.rs` / `in_memory.rs`）**

`JsonlSessionSink::new`（`crates/elysper-session/src/jsonl.rs:207`）今天用
`count_lines(...)` 给 `next_seq` 播种。改成：

- 读出文件里的最后一行（尾读即可，不必扫全文件），若它带 `seq` 且整份日志都带 `seq`
  （用 `log_is_stamped` 的同一判定）→ `next_seq` = 那个 `seq`；
- 否则（旧日志 / 无 `seq` 字段）→ 保持今天的 `count_lines`。

`InMemorySessionSink` 不需要改（它的计数器跟着自己的行走）。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p elysper-session --lib`
Expected: PASS。

- [ ] **Step 6: 抬 paradigm-surface 基线并提交**

```bash
# crates/elysper-session/src/lib.rs 加了 `pub use writer::LogWriter;` → 13 → 14
# （门禁是「不得超过基线」，所以这是唯一一次抬升；Task 4 删通道后行数降到 12，基线不动）
sed -i 's/^13$/14/' scripts/paradigm-surface/baselines/crates/elysper-session
cargo test -p elysper-session --lib
git add crates/elysper-session scripts/paradigm-surface/baselines/crates/elysper-session
git commit -m "feat(session): LogWriter — one writer per session, batched flush"
```

---

### Task 3: session crate 接线——注册表的写者、压缩检查点改持写者

**Files:**
- Modify: `crates/elysper-session/src/manager.rs`（`SessionRegistry::writer`）
- Modify: `crates/elysper-session/src/compaction_checkpoint.rs`（改持写者 + 同步回调走 spill）
- Modify: `crates/elysper-session/src/repair.rs`（补齐行的序号交给写者）
- Modify: `crates/elysper-session/examples/compaction_checkpoint.rs`（改用写者）
- Modify: `crates/elysper-server/src/session/controller/mod.rs`（控制器多持一个写者）
- Modify: `crates/elysper-server/src/session/controller/run_task.rs:86`（检查点改拿写者）
- Modify: `crates/elysper-server/src/state/app_state/sessions.rs`（构造控制器时取写者）
- Test: `crates/elysper-session/src/writer/tests.rs`、`crates/elysper-session/src/compaction_checkpoint.rs`

> **通道（`TypedEventSink`）本任务不删**——它还有使用者（harness 的 `EnvCaps.typed_sink`）。
> 删除放在 Task 4，与它最后的使用者一起切干净。

**Interfaces:**
- Consumes: Task 2 的 `LogWriter`。
- Produces:
  ```rust
  // crates/elysper-session/src/manager.rs
  impl SessionRegistry {
      /// 这个会话的唯一写者（与 `sink()` 并列，按会话缓存）。
      pub fn writer(&self, user_id: &str, session_id: &str) -> Arc<LogWriter>;
  }
  // crates/elysper-session/src/compaction_checkpoint.rs
  impl CompactionCheckpoint {
      /// 建一个检查点：写者负责落盘，账本负责定位被遮蔽的行。
      pub fn new(writer: Arc<LogWriter>, ledger: Arc<SurfaceLedger>) -> Arc<Self>;
  }
  ```

- [ ] **Step 1: 写失败的测试**

`crates/elysper-session/src/writer/tests.rs`：

```rust
#[tokio::test]
async fn compaction_row_survives_a_full_queue() {
    // 容量 1 的写者 + 一个挂着的 CompactionCheckpoint：
    // 触发一次压缩回调（同步）→ 压缩行进 spill → flush → 行在日志里，
    // 且它的 source_event_seqs 能被 try_fold_log_surface 解析。
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p elysper-session --lib compaction_row_survives`
Expected: FAIL —— `CompactionCheckpoint::new` 还不接受写者。

- [ ] **Step 3: `SessionRegistry::writer`**

`crates/elysper-session/src/manager.rs`：照 `sink()`（`:79`）的样子加一个
`writers: RwLock<HashMap<String, Arc<LogWriter>>>` 缓存，key 用同一个 `sink_key(user_id, session_id)`；
`remove()`（`:129`）要把 writer 一起从缓存里摘掉。

- [ ] **Step 4: `CompactionCheckpoint` 改持写者**

`crates/elysper-session/src/compaction_checkpoint.rs`：

- 字段 `sink: TypedEventSink` → `writer: Arc<LogWriter>`。
- `new(writer, ledger)`。
- `lifecycle_callback`（`:177`）与 `emit_record`（`:207`）里的 `sink.record(...)` →
  `writer.append_event_nowait(&event)`（同步路径，进 spill，绝不丢）。
- `resolve_pending()` 的语义不变（调用方拿回事件后用同一个写者落盘）。
- `crates/elysper-session/examples/compaction_checkpoint.rs`：改用写者（签名变了）。

- [ ] **Step 5: 补齐行的序号交给写者**

`crates/elysper-session/src/repair.rs`：`interrupted_turn_closers`（`:201-240`）今天自己算
`state.last_seq + 1`——那是**第三个序号来源**。改成：补齐事件带 `seq: 0`，由写者盖章
（Task 2 的 typed 行盖章规则已经覆盖：`seq`/`ts` 是 `type` 的兄弟字段，写者覆盖它们）。

- [ ] **Step 6: server 侧跟着改（保持工作区能编译）**

`CompactionCheckpoint::new` 的签名变了 → server 的调用点要改：

- `crates/elysper-server/src/session/controller/mod.rs`：跑 agent 的那一半（`ControllerInner`）多持一个
  `Arc<LogWriter>`；**只有拥有者路径（`spawn_with_lease`）拿写者**——`spawn_remote`（转发节点）
  不拿：转发节点不得创建会话的 sink 与目录（`sessions.rs:83-85` 的既有不变量）。
- `crates/elysper-server/src/state/app_state/sessions.rs`：构造控制器时用
  `state.session_manager.writer(user_id, session_id)` 取写者。
- `crates/elysper-server/src/session/controller/run_task.rs:86`：
  `CompactionCheckpoint::new(writer.clone(), Arc::clone(&ledger))`。

Run: `cargo build --workspace` → 预期：编译通过（server 的其余写点本轮不动）。

- [ ] **Step 7: 跑测试确认通过**

Run: `cargo test -p elysper-session --lib && cargo test -p elysper-server --lib`
Expected: PASS。

- [ ] **Step 8: 提交**

```bash
git add crates/elysper-session scripts/paradigm-surface/baselines/crates/elysper-session
git commit -m "refactor(session): one writer per session; drop the channel that drops rows"
```

---

### Task 4: harness 的结构行走写者，删掉 typed sink（公共 API 一次性切干净）

**Files:**
- Modify: `crates/elysper-session/src/typed_event_sink.rs`（**删掉通道**，留 `TypedEventRecord` 与 `CompactionRecordView`）
- Modify: `crates/elysper-session/src/lib.rs`（去掉通道的 re-export）
- Modify: `crates/elysper-harness/src/agent/sink.rs`、`agent/caps.rs`、`agent/re_act/agent.rs`、
  `agent/re_act/loop/mod.rs`、`agent/re_act/loop/finalize.rs`、`agent/re_act/stream.rs`、`src/lib.rs`
- Modify: `crates/elysper-harness/src/agent/strategy/tests.rs`
- Modify: `crates/elysper-strategy-cot/src/lib.rs`、`elysper-strategy-best-of-n/src/lib.rs`、
  `elysper-strategy-plan-execute/src/strategy.rs`（+ 各自 example）
- Modify: `crates/elysper/src/{prelude,lib,session}.rs`、`crates/elysper/tests/feature_gating.rs`、
  `crates/elysper/examples/assemble_from_zero.rs`、`crates/elysper-harness/examples/assemble_with_builder.rs`
- Modify: `MINIMAL.md`、`README.md`、`SEAMS.md`
- Delete: `crates/elysper-harness/src/log/`（三个文件）

**Interfaces:**
- Consumes: Task 2 的 `elysper_session::LogWriter`。
- Produces:
  ```rust
  // crates/elysper-harness/src/agent/sink.rs
  impl EventSink {
      /// 2 参（原来 3 参）：tx + 写者。
      pub fn new(tx: Arc<mpsc::UnboundedSender<AgentEvent>>, log_writer: Option<Arc<LogWriter>>) -> Self;
      /// 写一条 typed 会话事件（结构行）。写者没装时是 no-op。
      pub fn emit_typed(&self, event: SessionEvent);
  }
  // crates/elysper-harness/src/agent/re_act/agent.rs
  impl ReActAgent {
      /// 装上这个会话的写者。类型从 harness 的 LogWriter 换成 session 的。
      pub fn with_log_writer(mut self, writer: Arc<LogWriter>) -> Self;
  }
  ```
  `LogWriter` 的 facade 路径定为 **`elysper::session::LogWriter`**（harness 的 `log` 模块消失，
  `elysper::harness::LogWriter` 与 `elysper::harness::Durability` 随之消失）。

- [ ] **Step 1: 写失败的测试**

`crates/elysper-harness/src/agent/re_act/tests/lifecycle/` 加一条（并入 `lifecycle/mod.rs` 的
`mod` 列表）：

```rust
#[tokio::test]
async fn structural_rows_go_through_the_writer() {
    // 装一个写者（InMemorySessionSink）→ 跑一轮 →
    // 日志里有 step / usage 行，且每行都带 seq。
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p elysper-harness --lib structural_rows_go_through_the_writer`
Expected: FAIL —— `with_log_writer` 的类型还是 harness 的 `LogWriter`，且结构行还走老路。

- [ ] **Step 3: `EventSink` 与 caps：换类型、删老路**

- `crates/elysper-harness/src/agent/sink.rs`：`EventSink` 的 `typed_sink` 字段与 `has_typed()` 删除；
  `log_writer` 的类型改成 `elysper_session::LogWriter`；`new` 少一个参数；
  `emit_typed` 改成：写者在场 → `writer.append_nowait(row)`（同步路径，结构行不阻塞循环）；
  写者不在场 → 什么都不做（结构行是日志事实，没有写者就没有日志）。
- `crates/elysper-harness/src/agent/caps.rs`：`EnvCaps` 删 `typed_sink` 字段。
- `crates/elysper-harness/src/agent/re_act/agent.rs`：删 `with_typed_event_sink`；
  `with_log_writer` 的参数类型换成 `Arc<elysper_session::LogWriter>`。
- `crates/elysper-harness/src/agent/re_act/loop/mod.rs` 的 `flush_log()` 与
  `loop/finalize.rs`：调 `writer.flush()`（语义不变：调用模型之前 / 收尾时）。
- `crates/elysper-harness/src/agent/re_act/stream.rs`：那段 `emit_typed(AssistantChunk)`
  （`:39-66`）**本任务先保留**（Task 6 才删），但它现在走写者而不是通道。

Run: `cargo build -p elysper-harness` → 预期：harness 自身的编译错误（`EventSink::new` 的
调用点还没改），但**不**有类型错误。

- [ ] **Step 4: 删掉 harness 的 `log/` 模块 + session crate 的通道**

`crates/elysper-harness/src/lib.rs`：删 `pub use log::{Durability, LogWriter};` 与 `mod log;`；
删除 `crates/elysper-harness/src/log/`（`mod.rs` / `queue.rs` / `tests.rs`）——
写者的测试已经搬进 session crate（Task 2）。

`crates/elysper-session/src/typed_event_sink.rs`：删掉 `TypedEventSink` 与 `TypedEventReceiver`
两个类型与其实现；**保留** `TypedEventRecord` 与 `CompactionRecordView`（公共 API，
`CompactionRecordView` 还是 server 的公共 API 的一部分：
`crates/elysper-server/src/agent_run_config.rs:28,41,49`）。
`crates/elysper-session/src/lib.rs`：去掉这两个类型的 re-export（`lib.rs:161-166`）。
→ `pub use` 行数从 14 降到 12，**基线不动**（门禁是「不得超过基线」，不是「必须相等」）。

Run: `cargo build --workspace` → 预期：只剩 `EventSink::new` 调用点与通道使用者的编译错误。

- [ ] **Step 5: 三个 strategy crate 与所有调用点**

`EventSink::new` 从 3 参变 2 参 → 11 个调用点、7 个文件、4 个 crate：
`crates/elysper-strategy-cot/src/lib.rs`、`crates/elysper-strategy-best-of-n/src/lib.rs`、
`crates/elysper-strategy-plan-execute/src/strategy.rs`（+ `examples/plan_execute_demo.rs`）、
`crates/elysper-harness/src/agent/strategy/tests.rs`（`:186`、`:214`）、
`crates/elysper-harness/src/agent/re_act/stream.rs`（`:158`）、
`crates/elysper-harness/src/agent/re_act/agent.rs`（`:90`）。
它们调 `sink.emit_typed(...)` 的地方（`cot:138-141`、`best-of-n:288-291`）随之改成写者路径。

- [ ] **Step 6: facade、示例、文档**

- `crates/elysper/src/prelude.rs`：去掉 `TypedEventSink` 的 re-export；`Durability` 一并去掉。
- `crates/elysper/src/lib.rs`、`src/session.rs`：删掉提到这两个类型的文档行。
- `crates/elysper/tests/feature_gating.rs:46,93`：更新 re-export 断言。
- `crates/elysper/examples/assemble_from_zero.rs:191-199`、
  `crates/elysper-harness/examples/assemble_with_builder.rs:33,76,101`：改用写者。
- `MINIMAL.md`（**被编译的 doctest**）：它今天用
  `elysper::harness::{Durability, LogWriter, ReActAgent}`、`TypedEventSink::channel(64)`、
  `LogWriter::new(sink, Durability::StepBoundary)`、`.with_typed_event_sink(typed_sink)` →
  全部改成新的写法（`elysper::session::LogWriter::new(sink, clock)` + `with_log_writer`）。
- `README.md:258,270,275`、`SEAMS.md:21`：同步改写。

- [ ] **Step 7: 跑测试与门禁**

Run:
```bash
cargo test -p elysper-harness --lib
cargo test -p elysper-strategy-cot --lib
cargo test -p elysper-strategy-best-of-n --lib
cargo test -p elysper-strategy-plan-execute --lib
cargo test -p elysper --doc
make test-guides
make check-harness-docs
```
Expected: 全绿；`check-harness-docs` ≤ 56。

- [ ] **Step 8: 提交**

```bash
git add -A crates/elysper-harness crates/elysper-strategy-cot crates/elysper-strategy-best-of-n \
       crates/elysper-strategy-plan-execute crates/elysper MINIMAL.md README.md SEAMS.md
git commit -m "refactor(harness): structural rows go through the session writer"
```

---

### Task 5: server 一个会话一个写者

**Files:**
- Modify: `crates/elysper-server/src/session/controller/run_log.rs`、`run_task.rs`、`persist.rs`、
  `dispatch.rs`、`inner.rs`、`mod.rs`
- Modify: `crates/elysper-server/src/session/subagent_router.rs`
- Modify: `crates/elysper-server/src/routes/sessions.rs`、`routes/messages/mod.rs`
- Modify: `crates/elysper-server/src/agent_run_config.rs`（删 `typed_event_sink` 字段）
- Test: `crates/elysper-server/src/session/controller/tests/`

**Interfaces:**
- Consumes: Task 2 的 `LogWriter`、Task 3 的 `SessionRegistry::writer`。
- Produces: `RunLog` 改持 `Arc<LogWriter>`；`RunLog::append(&mut self, value: &Value) -> Result<u64, SessionError>`
  改为「取号 + 入队」（入队后由调用方决定何时 flush）。

- [ ] **Step 1: 写失败的测试**

`crates/elysper-server/src/session/controller/tests/`：

```rust
#[tokio::test]
async fn a_feedback_row_does_not_break_the_run_ordinals() {
    // run 的行与一条 feedback 行交错落盘 → try_fold_log_surface 仍通过，
    // 压缩引用可解析。
}

#[tokio::test]
async fn a_durable_row_is_on_disk_before_it_is_broadcast() {
    // mock sink + 广播钩子 → 断言广播时该行已能读回。
}

#[tokio::test]
async fn a_forked_session_restamps_its_inherited_rows() {
    // fork 之后读子会话的行 → 序号是子会话自己的一套（从 1 开始），
    // 不是父会话的号。
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p elysper-server --lib controller::tests`
Expected: FAIL（feedback 与 fork 两条今天就红）。

- [ ] **Step 3: `RunLog` 改持写者**

`crates/elysper-server/src/session/controller/run_log.rs`：

- 字段 `store: Arc<dyn SessionSink>` → `writer: Arc<LogWriter>`（账本另持）。
- `append` / `append_typed`：「取号 + 入队」，返回拿到的序号；**不再**在这里 fsync。
- 账本的喂入交给写者（`LogWriter::with_ledger`），`RunLog` 不再单独 `ledger.record`。
  → 账本与折叠器看到同一批行（不变量 I11）。

- [ ] **Step 4: 所有写点收口**

- `dispatch.rs:320`（feedback）、`inner.rs:166`（shutdown marker）、
  `subagent_router.rs:181,185,202,217,229`、`routes/sessions.rs:998,1128,1164,1505,1513,1651,1821,1829`、
  `routes/messages/mod.rs:531-532`：改成经该会话的写者
  （`state.session_manager.writer(user_id, session_id)`）。
- **会话 fork**（`routes/sessions.rs:990-999`）：继承行必须**重新盖章**——
  写者给每一行发新的号，父会话的号不带过去。
- `run_task.rs`：删 typed 通道的创建（`:76-77`）与末尾排空（`:525-533`）；
  删 `config.typed_event_sink = Some(typed_sink)`（`:94`）；
  写者装到 agent 上（`with_log_writer`）；
  账本种子改用写者（`writer.seed_ledger(&rows)`，用与折叠器同一套判定）。
- `agent_run_config.rs:50`：删 `typed_event_sink` 字段。

- [ ] **Step 5: 广播前落盘**

`crates/elysper-server/src/session/controller/persist.rs`：
`append_if_durable`（`:181`）入队后、广播前调 `writer.flush().await`——
「你看到的 durable 事实一定在盘上」的契约由这一步保证。

- [ ] **Step 6: 跑测试确认通过 + 单一写者的 grep 门禁**

Run: `cargo test -p elysper-server --lib`
Expected: PASS。

再跑一次「一个会话一个写者」的门禁（不变量 I1）——除写者自身以外，
全仓库不应再有直接写 sink 的地方：

```bash
grep -rn "session_store\.append\|sink\.append(\|\.append_many(" --include='*.rs' crates/ \
  | grep -v "crates/elysper-session/src/writer/" \
  | grep -v "crates/elysper-session/src/jsonl.rs" \
  | grep -v "crates/elysper-session/src/in_memory.rs" \
  | grep -v "crates/elysper-session/src/sink.rs" \
  | grep -v "/tests/" | grep -v "#\[cfg(test)\]"
```

Expected: 命中数为 0（或每一条都在注释/文档里）。有命中就说明还有写点没收口。

- [ ] **Step 7: 提交**

```bash
git add -A crates/elysper-server
git commit -m "refactor(server): every session row goes through one writer"
```

---

### Task 6: delta 不落盘，一轮一行

**Files:**
- Modify: `crates/elysper-harness/src/agent/re_act/stream.rs`（删逐 chunk 落盘；写整条 assistant 消息）
- Modify: `crates/elysper-harness/src/agent/re_act/loop/tools/commit.rs`（工具结果行由 harness 写）
- Modify: `crates/elysper-harness/src/events/mod.rs`（`is_durable()`：model 族全部 false）
- Test: `crates/elysper-harness/src/agent/re_act/tests/lifecycle/one_row_per_turn.rs`（新建）

**Interfaces:**
- Consumes: Task 2/4 的写者。
- Produces: `AgentEvent::is_durable()` 对 `Model(_)` / `ModelDone(_)` / `System(_)` 一律返回 `false`；
  `AgentEvent::Agent(_, inner)` 递归到 `inner.is_durable()`（也随之是 false）。

- [ ] **Step 1: 写失败的测试**

`crates/elysper-harness/src/agent/re_act/tests/lifecycle/one_row_per_turn.rs`：

```rust
#[tokio::test]
async fn a_streamed_reply_is_one_row_and_one_message_after_resume() {
    // 假 provider 吐 5 个文本 delta → 跑一轮 →
    // 日志里恰好一行 assistant 事实（type == "assistant_message"），
    // 内容是拼好的完整文本；
    // events_to_messages(日志) 返回 1 条 assistant 消息，内容等于完整文本。
}

#[tokio::test]
async fn a_reply_with_a_tool_call_keeps_text_and_tool_use_in_one_row() {
    // 假 provider 说一句话再调一个工具 → 日志里那一行同时含 Text 与 ToolUse。
}

#[tokio::test]
async fn deltas_still_stream_as_events_but_are_not_persisted() {
    // 5 个 delta → 事件流里仍收到 5 个 Model(Text) 事件（live 渲染不受影响），
    // 但日志里没有 assistant_chunk 行。
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p elysper-harness --lib one_row_per_turn`
Expected: FAIL —— 今天每个 delta 一行。

- [ ] **Step 3: 改 `is_durable()`**

`crates/elysper-harness/src/events/mod.rs:70-79`：model 族全部返回 `false`（delta 与工具结果
都由 harness 直接写日志，host 不再从事件里持久化它们）。

- [ ] **Step 4: harness 写整条 assistant 消息**

`crates/elysper-harness/src/agent/re_act/stream.rs`：

- 删掉 `ChunkAssembler::ingest` 里逐 chunk 的 `emit_typed(AssistantChunk)`（`:39-66`）。
- 模型调用结束时（`finalize_outcome` 已知组装好的 parts），把整条 assistant 消息写成**一行**：
  `SessionEvent::AssistantMessage { seq: 0, ts: String::new(), data: <Message 的 JSON>, surface_op: None }`
  → 经 `writer.append_event_nowait(...)` 落盘（结构行是同步路径）。
  消息的 `content` 要含**全部** parts（文本 + 工具调用），这样一轮就是一行。
- delta 仍然照旧作为 `AgentEvent::Model(ContentPart::Text(delta))` 发到事件流（live 渲染不变）。

- [ ] **Step 5: 工具结果行由 harness 写**

`crates/elysper-harness/src/agent/re_act/loop/tools/commit.rs`：
`commit_tool_result`（`:103`）除了发事件，还要经写者写一行
`SessionEvent::ToolResult { seq: 0, ts: String::new(), data: <ToolResult 消息的 JSON>, surface_op: None }`。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p elysper-harness --lib`
Expected: PASS。server 侧的测试可能因为「server 不再持久化 Model 事件」而需要调整——
那是 Task 5 的范围，但**本任务结束时 `cargo test -p elysper-server --lib` 必须仍然绿**：
若红，先在本任务内修好（server 的行为变化是被 `is_durable()` 驱动的，不需要改 server 的代码）。

- [ ] **Step 7: 提交**

```bash
git add -A crates/elysper-harness crates/elysper-server
git commit -m "feat(harness): one assistant turn is one row; deltas stay live-only"
```

---

### Task 7: context crate 的两个写点

**Files:**
- Modify: `crates/elysper-context/src/memory/typed.rs`（`repair_session`）
- Modify: `crates/elysper-context/src/memory/session.rs`（`SessionMemory`）
- Modify: `crates/elysper-context/src/memory/file/mod.rs:649`（`SessionMemory::new` 的生产调用点）
- Modify: `crates/elysper-server/src/state/app_state/sessions.rs`（调用点）

**Interfaces:**
- Consumes: Task 2 的 `LogWriter`。
- Produces:
  ```rust
  // crates/elysper-context/src/memory/typed.rs
  pub async fn repair_session(writer: Arc<LogWriter>) -> Result<(), ...>;
  // crates/elysper-context/src/memory/session.rs
  impl SessionMemory {
      pub fn new(writer: Arc<LogWriter>) -> Self;
  }
  ```

- [ ] **Step 1: 写失败的测试**

`crates/elysper-context/src/memory/` 的既有测试里，把直接 `sink.append(...)` 的写法换成写者；
新增一条断言：`repair_session` 补齐的行带 `seq`，且序号大于日志里已有的最大号
（它不再自己算 `state.last_seq + 1`——那是第三个序号来源）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p elysper-context --lib`
Expected: FAIL —— 签名还没改。

- [ ] **Step 3: 改两个写点**

- `typed.rs:145-148`：`repair_session` 拿写者，补齐行经 `writer.append_nowait(...)` 或
  `writer.repair(closers)`（序号由写者发）。
- `session.rs:211`：`SessionMemory::append` 改走写者；`SessionMemory::new(sink)` → `new(writer)`。
- `crates/elysper-context/src/memory/file/mod.rs:649`：`SessionMemory::new` 的生产调用点改传写者。
- `crates/elysper-server/src/state/app_state/sessions.rs:135`：调用点改传写者。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p elysper-context --lib && cargo test -p elysper-server --lib`
Expected: PASS。

- [ ] **Step 5: 提交**

```bash
git add -A crates/elysper-context crates/elysper-server
git commit -m "refactor(context): memory and repair write through the session writer"
```

---

### Task 8: web 的历史投影认 `assistant_message` 行

**Files:**
- Modify: `elysper-web/src/lib/session-to-messages.ts`
- Test: 无自动化测试套件（本仓按 `AGENTS.md` §4.2 不保留前端 e2e；验证用 Playwright MCP 驱动真实前端）

**Interfaces:**
- Consumes: Task 6 产出的 `assistant_message` 行形状
  （`{"type":"assistant_message","seq":N,"ts":"...","data":<Message 的 JSON>}`）。
- Produces: `applyHistoryEntry`（`:524`）多认一个 envelope。

- [ ] **Step 1: 改 `applyHistoryEntry`**

`elysper-web/src/lib/session-to-messages.ts`：

- 常量区（`:95-96`）加 `const EVENT_ASSISTANT_MESSAGE = 'assistant_message';`。
- `applyHistoryEntry`（`:524`）加一个分支：`assistant_message` 行的 `data` 是一整条
  `Message`（`role` + `content` 的数组）→ 产出一条 assistant 消息，
  `segments` 按 `content` 的每个 part 展开（文本 → text 段；tool_use → 工具块）。
  参照同文件里 `Model` 分支对 `tool_use` / `tool_result` 的处理（`:1178`、`:1185`），
  复用它的配对逻辑。

- [ ] **Step 2: 跑前端门禁**

Run: `make check-web`
Expected: PASS（`tsc --noEmit` + `eslint` + `prettier --check` 全绿）。

- [ ] **Step 3: 用 Playwright MCP 验真实前端**

起 `npm run dev` + `cargo run -p elysper-server`，打开 chat 页：

1. 发一条消息，让假 provider 回一段多 token 的文本。
2. 刷新页面 → 历史投影必须显示**一条**完整的 assistant 消息（不是碎片、不是空）。
3. 打开会话详情页 → 同一条消息在历史里只出现一次。

（`AGENTS.md` §4.2：后台 tab 要先 patch `visibilityState`，否则 app 会自己判成 Offline。）

- [ ] **Step 4: 提交**

```bash
git add elysper-web/src/lib/session-to-messages.ts
git commit -m "feat(web): history projection reads the assistant_message row"
```

---

## 收尾

- [x] **全量门禁**

```bash
cargo +nightly fmt --all
make ci
cargo test -p elysper-session --lib
cargo test -p elysper-harness --lib
cargo test -p elysper-server --lib
cargo test -p elysper-context --lib
```

- [x] **不变量逐条核对**（对应 §8 的 I1–I15；测试已在各任务里写）

| 不变量 | 归属任务 |
|---|---|
| I1 一个会话一个写者（grep 门禁） | Task 5 |
| I2 序号唯一、单调 | Task 2 |
| I3 折叠器用行里的 `seq` | Task 1 + Task 5 |
| I4 旧日志行为不变 | Task 1 |
| I5 混合日志不重复 | Task 1 |
| I6 队列满不丢行 | Task 2 |
| I7 同步路径不丢行 | Task 2 |
| I8 一次 flush 一次 `append_many` | Task 2 |
| I9 广播前已落盘 | Task 5 |
| I10 压缩行不消失 | Task 3 |
| I11 账本与折叠器同一批行 | Task 5 |
| I12 一轮一行 | Task 6 |
| I13 resume 后一条消息 | Task 6 |
| I14 文本与工具调用同行 | Task 6 |
| I15 重启不重用序号 | Task 2 |

- [x] **文档收尾**

- `CHANGELOG.md` 的 `[Unreleased]` 段：写清「写者唯一化」「序号以行里的 `seq` 为准」
  「一轮一行」「删除 `TypedEventSink` / `Durability`」。
- `SEAMS.md` 的「durable log」一行改成新的接缝描述。
- `docs/design/2026-10-23-harness-v2-design.md` 的状态从「待批准」改成「已落地（第一段）」，
  并指向本计划。
