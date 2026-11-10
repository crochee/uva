# 共享网关 crate 与 spec 收口 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让两个宿主（TUI 与桌面壳）共用同一套"拉起 / 附着网关 + 说网关协议"的代码，并把与代码矛盾的 spec 收干净。

**Architecture:** 新增 `crates/elysper-gateway`（`publish = false` 的应用支撑 crate，root workspace 成员）：
`supervisor` 管**进程**（拉起 `elysper-app-server serve`、等 `/readyz`、退避重启、退出回收），
`client` 管**协议**（网关 WebSocket 上的 JSON-RPC 2.0：按 id 关联请求响应、通知扇出、断线重连并
重放订阅）。公开面**只用 `futures`**（`BoxFuture` / `mpsc`），内部用 tokio —— 这样
`make check-public-api-runtime` 不需要为它开豁免名单（C7）。

**Tech Stack:** Rust 1.99（stable 通道）、tokio 1.53、futures 0.3、serde_json 1.0、thiserror 2.0、tokio-tungstenite 0.29、anyhow 1.0（仅二进制边界）

**Spec:** `docs/design/2026-11-09-desktop-frontend-tree-design.md` —— §7 的 D1 / D3 / D12 / D13 / D14，§8 的刀 0 与刀 1，§3 的 C1 / C5 / C7 / C11

## Global Constraints

- 错误处理：**lib 用 `thiserror`，app 用 `anyhow`**（AGENTS.md §3.1）。本 crate 是 lib ⇒ 只用 `thiserror`。
- 依赖版本一律 `major.minor`（`"1.53"` 而非 `"1.53.0"`）；**复用 `[workspace.dependencies]`**，
  crate 内写 `dep.workspace = true`（AGENTS.md §3.1）。
- tokio feature **按需声明**，禁止 `full`（AGENTS.md §1）。
- 公开面**不得出现 `tokio::` 路径**（`make check-public-api-runtime` 扫全部 `crates/**`，
  豁免只有 `elysper-app-server` 与 `elysper-mcp-server`）⇒ 用 `futures::future::BoxFuture`。
- 禁止 `pub use <内部模块>::*;`（AGENTS.md §3.7）；只写显式 `pub use`。
- 本 crate 依赖 `elysper-app-server`（`publish = false`）⇒ 本 crate **也 `publish = false`**。
- 新成员必须**同时**进 root `Cargo.toml` 的 `members` 与 `scripts/check-publishable.sh` 的 `ORDER`
  （该脚本用 `cargo metadata` 比对，两者必须完全一致）。
- 每次提交前：`cargo +nightly fmt --all` + `cargo clippy --all-targets --all-features --tests --all -- -D warnings`。
- 测试按 crate 分批：`cargo test -p elysper-gateway`（**禁止** `cargo test --workspace`，AGENTS.md §3.3）。
- 不 push 远程仓库。

## Review Focus

下面五类是规格暗示、但没有哪条测试覆盖，最可能咬人的地方。每条都在下面对应的任务里钉上测试。

1. **网关进程活着但 core 没起来**：网关 `/readyz` 只在 core 宣布 `ready` 后才 200。探针必须读
   **状态行**（`HTTP/1.1 200`），不能只看"端口能连"——否则会出现"网关已监听但 core 还在起"的假就绪。
2. **网关进程死了 vs 退避期间**：退避窗口内 `base_url()` / `ws_url()` 仍然返回旧值，
   而 `states()` 必须发出 `Restarting`；调用方若只看 URL 会连到一个不存在的服务。
3. **core 重启 ≠ 网关重启**：网关自己的 core 挂掉时**网关不重启**（网关会自己重启 core 并广播
   `ready`）；此时客户端的订阅被网关丢弃，**必须重放**——否则 TUI 的会话跟随永远回不来。
4. **订阅重复注册**：重连后重放订阅，若订阅处理函数重复挂上，会收到两份同样的通知。
5. **`port = 0` 的语义**：宿主传 0 表示"由本 crate 选一个空闲端口"；选出来的端口必须被
   **真正传给 `serve`**，且 `base_url()` 与 `ws_url()` 用的是同一个端口。

---

### Task 1: spec 收口（刀 0）

**Files:**
- Modify: `openspec/specs/web-projection/spec.md`（**本地层，不入库**，见 `.gitignore:41`）
- Create: `openspec/specs/desktop-projection/spec.md`（本地层）
- Create: `openspec/specs/shared-interface/spec.md`（本地层）
- Modify: `AGENTS.md`（§1 环境段）
- Modify: `CHANGELOG.md`（`[Unreleased]`）

**Interfaces:**
- Consumes: 无（纯文档任务）
- Produces: 本地 openspec 层与代码一致；入库文档说明 `crates/elysper-gateway` 的存在

- [ ] **Step 1: 改 `web-projection` 的两条禁令**

把《Requirement: 不引入 TUI 与 Desktop》整条替换为现状陈述，两句都改：

```
### Requirement: 表现层宿主与插件入口

本变更 MUST NOT 新增**第三套**表现层宿主；已批准的宿主是
`elysper-app-server`（网关与静态前端）、`elysper-web`（SPA）、`elysper-tui`（终端渲染）
与 `desktop/`（Tauri 壳）。表现层 crate 白名单为 `elysper-app-server`、`elysper-web`、
`elysper-tui`、`crates/elysper-gateway`、`desktop/`。

#### Scenario: 新增一个表现层 crate
- **WHEN** 检查新增 crate 与依赖
- **THEN** 不在白名单里的表现层 crate 即为违规
```

- [ ] **Step 2: 写 `desktop-projection` spec**

至少覆盖三条 Requirement：sidecar 契约（壳 spawn `elysper-app-server serve`，等 `/readyz`）、
生命周期（崩溃重启、退出回收、无孤儿进程）、单一前端（桌面与浏览器同一份 `dist`）。
每条 Requirement 至少一个 `#### Scenario:`，写法照 `web-projection/spec.md` 的既有格式。

- [ ] **Step 3: 写 `shared-interface` spec**

覆盖：三个宿主同一套界面（web/desktop 同一份 SPA，TUI 是终端渲染）；对话优先的骨架
（侧栏 / 对话流 / 待办进度 / 输入框）；设置面**只是 profile 的反推**——**没有 settings 文件**
（D20）：profile 行 = 插件 + 小配置，细粒度启用在行上，插件的私有设置归插件自己；
插件排布以 profile 为唯一来源；禁用 = 该行不挂载；族的表达是服务契约而不是父子挂载（D38）。

- [ ] **Step 4: 核对本地层不入库**

Run: `git check-ignore -v openspec/specs/web-projection/spec.md`
Expected: 输出 `.gitignore:41:openspec`（命中 ⇒ 本地层不进 diff，符合仓库既定立场）

- [ ] **Step 5: 确认无需提交**

`openspec/` 是本地层（`.gitignore:41`），**不进提交**；AGENTS.md / CHANGELOG 的登记
随 Task 2（crate 真正落地时）一起做——文档必须描述已存在的东西。

---

### Task 2: crate 骨架与公开类型

**Files:**
- Create: `crates/elysper-gateway/Cargo.toml`
- Create: `crates/elysper-gateway/src/lib.rs`
- Create: `crates/elysper-gateway/src/supervisor.rs`
- Create: `crates/elysper-gateway/src/client.rs`
- Create: `crates/elysper-gateway/src/tests.rs`
- Create: `crates/elysper-gateway/README.md`
- Modify: `Cargo.toml`（`[workspace.dependencies]` 加 `tokio-tungstenite`；`[workspace] members` 加本 crate）
- Modify: `scripts/check-publishable.sh`（`ORDER` 加 `elysper-gateway`）

**Interfaces:**
- Consumes: `elysper-app-server` 的 CLI 契约（`serve --bind --port --web-dir --directory --profile --bundle --patch`，见 `crates/elysper-app-server/src/main.rs:126`）
- 末尾两步：AGENTS.md §1 登记（在 `crates/elysper-tui/` 段之后加一段：elysper-gateway =
  两宿主共用的网关支撑——进程监管 + 网关 JSON-RPC 客户端，`publish = false`，公开面只用
  `futures`）与 `CHANGELOG.md` `[Unreleased]` 一条（"投影第二波的地基：新增 elysper-gateway"）
- Produces: 后续任务使用的类型

```rust
// crates/elysper-gateway/src/lib.rs
pub struct GatewayOptions {
    /// `elysper-app-server` 可执行文件路径。
    pub executable: PathBuf,
    /// 网关绑定地址，默认 `127.0.0.1`。
    pub bind: String,
    /// 网关端口；`0` 表示由本 crate 选一个空闲端口（D4）。
    pub port: u16,
    /// 工作区目录（透传给 `serve --directory`）。
    pub directory: PathBuf,
    pub profile: Option<PathBuf>,
    pub bundle: Option<PathBuf>,
    pub patch: Option<PathBuf>,
    /// 静态前端目录（透传给 `serve --web-dir`）。
    pub web_dir: Option<PathBuf>,
}

pub enum GatewayState {
    Starting,
    Ready { base_url: String, ws_url: String },
    Restarting { attempt: u32 },
    Failed { reason: String },
}

pub enum GatewayError {
    Spawn { program: String, source: std::io::Error },
    NotReady { reason: String },
    Shutdown,
}

pub struct Gateway { /* … */ }
impl Gateway {
    pub fn start(options: GatewayOptions) -> BoxFuture<'static, Result<Gateway, GatewayError>>;
    pub fn base_url(&self) -> &str;
    pub fn ws_url(&self) -> &str;
    pub fn states(&self) -> futures::channel::mpsc::UnboundedReceiver<GatewayState>;
    pub fn shutdown(&self);
}

pub enum ClientError {
    Connect { url: String, reason: String },
    Closed,
    Transport { reason: String },
    Server { code: i64, message: String },
}

pub struct GatewayClient { /* … */ }

```rust
// 刀 2 修正：订阅回调需要方法名（TUI 的 Notification 事件带 method），
// 落地为公开别名 `NotificationHandler = Arc<dyn Fn(&str, Value) + Send + Sync>`。
```
impl GatewayClient {
    pub fn connect(url: &str) -> BoxFuture<'static, Result<GatewayClient, ClientError>>;
    pub fn request(&self, method: &str, params: serde_json::Value)
        -> BoxFuture<'static, Result<serde_json::Value, ClientError>>;
    pub fn notify(&self, method: &str, params: serde_json::Value) -> Result<(), ClientError>;
    pub fn subscribe(&self, method: &str, handler: NotificationHandler)
        -> Result<Subscription, ClientError>;
    /// catch-all（刀 2 补）：本地全量分发，不上线注册、断线不重放——
    /// 网关按宿主自己的请求（如 `sessions/follow`）路由通知。
    pub fn subscribe_all(&self, handler: NotificationHandler)
        -> Result<Subscription, ClientError>;
    pub fn close(&self);
}

/// 订阅句柄：drop 即退订（对齐内核 Effect 的"注册即返回句柄"语义）。
pub struct Subscription { /* … */ }
```

可注入缝（测试用，**不公开给 crate 外**，`pub(crate)` 即可）：

```rust
// supervisor 内部
pub(crate) trait Launcher: Send + Sync + 'static {
    fn launch(&self, options: &GatewayOptions) -> std::io::Result<Box<dyn GatewayProcess>>;
}
pub(crate) trait GatewayProcess: Send + Sync + 'static {
    fn next_frame(&self) -> BoxFuture<'static, Option<Vec<u8>>>;
    fn wait(&self) -> BoxFuture<'static, std::io::Result<()>>;
    fn kill(&self);
}
```

- [ ] **Step 1: 写失败的测试**

`crates/elysper-gateway/src/tests.rs`：

```rust
use crate::{GatewayOptions, GatewayState};

#[test]
fn options_default_to_loopback_and_an_unbound_port() {
    let options = GatewayOptions::new(PathBuf::from("elysper-app-server"), PathBuf::from("/tmp"));
    assert_eq!(options.bind, "127.0.0.1");
    assert_eq!(options.port, 0);
    assert!(options.profile.is_none());
}

#[test]
fn the_ready_state_carries_the_base_and_ws_urls() {
    let state = GatewayState::Ready { base_url: "http://127.0.0.1:9000".into(), ws_url: "ws://127.0.0.1:9000/rpc".into() };
    match state {
        GatewayState::Ready { base_url, ws_url } => {
            assert!(base_url.starts_with("http://"));
            assert!(ws_url.ends_with("/rpc"));
        }
        other => panic!("unexpected state: {other:?}"),
    }
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p elysper-gateway --lib`
Expected: FAIL（`could not find crate elysper_gateway` / `unresolved import`）

- [ ] **Step 3: 建 crate 并实现类型**

`crates/elysper-gateway/Cargo.toml`（照 `crates/elysper-tui/Cargo.toml` 的写法与注释风格）：

```toml
[package]
name = "elysper-gateway"
description = "Shared gateway support for elysper hosts: process supervision plus the gateway's JSON-RPC client"
# Application support crate: it depends on `elysper-app-server`, which is
# `publish = false`, so this one is too — nothing here belongs on a registry.
publish = false
version.workspace = true
edition.workspace = true
rust-version.workspace = true
license.workspace = true
repository.workspace = true
homepage.workspace = true
documentation.workspace = true
readme = "README.md"
keywords.workspace = true
categories.workspace = true

[dependencies]
# The core's transport contract: the Content-Length frame codec is
# consumed from the public `rpc::framing` seam, never reimplemented.
# `elysper-app-server` is `publish = false`, which is why this crate is
# too — nothing here belongs on a registry.
elysper-app-server.workspace = true
serde_json.workspace = true
futures.workspace = true
thiserror.workspace = true
# The gateway speaks plain JSON per WebSocket message (the browser side
# does the same); the Content-Length framing is only between the gateway
# and the core.
tokio-tungstenite.workspace = true
# The supervisor's runtime: `rt-multi-thread` (the supervisor task),
# `macros` (`#[tokio::main]`, tests), `process` (spawning the server),
# `io-util` (the framed pipes), `net` (the readiness probe), `time`
# (the restart backoff), `sync` (the state channel).
tokio = { workspace = true, features = [
  "rt-multi-thread",
  "macros",
  "process",
  "io-util",
  "net",
  "time",
  "sync",
] }

[dev-dependencies]
tokio = { workspace = true, features = ["macros", "rt", "io-util", "time"] }
```

`src/lib.rs`：

```rust
//! Shared gateway support for elysper's hosts.
//!
//! Two halves:
//!
//! - [`supervisor`] — the **process**: launch `elysper-app-server serve`,
//!   wait for `/readyz`, restart it with a 1s → 5s backoff, and reap it on
//!   drop. The TUI and the desktop shell both need exactly this.
//! - [`client`] — the **protocol**: JSON-RPC 2.0 over the gateway's
//!   WebSocket (`/rpc`), correlating responses by `id`, fanning
//!   notifications out to subscribers, and replaying subscriptions after
//!   a reconnect.
//!
//! The public surface uses `futures` only (no `tokio::` paths), so
//! `make check-public-api-runtime` needs no exemption for this crate.
//! `tokio` is an implementation detail.

#![forbid(unsafe_code)]

mod client;
mod supervisor;
mod tests;

pub use client::{ClientError, GatewayClient, Subscription};
pub use supervisor::{Gateway, GatewayError, GatewayOptions, GatewayState};
```

把 `GatewayOptions::new(executable, directory)` 写进 `supervisor.rs`，
`GatewayOptions` 的字段全部 `pub`。

- [ ] **Step 4: 登记 crate 与依赖**

- root `Cargo.toml` 的 `[workspace.dependencies]` 加一行（版本格式 `major.minor`）：
  `tokio-tungstenite = "0.29"`
- root `Cargo.toml` 的 `[workspace] members` 加 `"crates/elysper-gateway",`
- `scripts/check-publishable.sh` 的 `ORDER` 加 `elysper-gateway`（放在 `elysper-gateway`
  依赖的 `elysper-app-server` **之后**，列表按依赖拓扑排序）

- [ ] **Step 5: 跑测试，确认通过**

Run: `cargo test -p elysper-gateway --lib`
Expected: PASS（2 passed）

- [ ] **Step 6: 确认门禁没被新 crate 破掉**

Run: `make check-public-api-runtime && make check-publishable`
Expected: 两个都输出 `OK:`（`check-publishable` 会因为新成员不在 `ORDER` 而失败——
所以 Step 4 必须做完再跑）

- [ ] **Step 7: 提交**

```bash
git add crates/elysper-gateway Cargo.toml scripts/check-publishable.sh
git commit -m "feat(gateway): 新增共享网关 crate 的骨架与公开类型"
```

---

### Task 3: supervisor —— 拉起网关并等就绪

**Files:**
- Modify: `crates/elysper-gateway/src/supervisor.rs`
- Modify: `crates/elysper-gateway/src/tests.rs`

**Interfaces:**
- Consumes: Task 2 的 `GatewayOptions` / `GatewayState` / `GatewayError` 与 `Launcher` / `GatewayProcess` 缝
- Produces: `Gateway::start`（真实实现）、`Gateway::states`

- [ ] **Step 1: 写失败的测试（假进程，不起真进程）**

在 `tests.rs` 里加一个假进程与假探针：

```rust
use crate::supervisor::{GatewayProcess, Launcher};
use std::sync::{Arc, Mutex};

/// 一个假网关进程：可以脚本化它的"就绪"与"死亡"。
struct FakeProcess {
    /// 探针会依次返回这些状态行；空表示还没就绪。
    ready_after: Mutex<Vec<String>>,
    exited: Mutex<bool>,
}

impl GatewayProcess for FakeProcess {
    fn next_frame(&self) -> futures::future::BoxFuture<'static, Option<Vec<u8>>> {
        Box::pin(async move { None })
    }
    fn wait(&self) -> futures::future::BoxFuture<'static, std::io::Result<()>> {
        Box::pin(async move {
            loop {
                if *self.exited.lock().unwrap() { return Ok(()); }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
    }
    fn kill(&self) { *self.exited.lock().unwrap() = true; }
}
```

再写两条测试：

```rust
#[tokio::test]
async fn the_supervisor_reports_ready_only_after_the_probe_returns_200() { /* … */ }

#[tokio::test]
async fn the_supervisor_gives_up_with_a_reason_when_the_probe_never_succeeds() { /* … */ }
```

第一条的断言：`states()` 里第一条是 `GatewayState::Starting`，探针返回 `HTTP/1.1 503` 时**不能**
出现 `Ready`，探针返回 `HTTP/1.1 200 OK` 之后才出现 `GatewayState::Ready { base_url, ws_url }`，
且 `base_url` 的端口 == `ws_url` 的端口 == 传给 `serve` 的端口。
第二条的断言：探针连续失败 ⇒ 最终收到 `GatewayState::Failed { reason }`，`reason` 里含
"ready" 或"就绪"这类可读信息（不是空串）。

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p elysper-gateway --lib supervisor`
Expected: FAIL（`supervisor` 行为未实现）

- [ ] **Step 3: 实现 `supervisor.rs`**

要点：

- `Gateway::start(options)` 是 `BoxFuture`：内部 `tokio::spawn` 一个监督任务，
  任务持有 `Arc<Mutex<Option<GatewayProcess>>>` 与状态发送端。
- **端口选择**（D4）：`options.port == 0` 时，先
  `std::net::TcpListener::bind(("127.0.0.1", 0))` 取端口、读出后 `drop`（释放占用），
  再把该端口写进 `serve --port`；`bind` 与 `port` 都进 `serve` 的参数。
- **拉起命令**：`serve --bind <bind> --port <port> --directory <directory>`，
  以及可选的 `--web-dir` / `--profile` / `--bundle` / `--patch`；`executable` 是可执行文件路径。
  子进程的 **stderr 继承**（日志落到调用方的 stderr，不进协议流），stdout 不读
  （网关的 stdout 不是协议通道——协议走 WebSocket）。
- **就绪探针**：用 `tokio::net::TcpStream` 连 `bind:port`，写
  `GET /readyz HTTP/1.1\r\nHost: <bind>:<port>\r\nConnection: close\r\n\r\n`，
  读**第一行**并解析状态码；只有 `200` 才算就绪。**不能**只看"端口能连"。
- **状态广播**：每代进程启动时发 `Starting`；探针成功发 `Ready { base_url, ws_url }`；
  进程死亡发 `Restarting { attempt }`；连续失败到上限发 `Failed { reason }` 并停止。
- `base_url()` / `ws_url()` 读同一份 `Arc<Mutex<GatewayUrls>>`，**端口始终一致**。
- `shutdown()`：置一个 `AtomicBool` 停止监督循环，并 `kill()` 当前子进程。

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p elysper-gateway --lib`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/elysper-gateway
git commit -m "feat(gateway): 监督网关进程并等它真正就绪"
```

---

### Task 4: supervisor —— 退避重启与回收

**Files:**
- Modify: `crates/elysper-gateway/src/supervisor.rs`
- Modify: `crates/elysper-gateway/src/tests.rs`

**Interfaces:**
- Consumes: Task 3 的 `supervisor` 与 `GatewayState`
- Produces: 重启语义（1s → 5s 退避）、`shutdown` 的回收保证

- [ ] **Step 1: 写失败的测试**

```rust
#[tokio::test]
async fn a_dead_gateway_is_restarted_with_a_backoff() { /* … */ }

#[tokio::test]
async fn dropping_the_gateway_kills_the_child_process() { /* … */ }
```

第一条断言：假进程在第一次 `wait()` 返回后被杀掉 ⇒ 监督任务发出
`GatewayState::Restarting { attempt: 1 }`，随后新一代进程被拉起（假 `Launcher` 记录启动次数）。
**退避用可注入的时钟**（测试里把 1s/5s 换成毫秒级），不要在测试里真等 1 秒。
第二条断言：`Gateway` 被 drop（或 `shutdown()` 被调用）后，假进程收到 `kill()`。

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p elysper-gateway --lib supervisor`
Expected: FAIL

- [ ] **Step 3: 实现退避与回收**

- 退避常量：`const RESTART_BACKOFF_MIN: Duration = Duration::from_secs(1);`
  `const RESTART_BACKOFF_MAX: Duration = Duration::from_secs(5);`
  第 n 次重启等待 `min(1s * 2^(n-1), 5s)`；**测试通过注入的 sleeper 跳过真实等待**
  （把等待抽成一个 `Box<dyn Fn(Duration) -> BoxFuture<'static, ()>>` 内部缝）。
- `Drop for Gateway`：`kill()` 子进程 + 停止监督任务（监督任务用 `CancellationToken` 语义的
  `AtomicBool` 即可，本 crate 不引 `elysper-core` 的 `CancelToken`）。
- **进程组**：若子进程可能再派生子进程，`kill()` 前先 `setsid` 不做（本轮不做进程组管理，
  在 README 里写明"杀的是直接子进程"）。

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p elysper-gateway --lib`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/elysper-gateway
git commit -m "feat(gateway): 网关崩溃后退避重启，退出时回收子进程"
```

---

### Task 5: client —— 网关 WebSocket 上的 JSON-RPC

**Files:**
- Modify: `crates/elysper-gateway/src/client.rs`
- Modify: `crates/elysper-gateway/src/tests.rs`

**Interfaces:**
- Consumes: Task 2 的 `GatewayClient` / `ClientError` / `Subscription`
- Produces: TUI 与桌面壳共用的协议客户端

- [ ] **Step 1: 写失败的测试（进程内 WS 服务器）**

用 `axum`（workspace 依赖，`features = ["ws"]`）在测试里起一个**进程内**的 WebSocket 服务器，
它把收到的帧原样回送（或按脚本回答），用来验证客户端：

```rust
#[tokio::test]
async fn a_request_is_answered_by_the_response_with_the_same_id() { /* … */ }

#[tokio::test]
async fn a_notification_reaches_every_subscriber() { /* … */ }

#[tokio::test]
async fn subscriptions_are_replayed_after_a_reconnect() { /* … */ }

#[tokio::test]
async fn a_server_error_object_becomes_a_client_error() { /* … */ }
```

第三条是 Review Focus #3/#4 的钉子：断开服务器 ⇒ 客户端重连 ⇒ **订阅处理函数只被调用一次**
（不是两次），且重连后仍能收到通知。

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p elysper-gateway --lib client`
Expected: FAIL

- [ ] **Step 3: 实现 `client.rs`**

- `connect(url)`：`tokio_tungstenite::connect_async`（或 `client_async`）——
  **url 形如 `ws://127.0.0.1:9000/rpc`**。
- 帧格式：**一条 WebSocket 文本消息 = 一个 JSON-RPC 2.0 报文**（与浏览器侧一致；
  Content-Length 分帧只在网关 ↔ core 之间）。
- id 关联：客户端自增 `c1`、`c2`…（与 `elysper-web/src/api/rpc.ts` 同风格），
  请求挂 `oneshot`；**响应按 id 路由**，字符串 id 与数字 id 归一到同一个键。
- 通知：按 `method` 分发给订阅者；订阅表用 `Arc<Mutex<HashMap<String, Vec<Arc<dyn Fn(Value)>>>>>`，
  **`Subscription` drop 时移除该条**（对齐内核 Effect 的"注册即返回句柄"）。
- **重连**：断线后按 250ms → 2s 退避重连；**重连成功后重放全部订阅**（但**不重复挂载**——
  重放前先清空再挂，保证处理函数不被调用两次）。
- 服务端错误：`{"error": {...}}` ⇒ `ClientError::Server { code, message }`。
- 公开面**不得出现 `tokio::`**：内部把 WS 流放进 `Arc<Mutex<...>>`，对外只暴露
  `BoxFuture` 与 `Arc<dyn Fn(Value)>`。

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p elysper-gateway --lib`
Expected: PASS

- [ ] **Step 5: 提交**

```bash
git add crates/elysper-gateway
git commit -m "feat(gateway): 网关 WebSocket 上的 JSON-RPC 客户端"
```

---

### Task 6: crate 收口 —— README、门禁与自检

**Files:**
- Create/Modify: `crates/elysper-gateway/README.md`
- Modify: `crates/elysper-gateway/src/lib.rs`（若需要补模块文档）

**Interfaces:**
- Consumes: 全部前置任务
- Produces: 可被 `make ci` 覆盖的 crate

- [ ] **Step 1: 写 README**

`crates/elysper-gateway/README.md` 至少写清：它是什么（两个宿主共用的网关支撑）、
两半的分工（进程 / 协议）、怎么用（`Gateway::start` → `base_url()` → `GatewayClient::connect`）、
以及两条边界（`publish = false`；公开面只用 futures，`tokio` 是实现细节）。
写成 markdown，遵循仓库既有的 README 风格（短段落 + 代码块），`.markdownlint.json` 覆盖它。

- [ ] **Step 2: 跑格式与 lint**

Run: `cargo +nightly fmt --all && cargo clippy --all-targets --all-features --tests --all -- -D warnings`
Expected: 无输出（0 warning）

- [ ] **Step 3: 跑本 crate 的测试**

Run: `cargo test -p elysper-gateway`
Expected: 全部 passed

- [ ] **Step 4: 跑门禁**

Run: `make ci`
Expected: 全绿（833s 量级）。若 `check-publishable` 报"成员与 ORDER 不一致"，
说明 Task 2 Step 4 漏了——补上再跑。

- [ ] **Step 5: 提交**

```bash
git add crates/elysper-gateway
git commit -m "docs(gateway): 补 README 并跑通全量门禁"
```
