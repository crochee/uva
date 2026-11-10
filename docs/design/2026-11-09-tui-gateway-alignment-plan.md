# TUI 并轨到网关与对话优先骨架 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** TUI 不再自己 spawn `core`、不再自己分帧：改连 `elysper-gateway`（与桌面壳、浏览器同一条路——`serve` 的 `/rpc` WebSocket）；界面按设计文档 §6.1 换成对话优先骨架（侧栏工作区树 + 右对齐气泡 + 思考块两态 + 待办进度 + bash 风格输入框）。

**Architecture:**
- **传输层**：`crates/elysper-tui/src/transport.rs` 瘦身成"网关客户端的领域适配"——`RequestKind` ↔ 方法名映射、按 id 关联请求响应、`ready` 后重发 `sessions/follow`。Content-Length 分帧、进程退避重启、stdout/stdin 管道**全部移出 TUI**（`elysper-gateway` 负责）。
- **进程模型**：TUI 拉起的是 `elysper-app-server serve`（网关），不再是 `core`；`--attach <url>` 连已在跑的网关（不 spawn）。
- **界面**：新增 `crates/elysper-tui/src/views/`（侧栏 / 对话流 / 待办 / 输入框 / 设置导航）；`app.rs` 只**新增**状态字段与动作，既有行的 reducer 语义不动（C12）。

**Tech Stack:** Rust stable（1.98 MSRV）、ratatui、crossterm、tokio（app crate 可用具体 runtime）、futures、`elysper-gateway`（workspace 依赖）

**Spec:** `docs/design/2026-11-09-desktop-frontend-tree-design.md` 的 §5.2 / §6.1 / 刀 2 / C5 / C12；Plan 1（`docs/design/2026-11-09-gateway-crate-plan.md`）的 `Gateway` / `GatewayOptions` / `GatewayClient` / `Subscription` 接口。

## Global Constraints

- TUI 是 app crate（`publish = false`）⇒ 错误用 `anyhow` 可以；但 lib 的公开面**仍不得出现 `tokio::` 路径**（C7：`scripts/check-public-api-runtime.sh` 扫全部 `crates/**`，豁免只有 `elysper-app-server` / `elysper-mcp-server`）。
- `elysper-tui` 依赖 `elysper-gateway` ⇒ `scripts/check-publishable.sh` 的 `ORDER` 里 `elysper-gateway` 必须排在 `elysper-tui` **之前**（Plan 1 已加 gateway 行，本计划 Task 2 核对这个顺序）。
- 依赖版本 `major.minor`；crate 内 `dep.workspace = true`，不写版本号。
- 禁止通配符导入；禁止 `pub use <内部模块>::*;`。
- 每次提交前：`cargo +nightly fmt --all` + `cargo clippy --all-targets --all-features --tests --all -- -D warnings`。
- 测试按 crate 分批：`cargo test -p elysper-tui`（**禁止** `cargo test --workspace`）。
- 不 push 远程仓库。

## Review Focus

1. **网关的 `ready` 会广播**（已核实：`crates/elysper-app-server/src/gateway.rs:213` —— `Some("ready") => self.broadcast(frame)`）。⇒ TUI 的 `TransportEvent::Ready { version, workspace_root }` 契约在新链路上**原样成立**，不需要网关改动。
2. **id 关联的形状变了**：旧 TUI 自己分配 id 并在响应里映射回 `RequestKind`；新 `GatewayClient::request()` 是一个 future ⇒ 必须**每个请求 spawn 一个 task**，把结果投递回事件通道（`TransportEvent::Response`），否则 `app.rs` 的既有契约就断了。这是"既有语义不动"的关键点。
3. **core 重启后订阅要重放**：网关重启 core 后，WS 连接**不断**（网关进程没死），但 core 的会话订阅没了 ⇒ 新一代 `ready` 通知到来时必须重发 `sessions/follow`。这个行为今天在 TUI 的 reducer 里（`apply_ready` 后发 follow），**保留原逻辑即可**，但要在测试里钉住。
4. **`--attach` 不 spawn**：连已存在的网关时，网关死掉 TUI **不能**自己重启网关 ⇒ UI 显示"网关不可达"，而不是假装在退避重启。
5. **旧机制删干净**：`LaunchOptions` / `SpawnedCore` / Content-Length 分帧消费（`elysper_app_server::rpc::framing`）/ 自建退避循环全部删除，不是留着不用；TUI 对 `elysper-app-server` 的直接依赖随之删除（改依赖 `elysper-gateway`）。

---

### Task 1: 网关 client 补 catch-all 订阅

Plan 1 的 `GatewayClient` 只有 `subscribe(method, handler)`（按方法名精确订阅）；TUI 需要**所有**通知（`ready` / `session/event` / `anthropic/messages/event` / `approval/request` / 未来新增的），逐个订阅会漏。

**Files:**
- Modify: `crates/elysper-gateway/src/client.rs`
- Modify: `docs/design/2026-11-09-gateway-crate-plan.md`（Task 2 的接口清单同步补一条，保持文档与实现一致）

**Interfaces:**
- Produces: `GatewayClient::subscribe_all(handler: Arc<dyn Fn(serde_json::Value) + Send + Sync>) -> Result<Subscription, ClientError>`

- [ ] **Step 1: 写失败的测试**

`crates/elysper-gateway/src/client.rs` 的测试模块（或 `tests.rs`）加：连一个进程内假 WS 服务器，推两条不同方法的通知，断言同一个 `subscribe_all` handler 两条都收到。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p elysper-gateway --lib`
Expected: FAIL（`no method named subscribe_all`）

- [ ] **Step 3: 实现**

`subscribe_all` 与 `subscribe` 共用同一条通知分发路径；`subscribe(method, …)` 保留给精确订阅者（桌面壳）。文档同步。

- [ ] **Step 4: 验证并提交**

Run: `cargo test -p elysper-gateway --lib && cargo clippy --all-targets --all-features --tests -p elysper-gateway -- -D warnings`
Expected: 全部通过

---

### Task 2: TUI 传输层换成网关 client

**Files:**
- Modify: `crates/elysper-tui/Cargo.toml`（`elysper-app-server` → `elysper-gateway`；tokio feature 按需复核）
- Rewrite: `crates/elysper-tui/src/transport.rs`
- Rewrite: `crates/elysper-tui/src/transport/tests.rs`
- Modify: `crates/elysper-tui/src/app.rs`（`apply` 加两个新 arm，`ConnectionState` 复用 `Starting`）
- Modify: `crates/elysper-tui/src/lib.rs`（模块文档改写：wire 契约 = 网关 WS）
- Modify: `scripts/check-publishable.sh`（核对 `ORDER` 顺序：`elysper-gateway` 在 `elysper-tui` 前）

**Interfaces:**
- Consumes: `elysper_gateway::{Gateway, GatewayOptions, GatewayClient, GatewayState, ClientError, Subscription}`
- Produces（公开面不变 + 两个新事件）:

```rust
// crates/elysper-tui/src/transport.rs —— 保留的公开项（签名不变）
pub enum RequestKind { /* … */ }
pub struct Request { /* … */ }          // create_session / follow / send_message /
                                        // respond_approval / cancel / unfollow / events
pub enum SendError { /* … */ }
pub struct RpcFailure { /* … */ }
pub enum TransportEvent {
    Ready { version: String, workspace_root: String },   // 不变（网关广播，gateway.rs:213）
    Response { id: Value, request: Option<RequestKind>, result: Result<Value, RpcFailure> },
    Notification { method: String, params: Value },
    Disconnected { reason: String },
    SpawnFailed { message: String, retry_in: Duration },
    GatewayStarting { reason: String },      // 新增：网关进程在起（GatewayState::Starting）
    GatewayRestarting { attempt: u32 },      // 新增：网关在退避重启
}
pub struct Transport { /* 内部持 GatewayClient */ }
impl Transport {
    pub async fn send(&self, request: &Request) -> Result<u64, SendError>;
}
pub struct Driver { /* 内部：Gateway（可选，attach 时无）+ GatewayClient + 事件通道 */ }
impl Driver {
    /// 拉起网关（`elysper-app-server serve`）并连它的 /rpc。
    pub fn launch(options: GatewayOptions) -> Self;
    /// 连一个已在跑的网关（ws://host:port/rpc 或 http://host:port）。
    pub fn attach(url: &str) -> Self;
    pub fn sender(&self) -> Transport;
    pub async fn recv(&mut self) -> Option<TransportEvent>;
}
```

- [ ] **Step 1: 写失败的测试**

`crates/elysper-tui/src/transport/tests.rs` 重写：起一个**进程内 axum WS 服务器**说 JSON-RPC 信封（不是 Content-Length 帧），断言：
1. `Driver::attach` 连上后收到 `Ready`（服务器推 `{"jsonrpc":"2.0","method":"ready","params":{…}}`）；
2. `Transport::send(Request::create_session())` 收到 `Response { request: Some(CreateSession), … }`；
3. 服务器断开 ⇒ `Disconnected`；
4. `GatewayState::Starting` 映射为 `GatewayStarting`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p elysper-tui --lib`
Expected: FAIL（旧 transport 测试是 Content-Length 假核心，编译期就会对不上）

- [ ] **Step 3: 实现 transport.rs**

1. 删除：`spawn_process` / `SpawnedCore` / 分帧循环 / 自建退避 / `LaunchOptions`。
2. `Driver::launch(options)`：`Gateway::start(options)` → 从 `states()` 收 `Ready { ws_url, .. }` → `GatewayClient::connect(&ws_url)`；网关状态流映射成 `GatewayStarting` / `GatewayRestarting`。
3. `Driver::attach(url)`：`url` 若是 `http(s)://` 转成 `ws(s)://…/rpc`；只连不 spawn。
4. `Transport::send`：分配 id → `tokio::spawn` 一个 task await `client.request(method, params)` → 把结果投回事件通道（保持 `TransportEvent::Response { request: Some(kind), .. }` 契约）。
5. 通知：`client.subscribe_all(...)` → `TransportEvent::Notification`。
6. `app.rs::apply` 加两个 arm：`GatewayStarting` → `ConnectionState::Starting`；`GatewayRestarting` → `ConnectionState::Starting` + status note（"gateway restarting, attempt n"）。其余 arm 不动。

- [ ] **Step 4: 验证并提交**

Run: `cargo test -p elysper-tui --lib && cargo clippy --all-targets --all-features --tests -p elysper-tui -- -D warnings`
Expected: 通过（含 `app/tests.rs` 的既有 reducer 测试全绿）

---

### Task 3: CLI（`--attach` / `--app-server` / `--directory` / `--profile`）

**Files:**
- Modify: `crates/elysper-tui/src/main.rs`（`Args` + 启动分支）

**Interfaces:**
- Consumes: Task 2 的 `Driver::launch` / `Driver::attach`、`GatewayOptions`

- [ ] **Step 1: 改 `Args`**

```rust
struct Args {
    #[arg(short, long, default_value = ".")]
    directory: PathBuf,
    #[arg(long, value_name = "PATH")]
    profile: Option<PathBuf>,
    #[arg(long, env = "ELYSPER_APP_SERVER", value_name = "PATH")]
    app_server: Option<PathBuf>,       // serve 二进制路径（不再有 core 直连开关）
    #[arg(long, value_name = "URL")]
    attach: Option<String>,            // 附着到已在跑的网关
}
```

- [ ] **Step 2: 启动分支**

`attach` 有值 ⇒ `Driver::attach(&url)`；否则 `GatewayOptions { executable, bind: "127.0.0.1", port: 0, directory, profile, ..Default::default() }` ⇒ `Driver::launch`。`main` 的 select 循环不动。

- [ ] **Step 3: 手工冒烟（TUI 是交互面，必须人跑一次）**

```bash
cargo build -p elysper-app-server && \
ELYSPER_APP_SERVER=target/debug/elysper-app-server cargo run -p elysper-tui -- --directory .
```
Expected: 起一轮对话（模型回答流式上屏）、审批用 `y` / `n` 应答、`Esc` 退出无残进程。
再跑：`cargo run -p elysper-app-server -- serve --port 8080 &` 然后 `cargo run -p elysper-tui -- --attach http://127.0.0.1:8080`。
Expected: 行为一致；杀掉 serve ⇒ TUI 显示"网关不可达"（不自己重启网关）。

---

### Task 4: 视图骨架（§6.1：侧栏 + 对话流 + 待办 + 输入框）

**Files:**
- Create: `crates/elysper-tui/src/views/mod.rs`、`sidebar.rs`、`conversation.rs`、`todo.rs`、`composer.rs`、`settings.rs`
- Modify: `crates/elysper-tui/src/app.rs`（新状态字段：`Pane` / `SidebarState` / `WorkspaceNode` / `TodoState`；新 `Action`）
- Modify: `crates/elysper-tui/src/render.rs`（`draw` 分发到 views；既有 status/transcript/composer 绘制搬进 views 但**语义不动**）
- Modify: `crates/elysper-tui/src/app/tests.rs`、`render/tests.rs`（新增用例）

**Interfaces:**
- Consumes: 网关的目录方法（`sessions/list`、`workspace` 相关——以 `crates/elysper-app-server/src/routes/` 的真实方法名为准，实现前先读）
- Produces: `views::*` 的纯绘制函数（`fn draw_xxx(frame: &mut Frame, area: Rect, app: &App)`），全部无状态无 I/O ⇒ `TestBackend` 可断言

- [ ] **Step 1: 写失败的渲染测试**

`render/tests.rs` 加用例（照既有 `TestBackend` 用例的写法）：
1. 侧栏：给定两个工作区各含两会话 ⇒ 缓冲里能找到组标题与条目；`Ctrl+b` 折叠后只剩图标列宽度。
2. 对话流：`Role::User` 的行**右对齐**；思考块两态（流式中展开多行 / 完成后一行 `已思考 >`）。
3. 待办：`TodoState { total: 3, done: 1 }` ⇒ 首行 `1/3`；无 todo ⇒ 输入框上方**没有**待办行（高度不占）。
4. 输入框：bash 风格 prompt（`❯ <目录> ` 前缀），底部一行 `+ / 工作区 / 模型 / 发送`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p elysper-tui --lib render`
Expected: FAIL（新断言找不到目标文本/布局）

- [ ] **Step 3: 实现**

`App` 加状态（默认值 = 今天的单会话视图，老测试不破）；views 逐个实现；`render::draw` 改为 `Layout::horizontal([侧栏, 主区])` + 主区内 vertical([标题, 对话流, 待办, 输入框])。审批覆盖层逻辑不动。

- [ ] **Step 4: 验证并提交**

Run: `cargo test -p elysper-tui --lib && cargo clippy --all-targets --all-features --tests -p elysper-tui -- -D warnings`
Expected: 通过

---

### Task 5: Vim 键位

**Files:**
- Modify: `crates/elysper-tui/src/main.rs`（`handle_key` 扩展）
- Modify: `crates/elysper-tui/src/app.rs`（`Action` 补齐：光标移动 / 半页 / 搜索 / 复制 id / 删除 / 折叠 / 命令面板占位）

**Interfaces:**
- Consumes: §6.1 的键位表（列表 `j/k` `gg`/`G` `Ctrl+d`/`Ctrl+u` `/` `n`/`N`；打开 `Enter` `y` `x` `Esc`；全局 `:` `?` `Ctrl+b` `Ctrl+k`）

- [ ] **Step 1: 写失败的 reducer 测试**

`app/tests.rs`：列表态下注入 `j` / `gg` / `G` / `Ctrl+d` ⇒ 光标位置变化正确；`/` 进入搜索态；`Ctrl+b` 切折叠；`y` 产出"复制 id"动作；`Esc` 取消搜索态。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p elysper-tui --lib app`
Expected: FAIL

- [ ] **Step 3: 实现**

`handle_key` 按 `Pane` 分派（输入框态优先：普通字符进 composer；`Esc` 回列表）。**既有键不删**：`Ctrl+c` 退出、`Enter` 提交、`y`/`n` 应答审批（审批存在时优先于 `y` 复制）。

- [ ] **Step 4: 验证并提交**

Run: `cargo test -p elysper-tui --lib && cargo clippy --all-targets --all-features --tests -p elysper-tui -- -D warnings`
Expected: 通过

---

### Task 6: README 与门禁收口

**Files:**
- Modify: `crates/elysper-tui/README.md`（三条宿主路径同源：浏览器 / TUI / 桌面壳都连 `serve`；`--attach` 用法；如何自建 server）
- Modify: `AGENTS.md` §1（elysper-tui 段落：不再 spawn core、改连网关）
- Modify: `CHANGELOG.md` `[Unreleased]`

- [ ] **Step 1: 文档三处更新**（README / AGENTS.md / CHANGELOG 各一段，事实只写已实现的）

- [ ] **Step 2: 门禁**

```bash
make fmt-check && make lint-rust && make check-pub-surface && make check-publishable
```
Expected: 全绿（TUI 的公开面无 `tokio::` 路径；publishable 顺序含 gateway→tui）

- [ ] **Step 3: 回归（core 的 stdio 契约不受影响）**

Run: `cargo test -p elysper-app-server --test stdio_roundtrip`
Expected: 通过（它 spawn 二进制逐帧驱动 core，与 TUI 改动无关，作回归钉）

- [ ] **Step 4: 提交**

```bash
git add crates/elysper-tui crates/elysper-gateway scripts/check-publishable.sh AGENTS.md CHANGELOG.md docs/design/2026-11-09-gateway-crate-plan.md
git commit -m "feat(tui): 并轨到网关（serve 的 /rpc），界面换对话优先骨架"
```

## 实施偏差记录（刀 2 执行中发现，以代码事实为准）

1. **`subscribe_all` 的回调带方法名**。`TransportEvent::Notification` 需要
   `method`，所以回调落地为公开别名
   `NotificationHandler = Arc<dyn Fn(&str, Value) + Send + Sync>`
   （`&str` 是通知方法名）；catch-all 只挂本地分发，不上线注册、断线不
   重放——网关按宿主自己的请求（`sessions/follow`）路由通知。
2. **初始 `Ready` 由探针合成**。网关只在 core 启动那一刻**广播** `ready`
   （`gateway.rs` 的 `route_frame`），`/readyz` 又要求 core 已 ready——所以
   TUI 的 WS 连上时广播早已错过，"广播原样成立"只对**core 重启**（WS 不断）
   成立。传输层用周期 `health/ping` 探针补上：首次（及断线后的首次）应答
   ⇒ 合成 `Ready{version 来自 ping, workspace_root 来自 launch 目录或广播}`
   并重发 follow；请求的 transport 失败 ⇒ 立即探针 + 一次性 `Disconnected`。
3. **"ready 后重发 follow"在传输层，不在 reducer**。`apply_ready` 只在无会话
   时发 `CreateSession`；既有会话的重发一直是传输层职责（旧
   `refollow_session`），新传输在"连接是新的"三个时机重发：core `ready`
   广播、网关 `Ready`/`Restarting` 状态、探针断线后首次应答。
4. **`SpawnFailed` 的 `retry_in` 恒为 0**：网关 supervisor 放弃后不再重试，
   UI 文案如实说明，不再假装退避。
5. **网关 crate 补一处行为**：`Gateway::start` 在结果送达前被放弃（宿主在
   boot 中途退出）时，supervise 收编该代子进程而不是永远重启孤儿（新测试
   `dropping_start_before_its_outcome_reaps_the_generation` 钉住）——这是
   "TUI 退出无残进程"冒烟项的前提。
6. **进度**：Task 1–3 与 Task 6（文档按已实现部分收口 + 门禁）已落地；
   Task 4（视图骨架）/ Task 5（Vim 键位）未实施，`views/` 模块尚未创建。
