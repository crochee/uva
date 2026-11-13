# harness v4 遗留收口与基座承诺 — 实施计划

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 修完 harness v4 验收留下的八项遗留（其中一项已在本分支完成），并落两条对外承诺（公开 API 冻结、日志格式版本化）与三件交接材料，使这条分支可以作为社区基座交出去。

**Architecture:** 十三项任务分四波。第一波改**持久化格式**（行的 durable key、压缩写 durable 事件、版本号门禁），因为后面所有验收都跑在新格式上；第二波修**语义**（resume 错误码、gate 生命周期），彼此独立；第三波**收口**（文档、规则去重）；第四波是**与 harness 无关的既有欠债**（四个 crate 的测试变绿），单独成波。最后两项落承诺与交接材料，末项重跑验收。

**Tech Stack:** Rust 2024 edition，MSRV 1.98；`thiserror`（库）/ `anyhow`（应用）；`tokio` 只出现在 app 与 opt-in crate；`serde_json` 为日志载体；前端为 TypeScript + React（`uva-web`）。

**Spec:** `docs/design/2026-10-26-harness-v4-remainders-design.md`（本计划的每条决定都出自它）
**验收记录（本轮要更新的那份）:** `docs/design/2026-10-26-harness-v4-acceptance.md`

## Global Constraints

以下每条都来自仓库规范或设计文档，**每个任务都隐含包含**：

- 库用 `thiserror`、应用用 `anyhow`；`max_width = 80`；禁通配符导入（`use foo::*`）。
- `uva-harness` **不得**有 `[features]` 表，闭包内**不得**出现 tokio，库 crate 的公开面**不得**出现运行时类型（`make check-no-runtime` / `make check-harness-features` / `make check-public-api-runtime`）。
- harness 私有项的 `///` 预算**正好 50/50**（`make check-harness-docs`）—— 新增任何一行 harness 私有文档前必须先删一行。
- 库生产代码**不得**直接调用 `chrono::Utc::now()`，一律走 `uva_core::Clock`（`make check-clock`）。
- `make check-test-layout`：内联 `#[cfg(test)] mod` 测试块 < 400 行；新测试放同级 `tests.rs`。
- `make check-pub-surface` / `make check-pub-surface-strict`：禁 glob re-export；七个范式 crate 的 `pub use` 计数不得增长。
- `make check-claim-language`：状态文档里不许有与代码矛盾的绝对化措辞。
- `make check-mvp-deps`：八个离线 tool crate 与 R-OPT 插件 crate 不得引入 `reqwest|hyper|rustls|h2|tower`。
- **TDD：先写失败的测试。** 提交信息用英文祈使句，一行。逐 crate 跑测试（`cargo test -p <crate>`），**禁止** `cargo test --workspace`。
- 不推送远端。工作区是 worktree `.worktrees/harness-v4`，分支 `feature/harness-v4`。

## Review Focus

设计文档没有逐字规定、但一个合理的人会期望成立的五件事。每一条在下面都指到了拥有它的任务，由那个任务自己的测试钉住。

1. **一个只写了一半的日志不能被读成完整的历史。** 任何行写到一半、flush 失败、日志尾部被截断时，读端要么给出明确错误，要么明确标记 —— 不能把半行当整行、把缺行当没发生。（Task 1、Task 4）
2. **一次压缩之后，重放出来的历史必须和当时实时看到的一模一样。** 不是"也差不多"，不是"配对仍然成立"—— 是逐行相同。（Task 3）
3. **一个远端插件不能顶掉二进制自带的工具。** 名字被内建工具占着时，插件的注册要被拒绝并说明原因，而不是后来者覆盖前者。（Task 6 的回归检查）
4. **一个读不到的日志绝不能被当成"没有待审批"。** 这两件事的区别是：前者是系统出故障，后者是一个决定。故障不能被当成决定。（Task 5）
5. **一个 v1 的老会话被这一版拒绝打开时，错误必须说清它读到了哪个版本、支持哪个版本、下一步该做什么。** 一个泛化的 IO 错误会让用户以为会话丢了。（Task 4）

---

## 文件结构

| 文件 | 责任 | 本轮动它吗 |
|---|---|---|
| `crates/uva-session/src/ledger_key.rs`（新） | 一行/一条消息的 durable identity 的**唯一**推导处 | Task 2 建 |
| `crates/uva-session/src/log_surface.rs` | ledger 与 fold 共享的行分类与 key 解析 | Task 2、3 |
| `crates/uva-session/src/jsonl.rs` | 日志读写、头行、版本号 | Task 1、4 |
| `crates/uva-session/src/writer/mod.rs` | 行号分配、盖章、队列、spill、批量写 | Task 1 |
| `crates/uva-session/src/events.rs` | `SessionEvent` 的变体与表 | Task 3、4 |
| `crates/uva-context/src/summarizing/manager.rs` | 自动压缩的 splice 与 `CompactionRecord` | Task 2、3 |
| `crates/uva-context/src/summarizing/lifecycle.rs` | `CompactionRecord` 的形状 | Task 3 |
| `crates/uva-app-server/src/routes/follow.rs` | follow / resume 路由 | Task 5、8 |
| `crates/uva-app-server/src/session/controller/inner.rs` | controller 的日志派生状态 | Task 5 |
| `crates/uva-app-server/src/state/app_state/{mod,sessions}.rs` | `AppState` 与会话表 | Task 6 |
| `crates/uva-web/src/**` | 前端 | Task 5 |
| `scripts/check-frozen-api.sh`（新） | 公开 API 冻结门禁 | Task 11 建 |
| `docs/design/2026-10-26-harness-v4-acceptance.md` | 验收记录 | Task 13 改 |

---

### Task 1: writer 的重试不重复写行

**Files:**
- Modify: `crates/uva-session/src/jsonl.rs`（`JsonlSessionSink::append_many`，`:358`）
- Modify: `crates/uva-session/src/writer/mod.rs`（`write_batch`，`:480`；`stamp`，`:525`）
- Test: `crates/uva-session/src/writer/tests.rs`（已有 `GatedSink`，`:584`，可复用）

**Interfaces:**
- Consumes: 现有 `SessionSink::append_many(&self, events: &[Value])`（`sink.rs:324`）；行上已由 `stamp` 写入的顶层 `seq` 与 `ts`。
- Produces: `JsonlSessionSink` 内部新增 `max_seq_written: u64`；`append_many` 对**已写过**的行做幂等跳过。

**背景（已核实）:** `write_batch`（`writer/mod.rs:480`）把行交给 `sink.append_many`；失败时把行**按原 seq** 放回 spill 并返回错误。若后端在"字节已落盘、fsync 失败"之后报错，重试会把同一批行再写一遍 —— 日志里出现两个同 `seq` 的行。行在**入队时**就盖好了 `seq`（`enqueue`，`:555` → `stamp`，`:525`），spill 里保留 `StampedRow { seq, value }`（`:78`），所以重试认的就是同一个 `seq`：**身份已经存在，缺的是去重。**

- [ ] **Step 1: 写失败的测试**

在 `crates/uva-session/src/writer/tests.rs` 加一个测试，用一个"写成功但随后报错"的后端（`GatedSink` 已有类似形状，`:584`）：第一次 `flush()` 返回 `Err`，第二次 `flush()` 返回 `Ok`；然后断言后端**最终持有**的行里，每个 `seq` 只出现一次，且行数等于第一次尝试的批大小。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-session --lib a_retry_after_a_failed_flush_does_not_duplicate_a_row`
Expected: FAIL —— 重复行出现。

- [ ] **Step 3: 让 `JsonlSessionSink::append_many` 幂等**

在 `JsonlSessionSink` 的状态里加 `max_seq_written: u64`（初值 0）。`append_many` 在写之前：从每个 event 读顶层 `seq`；**`seq` 缺失的行照写**（不能凭猜测跳过），`seq <= max_seq_written` 的行**跳过**（它已经落盘）。写成功后把 `max_seq_written` 更新为这批里最大的 `seq`。跳过的行不计入 `rows_written`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p uva-session --lib`
Expected: 全绿。

- [ ] **Step 5: 跑门禁**

Run: `cargo clippy -p uva-session --all-targets -- -D warnings && cargo +nightly fmt --all --check`
Expected: 干净。

- [ ] **Step 6: 提交**

```bash
git add crates/uva-session/src/jsonl.rs crates/uva-session/src/writer
git commit -m "fix(session): skip rows a failed flush already wrote"
```

---

### Task 2: 每个 surface 行都有 durable identity

**Files:**
- Create: `crates/uva-session/src/ledger_key.rs`
- Modify: `crates/uva-session/src/lib.rs`（导出新模块）
- Modify: `crates/uva-session/src/log_surface.rs`（`classify_typed_message`，`:168`；`decode_message`，`:187`）
- Modify: `crates/uva-context/src/summarizing/manager.rs`（`source_keys` 的来源，`:464`）
- Test: `crates/uva-session/src/log_surface/tests.rs`（若该文件已存在则追加；否则在 `log_surface.rs` 内联 `#[cfg(test)] mod`）

**Interfaces:**
- Consumes: `uva_provider::types::Message`（字段 `role` / `content` / `tool_call_id`）。
- Produces:
  ```rust
  // crates/uva-session/src/ledger_key.rs
  /// The durable identity of a message, or `None` when it carries none.
  ///
  /// The identity must survive a copy into another log — a row's
  /// ordinal does not (a copy remaps it) — so it is derived from the
  /// message's own content.
  pub fn key_of(message: &Message) -> Option<String>;
  ```
  规则（**逐条照抄，不要自行发挥**）：
  - `role == Tool` 且 `tool_call_id` 是 `Some(id)` → `Some(format!("tool:{id}"))`
  - `role == Assistant` 且 content 里**第一个** `tool_use` 部件的 id 是 `X` → `Some(format!("assistant:{X}"))`
  - 其余 → `None`
  - 前缀（`tool:` / `assistant:`）是**必须**的：tool_result 行的 key 与 assistant 行的 key
    来自同一个 tool_call_id，不加前缀会撞成 `AmbiguousKey`。
  - 一条 assistant 消息可以有多个 `tool_use` 部件；用**第一个**的 id，因为 tool_call_id 全局唯一，
    一个 id 足以唯一确定发出它的那一行。

**背景（已核实）:** `decode_message`（`log_surface.rs:187`）现在把 key 取成 `message.tool_call_id`，
于是**只有 tool_result 行有 key**，assistant 行的 key 恒为 `None`；`SurfaceLedger::resolve`
（`:844`）遇到 `None` 就返回 `MappingGap::MissingKey { position }`（`:861`）。
`CompactionRecord.source_keys`（`uva-context/src/summarizing/lifecycle.rs:27`）是
`Vec<Option<String>>`，它的文档已经写明："a `None` entry means the row has no durable identity
and the splice must not be written with invented provenance"。

- [ ] **Step 1: 写失败的测试**

在 `uva-session` 里加测试：
- `key_of` 对一条带 `tool_call_id` 的 tool 消息返回 `Some("tool:<id>")`；
- 对一条含两个 `tool_use` 部件的 assistant 消息返回 `Some("assistant:<第一个 id>")`；
- 对一条纯文本 assistant 消息返回 `None`；
- **一条 assistant 行和它的 tool_result 行在同一个 surface 上不撞 key**（用 `SurfaceLedger::record` +
  `resolve` 走一遍，断言不返回 `AmbiguousKey`）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-session --lib ledger_key`
Expected: FAIL —— 模块不存在。

- [ ] **Step 3: 建 `ledger_key` 模块**

按 Interfaces 里的规则实现 `key_of`。`Message` 的 content 是 `uva_provider::types::Content`；
`tool_use` 部件是 `ContentPart::ToolUse`，其 id 字段按现有 `collect_tool_use_ids`
（`crates/uva-session/src/tool_pairing.rs:296`）的取法读 —— **复用那里的取法**，不要另写一份。

- [ ] **Step 4: 让 log_surface 用它**

`classify_typed_message` / `decode_message`（`log_surface.rs:168,187`）改为调用
`uva_session::ledger_key::key_of`，不再自己取 `tool_call_id`。

- [ ] **Step 5: 让压缩的 `source_keys` 用它**

`uva-context/src/summarizing/manager.rs:464` 的 `source_keys` 从 `msg.tool_call_id.clone()`
改为 `uva_session::ledger_key::key_of(msg)`。

- [ ] **Step 6: 跑测试确认通过**

Run: `cargo test -p uva-session --lib && cargo test -p uva-context --lib`
Expected: 全绿。

- [ ] **Step 7: 提交**

```bash
git add crates/uva-session crates/uva-context
git commit -m "feat(session): give every shadowable row a durable identity"
```

---

### Task 3: 压缩写 durable 事件，重放 == 实时

**Files:**
- Modify: `crates/uva-context/src/summarizing/manager.rs`（`CompactionRecord` 的发出，`:353`）
- Modify: `crates/uva-session/src/events.rs`（durable `Compaction` 事件）
- Modify: `crates/uva-session/src/log_surface.rs`（`SurfaceLedger::record` 对压缩行的处理）
- Test: `crates/uva-session/src/history/tests.rs`（`crates/uva-session/src/history.rs`）

**Interfaces:**
- Consumes: Task 2 的 `uva_session::ledger_key::key_of`；`SurfaceLedger::resolve(&[Option<String>]) -> Result<ResolvedReplace, MappingGap>`（`log_surface.rs:844`）。
- Produces: durable 的 `SessionEvent::Compaction` 行，其 `source_keys` 每项都 `Some`。

**背景（已核实）:** 压缩的 splice 目前只在内存里发生。durable 侧的检查点因为
`SurfaceLedger::resolve` 撞 `MissingKey` 而把记录 park 并丢弃（验收记录 §6），于是日志重放出来的是
**压缩前**的历史。Task 2 之后 key 不再是 `None`，这条路才通。

- [ ] **Step 1: 写失败的测试**

在 `crates/uva-session/src/history/tests.rs` 加测试：构造一个 surface（一条 assistant 行 + 两条
tool_result 行），跑一次压缩（用 `uva-context` 的 summariser），把产生的 durable 行读进日志，
然后**分别**用实时 fold 与重放 fold 计算 surface，断言**两者逐行相同**，且压缩记录**在**日志里
（不是被 park 掉）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-session --lib a_compacted_surface_replays_identically`
Expected: FAIL —— 重放的面与实时的面不同。

- [ ] **Step 3: 让压缩写出 durable 事件**

`uva-context` 的 `CompactionRecord` 发出点（`manager.rs:353`）在 `source_keys` 全部 `Some` 时
写出 durable 的 `SessionEvent::Compaction`；有任何一个 `None` 时**不写**（不编造 provenance），
并 `tracing::warn!` 记下被跳过的原因。

- [ ] **Step 4: 让 ledger 认这条事件**

`SurfaceLedger::record`（`log_surface.rs:809`）收到压缩行时，用该行的 `source_keys` 走
`resolve` 解析出被替换的区间，`Replace` 的 `start`/`end` 用解析结果，而不是行里写死的下标。
解析失败时按今天的行为 warn 并忽略（不改变 fold 的结果）。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p uva-session --lib && cargo test -p uva-context --lib`
Expected: 全绿。

- [ ] **Step 6: 提交**

```bash
git add crates/uva-session crates/uva-context
git commit -m "fix(session): write a durable compaction event so replay matches the live surface"
```

---

### Task 4: 日志格式版本门禁 —— 升 v2，拒绝旧版

**Files:**
- Modify: `crates/uva-session/src/jsonl.rs`（`CURRENT_SCHEMA_VERSION`，`:78`；`detect_schema_version_from_line`；`read`）
- Modify: `crates/uva-session/src/errors.rs`（或 `SessionError` 所在处）—— 新增错误变体
- Modify: `crates/uva-session/src/events.rs`（若变体登记表需要）
- Test: `crates/uva-session/src/jsonl/tests.rs`

**Interfaces:**
- Consumes: 现有 `CURRENT_SCHEMA_VERSION: u32`（`jsonl.rs:78`）与头行 `_meta.schema_version`（`:104`）。
- Produces:
  ```rust
  // uva-session
  pub const CURRENT_SCHEMA_VERSION: u32 = 2;

  // SessionError 的新变体
  UnsupportedSchemaVersion {
      /// 版本号从哪来：文件头行，或 0（无头的旧文件）
      found: u32,
      /// 这一版支持的版本
      supported: u32,
  },
  ```

**规则（维护者已定，逐条照抄）:**
- 读到 `schema_version == 2` → 正常读。
- 读到 `schema_version == 0`（无头）或 `1` → **拒绝**，报 `UnsupportedSchemaVersion { found, supported: 2 }`。
- 读到 `> 2` → **同样拒绝**，同一个变体（`found` 带上实际值）。理由不同，文案要能区分"更旧"与"更新"。
- 错误文案必须带上：实际读到的版本、这一版支持的版本、下一步（导出或重开会话）。
- **版本号是文件级的**（头行 `_meta.schema_version`），所以混版本文件在打开时就能发现。

- [ ] **Step 1: 写失败的测试**

在 `crates/uva-session/src/jsonl/tests.rs` 加测试：写一个 `schema_version: 1` 的头行 + 一行事件，
用 `JsonlSessionSink::read` 读它，断言返回 `Err(SessionError::UnsupportedSchemaVersion { found: 1, supported: 2 })`；
再写一个 `schema_version: 3` 的，断言同样被拒且 `found == 3`；再写一个 v2 的，断言能读。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-session --lib schema_version`
Expected: FAIL —— v1 今天能读。

- [ ] **Step 3: 升版本并加门禁**

`CURRENT_SCHEMA_VERSION` 改 2。读端在读到头行的 `schema_version` 时与 2 比较，不同就返回
`UnsupportedSchemaVersion`。**无头的文件按 0 处理，同样拒绝。**

- [ ] **Step 4: 重造仓库自己的 fixture**

仓库里凡是写死 `schema_version: 1`（或写无头的）的 fixture 与测试数据改成 2（或让它们走
`SessionMetadataHeader::fresh`）。用 `grep -rn "schema_version" crates/` 找全。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p uva-session --lib && cargo test -p uva-app-server --lib`
Expected: 全绿。

- [ ] **Step 6: 提交**

```bash
git add crates/uva-session crates/uva-app-server
git commit -m "feat(session): version the log format and refuse the versions we cannot read"
```

---

### Task 5: resume 的三种结局分成三个码

**Files:**
- Modify: `crates/uva-app-server/src/routes/follow.rs`（`:886`、`:898`、`:913`、`:924` 四处）
- Modify: `crates/uva-web/src/**`（resume 的错误处理）
- Test: `crates/uva-app-server/src/routes/tests/follow.rs`

**Interfaces:**
- Consumes: 现有的 `not_suspended` / `stale_resume` 两个 code。
- Produces: 三个 code，各自语义固定：
  | code | 什么时候 | HTTP | 客户端该做什么 |
  |---|---|---|---|
  | （无 code，成功） | token 对应的决定**已经应用过** | 200 | 幂等成功，不再报错 |
  | `resume_conflict` | token 的 `last_seq` 落后于日志尾部，或 pending 集合与日志对不上 | 409 | 重新拉状态，再让用户决定 |
  | `resume_no_snapshot` | 日志里根本没有请求快照 | 409 | 明确告知这一轮不能续，建议重开会话 |

- [ ] **Step 1: 写失败的测试**

在 `crates/uva-app-server/src/routes/tests/follow.rs` 加三个测试，分别构造上面三种情形，
断言各自的 code 与 HTTP 状态。第一个测试要断言**成功**而不是错误。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-app-server --lib resume_`
Expected: FAIL —— 现在三种情形都是 `stale_resume` 或 `not_suspended`。

- [ ] **Step 3: 拆码**

`follow.rs` 的四处按上表分开。第一种情形（已应用）现在大概报 `409 not_suspended`（`:886`），
改成幂等成功。

- [ ] **Step 4: 前端跟着改**

`uva-web` 里处理 resume 错误的地方按新 code 分三种反应。改完跑 `make check-web`。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p uva-app-server --lib && make check-web`
Expected: 全绿。

- [ ] **Step 6: 提交**

```bash
git add crates/uva-app-server uva-web
git commit -m "feat(server): tell a client apart from a stale, a spent and a snapshotless resume"
```

---

### Task 6: spawn gate 归 controller 所有

**Files:**
- Modify: `crates/uva-app-server/src/state/app_state/mod.rs`（`spawn_gates` 字段，`:135`）
- Modify: `crates/uva-app-server/src/state/app_state/sessions.rs`（`SessionSpawnGates`，`:446`、`:454`）
- Test: `crates/uva-app-server/src/state/app_state/tests.rs`（或就近的同级 `tests.rs`）

**Interfaces:**
- Consumes: `AppState::spawn_gates: Arc<crate::state::SessionSpawnGates>`（`mod.rs:135`）。
- Produces: gate 由 controller 持有，映射以 `Weak` 引用 controller；controller 被移除时 gate 一起消失。

**背景（已核实）:** `spawn_gates` 与 session controller **同在 `uva-app-server`**，所以这是本 crate
内的局部改动，不跨 crate 边界，也不动任何库 crate 的公开面。

- [ ] **Step 1: 写失败的测试**

加测试：开 N 个会话再全部关掉，断言 gate 表**不增长**（今天它靠 `forget_session` 兜底，
那条路径不覆盖所有移除方式）。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-app-server --lib spawn_gate`
Expected: FAIL —— 表在增长。

- [ ] **Step 3: 改成 controller 持有**

gate 的映射改成以 controller 的 `Weak` 为键，或让 controller 持有 gate 并在 `Drop` 里释放。
**两者选其一**，只要"controller 没了，gate 也没了"成立。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p uva-app-server --lib`
Expected: 全绿。

- [ ] **Step 5: 提交**

```bash
git add crates/uva-app-server
git commit -m "fix(server): let a session's spawn gate die with its controller"
```

---

### Task 7: 文档说实话（依赖面 + 公开面）

**Files:**
- Modify: `crates/uva-harness/README.md`
- Modify: `docs/tutorial/complete-agent.md`
- Modify: `docs/examples/harness-only-consumer/Cargo.toml`（若需要补注释说明它依赖哪几个 crate）
- Modify: `crates/uva-harness/src/lib.rs`（模块文档里说明公开面的分层）

**Interfaces:**
- Consumes: `crates/uva-harness/src/lib.rs` 现有的精选面（**60 个**名字）。
- Produces: 文档里三处一致的说明。

**要写清楚的三件事:**
1. **"只依赖 harness" 需要 4 个 crate。** 消费方要写 `uva-harness` + `uva-core` +
   `uva-provider` + `uva-tool`（外加 `async-trait`），因为 harness **不** re-export
   `Spawner` / `ModelProvider` / `Tool` —— 理由不是会加依赖（harness 本来就依赖 core/provider/tool），而是：被承诺的那张面应该是 harness 自己的面，零运行时的承诺不该被别的 crate 的 feature 集绑住。
   文档给这四个 crate 与各自负责的东西。
2. **公开面分两层。** 精选面（60 个名字）是稳定面；走深路径
   （`uva_harness::agent::…`；`re_act` 是 `pub(crate)`，不在公开面上）够得着的另外 64 个 `pub` 名**不在承诺内**，将来可动。
3. **依赖数要与 `cargo tree` 一致。** 写进文档的依赖数必须等于实际跑出来的数。

- [ ] **Step 1: 写下新的说明**

三处都改成上面这三条。harness 的 crate 级文档里加一句"本 crate 不 re-export provider/tool 的
trait，它们是独立 crate 的"，并说明为什么。

- [ ] **Step 2: 核实依赖数**

Run: `cargo tree --manifest-path docs/examples/harness-only-consumer/Cargo.toml`
把实际结果与文档里写的数对齐。

- [ ] **Step 3: 跑门禁**

Run: `make check-claim-language && make check-web && cargo doc --no-deps -p uva-harness`
Expected: 全绿（`cargo doc` 的 `-D warnings` 由 `make doc-check` 覆盖，可一并跑）。

- [ ] **Step 4: 提交**

```bash
git add crates/uva-harness/README.md crates/uva-harness/src/lib.rs docs
git commit -m "docs: say what a harness consumer actually depends on"
```

---

### Task 8: 转场边界规则去重 + 旧 park 行的处置

**Files:**
- Modify: `crates/uva-app-server/src/routes/follow.rs`（`:323-338` 附近重复实现的边界规则）
- Modify: `crates/uva-session/src/repair.rs`（`parked_prompts`）
- Test: `crates/uva-app-server/src/routes/tests/follow.rs`、`crates/uva-session/src/repair/tests.rs`

**Interfaces:**
- Consumes: `uva_harness::approval::suspended_segment`（唯一真源）。
- Produces: `follow.rs` 不再自带一份边界规则。

- [ ] **Step 1: 写失败的测试**

加一个测试：同一段日志，`follow` 算出来的"最后一段被挂起的对话"与
`uva_harness::approval::suspended_segment` 算出来的一致。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-app-server --lib suspended_segment`
Expected: FAIL —— 两份实现不一致（或测试证明它们今天一致但代码重复；那就让测试锁住"只有一份实现"）。

- [ ] **Step 3: 删掉重复实现**

`follow.rs` 改用 `suspended_segment`，本地的那份删掉。

- [ ] **Step 4: 处置旧 park 行**

`repair::parked_prompts` 现在把没有 `park_id` 的 `ParkedPrompt`（`events.rs:635`）按**序号**认。
**处置：弃用 + 明说** —— 读端遇到没有 `park_id` 的 park 行时，在日志里 `warn!` 一次并跳过它，
不做位置推断（位置推断会在别的行插进来时指错）。文档里写明"这一版的日志需要 `park_id`"。

- [ ] **Step 5: 跑测试确认通过**

Run: `cargo test -p uva-app-server --lib && cargo test -p uva-session --lib`
Expected: 全绿。

- [ ] **Step 6: 提交**

```bash
git add crates/uva-app-server crates/uva-session
git commit -m "refactor(server): keep one turn-boundary rule and retire keyless park rows"
```

---

### Task 9: uva-resilience 与 uva-tool-memo 的测试变绿

**Files:**
- Modify: `crates/uva-resilience/Cargo.toml`（`tokio` 的 feature）
- Modify: `crates/uva-resilience/src/provider.rs`（`:393` 附近）
- Modify: `crates/uva-tool-memo/Cargo.toml`（`tokio` 的 feature）
- Modify: `crates/uva-tool-memo/src/tests.rs`（`:304` 附近）

**Interfaces:**
- Consumes: 无（独立的既有欠债）。
- Produces: 两个 crate 的 `cargo test -p <crate>` 退出 0。

**背景（已核实）:** 两个 crate 都报 `error[E0433]: cannot find 'time' in 'tokio'`，而它们的
`tokio` 依赖只声明了 `["macros", "rt", "rt-multi-thread"]`。本分支对这两个 crate 的 diff 是空的，
它们在 `master` 上就是红的。

**修法必须遵守 AGENTS.md 的 tokio 规则:** 先看 `tokio::time` 用在哪 ——
- 只在**测试**里用 → 把 `tokio` 挪到 `[dev-dependencies]`，生产依赖里不留。
- 在**生产代码**里用 → 给 `tokio` 的 feature 列表加 `"time"`，并附一行原因注释。

- [ ] **Step 1: 复现失败**

Run: `cargo test -p uva-resilience -p uva-tool-memo`
Expected: 两个都编译失败，报 `cannot find 'time' in 'tokio'`。

- [ ] **Step 2: 定位 `tokio::time` 的用处**

Run: `grep -rn "tokio::time" crates/uva-resilience/src crates/uva-tool-memo/src`
看清是生产代码还是测试代码。

- [ ] **Step 3: 按规则修**

按 Step 2 的结论二选一改 `Cargo.toml`（必要时改代码）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p uva-resilience && cargo test -p uva-tool-memo`
Expected: 退出 0。

- [ ] **Step 5: 提交**

```bash
git add crates/uva-resilience crates/uva-tool-memo
git commit -m "fix(resilience): declare the tokio features the crate actually uses"
```

---

### Task 10: uva-budget 与 uva-semantic-cache 的 doctest 变绿

**Files:**
- Modify: `crates/uva-budget/src/lib.rs`（crate 级 doctest）
- Modify: `crates/uva-semantic-cache/src/lib.rs`（crate 级 doctest）

**Interfaces:**
- Consumes: `LatencyBudget::new` 的当前签名；`SemanticCacheProvider::new` 的当前签名。
- Produces: 两个 crate 的 `cargo test -p <crate>` 退出 0（doctest 会被跑到）。

**背景（已核实）:**
- `uva-budget` 的 crate 级 doctest 还在调 `LatencyBudget::new(one_arg)`，并把
  `Arc<Arc<StaticPricingTable>>` 传给要 `Arc<dyn PricingTable>` 的地方。
- `uva-semantic-cache` 的 crate 级 doctest 在一个**没有开** `uva_provider` 的 feature 组合下
  `use uva_provider::AnthropicProvider`，并用 5 个参数里的 4 个调 `SemanticCacheProvider::new`
  （缺 `Arc<dyn Clock>`）。

- [ ] **Step 1: 复现失败**

Run: `cargo test -p uva-budget && cargo test -p uva-semantic-cache`
Expected: 两个都失败（doctest 编译不过）。

- [ ] **Step 2: 读当前签名并改 doctest**

读 `LatencyBudget::new` 与 `SemanticCacheProvider::new` 的真实签名，把 doctest 改成对的调用。
`semantic-cache` 那条：如果 doctest 需要 provider，就把 doctest 标成
`#[cfg(feature = "provider")]` 门起来，或者在该 crate 的 feature 组合里补上。

- [ ] **Step 3: 跑测试确认通过**

Run: `cargo test -p uva-budget && cargo test -p uva-semantic-cache`
Expected: 退出 0。

- [ ] **Step 4: 提交**

```bash
git add crates/uva-budget crates/uva-semantic-cache
git commit -m "fix(budget): bring the crate doctests back in line with the signatures"
```

---

### Task 11: 公开 API 冻结 —— 承诺清单 + 门禁

**Files:**
- Create: `docs/api-frozen.md`（承诺清单，人读）
- Create: `scripts/check-frozen-api.sh`（门禁，机器读）
- Create: `scripts/frozen-api/baseline`（冻结的名字清单，机器读）
- Modify: `Makefile`（加 `check-frozen-api` 目标；并把它加进 `ci`）
- Modify: `scripts/paradigm-surface/baselines/*`（若冻结面覆盖到范式 crate）

**Interfaces:**
- Consumes: `crates/uva-harness/src/lib.rs` 的精选面（60 个名字）；七个范式 crate 的精选面。
- Produces:
  - `docs/api-frozen.md`：人读的承诺 —— 哪些名字稳定、承诺到哪个版本、改动要走过什么流程。
  - `scripts/frozen-api/baseline`：每行一个冻结的公开名，形如 `uva-harness::ReActAgent`。
  - `scripts/check-frozen-api.sh`：读每个冻结 crate 的公开名，**新增**了 baseline 里没有的名字 → 退出非 0，
    错误信息点名新增的是哪个。

**范围（维护者已定）:**
- 只覆盖**精选面**（外部能 `use` 到的名字）。深路径**不**冻结。
- 不覆盖 server 的 HTTP 面。
- 冻结基线在 Task 1–8 **全部落地之后**才生成（先冻结再改等于第一波就违约）。

- [ ] **Step 1: 生成基线**

从每个冻结 crate 的 `lib.rs` 精选面导出名字（格式 `crate::Name`），写进
`scripts/frozen-api/baseline`。**在 Task 8 完成之后**生成 —— 不要提前。

- [ ] **Step 2: 写门禁脚本**

`scripts/check-frozen-api.sh`：
- 读 `scripts/frozen-api/baseline`；
- 对每个冻结 crate，列出它当前精选面的名字；
- 有 baseline 里没有的名字 → 打印 `NEW: crate::Name` 并退出 1；
- baseline 里有、现在没有的名字 → 打印 `REMOVED: crate::Name` 并退出 1（**移除也是违约**）。

- [ ] **Step 3: 写承诺文档**

`docs/api-frozen.md`：列出冻结的名字（可以从 baseline 生成，但正文要人读）、承诺到哪个版本、
改动流程、明确说深路径不在承诺内。语言用**英文**（面向外部）。

- [ ] **Step 4: 接到 Makefile**

加 `check-frozen-api` 目标，并加进 `ci` 依赖。脚本要过 shellcheck（`make lint-sh` 的名单里要加上它）。

- [ ] **Step 5: 验证门禁真的会拦**

临时在某个冻结 crate 的 `lib.rs` 里加一个 `pub` 名字，跑门禁，确认它失败并点名；
然后撤掉。

- [ ] **Step 6: 提交**

```bash
git add docs/api-frozen.md scripts/check-frozen-api.sh scripts/frozen-api Makefile
git commit -m "feat: freeze the curated public surface and gate new names"
```

---

### Task 12: 交接材料三件套

**Files:**
- Create: `docs/api-surface.md`（新用户视角的公开面清单）
- Create: `docs/log-format.md`（格式兼容说明）
- Create: `docs/contributing/first-steps.md`（外部贡献者第一份材料）
- Modify: `crates/uva-harness/README.md`（指向上面三份）

**Interfaces:**
- Consumes: Task 11 的 `docs/api-frozen.md` 与 baseline；Task 4 的版本规则。
- Produces: 三份英文文档。

- [ ] **Step 1: 写 `docs/api-surface.md`**

新用户视角：这个 crate 给你什么、哪些名字你该依赖、哪些是实验性的。
（内容可由 `docs/api-frozen.md` 派生，但视角不同：一份是承诺，一份是导览。）

- [ ] **Step 2: 写 `docs/log-format.md`**

v1 与 v2 各是什么形状、碰到旧版本会怎样（**拒绝**，错误长什么样）、为什么不做迁移、
往后改格式的路径是什么。

- [ ] **Step 3: 写 `docs/contributing/first-steps.md`**

从 clone 到跑通一条 turn：装什么、跑什么命令、看到什么输出算成功、零 tokio 那条路怎么走。
**每条命令都要是你实际跑过的**，输出照抄。

- [ ] **Step 4: 接进 harness 的 README**

`crates/uva-harness/README.md` 里链到这三份。

- [ ] **Step 5: 跑门禁**

Run: `make check-claim-language && make check-web`
Expected: 全绿。

- [ ] **Step 6: 提交**

```bash
git add docs crates/uva-harness/README.md
git commit -m "docs: hand the harness to the next reader"
```

---

### Task 13: 重跑验收并更新验收记录

**Files:**
- Modify: `docs/design/2026-10-26-harness-v4-acceptance.md`
- Modify: `Makefile`（把 `test-crates` 加进 `ci`）
- Modify: `CHANGELOG.md`

**Interfaces:**
- Consumes: 前十二项任务的一切产出。
- Produces: 一份按项列出的收口验收记录。

- [ ] **Step 1: 把 `make test-crates` 加进 `make ci`**

`ci` 现在看不见 doctest 与四个 crate 的失败（验收记录 §4 的第 3 条）。加进去，
并确认 `make ci` 仍然退出 0。

- [ ] **Step 2: 跑全部门禁**

```bash
make ci
make check-web
make examples
make bench-check
make test-crates
```
五个都要退出 0。**任何一个不是 0，都要写进验收记录并说明为什么**，不许改门禁让它变绿。

- [ ] **Step 3: 重跑端到端场景**

日志格式已经升到 v2，**场景 A–I 必须重跑一遍**（v1 的日志现在会被拒绝）。
按 `docs/design/2026-10-26-harness-v4-acceptance.md` 里的原样命令跑，每条留一条证明输出。
特别要验：**v1 日志被拒绝时，错误文案是不是可操作的**（Task 4 的验收点）。

- [ ] **Step 4: 改写验收记录**

把现有记录改成"按项列出"的收口记录：每一项遗留一行，写明它的不变量、证据（测试名或命令）、
结果。保留原来那次运行的证据（那是发现问题的记录），但结论行要更新成现在的状态。

- [ ] **Step 5: 写 CHANGELOG**

在 `[Unreleased]` 里写这一轮：格式升到 v2（**旧日志不再被读取**）、公开 API 冻结、
八项遗留的修法。

- [ ] **Step 6: 提交**

```bash
git add Makefile docs/design/2026-10-26-harness-v4-acceptance.md CHANGELOG.md
git commit -m "docs: record the harness v4 remainders acceptance"
```

---

## 自查

**Spec 覆盖：** G1 → Task 1–10；G2 → Task 4；G3 → Task 11；G4 → Task 12；G5 → Task 9、10、13；
G6 → Task 13。设计文档 §6.2 的八项里，**B（awaiting_approval 不吞错）已在本分支完成** ——
`inner.rs:348` 现在用 `try_open_interrupts().await?` 传播，剩下只有 status 用途的
`open_interrupts`（`:305`）按设计吞掉（它没有错误通道，且 run 路径走的是另一条）。
所以本计划只做七项，不是八项。

**步骤粒度：** 每个代码步骤都给了确切的文件、签名、取值；每个测试步骤给了测试名与断言。
没有"处理边界情况"这类空步骤。

**类型一致性：** `key_of` 的签名在 Task 2 定义，Task 3 用它；`UnsupportedSchemaVersion` 在
Task 4 定义，Task 13 用它；三个 resume code 在 Task 5 定义，前端同任务里用。

**Review Focus：** 五条分别落在 Task 1、4、6、5、4 —— 都有对应的测试步骤。

**比例：** 十三项任务，每项 5–7 步，计划长度约为设计文档的 1.6 倍。
