# Harness V4 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `uva-harness` 变成依赖闭包零 tokio 的真基座，落地"静态 Rust 插件 + 动态跨语言插件"双通道与运行时组合层，并把 `uva-app-server` 改造成计算节点无状态。

**Architecture:** 三段有依赖顺序的工程。W1 先把 harness 的 tokio 依赖闭包清零（新增 `Sleeper` / `Blocking` 两个 runtime-free 接缝，tokio 实现进兄弟 crate `uva-harness-tokio`），再做泛型核心化与公开面收窄。W2 新增两个 crate：`uva-plugin-protocol`（serde-only、零运行时、不依赖 harness）与 `uva-plugin-host`（把远端进程的能力翻译成 `Host` 注册），并把"这次部署带哪些能力"从 45 条 cargo feature 搬到 config 驱动的运行时组合层。W3 把 server 的会话真源收敛到 workspace 上的 JSONL 日志，排队中的多模态载荷与 pending resume 落盘，`follow` 基线改尾读。

**Tech Stack:** Rust 1.98、edition 2024、`futures`、`serde` / `serde_json`、`thiserror`（库）/`anyhow`（server）。不新增第三方依赖，不给 `uva-harness` 加任何 cargo feature。

**Spec:** `docs/design/2026-10-26-harness-v4-design.md`
**Supporting evidence:** `docs/research/harness-comparison-pi-dsh-2026-10-25.md`、`docs/design/2026-10-25-harness-v3-design.md`

## Global Constraints

- 库 crate 只用 `thiserror`；`uva-app-server` 是 app crate，用 `anyhow`。
- 依赖版本一律 `major.minor`；新依赖只能来自根 `Cargo.toml` 的 `[workspace.dependencies]`。
- `max_width = 80`；禁通配符导入；`uva-harness` 生产代码无函数 >100 行、无嵌套 >4 层。
- 库代码不得直接 `chrono::Utc::now()`；墙钟走 `uva_core::Clock`，ID 走 `uva_core::IdGen`。
- 库 crate 的公开 API 不得出现具体运行时类型（`tokio::` 等）。
- `uva-harness` 不得有 `[features]` 表，不得依赖 tokio。
- 测试按 crate 分批执行，禁止 `cargo test --workspace`。
- 破坏性 API 一次切干净，不留 shim、别名或兼容分支。
- 每个任务结束：工作区能编译、`cargo +nightly fmt --all` 干净、该 crate 测试绿。
- 提交信息用英文祈使句。

## Review Focus

1. **provider 的 `tokio` 依赖边没被真正移除。** 期望：`cargo tree -p uva-harness -e normal` 里零 tokio 条目 —— gate 掉代码不算，必须动 manifest。检查在 Task 4 Step 5。
2. **去掉 normal 侧的 `uva-core/tokio-util` 后 server 编译不过。** 期望：server 显式取得该 feature 或改用 `AtomicCancelToken`，不靠 provider/steering 搭便车。处理在 Task 4 Step 4。
3. **远端插件用 `destructive_hint: false` 躲掉审批。** 期望：远端工具的 risk 由宿主策略决定，插件自报的注解只作展示。测试在 Task 13。
4. **远端 transformer 改写工具结果却不落审计行。** 期望：日志里有 `tool_result_transformed`，崩溃恢复后重放得到相同结果。测试在 Task 13。
5. **排队中的多模态 prompt 随节点崩溃丢失。** 期望：字节进 attachment store、日志存可回读 ref、重启后 prompt 仍在。测试在 Task 16。

---

## File Structure

**uva-core**

- Create `src/sleeper.rs`：`Sleeper` trait、`ThreadSleeper`、`FixedSleeper`（测试用）。
- Create `src/blocking.rs`：`Blocking` trait、`ThreadBlocking`。
- Modify `src/lib.rs`：导出两个 trait 与默认实现。

**uva-harness-tokio**

- Modify `src/lib.rs`：加 `TokioSleeper`、`TokioBlocking`（与 `TokioSpawner` 同款先例）。

**uva-session**

- Create `src/fs.rs`：把 `jsonl.rs` 的 fs 读写搬到 runtime-free 形态（`Blocking` 接缝驱动）。
- Modify `src/compaction.rs`：`SummaryCompactor` 的超时改走 `Sleeper`，去掉 `tokio::select!`。
- Modify `src/manager.rs`：`InputQueue` 移出（挪去 server）。
- Modify `src/search/backend.rs`：fs 读改走 `Blocking`。
- Modify `src/jsonl.rs`：改用 `src/fs.rs`。
- Modify `src/lib.rs`：导出变化。

**uva-provider**

- Modify `Cargo.toml`：`tokio` 改 `optional`，`anthropic` / `openai` 各自开它。
- Modify `src/retry.rs`：删两个死 async 函数；剩下的 gate 到 adapter feature。
- Modify `src/streaming/idle_watchdog.rs`：无条件项与 adapter feature 对齐。
- Modify `src/lib.rs`：删两个死 re-export。

**uva-steering**

- Modify `Cargo.toml`：`tokio-util` 与 `uva-core/tokio-util` 移到 `[dev-dependencies]`。

**uva-harness**

- Modify `src/agent/caps.rs`：`ModelCaps` 泛型化。
- Modify `src/agent/re_act/agent.rs`：`ReActAgent` 泛型化（增量，旧签名不变）。
- Modify `src/prompt/mod.rs`：渲染 host 的 prompt sections。
- Modify `src/agent/re_act/loop/tools/route.rs`、`guard.rs`：消除 `call.input` 深拷贝。
- Modify `src/lib.rs`：导出变化。

**uva-plugin-protocol**（新 crate）

- Create `src/lib.rs`：方法名常量、能力协商、错误码、wire 类型。
- Create `src/wire.rs`：wire 事件与决策类型。
- Modify `Cargo.toml`。

**uva-plugin-host**（新 crate）

- Create `src/lib.rs`：`RemotePlugin`、`PluginRegistry`。
- Create `src/process.rs`：进程管理。
- Create `src/client.rs`：JSON-RPC 客户端。
- Create `src/remote_tool.rs`：远端 `Tool` 实现（含 risk 覆写）。

**uva-app-server**

- Create `src/session/input_queue.rs`：从 session 挪来的 `InputQueue`。
- Create `src/composition.rs`：运行时组合层（config → 插件/工具装配）。
- Modify `src/config/server.rs`：新增 `[plugins]` 配置段。
- Modify `src/routes/follow.rs`：基线改尾读。
- Modify `src/session/controller/inner.rs`：`pending_multimodal` 落盘。

**docs/examples**

- Create `harness-only-consumer/`：只依赖 `uva-harness` 的零 tokio 消费方。

---

## Task 1: `uva-core` 的 `Sleeper` 接缝

**Files:**
- Create: `crates/uva-core/src/sleeper.rs`
- Modify: `crates/uva-core/src/lib.rs`
- Test: `crates/uva-core/src/sleeper/tests.rs`

**Interfaces:**
- Consumes: 无（首个任务）
- Produces:
  ```rust
  // uva_core::sleeper
  pub trait Sleeper: Send + Sync + 'static {
      fn sleep(&self, duration: std::time::Duration)
          -> futures::future::BoxFuture<'static, ()>;
  }
  #[derive(Debug, Clone, Default)]
  pub struct ThreadSleeper;
  impl Sleeper for ThreadSleeper { /* std::thread::sleep + oneshot */ }
  #[derive(Debug, Clone)]
  pub struct FixedSleeper;  // 立即返回，测试用
  impl Sleeper for FixedSleeper { /* 立刻 resolve */ }
  ```

- [ ] **Step 1: 写失败的测试**

在 `crates/uva-core/src/sleeper/tests.rs` 建文件，内容：

```rust
use std::time::{Duration, Instant};

use super::{FixedSleeper, Sleeper, ThreadSleeper};

#[test]
fn fixed_sleeper_resolves_immediately() {
    let start = Instant::now();
    futures::executor::block_on(FixedSleeper.sleep(Duration::from_secs(3600)));
    assert!(start.elapsed() < Duration::from_millis(50));
}

#[test]
fn thread_sleeper_waits_at_least_the_requested_duration() {
    let start = Instant::now();
    futures::executor::block_on(ThreadSleeper.sleep(Duration::from_millis(30)));
    assert!(start.elapsed() >= Duration::from_millis(25));
}

#[test]
fn sleeper_is_usable_as_a_trait_object() {
    let sleeper: std::sync::Arc<dyn Sleeper> = std::sync::Arc::new(ThreadSleeper);
    futures::executor::block_on(sleeper.sleep(Duration::from_millis(1)));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-core sleeper --lib`
Expected: FAIL —— `cannot find module or type sleeper`（模块还不存在）

- [ ] **Step 3: 实现 `crates/uva-core/src/sleeper.rs`**

文件头写 `//! Runtime-neutral delay seam.`。定义上面 Interfaces 里的三个类型。

`ThreadSleeper::sleep` 的实现要点：用 `std::thread::spawn` 睡满，然后通过
`futures::channel::oneshot` 把完成信号送回调用方；返回的 future 等待那个 oneshot。
**不要**在 async fn 里直接 `std::thread::sleep` —— 那会把等待线程占住。

`FixedSleeper::sleep` 立刻返回 ready future。

`lib.rs` 加 `pub mod sleeper;` 与
`pub use sleeper::{FixedSleeper, Sleeper, ThreadSleeper};`（与其他模块的导出一致）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p uva-core sleeper --lib`
Expected: PASS，3 个用例全绿

- [ ] **Step 5: 提交**

```bash
git add crates/uva-core/src/sleeper.rs crates/uva-core/src/sleeper/tests.rs crates/uva-core/src/lib.rs
git commit -m "feat(core): add runtime-neutral Sleeper seam"
```

---

## Task 2: `uva-core` 的 `Blocking` 接缝

**Files:**
- Create: `crates/uva-core/src/blocking.rs`
- Modify: `crates/uva-core/src/lib.rs`
- Test: `crates/uva-core/src/blocking/tests.rs`

**Interfaces:**
- Consumes: 无
- Produces:
  ```rust
  // uva_core::blocking
  pub trait Blocking: Send + Sync + 'static {
      /// Run `f` on a blocking thread; the returned future resolves
      /// when `f` has completed. The closure's own return value is
      /// carried by the caller (a oneshot channel), keeping the
      /// trait object-safe.
      fn run(
          &self,
          f: Box<dyn FnOnce() + Send + 'static>,
      ) -> futures::future::BoxFuture<'static, ()>;
  }
  #[derive(Debug, Clone, Default)]
  pub struct ThreadBlocking;
  impl Blocking for ThreadBlocking { /* std::thread::spawn + oneshot */ }
  ```

**注意**：`uva_core::spawn::Spawner` 已经有 `spawn_blocking`，但它是
fire-and-forget（返回值被擦掉）。`Blocking` 与它的区别是**可 await** —— session 的 fs 写需要
拿到 `Result`，所以必须有完成信号。两者并存不冲突：`Spawner::spawn_blocking` 的文档已经说明
"a caller that needs the return value hands it out through a channel of its own"。

- [ ] **Step 1: 写失败的测试**

在 `crates/uva-core/src/blocking/tests.rs` 建文件：

```rust
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use super::{Blocking, ThreadBlocking};

#[test]
fn thread_blocking_runs_the_closure() {
    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    futures::executor::block_on(ThreadBlocking.run(Box::new(move || {
        flag.store(true, Ordering::SeqCst);
    })));
    assert!(ran.load(Ordering::SeqCst));
}

#[test]
fn the_closure_may_carry_its_own_result_channel() {
    let (tx, rx) = futures::channel::oneshot::channel::<u32>();
    futures::executor::block_on(ThreadBlocking.run(Box::new(move || {
        let _ = tx.send(42);
    })));
    assert_eq!(futures::executor::block_on(rx), Ok(42));
}

#[test]
fn blocking_is_usable_as_a_trait_object() {
    let blocking: Arc<dyn Blocking> = Arc::new(ThreadBlocking);
    futures::executor::block_on(blocking.run(Box::new(|| {})));
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-core blocking --lib`
Expected: FAIL —— 找不到 `blocking` 模块

- [ ] **Step 3: 实现 `crates/uva-core/src/blocking.rs`**

按 Interfaces 的签名实现。`ThreadBlocking::run` 用 `std::thread::spawn` 跑闭包，跑完后
通过 oneshot 发出完成信号；返回的 future 等这个信号。**闭包必须真的在线程上跑** ——
不能退回 `Spawner` 那种"在 executor 上跑"的做法，否则 fs 写会堵住 worker 线程。

`lib.rs` 加 `pub mod blocking;` 与
`pub use blocking::{Blocking, ThreadBlocking};`。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p uva-core blocking --lib`
Expected: PASS，3 个用例全绿

- [ ] **Step 5: 提交**

```bash
git add crates/uva-core/src/blocking.rs crates/uva-core/src/blocking/tests.rs crates/uva-core/src/lib.rs
git commit -m "feat(core): add awaitable Blocking seam"
```

---

## Task 3: `uva-harness-tokio` 的 tokio 实现

**Files:**
- Modify: `crates/uva-harness-tokio/src/lib.rs`
- Modify: `crates/uva-harness-tokio/Cargo.toml`
- Test: `crates/uva-harness-tokio/tests/tokio_seams.rs`

**Interfaces:**
- Consumes: `uva_core::sleeper::Sleeper`、`uva_core::blocking::Blocking`（Task 1、2）
- Produces:
  ```rust
  // uva_harness_tokio
  pub struct TokioSleeper;
  impl Sleeper for TokioSleeper { /* tokio::time::sleep */ }
  pub struct TokioBlocking;
  impl Blocking for TokioBlocking { /* tokio::task::spawn_blocking */ }
  ```

- [ ] **Step 1: 写失败的测试**

建 `crates/uva-harness-tokio/tests/tokio_seams.rs`：

```rust
use std::time::{Duration, Instant};

use uva_core::blocking::Blocking;
use uva_core::sleeper::Sleeper;
use uva_harness_tokio::{TokioBlocking, TokioSleeper};

#[tokio::test]
async fn tokio_sleeper_waits() {
    let start = Instant::now();
    TokioSleeper.sleep(Duration::from_millis(30)).await;
    assert!(start.elapsed() >= Duration::from_millis(25));
}

#[tokio::test]
async fn tokio_blocking_runs_off_the_worker_thread() {
    let (tx, rx) = tokio::sync::oneshot::channel::<u32>();
    TokioBlocking.run(Box::new(move || {
        let _ = tx.send(7);
    })).await;
    assert_eq!(rx.await.unwrap(), 7);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-harness-tokio --test tokio_seams`
Expected: FAIL —— `unresolved import uva_harness_tokio::TokioSleeper`

- [ ] **Step 3: 实现两个适配器**

`Cargo.toml` 的 `tokio` feature 从 `["rt"]` 改成 `["rt", "time"]`（`time` 是 `tokio::time::sleep`
需要的）。`lib.rs` 加两个类型与两个 impl，形状照 `TokioSpawner`：

```rust
#[derive(Debug, Default)]
pub struct TokioSleeper;

impl Sleeper for TokioSleeper {
    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(duration))
    }
}

#[derive(Debug, Default)]
pub struct TokioBlocking;

impl Blocking for TokioBlocking {
    fn run(&self, f: Box<dyn FnOnce() + Send + 'static>) -> BoxFuture<'static, ()> {
        Box::pin(async move {
            drop(tokio::task::spawn_blocking(f).await);
        })
    }
}
```

`BoxFuture` 从 `uva_core::spawn::BoxFuture` 取（该 crate 已经导出了它）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p uva-harness-tokio --test tokio_seams`
Expected: PASS，2 个用例全绿

- [ ] **Step 5: 提交**

```bash
git add crates/uva-harness-tokio/src/lib.rs crates/uva-harness-tokio/Cargo.toml crates/uva-harness-tokio/tests/tokio_seams.rs
git commit -m "feat(harness-tokio): add TokioSleeper and TokioBlocking"
```

---

## Task 4: provider / steering 去掉 normal 侧的 tokio 边

**Files:**
- Modify: `crates/uva-provider/Cargo.toml`
- Modify: `crates/uva-provider/src/retry.rs`
- Modify: `crates/uva-provider/src/streaming/idle_watchdog.rs`
- Modify: `crates/uva-provider/src/lib.rs`
- Modify: `crates/uva-steering/Cargo.toml`
- Modify: `crates/uva-app-server/Cargo.toml`
- Test: `crates/uva-provider/tests/tokio_edges.rs`

**Interfaces:**
- Consumes: 无
- Produces: 无新公开 API（这是依赖边清理）

**关键**：`uva-provider` 的 `tokio` 依赖是 **normal 无条件依赖**（`Cargo.toml:30`），
与 `anthropic` / `openai` feature 无关 —— 而 harness 的树里那两个 feature 是关的。
**只 gate 代码不去掉依赖边，`cargo tree` 里 tokio 还在。**

- [ ] **Step 1: 写失败的测试**

建 `crates/uva-provider/tests/tokio_edges.rs` —— 真正的门禁是 **Step 5 的 `cargo tree`
检查**（依赖边只能从 manifest 上看出来）。测试文件的作用是**守住那些不依赖边也守不住的
语义**：无 adapter 时 retry 的分类逻辑仍然可用、仍然被导出。

```rust
// crates/uva-provider/tests/tokio_edges.rs
use uva_provider::retry::{RetryClass, classify_error, is_retryable_error};

#[test]
fn retry_classifiers_need_no_adapter() {
    assert!(is_retryable_error(429));
    assert!(!is_retryable_error(400));
    let _ = RetryClass::Transient;
}

#[test]
fn error_classification_works_without_an_adapter() {
    // `classify_error` takes the crate's own `Error` type, which must
    // be constructible without any adapter feature enabled.
    let classified = classify_error(&uva_provider::Error::Internal {
        message: "boom".into(),
    });
    assert!(matches!(
        classified,
        RetryClass::Transient | RetryClass::Unknown
    ));
}
```

`cargo test -p uva-provider --no-default-features --test tokio_edges`
必须能跑通 —— 这本身就是"无 adapter 时 crate 仍自洽"的证据。

**不要**写 `assert!(true)` 之类的占位断言：它不证明任何东西，却会让这条测试看起来
在保护什么。依赖边的证明在 `cargo tree` 那一行命令，不在测试里。

- [ ] **Step 2: 跑测试确认现状**

Run: `cargo test -p uva-provider --no-default-features --test tokio_edges`
Expected: PASS（纯函数本来就与 adapter 无关）—— 这一步是**记录基线**，不是确认失败。
真正的失败在 Step 5。

- [ ] **Step 3: 改 `uva-provider/Cargo.toml`**

把 `tokio = { workspace = true, features = ["macros", "time"] }` 改成
`tokio = { workspace = true, features = ["macros", "time"], optional = true }`，
并让两个 adapter feature 各自带上它：

```toml
anthropic = ["dep:reqwest", "dep:base64", "dep:tokio", "uva-core/reqwest"]
openai = ["dep:reqwest", "dep:base64", "dep:tokio", "uva-core/reqwest"]
```

`tokio-util`（`Cargo.toml:31`）同样改 `optional = true`，只有
`anthropic` / `openai` 需要它（生产代码里唯一用处是 `idle_watchdog.rs` 的测试）。
`uva-core` 的 `features = ["tokio-util"]`（`:15`）从 normal 侧移除 —— 移到
`[dev-dependencies]`。

- [ ] **Step 4: 改代码与 `uva-steering/Cargo.toml`**

`crates/uva-steering/Cargo.toml`：`tokio-util.workspace = true` 与
`uva-core = { workspace = true, features = ["tokio-util"] }` 从 `[dependencies]`
移到 `[dev-dependencies]`。

`crates/uva-provider/src/retry.rs`：
- **删除** `retry_with_backoff`（`:97`）与 `retry_with_retry_after`（`:145`）——
  全仓零调用点，只剩 `lib.rs:101,103` 的 re-export。注意
  `uva-resilience/src/provider.rs:374` 有一个**同名但无关**的
  `retry_with_backoff`，不要动它。
- `retry_with_classification`（`:499`）保留，但函数体里的 `tokio::time::sleep`
  换成 `Sleeper` 接缝：给 `SummaryCompactor` 那类调用方一个 `Arc<dyn Sleeper>`
  参数。**若判断改动过大，可先只把它 `#[cfg(any(feature = "anthropic", feature = "openai"))]`
  gate 起来** —— 它只被两个 adapter 调用，这样零依赖。
  选定 `cfg` 方案时：`lib.rs` 里对应的 re-export 也要同样 gate。

`crates/uva-provider/src/lib.rs`：删 `retry_with_backoff` 与
`retry_with_retry_after` 的 re-export。

`crates/uva-provider/src/streaming/idle_watchdog.rs`：
- 四个 tokio-free 导出（`DEFAULT_STREAM_IDLE_TIMEOUT` / `IDLE_TIMEOUT_MARKER` /
  `idle_timeout_error` / `is_idle_timeout`）**保持无条件** —— 它们与 adapter 无关。
- 里面的 tokio 项已经逐项 gate 过（`:16,62,69,135,178`），检查是否还有漏网的无条件
  tokio 引用；`tokio-util` 的测试 import 在 `:221` 的 `#[cfg(all(test, ...))]` 内，
  改成 `#[cfg(test)]` 即可（测试总是开着）。

**`uva-app-server` 的连带处理**：`crates/uva-app-server/Cargo.toml` 目前**没有**请求
`uva-core/tokio-util` —— 它是搭 provider / steering 的便车拿到的。而
`crates/uva-app-server/src/session/controller/run_stream.rs:24` 的生产代码要
`Arc<tokio_util::sync::CancellationToken>`。两个选择：
(a) server 显式请求（facade 上加一条 feature 转发 `uva-core/tokio-util`）；
(b) 改用 runtime-free 的 `AtomicCancelToken`（`workflow_host.rs:80` 已经这么用）。
**选 (b)** —— server 是 app crate 但不该为了一个取消令牌多背一条 feature，
且 `AtomicCancelToken` 已经是既有的 runtime-free 等价物。
`run_stream.rs` 里凡是 `tokio_util::sync::CancellationToken` 都要换成
`uva_core::cancel::AtomicCancelToken`（注意 `run_stream.rs:97` 把它传给
`agent.run(input, cancel)`，后者收 `Arc<dyn CancelToken>` —— `AtomicCancelToken`
实现了这个 trait）。

- [ ] **Step 5: 验证依赖边真的没了**

Run:
```bash
cargo tree -p uva-harness -e normal | grep -c tokio
```
Expected: `0`

若不为 0，用 `cargo tree -p uva-harness -e normal -i tokio` 找到还剩哪条边。

再跑：
```bash
cargo tree -p uva-provider -e normal --no-default-features | grep -c tokio
```
Expected: `0`

- [ ] **Step 6: 跑相关 crate 的测试**

Run:
```bash
cargo test -p uva-provider --lib
cargo test -p uva-steering --lib
cargo test -p uva-app-server --lib
```
Expected: 全绿。`uva-app-server` 若因 `CancellationToken` 换类型而编译失败，
按 Step 4 的 (b) 方案改完再跑。

- [ ] **Step 7: 提交**

```bash
git add crates/uva-provider crates/uva-steering crates/uva-app-server
git commit -m "fix(provider): drop normal-side tokio dependency edges"
```

---

## Task 5: session 的 runtime-free 化

**Files:**
- Create: `crates/uva-session/src/fs.rs`
- Modify: `crates/uva-session/src/jsonl.rs`
- Modify: `crates/uva-session/src/compaction.rs`
- Modify: `crates/uva-session/src/search/backend.rs`
- Modify: `crates/uva-session/src/manager.rs`
- Modify: `crates/uva-session/src/lib.rs`
- Create: `crates/uva-session/src/fs/tests.rs`
- Test: `crates/uva-session/tests/runtime_free.rs`

**Interfaces:**
- Consumes: `uva_core::sleeper::Sleeper`、`uva_core::blocking::Blocking`（Task 1、2）
- Produces:
  ```rust
  // uva_session::fs
  pub struct FsBackend { blocking: Arc<dyn Blocking> }
  impl FsBackend {
      pub fn new(blocking: Arc<dyn Blocking>) -> Self;
      pub async fn read_file(&self, path: &Path) -> Result<Vec<u8>, String>;
      pub async fn append_lines(
          &self,
          path: &Path,
          header: Option<&str>,
          lines: &[String],
      ) -> Result<(), String>;
  }
  ```
  `uva_session::SummaryCompactor` 新增一个 builder：
  ```rust
  pub fn with_sleeper(mut self, sleeper: Arc<dyn Sleeper>) -> Self;
  ```

- [ ] **Step 1: 写失败的测试**

建 `crates/uva-session/src/fs/tests.rs`：

```rust
use std::sync::Arc;

use super::FsBackend;
use uva_core::blocking::ThreadBlocking;

#[test]
fn appends_then_reads_back() {
    let dir = std::env::temp_dir().join(format!("uva-fs-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("events.jsonl");
    let backend = FsBackend::new(Arc::new(ThreadBlocking));

    futures::executor::block_on(backend.append_lines(
        &path,
        Some("header"),
        &["a".to_string(), "b".to_string()],
    )).unwrap());

    let bytes = futures::executor::block_on(backend.read_file(&path)).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.starts_with("header\n"));
    assert!(text.contains("a\n"));
    assert!(text.contains("b\n"));

    std::fs::remove_dir_all(&dir).ok();
}
```

再建 `crates/uva-session/tests/runtime_free.rs` —— 真正的门禁是 **Step 8 的 `cargo tree`
检查**。这个测试文件守的是**行为**：session 的读写路径在没有 tokio 的情况下仍然正确。

```rust
// crates/uva-session/tests/runtime_free.rs
use std::sync::Arc;

use uva_core::blocking::ThreadBlocking;
use uva_session::{JsonlSessionSink, SessionSink};

#[test]
fn a_jsonl_sink_round_trips_without_a_runtime() {
    let dir = std::env::temp_dir().join(format!("uva-rt-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let sink = JsonlSessionSink::new(dir.path()).unwrap()
        .with_blocking(Arc::new(ThreadBlocking));

    // The append path runs on the blocking pool; the read path
    // returns the rows in order.
    futures::executor::block_on(async {
        sink.append(&serde_json::json!({"type": "Test", "data": {"n": 1}}))
            .await
            .unwrap();
        sink.append(&serde_json::json!({"type": "Test", "data": {"n": 2}}))
            .await
            .unwrap();
        let rows = sink.read().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["data"]["n"], 1);
    });

    std::fs::remove_dir_all(&dir).ok();
}
```

（`JsonlSessionSink::with_blocking` 是本任务 Step 4 新增的构造器 —— 默认用
`ThreadBlocking`，tokio 宿主可换成 `TokioBlocking`。）

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-session --lib fs`
Expected: FAIL —— 找不到 `fs` 模块

- [ ] **Step 3: 实现 `crates/uva-session/src/fs.rs`**

按 Interfaces 的签名。`read_file` 与 `append_lines` 都通过 `blocking.run(...)` 把阻塞
fs 调用挪到线程上，**返回值经调用方自己的 oneshot 传回**（`Blocking` 的契约）。

`append_lines` 必须保留现有 `jsonl.rs` 的语义：
- 一次 `write_all` 写完整批 + 一次 `sync_all`；
- 写失败时 `set_len(previous_len)` 回滚，不留半行；
- 返回写入了多少字节。

从 `jsonl.rs` 把 `append_lines_sync` / `append_lines_batch_sync` / `write_lines_sync`
搬进来（它们是纯 fs 代码，本就不依赖 tokio）。

- [ ] **Step 4: 让 `jsonl.rs` 用 `FsBackend`**

`JsonlSessionSink` 的三个 `tokio::task::spawn_blocking` 调用点（`:286`、`:377`、`:436`）
改成走 `FsBackend`。`jsonl.rs` 的 `tokio::sync::Mutex` 换成 `futures::lock::Mutex`
（`writer/mod.rs:52-55` 已经这么用了）—— 它保护文件句柄、跨 `.await` 持有，
`futures::lock::Mutex` 的 guard 不要求 `Send`，正好合适。

`JsonlSessionSink` 需要一个 `FsBackend`。**默认构造用 `ThreadBlocking`**，
另加一个 `with_blocking(Arc<dyn Blocking>)` 供 tokio 宿主换成 `TokioBlocking`。

- [ ] **Step 5: 让 `compaction.rs` 的超时走 `Sleeper`**

`SummaryCompactor::summarize`（`compaction.rs:301`）里的 `tokio::select!` +
`tokio::time::timeout` 换成 `futures::select!` 竞速 `cancel.cancelled()` 与
`sleeper.sleep(timeout)`。

给 `SummaryCompactor` 加 `sleeper: Arc<dyn Sleeper>` 字段，默认
`Arc::new(ThreadSleeper)`，加 `with_sleeper` builder。`timeout` 为 `None` 时
只等 `cancel.cancelled()`。

- [ ] **Step 6: `search/backend.rs` 改走 `Blocking`**

`:130` 的 `tokio::task::spawn_blocking` 改成 `Blocking` 接缝。

- [ ] **Step 7: 把 `InputQueue` 挪出 session**

`crates/uva-session/src/manager.rs` 的 `InputQueue`（`:252`）与它依赖的
`PendingEntry` **移到 `uva-app-server`**。本任务里：
- 在 session 里删掉 `InputQueue` 与 `PendingEntry`；
- 在 `crates/uva-app-server/src/session/input_queue.rs` 建同样的类型（用
  `futures::lock::Mutex`，不是 `tokio::sync::Mutex`）；
- 更新 server 的引用点（`controller/deps.rs` 与 `controller/dispatch.rs` 的
  `push_text_input`）；
- 更新 `crates/uva/src/session.rs`（facade 的 glob 会自动跟随，但确认没有
  别的 crate 引用 `uva_session::manager::InputQueue`）；
- 更新 `scripts/paradigm-surface/baselines/` 里 `uva-session` 的基线。

**这一步是生态可见的破坏**：`uva::session::manager::InputQueue` 从 facade 公开面上
消失，不留别名。

- [ ] **Step 8: 验证依赖闭包**

Run: `cargo tree -p uva-harness -e normal | grep -c tokio`
Expected: `0`

Run: `cargo tree -p uva-session -e normal | grep -c tokio`
Expected: `0`

- [ ] **Step 9: 跑测试**

Run:
```bash
cargo test -p uva-session --lib
cargo test -p uva-app-server --lib
```
Expected: 全绿

- [ ] **Step 10: 提交**

```bash
git add crates/uva-session crates/uva-app-server/src/session crates/uva/src/session.rs scripts/paradigm-surface/baselines
git commit -m "refactor(session): make the session crate runtime-free"
```

---

## Task 6: `ReActAgent` 泛型化

**Files:**
- Modify: `crates/uva-harness/src/agent/caps.rs`
- Modify: `crates/uva-harness/src/agent/re_act/agent.rs`
- Modify: `crates/uva-harness/src/agent/mod.rs`
- Modify: `crates/uva-harness/src/lib.rs`
- Test: `crates/uva-harness/src/agent/re_act/tests/generic_agent.rs`

**Interfaces:**
- Consumes: 无
- Produces:
  ```rust
  // uva_harness
  pub struct ReActAgent<P = Arc<dyn ModelProvider>> { /* … */ }
  impl<P: ModelProvider + 'static> ReActAgent<P> {
      pub fn with_provider(provider: P) -> Self;  // 泛型构造
      pub fn provider(&self) -> &P;
  }
  // 既有 ReActAgent::new(provider: Arc<dyn ModelProvider>, registry) 签名不变
  ```

- [ ] **Step 1: 写失败的测试**

建 `crates/uva-harness/src/agent/re_act/tests/generic_agent.rs`：

```rust
use std::sync::Arc;

use uva_provider::{CompletionRequest, CompletionResponse, ModelProvider};
use uva_tool::ToolRegistry;

use crate::ReActAgent;

/// A provider with a concrete type — the point of the generic agent.
struct CountingProvider {
    calls: Arc<std::sync::atomic::AtomicUsize>,
}

impl ModelProvider for CountingProvider {
    fn name(&self) -> &str {
        "counting"
    }
    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, uva_provider::ProviderError> {
        self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(CompletionResponse::default())
    }
}

#[test]
fn generic_agent_accepts_a_concrete_provider() {
    let provider = CountingProvider {
        calls: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
    };
    let agent = ReActAgent::with_provider(provider);
    // The concrete type is preserved — no dyn erasure happened.
    let _: &CountingProvider = agent.provider();
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-harness --lib generic_agent`
Expected: FAIL —— `with_provider` 不存在

- [ ] **Step 3: 泛型化 `ModelCaps` 与 `ReActAgent`**

`caps.rs`：
```rust
#[derive(Clone)]
pub struct ModelCaps<P = Arc<dyn ModelProvider>> {
    pub provider: P,
    pub descriptor: AgentDescriptor,
    pub prompt_context: Arc<PromptContext>,
}
```
`AgentCaps` 相应加泛型参数并逐层传递（`AgentCaps<S, M, T, C, E>` 会太宽 ——
**只把 provider 泛型化**，其余层保持 `dyn`：
`ModelCaps<P>` 泛型，`AgentCaps<P>` 跟着泛型，`ToolCaps` / `ControlCaps` / `EnvCaps`
不动）。

`agent.rs`：
```rust
pub struct ReActAgent<P = Arc<dyn ModelProvider>> {
    pub(super) caps: AgentCaps<P>,
}

impl<P: ModelProvider + 'static> ReActAgent<P> {
    pub fn with_provider(provider: P) -> Self { /* 默认其余能力 */ }
    pub fn provider(&self) -> &P { &self.caps.model.provider }
}
```
既有 `ReActAgent::new(provider: Arc<dyn ModelProvider>, registry: Arc<ToolRegistry>)`
的签名**保持不变** —— 它落在 `impl ReActAgent<Arc<dyn ModelProvider>>` 上。
所有 `with_*` setter 方法在两个 impl 里重复会造成大量重复代码，**用宏或
`impl<P: ModelProvider + 'static> ReActAgent<P>` 统一承载**（setter 与 provider 类型无关，
可以只写一份）。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p uva-harness --lib`
Expected: 全绿（含既有测试 —— setter 签名没变，既有调用方零改动）

- [ ] **Step 5: 提交**

```bash
git add crates/uva-harness
git commit -m "feat(harness): generic agent over the provider type"
```

---

## Task 7: `prompt_section` 链接上

**Files:**
- Modify: `crates/uva-harness/src/prompt/mod.rs`
- Modify: `crates/uva-harness/src/agent/re_act/agent.rs`
- Test: `crates/uva-harness/src/prompt/tests/sections.rs`

**Interfaces:**
- Consumes: `Host::prompt_sections()`（已存在，`host.rs:856`）
- Produces: 无新公开 API（行为修复）

- [ ] **Step 1: 写失败的测试**

建 `crates/uva-harness/src/prompt/tests/sections.rs`：

```rust
use std::sync::Arc;

use crate::agent::host::{Host, PromptSection};
use crate::prompt::{PromptContext, assemble_prompt};

#[test]
fn host_sections_reach_the_assembled_prompt() {
    let mut host = Host::new();
    host.prompt_section(PromptSection {
        title: "Audit".to_string(),
        body: "Always cite the source.".to_string(),
    });

    let prompt = assemble_prompt(&host, &PromptContext::default());
    assert!(
        prompt.contains("Always cite the source."),
        "plugin prompt section missing from the prompt: {prompt}"
    );
}

#[test]
fn sections_render_in_registration_order() {
    let mut host = Host::new();
    host.prompt_section(PromptSection { title: "First".into(), body: "one".into() });
    host.prompt_section(PromptSection { title: "Second".into(), body: "two".into() });

    let prompt = assemble_prompt(&host, &PromptContext::default());
    let one = prompt.find("one").unwrap();
    let two = prompt.find("two").unwrap();
    assert!(one < two, "sections must render in registration order");
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-harness --lib sections`
Expected: FAIL —— `assemble_prompt` 不存在（函数名待实现者按实际签名对齐）或
断言失败（section 没进 prompt）

- [ ] **Step 3: 让 prompt 组装器读 host 的 sections**

`prompt/mod.rs` 现在的组装函数不知道 host 的存在。改动要点：

- prompt 组装的那一层要能拿到 `Host` 的 sections。`PromptContext` 是已有的
  "prompt 的输入"载体（`caps.rs` 里 `prompt_context: Arc<PromptContext>`），
  **优先复用它**而不是给组装函数加一个新参数 —— 加参数会波及所有调用方。
  在 `PromptContext` 上加：
  ```rust
  impl PromptContext {
      pub fn with_sections(mut self, sections: Vec<PromptSection>) -> Self;
      pub fn sections(&self) -> &[PromptSection];
  }
  ```
  `PromptSection` 现在住在 `uva_harness::agent::host` —— `PromptContext` 在
  `uva_harness::prompt`，同 crate 内可以直接用。
- `ReActAgent` 在构造 caps 时，把 `host.prompt_sections()` 拷进 `PromptContext`。
  **拷贝时机**：每次 run 开始时（`prepare.rs` 的 prepare 阶段）刷新一次，
  不要只在构造时取 —— 插件可以在 run 之间注册 section。
- **插入位置**：身份段之后、steering hint 之前。现有 prompt 组装（`prompt/mod.rs:138,172,187,227`）
  有确定的段落顺序，按那个顺序插入，不要重排既有段落。
- **渲染格式**：`## {title}\n\n{body}`，多段之间空一行。标题为空时只渲染 body。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p uva-harness --lib`
Expected: 全绿

- [ ] **Step 5: 提交**

```bash
git add crates/uva-harness
git commit -m "feat(harness): render plugin prompt sections"
```

---

## Task 8: 热路径 —— 消除 `call.input` 深拷贝并修正基准

**Files:**
- Modify: `crates/uva-harness/src/agent/re_act/loop/tools/route.rs`
- Modify: `crates/uva-harness/src/agent/re_act/loop/guard.rs`
- Modify: `crates/uva-harness/benches/hot_paths.rs`
- Test: `crates/uva-harness/src/agent/re_act/tests/lifecycle/tool_execution.rs`

**Interfaces:**
- Consumes: 无
- Produces: 无新公开 API

- [ ] **Step 1: 写失败的测试**

在 `crates/uva-harness/src/agent/re_act/tests/lifecycle/tool_execution.rs` 追加：

```rust
/// The tool input must reach the tool without a deep copy on the
/// way. The tool records the pointer it was handed; the gate path
/// must not have replaced it with a fresh allocation.
#[test]
fn the_tool_receives_the_same_input_allocation_the_gate_saw() {
    // A tool that records the `Arc` pointer of the input it was
    // given. The gate path and the dispatch path must hand over the
    // same allocation — a `Value::clone()` in between would break
    // `Arc::ptr_eq`.
    let seen: Arc<std::sync::Mutex<Option<usize>>> =
        Arc::new(std::sync::Mutex::new(None));

    // …drive one tool call through the loop with this fixture
    // installed, then assert the recorded pointer equals the input
    // the loop handed to the gate.
    //
    // Implementation note: if the input type is still
    // `serde_json::Value` at this point (i.e. Step 3 chose the
    // "clone once upstream" option rather than switching to
    // `Arc<Value>`), this test cannot observe the allocation and
    // MUST be replaced by the benchmark assertion in Step 4 — a
    // test that always passes is worse than no test.
}
```

**不要**把"clone 计数"当作验收标准 —— `commit.rs` 的内容 clone 今天恰好就是 1，
标准没有区分力（见设计文档 §5.6）。可区分的标准是**同一个 allocation**：
`Arc::ptr_eq` 成立即为未深拷贝。

- [ ] **Step 2: 跑测试确认现状**

Run: `cargo test -p uva-harness --lib tool_execution`
Expected: PASS（记录基线）

- [ ] **Step 3: 消除两处深拷贝**

`route.rs:39` 的 `call.input.clone()` 与 `guard.rs:286` 的
`arguments: call.input.clone()` 是两处 `serde_json::Value` 的深拷贝。

改法：把工具调用上的 `input` 从 `serde_json::Value` 改成
`Arc<serde_json::Value>`，两处都传 `Arc::clone`。这要求 `ToolCall` 的
`input` 字段类型变化 —— 查 `uva-provider/src/types/tool.rs` 里 `ToolCall` 的定义，
若是 `serde_json::Value`，改成 `Arc<serde_json::Value>` 会影响 provider 的
序列化（`CompletionRequest` 的 serde 派生对 `Arc<Value>` 需要 `serde` 的 `rc`
feature —— `uva-provider/Cargo.toml:17` 已经开了 `features = ["rc"]`）。

**若改动面过大**，退一步：只让 `guard.rs` 复用 `route.rs` 已经 clone 好的那份
（把 clone 提到调用链的更上游一次），使全路径只 clone 一次。**判据是"审批门控路径上
深拷贝 ≤ 1 处"**。

- [ ] **Step 4: 修正基准的测量污染**

`benches/hot_paths.rs:658` 把 `Arc::new(messages.clone())` 放在被计时的闭包**内部** ——
测出来的是"context manager 的工作 + 401 条消息的深拷贝"。修法：

```rust
// 把 fixture 克隆移出计时区
let input = Arc::new(messages.clone());
Bench {
    label: "TruncatingContextManager (401 msgs, over window)",
    iters: 2_000,
}
.run("fits check + evict", || {
    let mut state = AgentState::with_window(2_000);
    black_box(futures::executor::block_on(
        TruncatingContextManager.prepare_arc(Arc::clone(&input), &mut state),
    ));
});
```

同时把 label 从 "200 msgs" 改成真实的 **401**（fixture 是
`tool_heavy_history(100)` = 1 条 system + 4 条一对 = 401 条），
`parse_tool_input` 的 label 从 "4 KB strict" 改成真实的 ≈7.6 KB
（fixture 是 `nested_tool_args(120)`）。

**取新基线并把它记进 commit message** —— 旧基线（386.9 µs，含 clone）不再有效。

- [ ] **Step 5: 跑基准并记录**

Run: `cargo bench -p uva-harness --bench hot_paths`
Expected: 打印新表。`TruncatingContextManager` 的新数字**应该显著低于 386.9 µs**
—— 但那部分来自"不再把 clone 算进去"，**不是**性能改进。要区分二者：
在 commit message 里同时写"修基准后的数字"与"真实改进量"。

- [ ] **Step 6: 跑测试**

Run: `cargo test -p uva-harness --lib`
Expected: 全绿

- [ ] **Step 7: 提交**

```bash
git add crates/uva-harness
git commit -m "perf(harness): drop the duplicate tool-input copy and fix the bench fixture"
```

---

## Task 9: 公开面收窄与死代码清理

**Files:**
- Modify: `crates/uva-session/src/typed_event_sink.rs`
- Modify: `crates/uva-session/src/compaction.rs`
- Modify: `crates/uva-provider/Cargo.toml`（`ahash` 降 dev-dependency）
- Modify: `crates/uva-provider/src/config.rs`
- Modify: `crates/uva-provider/src/cache_mark.rs`
- Modify: `crates/uva-provider/src/profile/openai.rs`
- Modify: `crates/uva-context/src/summarizing/manager.rs`
- Modify: `crates/uva-app-server/Cargo.toml`（`publish = false`）
- Modify: `crates/uva-harness/src/agent/re_act/loop/tools/mod.rs`（注释漂移）
- Modify: `scripts/paradigm-surface/baselines/crates/uva-harness`（5 → 4）
- Delete: `crates/uva-app-server/src/config/server.rs.tmp`

（**不动** `crates/uva/Cargo.toml`：facade 保持可发布。**不动**
`crates/uva-provider/src/lib.rs` 与 `retry.rs`：两个 retry helper 已由 Task 4 删除。
**不动** `crates/uva-app-server/src/lib.rs`：见 Step 2 的撤销。）

**Interfaces:**
- Consumes: 无
- Produces: 无新公开 API（这是删除）

- [ ] **Step 1: 逐个复验候选**

对下面 6 个符号，**逐个**在全仓 grep（排除自身定义）确认引用数为 0，再删：

| 符号 | 位置 |
|---|---|
| `uva_session::as_value` | `typed_event_sink.rs:57` |
| `uva_session::with_prune_threshold` | `compaction.rs:244` |
| `uva_provider::create_default_provider` | `config.rs:299` |
| `uva_provider::cache_mark::…::hash_to_u64` | `cache_mark.rs:63`（**收窄不删**，见下） |
| `uva_provider::openai_compat` | `profile/openai.rs:69` |
| `uva_context::with_archive` | `summarizing/manager.rs:97` |

**注意**：符号名是 `hash_to_u64`（不是 `hash_to_u`）；`openai_compat` 在
`profile/openai.rs`（是个常量或函数，按实际形态删）。

**例外**：`hash_to_u64` 的 10 处引用全部在本模块的**测试**里，删除会逼四条断言各自重推哈希。
改为 `#[cfg(test)]` 的私有函数（收窄而非删除），`ahash` 与 `Hash`/`Hasher` 随之只在测试里用，
`ahash` 挪到 `[dev-dependencies]`。

**任何一个发现还有引用，就不删** —— 收窄或改记进文档的"留给实现者"。

- [ ] **Step 2: 删 server 的多余 `pub`**（**已撤销**）

原计划把 14 个 `pub mod` 一律降 `pub(crate)`。**不做**：`tests/` 下 14 个 integration test 是
独立 crate，直接 `use uva_server::…`；`main.rs` 用
`build_info` / `config::server::BindOverrides` / `server::boot_server`。这些路径触达的模块必须保持
`pub`。server 既不可发布（Step 3），其 `pub` 面已非出厂契约，批量降级是无消费者的 churn。

- [ ] **Step 3: 加 `publish = false`（只加到 server）**

**只**在 `crates/uva-app-server/Cargo.toml` 加 `publish = false`。**`crates/uva/Cargo.toml`
不加** —— facade 是文档化的消费入口（`cargo add uva`），标成不可发布会砍掉主分发路径。
全仓 39 个 crate 目前无一标 `publish = false`，只有两个 example consumer 标了。

- [ ] **Step 4: 删 `.tmp`**

`git rm crates/uva-app-server/src/config/server.rs.tmp` —— 它是已入 git 的 **0 字节
占位文件**（blob `e69de29b…`）。

- [ ] **Step 5: 跑门禁**

Run:
```bash
make check-pub-surface
make check-pub-surface-strict
make check-harness-docs
make check-harness-shape
```
Expected: 全绿（基线若因删除而变化，更新基线文件并在 commit message 里说明）

- [ ] **Step 6: 跑测试**

Run:
```bash
cargo test -p uva-provider --lib
cargo test -p uva-session --lib
cargo test -p uva-context --lib
cargo test -p uva-app-server --lib
```
Expected: 全绿

- [ ] **Step 7: 提交**

```bash
git add -A crates/uva-provider crates/uva-session crates/uva-context crates/uva-app-server crates/uva scripts
git commit -m "refactor: drop dead public surface and unpublish the server crate"
```

---

## Task 10: `uva-plugin-protocol` crate 骨架

**Files:**
- Create: `crates/uva-plugin-protocol/Cargo.toml`
- Create: `crates/uva-plugin-protocol/src/lib.rs`
- Create: `crates/uva-plugin-protocol/src/wire.rs`
- Test: `crates/uva-plugin-protocol/tests/wire_shape.rs`

**Interfaces:**
- Consumes: 无
- Produces:
  ```rust
  // uva_plugin_protocol
  pub mod methods;   // 方法名常量
  pub mod wire;      // wire 类型

  // methods
  pub const INITIALIZE: &str = "uva/initialize";
  pub const TOOLS_LIST: &str = "uva/tools/list";
  pub const TOOLS_CALL: &str = "uva/tools/call";
  pub const TRANSFORM: &str = "uva/transform";
  pub const EVENT: &str = "uva/event";
  pub const GATE: &str = "uva/gate";

  // wire
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
  pub struct PluginCapabilities {
      pub tools: bool,
      pub observe: bool,
      pub transform: bool,
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  pub struct ToolSpec {
      pub name: String,
      pub description: String,
      pub input_schema: serde_json::Value,
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  #[serde(tag = "type", rename_all = "snake_case")]
  pub enum PluginEvent {
      BeforeTool { tool_name: String, call_id: String, arguments: serde_json::Value },
      AfterTool { tool_name: String, call_id: String, is_error: bool },
      BeforeStep { iteration: u32 },
      AfterStep { iteration: u32 },
      TurnStart { agent_name: String },
      TurnEnd { agent_name: String, reason: String },
  }

  #[derive(Debug, Clone, Serialize, Deserialize)]
  #[serde(tag = "type", rename_all = "snake_case")]
  pub enum PluginDecision {
      Allow,
      Block { reason: String },
      Ask { reason: String },
      Terminate { reason: String },
  }

  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  #[serde(rename_all = "snake_case")]
  pub enum PluginError {
      Unavailable,
      Timeout,
      Denied,
      BadRequest,
      Internal,
  }
  ```

- [ ] **Step 1: 写失败的测试**

建 `crates/uva-plugin-protocol/tests/wire_shape.rs`：

```rust
use uva_plugin_protocol::{
    PluginCapabilities, PluginDecision, PluginError, PluginEvent,
};

#[test]
fn capabilities_round_trip() {
    let caps = PluginCapabilities { tools: true, observe: false, transform: true };
    let json = serde_json::to_string(&caps).unwrap();
    assert_eq!(serde_json::from_str::<PluginCapabilities>(&json).unwrap(), caps);
}

#[test]
fn event_shape_is_tagged() {
    let event = PluginEvent::BeforeTool {
        tool_name: "read".into(),
        call_id: "c1".into(),
        arguments: serde_json::json!({"path": "a.txt"}),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "before_tool");
}

#[test]
fn decision_shape_is_tagged() {
    let json = serde_json::to_value(PluginDecision::Block { reason: "no".into() }).unwrap();
    assert_eq!(json["type"], "block");
    assert_eq!(json["reason"], "no");
}

#[test]
fn error_codes_are_stable() {
    assert_eq!(
        serde_json::to_value(PluginError::Timeout).unwrap(),
        serde_json::json!("timeout")
    );
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-plugin-protocol`
Expected: FAIL —— crate 还不存在（`cargo` 报 "no such package"）

- [ ] **Step 3: 建 crate**

`Cargo.toml`（照 `uva-tool` 的形状）：
```toml
[package]
name = "uva-plugin-protocol"
description = "Wire protocol for Uva's dynamic plugin channel: JSON-RPC method names, capability negotiation, and stable event/decision types"
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
homepage.workspace = true
documentation.workspace = true
readme.workspace = true
keywords.workspace = true
categories.workspace = true

[dependencies]
serde = { workspace = true, features = ["derive"] }
serde_json.workspace = true

[dev-dependencies]
```

**只依赖 `serde` 与 `serde_json`** —— 这是"零运行时"的全部含义。**不要**加
`uva-harness`、`uva-core` 或任何 tokio。

根 `Cargo.toml` 的 `[workspace.dependencies]` 加
`uva-plugin-protocol = { path = "crates/uva-plugin-protocol" }`，
`[workspace.members]` 加 `"crates/uva-plugin-protocol"`。

`src/lib.rs` 按 Interfaces 定义模块与导出。`src/wire.rs` 放 wire 类型。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p uva-plugin-protocol`
Expected: PASS，4 个用例全绿

- [ ] **Step 5: 验证零依赖**

Run: `cargo tree -p uva-plugin-protocol -e normal`
Expected: 只有 `serde` / `serde_json` / `proc-macro2` 一类，**没有 tokio**

- [ ] **Step 6: 提交**

```bash
git add crates/uva-plugin-protocol Cargo.toml Cargo.lock
git commit -m "feat(protocol): add the dynamic plugin wire protocol crate"
```

---

## Task 11: facade feature 表收窄

**Files:**
- Modify: `crates/uva/Cargo.toml`
- Modify: `crates/uva/src/lib.rs`（模块的 `#[cfg(feature = ...)]`）
- Modify: `crates/uva-tool-scheduler/Cargo.toml`
- Modify: `crates/uva-provider/Cargo.toml`
- Modify: `crates/uva-search/Cargo.toml`
- Test: `crates/uva/tests/feature_surface.rs`

**Interfaces:**
- Consumes: 无
- Produces: 收窄后的 facade feature 表

**收窄规则**（设计文档 §5.10）：
1. 一条 feature **最多拉一个 crate**，且那个 crate 必须是它的同名兄弟。
2. "两个 crate 一起才有意义"写进 crate 自己的 Cargo.toml，不在 facade 上开组合 feature。
3. 表达"这次部署带哪些能力"的意图，一律搬到运行时组合层（Task 13）。
4. 指向不存在目标的空转发 feature，直接删除。

- [ ] **Step 1: 删两条空转发 feature**

`crates/uva/Cargo.toml`：
- 删 `checkpoint = ["harness"]` —— `uva-harness` **没有 `[features]` 表**，
  这条什么都不启用。
- 删 `transcript = ["eval"]` —— `uva-eval` **也没有 `[features]` 表`**。

**删之前先确认** `uva-eval` 与 `uva-harness` 确实没有 `[features]` 表
（`grep -c "^\[features\]"` 应为 0），并且没有 crate 依赖 facade 的这两条 feature。

同时更新 `crates/uva/src/lib.rs` 里对应的 `#[cfg(feature = "checkpoint")]` /
`#[cfg(feature = "transcript")]` 模块门控。

- [ ] **Step 2: 把组合 feature 拆到 crate 自己的依赖里**

`tool-scheduler` 现在在 facade 上是 `tool-scheduler = ["dep:uva-tool-scheduler", "tool", "scheduler", ...]`
—— 它同时拉 `tool` 与 `scheduler`。**改**：让
`crates/uva-tool-scheduler/Cargo.toml` 自己依赖 `uva-scheduler`，
facade 上的 `tool-scheduler` 只剩 `["dep:uva-tool-scheduler"]`。

同样处理其它"拉 ≥2 个 crate"的组合 feature：`context`、`tool`、`session`、`steering`、
`harness`、`skill`、`mcp`、`telemetry`、`safety`、`eval`、`workflow`、
`semantic-cache`、`tool-memo`、`resilience`、`strategy-*`、`reflection`、`budget`、
`hpc-batch`、`test-support`、`sqlite`、`cron`、`hnsw`、`hpc-pool`、`hpc-simd`、
`tool-search`、`provider-anthropic`、`provider-openai`。

**逐条处理原则**：
- 若 feature X 只是为了把 crate C 拉进来 → 改成 `["dep:uva-c"]`，
  C 自己的依赖写进 C 的 Cargo.toml。
- 若 feature X 转发的是 crate C 的**子 feature**（`uva-c/sub`）→
  保留转发，但**同时**把 C 本身拉进来：`X = ["dep:uva-c", "uva-c/sub"]`。
- 若 feature X 是纯能力开关且无对应 crate → 按规则 4 删除或降级。

- [ ] **Step 3: 收窄 `default`**

`default` 现在拉 16 条。收窄后**工具插件不进 default**（它们是典型组合意图），
只保留"一个 agent 跑起来必需的最小集"。

**判据**：`docs/examples/harness-only-consumer`（Task 17）必须能只依赖
`uva-harness` 就跑通一轮 —— 那说明 `harness` 这一条 feature 足够。

具体保留哪些由实现者定，但 `default` 里**不应**再出现 `tool-read` / `tool-write` /
`tool-shell` / `tool-todo` / `tool-web` / `tool-task` 这六条工具插件。

- [ ] **Step 4: 写失败的测试**

建 `crates/uva/tests/feature_surface.rs`：

```rust
//! The facade's feature table must express crate availability only.
//! Composition belongs to the runtime composition layer.

#[test]
fn facade_features_are_declared_consistently() {
    // Every facade feature must be declared in Cargo.toml; a module
    // gated on an undeclared feature silently disappears.
    let manifest = std::fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/Cargo.toml"),
    )
    .unwrap();
    assert!(manifest.contains("harness = ["));
}
```

（这条测试是弱断言，但**真正���门禁是 Step 5 的 `cargo check` 矩阵** ——
每个 feature 组合都要能编译。）

- [ ] **Step 5: 验证每个 feature 组合都能编译**

Run:
```bash
for f in $(grep -E "^[a-z0-9_-]+ = \[" crates/uva/Cargo.toml | cut -d' ' -f1); do
  cargo check -p uva --no-default-features --features "$f" || echo "FAILED: $f"
done
```
Expected: 无 `FAILED:` 输出

再跑 `cargo check -p uva`（default 集）与
`cargo check -p uva --all-features`。

- [ ] **Step 6: 跑门禁与测试**

Run:
```bash
make check-mvp-deps
cargo test -p uva --lib
```
Expected: 全绿

- [ ] **Step 7: 提交**

```bash
git add crates/uva crates/uva-tool-scheduler crates/uva-provider crates/uva-search Cargo.lock
git commit -m "refactor(facade): narrow features to crate availability"
```

---

## Task 12: `uva-plugin-host` crate

**Files:**
- Create: `crates/uva-plugin-host/Cargo.toml`
- Create: `crates/uva-plugin-host/src/lib.rs`
- Create: `crates/uva-plugin-host/src/client.rs`
- Create: `crates/uva-plugin-host/src/remote_tool.rs`
- Create: `crates/uva-plugin-host/src/process.rs`
- Test: `crates/uva-plugin-host/tests/remote_tool.rs`

**Interfaces:**
- Consumes: `uva_plugin_protocol`（Task 10）、`uva_harness::Host`、
  `uva_tool::Tool`、`uva_core::blocking::Blocking`
- Produces:
  ```rust
  // uva_plugin_host
  pub struct RemotePluginConfig {
      pub name: String,
      pub command: Vec<String>,
      pub transport: TransportKind,   // Stdio | Http { url: String }
      pub capabilities: PluginCapabilities,
      pub authority: Authority,       // Reduced | Full
      pub risk: Risk,                 // 远端工具的 risk，由宿主决定
  }

  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub enum Authority { Reduced, Full }

  pub struct RemotePlugin { /* … */ }
  impl RemotePlugin {
      pub fn new(config: RemotePluginConfig) -> Self;
      pub async fn install(&self, host: &mut Host) -> Result<PluginHandle, HostError>;
  }
  ```

- [ ] **Step 1: 写失败的测试**

建 `crates/uva-plugin-host/tests/remote_tool.rs`：

```rust
use std::sync::Arc;

use uva_core::blocking::ThreadBlocking;
use uva_harness::host::{Host, PluginHandle};
use uva_plugin_host::{Authority, RemotePlugin, RemotePluginConfig};
use uva_plugin_protocol::PluginCapabilities;
use uva_tool::{Risk, Tool, ToolRegistry};

/// A plugin that advertises one tool and refuses to block.
struct FakePlugin;

#[test]
fn remote_tool_overrides_risk_with_the_host_policy() {
    // The remote tool's risk comes from the host config, never from
    // what the plugin claims in its own annotations.
    let config = RemotePluginConfig {
        name: "fake".into(),
        command: vec!["true".into()],
        transport: uva_plugin_host::TransportKind::Stdio,
        capabilities: PluginCapabilities { tools: true, observe: false, transform: false },
        authority: Authority::Reduced,
        risk: Risk::Unknown,
    };
    let plugin = RemotePlugin::new(config);
    let risk = plugin.remote_risk();
    assert_eq!(risk, Risk::Unknown);
}

#[test]
fn reduced_authority_exposes_no_gate_method() {
    let config = RemotePluginConfig {
        name: "fake".into(),
        command: vec!["true".into()],
        transport: uva_plugin_host::TransportKind::Stdio,
        capabilities: PluginCapabilities { tools: true, observe: false, transform: false },
        authority: Authority::Reduced,
        risk: Risk::Low,
    };
    let plugin = RemotePlugin::new(config);
    assert!(!plugin.exposes_gate());
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-plugin-host`
Expected: FAIL —— crate 不存在

- [ ] **Step 3: 建 crate 与骨架**

`Cargo.toml` 依赖：`uva-plugin-protocol`、`uva-harness`、`uva-tool`、
`uva-core`、`serde`、`serde_json`、`async-trait`、`futures`。
根 `Cargo.toml` 的 `[workspace.dependencies]` 与 `[workspace.members]` 各加一条。

`src/lib.rs` 定义 `RemotePluginConfig`、`Authority`、`TransportKind`、`RemotePlugin`、
`PluginRegistry`。**`RemotePlugin::install` 的行为**：

- `capabilities.tools` 为真 → 向 `host` 注册一个 `RemoteTool`（`src/remote_tool.rs`）。
- `capabilities.observe` 为真 → 向 `host` 注册一个远端观察者（用
  `uva/event` 通知，无返回值）。
- `capabilities.transform` 为真 → 注册一个远端 transformer。
- `authority == Reduced` → **不注册任何 gate**，`exposes_gate()` 返回 `false`。
- `authority == Full` → 注册一个远端 gate，其决策经 `PluginDecision` 映射到
  `uva_steering::HookDecision`。

`src/remote_tool.rs` 的 `RemoteTool` 实现 `uva_tool::Tool`：
- `name` / `description` / `parameters` 来自 `tools/list` 的缓存。
- `call` 通过 JSON-RPC 调 `uva/tools/call`。
- **`fn risk(&self, _input: &serde_json::Value) -> Risk` 必须覆写**，返回宿主配置的值
  —— **绝不**用 `Risk::from_annotations(self.annotations())`（那是默认实现，
  会让插件自报的注解绕过审批）。

`src/client.rs`：JSON-RPC 客户端。请求用 `uva_plugin_protocol::methods` 的常量。
响应解析失败要返回 `PluginError::BadRequest` 而不是 panic。

`src/process.rs`：进程管理（spawn + stdin/stdout）。**stdio 传输**用
`std::process::Command` + `std::io::Write` / `BufRead`，不引 tokio。

- [ ] **Step 4: 跑测试确认通过**

Run: `cargo test -p uva-plugin-host`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/uva-plugin-host Cargo.toml Cargo.lock
git commit -m "feat(plugin-host): add the dynamic plugin host crate"
```

---

## Task 13: 远端权限矩阵与审计行

**Files:**
- Modify: `crates/uva-plugin-host/src/remote_tool.rs`
- Modify: `crates/uva-plugin-host/src/lib.rs`
- Modify: `crates/uva-session/src/events.rs`
- Test: `crates/uva-plugin-host/tests/authority.rs`

**Interfaces:**
- Consumes: `uva_plugin_protocol`（Task 10）、`uva_session::SessionEvent`（Task 10 之后的 session）
- Produces:
  ```rust
  // uva_session::events
  SessionEvent::ToolResultTransformed {
      plugin: String,
      tool_name: String,
      call_id: String,
      before: String,
      after: String,
  }
  ```

- [ ] **Step 1: 写失败的测试**

建 `crates/uva-plugin-host/tests/authority.rs`：

```rust
use uva_plugin_host::{Authority, RemotePlugin, RemotePluginConfig};
use uva_plugin_protocol::PluginCapabilities;
use uva_tool::Risk;

fn config(authority: Authority) -> RemotePluginConfig {
    RemotePluginConfig {
        name: "fake".into(),
        command: vec!["true".into()],
        transport: uva_plugin_host::TransportKind::Stdio,
        capabilities: PluginCapabilities { tools: true, observe: false, transform: false },
        authority,
        risk: Risk::Unknown,
    }
}

#[test]
fn reduced_authority_cannot_block() {
    let plugin = RemotePlugin::new(config(Authority::Reduced));
    assert!(!plugin.exposes_gate());
}

#[test]
fn full_authority_can_block() {
    let plugin = RemotePlugin::new(config(Authority::Full));
    assert!(plugin.exposes_gate());
}

#[test]
fn remote_risk_ignores_plugin_annotations() {
    // Even a plugin that claims to be read-only gets the host's risk.
    let plugin = RemotePlugin::new(config(Authority::Reduced));
    assert_eq!(plugin.remote_risk(), Risk::Unknown);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-plugin-host --test authority`
Expected: FAIL —— 缺失的项尚未实现

- [ ] **Step 3: 实现权限矩阵**

`RemotePlugin` 加两个方法：
```rust
pub fn exposes_gate(&self) -> bool {
    matches!(self.config.authority, Authority::Full)
}
pub fn remote_risk(&self) -> Risk {
    self.config.risk
}
```
`install` 里：只有 `exposes_gate()` 为真才注册 gate。

**远端 gate 的决策映射**：`PluginDecision::Block { reason }` →
`uva_steering::HookDecision::Block { reason }`；
`Ask { reason }` → `HookDecision::Ask { reason }`；
`Terminate { reason }` → `HookDecision::Terminate { reason }`；
`Allow` → `Allow`。

**降权路径下宿主不发送 `uva/gate` 请求** —— 插件即使实现了这个方法也收不到调用。

- [ ] **Step 4: 加审计行**

`uva-session/src/events.rs` 加 `SessionEvent::ToolResultTransformed` 变体
（字段见 Interfaces）。远端 transformer 改写工具结果后，宿主追加这一行。

**这个事件是 log-only**（不进模型历史）。若 session 的 `SessionEvent` 有
"是否进 surface"的标注机制，按现有惯例标注。

- [ ] **Step 5: 跑测试**

Run:
```bash
cargo test -p uva-plugin-host
cargo test -p uva-session --lib
```
Expected: 全绿

- [ ] **Step 6: 提交**

```bash
git add crates/uva-plugin-host crates/uva-session
git commit -m "feat(plugin-host): enforce the reduced-authority matrix and audit transforms"
```

---

## Task 14: `PluginEvent` 逃生舱

**Files:**
- Modify: `crates/uva-session/src/events.rs`
- Modify: `crates/uva-harness/src/events/mod.rs`
- Test: `crates/uva-harness/src/events/tests/plugin_event.rs`

**Interfaces:**
- Consumes: `uva_plugin_protocol::PluginEvent`（Task 10）
- Produces:
  ```rust
  // uva_session::events
  SessionEvent::Plugin { kind: String, payload: serde_json::Value }
  // uva_harness::events
  SystemEvent::Plugin { kind: String, payload: serde_json::Value }
  ```

- [ ] **Step 1: 写失败的测试**

建 `crates/uva-harness/src/events/tests/plugin_event.rs`：

```rust
use crate::events::SystemEvent;

#[test]
fn plugin_event_round_trips_through_json() {
    let event = SystemEvent::Plugin {
        kind: "audit.recorded".into(),
        payload: serde_json::json!({"count": 3}),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["kind"], "audit.recorded");
    assert_eq!(json["payload"]["count"], 3);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-harness --lib plugin_event`
Expected: FAIL —— `SystemEvent::Plugin` 不存在

- [ ] **Step 3: 实现两个变体**

`SessionEvent::Plugin { kind: String, payload: serde_json::Value }` — durable，
落进日志。

`SystemEvent::Plugin { kind: String, payload: serde_json::Value }` — live，
只走 `AgentEvent`，不落盘。

`kind` 用 `String`（不是 `Cow<'static, str>` —— 插件的 kind 是运行期来的，
`'static` 约束不成立）。这个修正要同步回设计文档 §5.8。

- [ ] **Step 4: 跑测试**

Run:
```bash
cargo test -p uva-harness --lib
cargo test -p uva-session --lib
```
Expected: 全绿

- [ ] **Step 5: 提交**

```bash
git add crates/uva-harness crates/uva-session
git commit -m "feat(events): add the PluginEvent escape hatch"
```

---

## Task 15: 运行时组合层

**Files:**
- Create: `crates/uva-app-server/src/composition.rs`
- Modify: `crates/uva-app-server/src/config/server.rs`
- Modify: `crates/uva-app-server/src/state/app_state/boot.rs`
- Modify: `crates/uva-app-server/src/agent_assembly.rs`
- Test: `crates/uva-app-server/src/composition/tests.rs`

**Interfaces:**
- Consumes: `uva_plugin_host::RemotePlugin`（Task 12）、`uva_harness::Host`
- Produces:
  ```rust
  // uva_server::config
  #[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
  pub struct PluginsConfig {
      pub plugins: Vec<PluginEntry>,
  }
  #[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
  #[serde(tag = "kind", rename_all = "snake_case")]
  pub enum PluginEntry {
      Static { name: String, tools: Vec<String> },
      Remote {
          name: String,
          command: Vec<String>,
          transport: String,
          capabilities: Vec<String>,
          authority: String,
          risk: String,
      },
  }
  ```

- [ ] **Step 1: 写失败的测试**

建 `crates/uva-app-server/src/composition/tests.rs`：

```rust
use crate::config::PluginEntry;

#[test]
fn plugin_entries_parse_from_yaml_shape() {
    let yaml = r#"
plugins:
  - name: eraser
    kind: remote
    command: ["python", "-m", "eraser"]
    transport: stdio
    capabilities: [tools, observe]
    authority: reduced
    risk: unknown
"#;
    let parsed: crate::config::PluginsConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(parsed.plugins.len(), 1);
    match &parsed.plugins[0] {
        PluginEntry::Remote { name, authority, .. } => {
            assert_eq!(name, "eraser");
            assert_eq!(authority, "reduced");
        }
        _ => panic!("expected a remote plugin"),
    }
}

#[test]
fn authority_full_is_accepted() {
    let yaml = r#"
plugins:
  - name: gate
    kind: remote
    command: ["gate"]
    transport: stdio
    capabilities: [tools]
    authority: full
    risk: high
"#;
    let parsed: crate::config::PluginsConfig = serde_yaml::from_str(yaml).unwrap();
    assert_eq!(parsed.plugins.len(), 1);
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-app-server --lib composition`
Expected: FAIL —— `PluginsConfig` 不存在

- [ ] **Step 3: 加配置段**

`crates/uva-app-server/src/config/server.rs` 的 `ServerConfig` 加
`pub plugins: PluginsConfig`，带 `#[serde(default)]`。

`PluginEntry` 的 `Remote` 变体按 Interfaces 定义。`authority` 的合法值是
`reduced` / `full`；`risk` 的合法值是 `low` / `high` / `unknown`。
**非法值要报清晰的错误**，不要静默降级。

- [ ] **Step 4: 实现组装器**

`crates/uva-app-server/src/composition.rs`：

```rust
pub struct Composition {
    pub plugins: Vec<InstalledPlugin>,
}
pub struct InstalledPlugin {
    pub name: String,
    pub handle: PluginHandle,
}

impl Composition {
    /// Build the deployment's plugins from config and install them
    /// into `host`. Remote plugins are spawned; a plugin that fails
    /// to start is reported and skipped, not fatal.
    pub async fn build(
        config: &PluginsConfig,
        host: &mut Host,
    ) -> Result<Composition, CompositionError>;
}
```

**失败策略**：一个远端插件起不来**不应让整个 server 起不来** —— 记一条诊断、跳过它、
继续装配其它插件。理由：插件是可选能力，不是启动前提。（这与 harness 的 gate
fail-closed 语义不同 —— gate 是安全决策，插件启动失败不是。）

`boot.rs` 在建 agent 之后、`AppState` 组装时调用 `Composition::build`，把产生的
`PluginHandle` 存进 `AppState`（**handle 必须活到 agent 结束**，否则 drop 会撤销注册）。

- [ ] **Step 5: 跑测试**

Run: `cargo test -p uva-app-server --lib`
Expected: 全绿

- [ ] **Step 6: 提交**

```bash
git add crates/uva-app-server
git commit -m "feat(server): add the runtime plugin composition layer"
```

---

## Task 16: server 无状态化

**Files:**
- Modify: `crates/uva-app-server/src/session/controller/inner.rs`
- Modify: `crates/uva-app-server/src/session/controller/dispatch.rs`
- Modify: `crates/uva-app-server/src/session/controller/run_task.rs`
- Modify: `crates/uva-session/src/events.rs`
- Modify: `crates/uva-session/src/repair.rs`
- Modify: `crates/uva-attachment/src/lib.rs`
- Modify: `crates/uva-app-server/src/routes/follow.rs`
- Modify: `crates/uva-session/src/jsonl.rs`
- Test: `crates/uva-app-server/src/session/controller/tests/parked_prompt.rs`

**Interfaces:**
- Consumes: `uva_attachment::AttachmentStore`（已存在，`lib.rs:305`）
- Produces:
  ```rust
  // uva_session::events
  SessionEvent::ParkedPrompt {
      text: String,
      attachments: Vec<AttachmentRefRow>,
  }
  // uva_server::routes::follow
  pub(crate) async fn read_tail(
      sink: &dyn SessionSink,
      window: usize,
  ) -> Result<Vec<serde_json::Value>, SessionError>;
  ```

- [ ] **Step 1: 写失败的测试**

建 `crates/uva-app-server/src/session/controller/tests/parked_prompt.rs`：

```rust
#[tokio::test]
async fn a_parked_prompt_is_durable() {
    // Park a prompt, then assert the session log carries a row for
    // it — the row must survive a restart.
    let dir = tempdir();
    let sink = JsonlSessionSink::new(dir.path()).unwrap();
    let writer = LogWriter::new(sink.clone(), Arc::new(SystemClock));
    writer.append_event(&SessionEvent::ParkedPrompt {
        text: "hello".into(),
        attachments: vec![],
    }).await.unwrap();
    writer.flush().await.unwrap();

    let rows = sink.read().await.unwrap();
    assert!(
        rows.iter().any(|row| row["type"] == "ParkedPrompt"),
        "parked prompt row missing: {rows:?}"
    );
}
```

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-app-server --lib parked_prompt`
Expected: FAIL —— `SessionEvent::ParkedPrompt` 不存在

- [ ] **Step 3: 让排队的多模态 prompt 落盘**

`uva-session/src/events.rs` 加 `SessionEvent::ParkedPrompt`。

`dispatch.rs:263`（`handle_prompt_multi_op`）在 park 的同时追加这一行。
**字节不进日志**：图片/音频先经 `AttachmentStore::save_image` 存下（内容寻址，
`hash` 做键），日志行里存**可回读的 ref**（含 `hash`）——
现在的 `attachment_refs_of_parts`（`run_log.rs:100`）只产出元数据
（`kind` / `name` / `mime_type` / `byte_len`），**没有 `hash`，回读不了**，
要给它补上 `hash` 字段。

`uva-attachment/src/lib.rs` 的 `ImageAttachmentRef` 已有 `hash` 与
`matches()` —— ref 行的字段按它来。

`repair.rs` 要认识 `ParkedPrompt`：崩溃后残留的排队 prompt 要能被恢复。

- [ ] **Step 4: 让 `follow` 基线改尾读**

现在 `follow.rs:174-179` 调 `sink.read()` → `JsonlSessionSink::read()` →
对整个 `events.jsonl` 做 `fs::read`（`jsonl.rs:436`），200 行的窗口是**读完之后
在内存里切的**（`follow.rs:210-214`）。长会话下每次订阅都整文件读。

改法：`uva-session/src/jsonl.rs` 加一个按尾部窗口读的路径
（`read_tail(window)`：从文件尾反向读行，取最后 `window` 行），
`follow.rs` 改调它。

**注意**：`sink.read()` 的全量语义被别处依赖（`derive_history` 要全量）——
**不要改 `read()` 本身**，新加 `read_tail` 并只让 `follow` 用它。

- [ ] **Step 5: 租约转移后的 controller 重建**

`cluster/` 现有的是 TTL 过期 + 重新获取（`redis_bus.rs:151-181`、
`local.rs:111-143`），**没有转移语义**。本步加：当一个节点因租约过期而不再持有
会话时，另一个节点接手，从日志重建 controller 状态
（`repair` → `fold` → `SessionState`）。

`pending_resume` 的恢复按设计文档 §5.11 的取舍：**靠客户端重发**，服务端不替客户端
记住决策（`ResumeToken` 按最后 durable seq + 未决集合摘要派生，重复提交幂等）。

- [ ] **Step 6: 跑测试**

Run:
```bash
cargo test -p uva-session --lib
cargo test -p uva-app-server --lib
```
Expected: 全绿

- [ ] **Step 7: 提交**

```bash
git add crates/uva-session crates/uva-app-server crates/uva-attachment
git commit -m "feat(server): make parked prompts durable and read the follow baseline by tail"
```

---

## Task 17: 零 tokio 消费方 fixture 与前后端贯通

**Files:**
- Create: `docs/examples/harness-only-consumer/Cargo.toml`
- Create: `docs/examples/harness-only-consumer/src/main.rs`
- Modify: `Makefile`
- Modify: `uva-web/src/...`（组合层与远端插件状态）
- Modify: `docs/tutorial/complete-agent.md`
- Test: `docs/examples/harness-only-consumer/` 自身的运行

**Interfaces:**
- Consumes: `uva-harness`（Task 6）、`uva-harness-tokio`（Task 3）、
  `uva_plugin_host`（Task 12）
- Produces: 无新公开 API（这是验收 fixture 与文档）

- [ ] **Step 1: 建只依赖 harness 的消费方**

`docs/examples/harness-only-consumer/Cargo.toml`：
```toml
[package]
name = "harness-only-consumer"
version = "0.0.0"
edition = "2024"

[dependencies]
uva-harness = { path = "../../../crates/uva-harness" }
futures = "0.3"
serde_json = "1"
```

**没有 tokio。** `main.rs` 用 `futures::executor::block_on` 跑一轮
（照 `crates/uva-harness/examples/runtime_agnostic.rs` 的形状）。

- [ ] **Step 2: 验证零 tokio**

Run:
```bash
cargo tree --manifest-path docs/examples/harness-only-consumer/Cargo.toml | grep -c tokio
```
Expected: `0`

若不为 0，说明 harness 的闭包还有 tokio —— 回查 Task 4/5。

- [ ] **Step 3: 跑通一轮**

Run: `cargo run --manifest-path docs/examples/harness-only-consumer/Cargo.toml`
Expected: 打印一行 proof（照既有 example 的形状，如 `HARNESS-ONLY: OK`）

- [ ] **Step 4: 接进 Makefile**

`make examples` 要跑这个新 fixture（照其它 example 的接法）。

- [ ] **Step 5: 前端接组合层与远端插件状态**

前端要能看到"这个部署装了哪些插件、哪些是远端的、它们的能力是什么"。
加一个 `GET /api/v1/plugins` 端点（server 侧从 `Composition` 读），
前端在 session 详情或设置页显示。

**具体 UI 由实现者定**，但要有可观察断言（端点返回配置里的插件列表）。

- [ ] **Step 6: 补教程**

`docs/tutorial/complete-agent.md` 加两章：
- "写一个静态插件"（Rust crate，`impl Plugin` → `install` → `PluginHandle`）
- "写一个动态插件"（任意语言进程，按 `uva-plugin-protocol` 的方法集）

从空环境照做，不需要读源码。

- [ ] **Step 7: 跑完整门禁**

Run:
```bash
make fmt-check
make lint-rust
make ci
make check-web
make examples
```
Expected: 全绿

- [ ] **Step 8: 提交**

```bash
git add -A docs/examples uva-web Makefile docs/tutorial
git commit -m "docs: add the harness-only consumer fixture and the plugin tutorials"
```

---

## Task 18: 全量验收

**Files:**
- 无新文件（跑门禁 + 记录）

**Interfaces:**
- Consumes: 全部前 17 个任务的产出
- Produces: 无新公开 API（这是验收任务）

- [ ] **Step 1: 跑机械验收**

Run:
```bash
cargo tree -p uva-harness -e normal | grep -c tokio
cargo tree -p uva-harness -e normal | grep -cE "reqwest|hyper|rustls|h2|tower"
cargo tree -p uva-plugin-protocol -e normal | grep -c tokio
```
Expected: `0` / `0` / `0`

- [ ] **Step 2: 跑全部门禁**

Run:
```bash
make ci
make check-web
make examples
make bench-check
```
Expected: 全绿

- [ ] **Step 3: 跑端到端剧本**

按 `docs/tutorial/complete-agent.md` 的 A–G 剧本，加 H（远端插件）、I（无状态重启），
在真实 `uva-app-server` + `uva-web` 上跑。每条要可观察断言。

- [ ] **Step 4: 记录验收结果**

把 A–I 的观察记录写进 `docs/design/2026-10-26-harness-v4-plan.md` 末尾的
"验收记录"小节（或新建 `docs/design/2026-10-26-harness-v4-acceptance.md`）。

- [ ] **Step 5: 提交**

```bash
git add docs/design
git commit -m "docs: record the harness v4 acceptance pass"
```
