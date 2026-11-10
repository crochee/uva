# TUI 补齐两波（会话 + 对话全域 · 通用目录七域）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 TUI 从"一块对话界面"补到设计文档 G7 的能力面。波次 1（刀 5）吃下**会话 + 对话全域**：会话列表（工作区树列吃同一份分页数据）、会话详情、既有对话流、以及 `sessions` / `chat` 两域的全部方法面。波次 2（刀 6）用同一套骨架吃下**通用目录七域**（`agents` / `skills` / `tools` / `tasks` / `schedules` / `workflows` / `evals`，与设计文档刀 6 段逐字一致）并把 `models` / `plugins` / `search` / `ui` 按同一机制接入。

**Architecture:** 不逐页移植（D16）。新增三件东西：`crates/elysper-tui/src/domains.rs` 是**每域一张列配置表**（数据，不是代码：域 → list 方法、行数组取项指针、显示列、get 方法、操作表）；`src/views/list.rs` 吃 `List<T>` 统一信封（`data` / `next_cursor` / `total`，`crates/elysper-app-server/src/api/v1/list.rs:27`）渲染**通用列表**；`src/views/detail.rs` 把单对象摊成**通用键值表**。`src/views/catalog.rs` 是目录浏览状态机（域 → list / detail 两级 + `:` 命令面板 + 写操作确认模态）。reducer（`app.rs`）与渲染的**既有语义不动**：只新增一个 `catalog` 状态字段、`Pane` 加一个变体、`RequestKind` 加一个 catch-all 变体（编译器穷尽性要求的两个空 arm 除外，见 Review Focus #5）。刀 2 已建好的 `views/` 骨架（侧栏 / 对话流 / 待办 / 输入框 / `Pane` 分派 / Vim 键位）是本计划的地基，**不改写**。

**Tech Stack:** Rust 1.99（stable 通道）、ratatui + crossterm（既有）、serde_json 1.0（既有）、tokio（既有 feature 集合，**零新增依赖、零新增 feature**）

**Spec:** `docs/design/2026-11-09-desktop-frontend-tree-design.md` —— §6.1（共享界面契约 + Vim 键位表）、§5.2 / D16（通用渲染器路线）、D17（等价的定义）、D30（键位）、D35（默认工作区常驻）、C12（reducer / 渲染与传输无关）、刀 5 与刀 6 两段、R-10（写操作允许先只读）；刀 2 计划 `docs/design/2026-11-09-tui-gateway-alignment-plan.md` 的 Task 4 / Task 5 产物（`views/` 骨架、`Pane`、`WorkspaceNode`、Vim 键位分派、`--attach` CLI）；Plan 1 `docs/design/2026-11-09-gateway-crate-plan.md` 的 `GatewayClient` 接口

## Global Constraints

- **前置依赖**（刀序，见设计文档 §8 依赖图）：刀 1（`crates/elysper-gateway`）、刀 2（TUI 并轨：`views/` 骨架 + `Pane` + 键位分派 + 网关传输）、刀 4（`docs/host-capabilities.yaml` + `scripts/check-host-capabilities.sh` + `make check-host-capabilities`）**都已落地**。本计划**不改** `crates/elysper-gateway/**` 与 `crates/elysper-app-server/**`——只消费后者的方法。
- **既有语义不动**（任务验收线）：`app.rs` 的 `apply` / `apply_success` / `apply_failure` / `apply_follow` **既有行不改**（新增字段赋值、新增 arm、新增 getter 允许）；`render.rs` 与 `views/{sidebar,conversation,todo,composer,settings}.rs`（刀 2 产物）的既有绘制不改，只加分支。`transport/tests.rs` / `app/tests.rs` / `render/tests.rs` 的**既有用例必须全绿**——这是"语义不动"的可执行证明。
- **方法名的唯一真源**是服务器注册表：`crates/elysper-app-server/src/rpc/methods/**` 里逐个 `router.register("<域>/<动作>", …)`。`domains.rs` 引用的每个方法名都必须能在那里 grep 到（矩阵规则 ⑤ 同款自查，本计划 Interfaces 段的方法名已逐一核对，见文末清单）。
- 错误处理：TUI 是 app crate，边界 `anyhow`；crate 内新错误类型用 `thiserror`（既有 `SendError` / `RpcFailure` 同款）。本计划预计**不需要**新错误枚举（目录失败走 `RpcFailure` + 状态行）。
- 依赖版本 `major.minor` 且复用 `[workspace.dependencies]`——本计划**零新增依赖**，不触发。
- tokio feature 按需声明——不变（不新增 feature）。
- 公开面不得出现 `tokio::` 路径（`make check-public-api-runtime`）：新公开项只含 ratatui / crossterm / serde_json 类型。
- 禁止 `pub use <mod>::*;` 与通配符导入；新测试放同级 `tests.rs`（`make check-test-layout`：内联 `#[cfg(test)] mod` ≥300 行即超基线 0），沿用 `app.rs` + `app/tests.rs` 的既有组织。
- 测试按 crate 分批：`cargo test -p elysper-tui --lib`（**禁止** `cargo test --workspace`）。渲染断言一律 `ratatui::backend::TestBackend` 读缓冲文本，不起 tty。
- 每次提交前：`cargo +nightly fmt --all` + `cargo clippy --all-targets --all-features --tests --all -- -D warnings`。
- 不 push 远程仓库。

## 波次依赖

- **波次 1（Task 1–6）建立在刀 2 的骨架上**：`Pane` 分派、`views/` 模块、侧栏工作区树、`handle_key` 的键位表都已存在；本波只往骨架里加"目录浏览"这一层（`domains.rs` 表结构 + `ListView` / `DetailView` / `Catalog` 状态机 + palette + 确认模态），并用 `sessions` / `chat` 两域把它跑通。
- **波次 2（Task 7–9）依赖波次 1 的目录浏览骨架**：七域与 models / plugins / search / ui 只往 `domains.rs` 的表里**加行**、往侧栏**加条目**，**不写任何新界面**——`ListView` / `DetailView` / `Catalog` / palette / 确认模态全部复用波次 1 的产物。这正是刀 6 段"各加一张列配置表，不加界面"的含义。
- 两波各自一次 PR 级收口（`make ci` 全绿 + 矩阵缺口数下降）。

## Review Focus

下面六类是规格暗示、但没有哪条测试覆盖，最可能咬人的地方。每条都在下面对应的任务里钉上测试。

1. **`sessions` 域的 id 键不统一**：`sessions/get|delete|export|fork|status|events` 用 `{"id"}`，而 `sessions/usage|compact`、`session/unfollow|sessions/resume` 用 `{"session_id"}`（`crates/elysper-app-server/src/rpc/methods/sessions.rs:272-280` 实测）。⇒ 域配置表必须**每个操作各自声明 params 键**（`Op.id_key`），不能域级一个键。Task 2 的表 + 测试钉住。
2. **不是所有域都回 `List<T>` 信封**：`models/list` → `ModelsResponse { models, default_provider, default_model }`（`routes/health/mod.rs:141`）、`plugins/list` → `PluginsResponse { resolved, skipped, bundles }`（`routes/plugins/mod.rs:87`）、`ui/schema` → `UiSchemaResponse { schemas }`（`routes/ui.rs:15`）、`search/all` 还要必填 `q`（`routes/search.rs:29`）。⇒ 适配表用**取项指针**声明（`rows_pointer`，刀 6 段明文允许），不改服务器 wire。Task 2 的结构 + Task 8 的表钉住。
3. **写操作必须先确认，且不得与审批抢键**：`x`（`dd`）删除、run、tick、compact、fork 是"一键不可逆"，刀 6 段要求先给确认提示；而 `y` 在审批挂起时 = allow（既有语义，`main.rs` 的 `handle_key` 已把审批放在最前）——目录态的 `y`（复制 id）**必须**排在审批判断之后。确认模态打开时键位进入模态态（`y`/`Enter` 确认、`Esc` 取消）。Task 4 / Task 5 的测试钉住。
4. **`sessions/resume` 的 token 从 follow 基线拼**：服务器自己注明 token 可取自"the `Suspended` event / the follow baseline"（`routes/follow.rs:992`），`ResumeToken { last_seq, pending, request_snapshot_hash }`（`crates/elysper-harness/src/agent/approval.rs:243`）三样都能从 `FollowSnapshot` 拿到：`cursor` / `pending_interrupts[].call_id` / `request_snapshot_hash`（`routes/follow.rs:140-156`）。但 TUI 今天的 reducer 不留存后两者 ⇒ `app.rs` **只新增**两个字段的赋值。无挂起中断时 resume 禁用（菜单置灰）。Task 5 钉住。
5. **`RequestKind::Catalog` 的响应不得进 reducer**：`main.rs` 必须先拦 `TransportEvent::Response { request: Some(RequestKind::Catalog(method)), .. }` 路由给 `views::catalog`，否则目录响应会落进 `app.rs` 的通用 status 分支，把连接状态行覆盖成目录文案。`app.rs` 里为此加的两个空 arm 是编译器穷尽性要求的占位，注释注明"由 main 先拦，永不触达"。另外：网关重启会让在途目录请求失败（网关 `drop_subscriptions` 并广播 `ready`）——目录是**按需拉取**不是订阅，`ready` 后**不自动重放**目录请求，失败显示为该域的错误行，由操作者重开。Task 1 / Task 5 钉住。
6. **窄终端的列宽**：ratatui 的表格在窄终端截断最右列；列宽之和必须 ≤ 主区宽，超出时从最右列收缩（`Column.width` 是建议宽不是硬宽）。SSH 里的 80 列终端是真实场景。Task 3 的测试用一个 60 列的 `TestBackend` 钉住。

---

### Task 1: 传输缝 —— `RequestKind::Catalog` 让任意目录方法走同一条管

既有 `RequestKind` 只有七个对话/会话变体（`crates/elysper-tui/src/transport.rs:64`），目录方法（`sessions/list`、`agents/get`…）进不了 `Transport::send`。加一个携带方法名的 catch-all 变体，复用全部既有 id 关联机制。

**Files:**
- Modify: `crates/elysper-tui/src/transport.rs`（`RequestKind` 加变体；`method()` 加 arm；`Request::catalog` 构造器）
- Modify: `crates/elysper-tui/src/app.rs`（`apply_failure` / `apply_success` 各加一个**不可达空 arm**）
- Modify: `crates/elysper-tui/src/transport/tests.rs`（catalog 往返用例）

**Interfaces:**
- Consumes: 刀 2 重写后的 transport 测试底座（进程内 axum WS 假服务器说 JSON-RPC 信封）
- Produces:

```rust
// crates/elysper-tui/src/transport.rs
pub enum RequestKind {
    // …既有七个变体（CreateSession / Follow / SendMessage /
    // ApprovalRespond / Cancel / Unfollow / Events）一字不动…
    /// 任意目录方法（`sessions/list`、`agents/get`、`evals/run`…）。
    /// 方法名由变体携带；响应在 `main` 里**先于 reducer** 路由给
    /// `views::catalog`，reducer 里的同名 arm 永不触达。
    Catalog(&'static str),
}

impl Request {
    /// 任意网关方法的一次调用；`params` 原样透传。
    /// 方法名必须是 `crates/elysper-app-server/src/rpc/methods/**`
    /// 里 `router.register` 过的名字，否则服务器回 JSON-RPC error。
    #[must_use]
    pub fn catalog(method: &'static str, params: Value) -> Self {
        Self { kind: RequestKind::Catalog(method), params }
    }
}
```

`app.rs` 的两处穷尽 match 各加（`apply_failure` 与 `apply_success` 的**最后一个** arm，不动既有 arm）：

```rust
// 目录请求由 `main` 先拦下路由给 `views::catalog`；
// 这两个 arm 是穷尽性要求的占位，运行时永不触达。
RequestKind::Catalog(_) => {}
```

- [ ] **Step 1: 写失败的测试**

`crates/elysper-tui/src/transport/tests.rs` 加一条（沿用刀 2 的假 WS 服务器 helper；服务器把收到的请求原样记录并按脚本回包）：

```rust
#[tokio::test]
async fn a_catalog_request_round_trips_its_method_name() {
    // 假服务器记录收到的 method/params，回 `{"rows": []}`。
    // 断言三件事：
    // 1. 服务器侧收到的 method == "sessions/list"、params == {"limit": 2}；
    // 2. TUI 侧收到 TransportEvent::Response {
    //        request: Some(RequestKind::Catalog("sessions/list")), .. };
    // 3. result 是服务器回的值。
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p elysper-tui --lib transport`
Expected: FAIL（编译失败：`error[E0599]: no variant or associated item named 'Catalog' found for enum 'RequestKind'`）

- [ ] **Step 3: 实现**

`transport.rs`：`RequestKind` 加 `Catalog(&'static str)` 变体（保持 `Copy`——`&'static str` 是 Copy）；`RequestKind::method()` 加 `Self::Catalog(method) => method` arm；`Request::catalog` 按上方签名。`app.rs` 两处加空 arm（带注释）。

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p elysper-tui --lib`
Expected: PASS（既有全部用例 + 新用例通过——既有用例不破是"语义不动"的证明）

- [ ] **Step 5: 格式 / lint / 提交**

```bash
cargo +nightly fmt --all
cargo clippy --all-targets --all-features --tests -p elysper-tui -- -D warnings
git add crates/elysper-tui
git commit -m "feat(tui): RequestKind 加 Catalog 变体承载任意目录方法"
```

Expected: clippy 0 warning；提交成功

---

### Task 2: `domains.rs` —— 每域一张列配置表 + `List<T>` 信封解码

纯数据模块：域配置（方法名 / 取项指针 / 列 / 操作）与信封解码。**这一步建立两波共用的表结构**；波次 2 只往 `DOMAINS` 里加条目。

**Files:**
- Create: `crates/elysper-tui/src/domains.rs`
- Create: `crates/elysper-tui/src/domains/tests.rs`
- Modify: `crates/elysper-tui/src/lib.rs`（`pub mod domains;`，模块文档一句话说明"每域一张列配置表，方法名的真源是服务器注册表"）

**Interfaces:**
- Consumes: `crate::transport::{Request, RequestKind}`；服务器 wire 形状（下方逐字段核对自 `crates/elysper-app-server/src/routes/**`）
- Produces:

```rust
// crates/elysper-tui/src/domains.rs
/// 一列：表头 + 行内取值指针（serde_json pointer 语法）+ 建议宽。
pub struct Column {
    pub label: &'static str,
    pub pointer: &'static str,
    pub width: u16,
}

/// 域内一个操作（读操作也走这里：结果摊成详情页）。
pub struct Op {
    pub label: &'static str,       // palette 与确认模态里显示
    pub method: &'static str,      // 服务器注册表里的真名
    pub id_key: &'static str,      // params 里 id 的键名（"id" / "session_id" / "name"）
    pub confirm: bool,             // true = 先弹确认模态（Review Focus #3）
}

/// Enter 在列表行上的语义。
pub enum EnterOp {
    /// 打开会话（sessions 域）：发既有 `sessions/follow` 并切到对话面板。
    Follow,
    /// 详情：get_method 存在则调 `<域>/get`，否则把列表行本身摊成详情。
    Detail,
}

pub struct DomainSpec {
    pub label: &'static str,       // 侧栏与标题
    pub list_method: &'static str,
    /// list 响应里行数组的指针：`List<T>` 信封 = "/data"；
    /// models = "/models"、plugins = "/resolved"、ui = "/schemas"
    /// （Review Focus #2）。
    pub rows_pointer: &'static str,
    pub next_cursor_pointer: &'static str,   // 信封域统一 "/next_cursor"
    pub get_method: Option<&'static str>,
    pub get_id_key: &'static str,            // agents/skills/tools/evals 是 "name"，其余 "id"
    pub id_pointer: &'static str,            // 列表行里 id 的指针
    pub enter: EnterOp,
    pub columns: &'static [Column],
    pub ops: &'static [Op],
    /// list 需要查询串时 params 里的键名（只有 search/all 是 Some("q")）。
    pub query_key: Option<&'static str>,
}

/// 已接的域，侧栏顺序。波次 1 只有 sessions 一条。
pub const DOMAINS: &[DomainSpec] = &[ /* 见 Step 3 */ ];

impl DomainSpec {
    #[must_use]
    pub fn by_list_method(method: &str) -> Option<&'static DomainSpec>;
    #[must_use]
    pub fn by_label(label: &str) -> Option<&'static DomainSpec>;
    /// `<域>/list` 的 params：cursor 透传（opaque base64，直接回传）、
    /// `limit: 50`（服务器默认 20、上限 100，见 api/v1/page_query.rs:32-42）、
    /// query_key 存在时附查询串。
    #[must_use]
    pub fn list_params(&self, cursor: Option<&str>, query: Option<&str>) -> Value;
    #[must_use]
    pub fn get_params(&self, id: &str) -> Value;
}

/// 解码后的一页（行保留原始 `Value`，列提取延迟到渲染）。
pub struct ListPage {
    pub rows: Vec<Value>,
    pub next_cursor: Option<String>,
    pub total: Option<u64>,
}

/// 按 spec 的指针从响应里取一页；指针落空时 rows 为空
/// （服务器改形状 ⇒ 空表而不是 panic，R-12 的容错线）。
#[must_use]
pub fn decode_list(value: &Value, spec: &DomainSpec) -> ListPage;

/// 一行一列的单元格文本：字符串原样、数字 `to_string`、
/// 布尔 true/false、数组/对象取长度（evals 的 "/cases" 显示用例数）。
#[must_use]
pub fn cell_text(row: &Value, pointer: &str) -> String;
```

波次 1 的 `sessions` 条目（**列与字段逐一核对自** `SessionSummary`，`crates/elysper-app-server/src/routes/sessions/mod.rs:85-107`）：

```rust
DomainSpec {
    label: "会话",
    list_method: "sessions/list",
    rows_pointer: "/data",
    next_cursor_pointer: "/next_cursor",
    get_method: Some("sessions/get"),
    get_id_key: "id",
    id_pointer: "/id",
    enter: EnterOp::Follow,
    columns: &[
        Column { label: "会话", pointer: "/id", width: 26 },
        Column { label: "状态", pointer: "/status", width: 12 },
        Column { label: "开始", pointer: "/created_at", width: 20 },
        Column { label: "agent", pointer: "/agent_name", width: 12 },
    ],
    ops: &[
        // id 键不统一是 Review Focus #1：逐操作声明。
        Op { label: "详情", method: "sessions/get", id_key: "id", confirm: false },
        Op { label: "用量", method: "sessions/usage", id_key: "session_id", confirm: false },
        Op { label: "运行状态", method: "sessions/status", id_key: "id", confirm: false },
        Op { label: "导出", method: "sessions/export", id_key: "id", confirm: false },
        Op { label: "fork", method: "sessions/fork", id_key: "id", confirm: true },
        Op { label: "resume", method: "sessions/resume", id_key: "session_id", confirm: true },
        Op { label: "压缩", method: "sessions/compact", id_key: "session_id", confirm: true },
        Op { label: "删除", method: "sessions/delete", id_key: "id", confirm: true },
    ],
    query_key: None,
}
```

- [ ] **Step 1: 写失败的测试**

`crates/elysper-tui/src/domains/tests.rs`（fixture 的键名照 `SessionSummary` 的 wire 形状写）：

```rust
use serde_json::json;
use crate::domains::{DOMAINS, cell_text, decode_list};

#[test]
fn the_sessions_row_decodes_from_the_list_envelope() {
    let spec = DOMAINS[0];
    assert_eq!(spec.list_method, "sessions/list");
    let page = decode_list(&json!({
        "data": [
            { "id": "s1", "status": "idle",
              "created_at": "2026-11-09T10:00:00Z", "agent_name": "agent" },
            { "id": "s2", "status": "running" },
        ],
        "next_cursor": "Y3Vyc29y",
        "total": 12,
    }), spec);
    assert_eq!(page.rows.len(), 2);
    assert_eq!(page.next_cursor.as_deref(), Some("Y3Vyc29y"));
    assert_eq!(page.total, Some(12));
}

#[test]
fn id_keys_are_declared_per_op_not_per_domain() {   // Review Focus #1
    let spec = DOMAINS[0];
    let keys: Vec<_> = spec.ops.iter().map(|op| op.id_key).collect();
    assert!(keys.contains(&"id") && keys.contains(&"session_id"));
}

#[test]
fn missing_rows_pointer_yields_an_empty_page() {    // Review Focus #2
    let spec = DOMAINS[0];
    let page = decode_list(&json!({ "unexpected": [] }), spec);
    assert!(page.rows.is_empty() && page.next_cursor.is_none());
}

#[test]
fn list_params_carry_the_cursor_and_limit() {
    let spec = DOMAINS[0];
    assert_eq!(spec.list_params(None, None), json!({ "limit": 50 }));
    assert_eq!(
        spec.list_params(Some("Y3Vyc29y"), None),
        json!({ "limit": 50, "cursor": "Y3Vyc29y" }),
    );
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p elysper-tui --lib domains`
Expected: FAIL（`error[E0432]: unresolved import crate::domains`——模块还不存在）

- [ ] **Step 3: 实现 `domains.rs` 与 `domains/tests.rs`**

按上方 Interfaces 写全；`lib.rs` 加 `pub mod domains;`。`cell_text` 用 `Value::pointer`；数组/对象分支返回长度字符串（`format!("{} 项", len)`）。

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p elysper-tui --lib`
Expected: PASS

- [ ] **Step 5: 格式 / lint / 提交**

```bash
cargo +nightly fmt --all
cargo clippy --all-targets --all-features --tests -p elysper-tui -- -D warnings
git add crates/elysper-tui
git commit -m "feat(tui): 每域一张列配置表与 List 信封解码"
```

Expected: clippy 0 warning

---

### Task 3: `views/list.rs` —— 通用列表（渲染 + D30 列表键位）

**Files:**
- Create: `crates/elysper-tui/src/views/list.rs`
- Create: `crates/elysper-tui/src/views/tests.rs`
- Modify: `crates/elysper-tui/src/views/mod.rs`（`pub mod list;` + `#[cfg(test)] mod tests;`——刀 2 已建该文件）

**Interfaces:**
- Consumes: `crate::domains::{Column, DomainSpec, cell_text}`；D30 列表键位表
- Produces:

```rust
// crates/elysper-tui/src/views/list.rs
pub struct ListView {
    pub rows: Vec<Value>,          // 已加载的行（翻页追加）
    pub cursor: usize,             // 焦点行下标（rows 里）
    pub offset: usize,             // 视口首行
    pub filter: Option<String>,    // `/` 进入，Esc 退出
    pub matches: Vec<usize>,       // 命中行下标（filter 存在时）
    pub match_at: usize,           // n/N 的位置
    pub next_cursor: Option<String>,
    pub total: Option<u64>,
    pub loading: bool,
    pub error: Option<String>,
    pending_g: bool,               // `gg` 两段式
    pending_d: bool,               // `dd` 两段式
}

impl ListView {
    #[must_use]
    pub fn new() -> Self;
    /// 过滤后可见行的下标序列（无 filter = 全部）。
    #[must_use]
    pub fn visible(&self) -> Vec<usize>;
    /// D30 列表键位（`j`/`k` 移动、`gg`/`G` 首尾、`Ctrl+d`/`Ctrl+u` 半页、
    /// `/` 搜索、`n`/`N` 下一个/上一个匹配）。半页 = `viewport` 的一半。
    /// 返回 true = 已消费。**不处理** Enter/y/x/Esc（Task 5 的 Catalog 接）。
    pub fn key(&mut self, code: KeyCode, modifiers: KeyModifiers, viewport: u16) -> bool;
    /// 焦点行还在视口外时滚动 offset 使它可见。
    pub fn scroll_to_cursor(&mut self, viewport: u16);
    /// 焦点行的 id（按 spec.id_pointer 取）。
    #[must_use]
    pub fn focused_id(&self, spec: &DomainSpec) -> Option<String>;
}

/// 画一张表：列头来自 spec.columns，单元格来自 cell_text，
/// 焦点行前缀 `>`，底部状态行（`n/total` + filter + next_cursor 提示）。
/// 列宽之和 > area.width 时从最右列收缩（Review Focus #6）。
pub fn draw_list(
    frame: &mut Frame,
    area: Rect,
    view: &ListView,
    spec: &DomainSpec,
);
```

- [ ] **Step 1: 写失败的测试**

`crates/elysper-tui/src/views/tests.rs`（照 `render/tests.rs` 的 `render_to_buffer` 写法，自备一个针对 `draw_list` 的 helper）：

```rust
#[test]
fn the_list_draws_headers_rows_and_the_cursor_marker() {
    // 3 行 sessions fixture，80×24 TestBackend。
    // 断言缓冲含 "会话"、"状态"、三行的 id、焦点行的 ">" 前缀。
}

#[test]
fn d30_keys_move_the_cursor() {
    // j → cursor+1；gg（两次 g）→ 0；G → 末行；
    // Ctrl+d → offset+半页且 cursor 跟随；k 回退。
    // 用 focused_id 断言位置，不碰渲染。
}

#[test]
fn slash_filters_rows_and_n_cycles_matches() {
    // 行含 "s1","s2","t1"；filter="s" ⇒ visible()==[0,1]；
    // n ⇒ match_at 1；N ⇒ 回 0。
}

#[test]
fn narrow_terminals_shrink_the_rightmost_column() {   // Review Focus #6
    // 同一 fixture，60 列 TestBackend：断言缓冲仍含第一列的 id
    // 且 draw 不 panic（截断发生在最右列）。
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p elysper-tui --lib views`
Expected: FAIL（`error[E0432]: unresolved import super::list` / 模块不存在）

- [ ] **Step 3: 实现**

`ListView` 状态机 + `draw_list`（ratatui `Table` / `Row` / `Cell`，标题块 `Block::bordered().title(spec.label)`）。`gg`/`dd` 两段式：首键置 pending，任意其它键清掉；`G` 用 `visible().last()`。半页 = `max(1, viewport / 2)`。

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p elysper-tui --lib`
Expected: PASS

- [ ] **Step 5: 格式 / lint / 提交**

```bash
cargo +nightly fmt --all
cargo clippy --all-targets --all-features --tests -p elysper-tui -- -D warnings
git add crates/elysper-tui
git commit -m "feat(tui): 通用列表视图与 D30 列表键位"
```

Expected: clippy 0 warning

---

### Task 4: `views/detail.rs` —— 通用详情（键值表 + 写操作确认模态）

**Files:**
- Create: `crates/elysper-tui/src/views/detail.rs`
- Modify: `crates/elysper-tui/src/views/mod.rs`（`pub mod detail;`）
- Modify: `crates/elysper-tui/src/views/tests.rs`

**Interfaces:**
- Consumes: `crate::domains::DomainSpec`
- Produces:

```rust
// crates/elysper-tui/src/views/detail.rs
pub struct DetailView {
    pub id: String,
    pub rows: Vec<(String, String)>,   // 摊平的 键路径 → 文本
    pub scroll: usize,
}

impl DetailView {
    /// 单对象摊成键值行：标量直接进；数组/对象递归
    /// （`cases/0/name`），深度上限 3；总行数上限 200，
    /// 超出显示 `<还有 n 项省略>`（sessions/get 的 history 很长）。
    /// 数组标量元素用 `cases/0` 这样的路径。
    #[must_use]
    pub fn from_value(id: &str, value: &Value) -> Self;
}

/// 写操作执行前的确认模态内容（Review Focus #3）。
pub struct Confirm {
    pub op_label: &'static str,
    pub method: &'static str,
    pub target: String,        // 操作对象的 id/name
}

pub fn draw_detail(frame: &mut Frame, area: Rect, view: &DetailView, spec: &DomainSpec);
/// 底部一行列出 spec.ops 的标签（palette 的快捷提示）。
pub fn draw_ops_footer(frame: &mut Frame, area: Rect, spec: &DomainSpec);
/// 居中确认模态：`[y] 执行  [Esc] 取消`。
pub fn draw_confirm(frame: &mut Frame, area: Rect, confirm: &Confirm);
```

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn a_nested_object_flattens_to_pointer_rows() {
    let view = DetailView::from_value("s1", &json!({
        "id": "s1",
        "header": { "state": "running", "agent_name": "agent" },
        "usage": { "input_tokens": 120, "output_tokens": 30 },
    }));
    // 断言 rows 含 ("id","s1")、("header/state","running")、
    // ("usage/input_tokens","120")。
}

#[test]
fn deep_arrays_are_capped_with_an_ellipsis_row() {
    // 300 个元素的数组 ⇒ rows.len() 有界且最后一行含 "省略"。
}

#[test]
fn the_confirm_modal_names_the_op_method_and_target() {
    // 80×24 TestBackend 画 draw_confirm ⇒ 缓冲含 op 标签、方法名、
    // target id、"[y]" 与 "[Esc]"。
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p elysper-tui --lib views`
Expected: FAIL（`unresolved import super::detail`）

- [ ] **Step 3: 实现**

摊平用显式栈或递归（深度 ≤3）；`draw_confirm` 复用 `render.rs` 既有 `centered` 的居中做法（该 helper 若仍是私有 `fn centered`，在 `render.rs` 把它提为 `pub(crate)`——**仅可见性**，函数体不动）。

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p elysper-tui --lib`
Expected: PASS

- [ ] **Step 5: 格式 / lint / 提交**

```bash
cargo +nightly fmt --all
cargo clippy --all-targets --all-features --tests -p elysper-tui -- -D warnings
git add crates/elysper-tui
git commit -m "feat(tui): 通用详情视图与写操作确认模态"
```

Expected: clippy 0 warning

---

### Task 5: `views/catalog.rs` —— 目录浏览状态机、会话域接线与 main 分派

把 Task 2–4 的件接成"目录浏览"：侧栏域入口 → 通用列表 → 详情/操作；`main.rs` 拦目录响应；对话面板的 palette 接 chat 域操作；工作区树吃同一份 sessions 分页。

**Files:**
- Create: `crates/elysper-tui/src/views/catalog.rs`
- Modify: `crates/elysper-tui/src/views/mod.rs`（`pub mod catalog;`）
- Modify: `crates/elysper-tui/src/views/sidebar.rs`（刀 2 产物：加"目录"分组，条目来自 `domains::DOMAINS`；工作区树的行改为引用 catalog 的 sessions 分页状态——**单一真源**，不建第二条请求路径）
- Modify: `crates/elysper-tui/src/app.rs`（**只新增**：`catalog: Catalog` 字段 + `catalog()` / `catalog_mut()` getter；`Pane` 加 `Catalog` 变体；`apply_follow` 里**新增两行**赋值 `follow_cursor` / `follow_request_hash`（Review Focus #4）；Task 1 已加的空 arm 不再动）
- Modify: `crates/elysper-tui/src/main.rs`（响应拦截 + `Pane::Catalog` 键位分派 + palette）
- Modify: `crates/elysper-tui/src/views/tests.rs`

**Interfaces:**
- Consumes: Task 1 的 `Request::catalog` / `RequestKind::Catalog`；Task 2 的 `domains::*`；Task 3/4 的 `ListView` / `DetailView` / `Confirm`；既有 `Action::Follow`（打开会话）
- Produces:

```rust
// crates/elysper-tui/src/views/catalog.rs
pub struct Catalog {
    pub domain: Option<&'static DomainSpec>,   // None = 未开目录
    pub list: ListView,
    pub detail: Option<DetailView>,
    pub confirm: Option<Confirm>,
    pub query: Option<String>,   // query_key 域（search）的查询输入
    pub palette: Option<Palette>,
    pub status: String,
}

/// `:` / Ctrl+k 命令面板：当前域的 ops + 对话面板时的 chat 操作
/// + `:json <method> <params>` 通用门（Task 8）。
pub struct Palette {
    pub input: String,
    pub cursor: usize,
}

/// resume 的拼装材料（Review Focus #4），从 app 的 follow 基线字段来。
pub struct ResumeMaterial {
    pub session_id: String,
    pub last_seq: u64,               // FollowSnapshot.cursor
    pub pending: Vec<String>,        // pending_interrupts[].call_id
    pub request_snapshot_hash: String,
}

impl Catalog {
    #[must_use]
    pub fn new() -> Self;
    /// 打开一个域（query_key 域先收查询串）。返回要发的 list 请求。
    pub fn open(&mut self, spec: &'static DomainSpec) -> Vec<Request>;
    /// 目录键位（Pane::Catalog 时接管；确认模态与 palette 打开时优先）。
    /// 返回要发送的请求（list 翻页 / get / op）。
    pub fn key(
        &mut self,
        code: KeyCode,
        modifiers: KeyModifiers,
        viewport: u16,
        resume: Option<&ResumeMaterial>,
    ) -> Vec<Request>;
    /// 目录响应（main 拦下的 `RequestKind::Catalog`）。
    /// list → 刷新 ListView；get/读 op → DetailView；
    /// 写 op → status 行 + 重新拉首页。
    pub fn apply_response(&mut self, method: &str, result: &Result<Value, RpcFailure>);
    /// 写操作完成后回列表刷新（复用当前 next_cursor=None 的首页）。
    pub fn refresh(&mut self) -> Vec<Request>;
}
```

键位落位（D30 全表，**审批优先级见 Review Focus #3**——`handle_key` 里审批存在的分支排在目录分支**之前**，既有顺序不动）：

| 键 | 目录态行为 |
|---|---|
| `j`/`k`、`gg`/`G`、`Ctrl+d`/`Ctrl+u`、`/`、`n`/`N` | `ListView::key` |
| `Enter` | `EnterOp::Follow` ⇒ 发既有 `sessions/follow`（经 `Action::Follow`）并切 `Pane::Conversation`；`EnterOp::Detail` ⇒ get 或行摊详情 |
| `y` | 复制焦点 id：OSC 52 序列（`\x1b]52;c;<base64>\x07`，零依赖）；status 行回显 id 兜底 |
| `x` / `dd` | 域 ops 里 label == "删除" 的操作 ⇒ 确认模态 |
| `Esc` | 关模态 / 关 palette / 关详情 → 回列表；列表态再 Esc 回对话面板 |
| `:` / `Ctrl+k` | 打开 palette（当前域 ops；对话面板时 = chat 域操作 `:regenerate` / `:feedback-up` / `:feedback-down`） |
| `?` | 快捷键帮助覆盖层（静态键位表几行） |
| `Ctrl+b` | 侧栏折叠（刀 2 既有，透传） |

chat 域操作的参数（核对自 `crates/elysper-app-server/src/rpc/methods/chat.rs`）：
- `chat/regenerate` `{"session_id": <当前会话>}`（`CancelParams`，chat.rs:89）
- `chat/feedback` `{"message_id": <标签>, "thumbs_up": true|false, "session_id": <当前会话>}`——`message_id` 是**客户端标签**（`routes/chat.rs:204-208` 注释明说"the id the chat UI sends is a client-side label"），TUI 用 `msg-<transcript 序号>` 标最后一条助手行
- `sessions/resume` 的 params 在 `Catalog::key` 里特判拼装：`{"session_id", "resume_token": {"last_seq", "pending", "request_snapshot_hash"}, "interrupts": [{"id": <call_id>, "status": "resolved"}…], "allow_drift": false}`（`ResumeRequest` / `InterruptDecision`，`routes/follow.rs:991-1013`）；`resume: None` 时菜单项置灰不发

`main.rs` 的两处新增（既有分支不动）：

```rust
// 1) 响应拦截（Review Focus #5）：目录响应先于 reducer。
maybe = driver.recv() => match maybe {
    Some(TransportEvent::Response {
        request: Some(RequestKind::Catalog(method)),
        result,
        ..
    }) => {
        app.catalog_mut().apply_response(method, &result);
        Vec::new()          // 不进 app.apply
    }
    Some(other) => app.apply(other),
    /* 既有 None 分支不动 */
},

// 2) 键位分派：Pane::Catalog 时走 catalog（审批优先分支保持在最前）。
```

- [ ] **Step 1: 写失败的测试**

`views/tests.rs` 加（状态机用例不起 tty）：

```rust
#[test]
fn opening_a_domain_asks_for_the_first_page() {
    let mut catalog = Catalog::new();
    let requests = catalog.open(DOMAINS[0]);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, RequestKind::Catalog("sessions/list"));
    assert_eq!(requests[0].params, json!({ "limit": 50 }));
}

#[test]
fn a_list_response_renders_rows_and_a_get_opens_detail() {
    // apply_response("sessions/list", Ok(信封)) ⇒ list.rows 非空；
    // 模拟 Enter(Detail) 产出的 get 请求；再
    // apply_response("sessions/get", Ok(detail)) ⇒ detail.is_some()。
}

#[test]
fn delete_requires_confirmation_before_sending() {   // Review Focus #3
    // 列表态按 x ⇒ confirm.is_some() 且**没有**请求发出；
    // 模态里按 y ⇒ 恰一条 RequestKind::Catalog("sessions/delete")
    // 且 params 是 {"id": 焦点行 id}。
}

#[test]
fn resume_is_disabled_without_a_token() {            // Review Focus #4
    // resume: None 时执行 resume 菜单项 ⇒ 不发请求、status 提示；
    // 有 ResumeMaterial 时 ⇒ params 里 resume_token 三字段齐全。
}

#[test]
fn catalog_responses_never_reach_the_reducer() {     // Review Focus #5
    // 构造 app + catalog；把 Catalog 响应事件按 main 的新分支喂
    // catalog 而非 app.apply ⇒ app.status() 仍是 "connected"。
}

#[test]
fn the_sidebar_lists_the_domains_and_the_workspace_tree_shows_sessions() {
    // 80×24 TestBackend 整屏 draw ⇒ 缓冲含 "会话" 域条目与树里的
    // 会话 id（树行来自 catalog 的 sessions 分页）。
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p elysper-tui --lib views`
Expected: FAIL（`unresolved import super::catalog` 等）

- [ ] **Step 3: 实现**

`views/catalog.rs` 状态机 + `views/mod.rs` 声明 + `views/sidebar.rs` 域分组与树行换源 + `app.rs` 的**纯新增**字段/变体/getter/两行赋值 + `main.rs` 两处分支 + `render.rs`（或 views 的既有分发点）加 `Pane::Catalog` 分支调 `draw_list` / `draw_detail`。palette 是 `Catalog` 的内嵌态，但 `:` / `Ctrl+k` 从**任何面板**都能打开（刀 2 留的占位在这里做实）：目录面板列出当前域的 ops，对话面板列出 chat 域操作（`:regenerate` / `:feedback-up` / `:feedback-down` / `:search`）；输入串前缀过滤，`Enter` 执行（confirm 的仍走模态）。

- [ ] **Step 4: 跑测试，确认通过（含既有用例全绿）**

Run: `cargo test -p elysper-tui --lib`
Expected: PASS

- [ ] **Step 5: 格式 / lint / 提交**

```bash
cargo +nightly fmt --all
cargo clippy --all-targets --all-features --tests -p elysper-tui -- -D warnings
git add crates/elysper-tui
git commit -m "feat(tui): 目录浏览状态机与会话/对话全域接线"
```

Expected: clippy 0 warning

---

### Task 6: 波次 1 收口 —— README、矩阵、门禁与手工冒烟

**Files:**
- Modify: `crates/elysper-tui/README.md`（新增"目录浏览"一节：域入口、D30 键位表、palette、确认模态、`:json` 门见 Task 8）
- Modify: `docs/host-capabilities.yaml`（刀 4 产物）：`sessions` 行 tui 列 → `full`（备注：`sessions/search` 能力由搜索域承载）、`chat` 行 tui 列 → `adapted`（备注：`chat/usage` / `chat/operation` 未接，锚点指向本计划）；两格证据指向 `crates/elysper-tui/src/views/tests.rs`
- Modify: `CHANGELOG.md`（`[Unreleased]` 一条）

**Interfaces:**
- Consumes: Task 1–5 全部产物
- Produces: 波次 1 的可度量收口（矩阵缺口数下降一格）

- [ ] **Step 1: 文档三处更新**（只写已实现的事实；README 键位表照 §6.1 抄全）

- [ ] **Step 2: 矩阵门禁**

Run: `make check-host-capabilities`
Expected: `OK:` 且缺口数较基线下降（sessions/chat 两行不再计入缺口；`full`/`adapted` 的证据文件都在磁盘上）

- [ ] **Step 3: 全量格式与 lint**

Run: `cargo +nightly fmt --all && cargo clippy --all-targets --all-features --tests --all -- -D warnings`
Expected: 无输出（0 warning）

- [ ] **Step 4: 本 crate 测试**

Run: `cargo test -p elysper-tui`
Expected: 全部 passed（lib + bin）

- [ ] **Step 5: 手工冒烟（刀 5 验收）**

```bash
cargo build -p elysper-app-server && \
ELYSPER_APP_SERVER=target/debug/elysper-app-server \
cargo run -p elysper-tui -- --directory .
```

Expected（逐项核对，设计文档刀 5 验收原文）：
1. 侧栏/目录列出会话（`sessions/list` 首页）；
2. 会话行 `Enter` 打开会话进入对话流（既有 follow 路径）；
3. `:用量` 打开 `sessions/usage` 的键值详情；
4. `x` 删除弹确认、`y` 执行后列表刷新；
5. `:fork` / `:压缩` / `:resume` 同样先确认（resume 在无挂起中断时置灰）；
6. `j/k/gg/G/Ctrl+d/Ctrl+u//n/N`、`:`、`?`、`Ctrl+b`、`y` 按 §6.1 生效；
7. 对话面板 `:regenerate` 重生成一轮。

- [ ] **Step 6: 提交**

```bash
git add crates/elysper-tui docs/host-capabilities.yaml CHANGELOG.md
git commit -m "feat(tui): 波次 1 收口——会话与对话全域 + 矩阵缺口下降"
```

---

### Task 7: 波次 2 · 目录七域 —— 只加表，不加界面

**依赖波次 1**：`DomainSpec` 表结构、`ListView` / `DetailView` / `Catalog` / palette / 确认模态全部复用；本任务**只在 `domains.rs` 的 `DOMAINS` 里加七条**并在侧栏"目录"分组露出。七域 = `agents` / `skills` / `tools` / `tasks` / `schedules` / `workflows` / `evals`（设计文档刀 6 段原文）。

**Files:**
- Modify: `crates/elysper-tui/src/domains.rs`（`DOMAINS` 加七条）
- Modify: `crates/elysper-tui/src/domains/tests.rs`

**Interfaces:**
- Consumes: Task 2 的表结构；以下每条的列/字段**逐一核对自** `crates/elysper-app-server/src/routes/**` 的真实 wire 形状：

| 域 | list 方法 | 行类型与列（指针） | get（id 键） | 单 id 写操作 |
|---|---|---|---|---|
| `agents` | `agents/list`（`PageQuery`） | `AgentDetail`（`routes/agents.rs:87`）：名称 `/name` 20 · 类型 `/kind` 10 · 版本 `/version` 8 | `agents/get` `{"name"}` | 删除 `agents/delete` `{"name"}`（确认） |
| `skills` | `skills/list` | `SkillInfo`（`routes/skills.rs:47`）：名称 `/name` 20 · 说明 `/description` 44 | `skills/get` `{"name"}` | 删除 `skills/delete` `{"name"}`（确认）；域级 `skills/reload`（无 id，palette） |
| `tools` | `tools/list` | `ToolInfo`（`routes/tool.rs:26`）：名称 `/name` 20 · 说明 `/description` 44 | `tools/get` `{"name"}` | 注销 `tools/unregister` `{"name"}`（确认） |
| `tasks` | `tasks/list`（默认序 `-started_at`） | `TaskDelegation`（`routes/tasks.rs:61`）：id `/id` 24 · agent `/agent` 12 · 状态 `/status` 10 · 开始 `/started_at` 20 | `tasks/get` `{"id"}` | 无（服务器只有 list/get） |
| `schedules` | `schedules/list` | `ScheduleInfo`（`routes/schedules.rs:45`）：id `/id` 22 · 名称 `/name` 16 · 状态 `/status` 10 · 下次 `/next_fire_at` 20 | `schedules/get` `{"id"}` | 触发 `schedules/tick` `{"id"}`（确认）· 删除 `schedules/delete` `{"id"}`（确认） |
| `workflows` | `workflows/list` | `WorkflowSummary`（`routes/workflows.rs:31`）：id `/id` 28 · 步骤 `/step_count` 8 · 阶段 `/phase_count` 8 | `workflows/get` `{"id"}` | 运行 `workflows/run` `{"id"}`（确认）· 删除 `workflows/delete` `{"id"}`（确认） |
| `evals` | `evals/list` | `EvalSuite`（`crates/elysper-eval/src/lib.rs:117`，wire = `name` + `cases`）：名称 `/name` 24 · 用例 `/cases`（长度）8 | `evals/get` `{"name"}` | 运行 `evals/run` `{"name"}`（确认）· 删除 `evals/delete` `{"name"}`（确认） |

七条全部 `rows_pointer: "/data"`、`enter: EnterOp::Detail`、`query_key: None`。多字段 body 的 `create` / `patch` / `replace` / `register` **不接专用键位**——经 Task 8 的 `:json` 门可达（R-10 允许；矩阵备注注明）。

- [ ] **Step 1: 写失败的测试**

`domains/tests.rs` 加一条**逐域核对**的表测试（方法名与 id 键是矩阵规则 ⑤ 的客户端版）：

```rust
#[test]
fn the_seven_catalog_domains_declare_the_registered_methods() {
    let labels: Vec<_> = DOMAINS.iter().map(|d| d.list_method).collect();
    for method in [
        "agents/list", "skills/list", "tools/list", "tasks/list",
        "schedules/list", "workflows/list", "evals/list",
    ] {
        assert!(labels.contains(&method), "missing {method}");
    }
    // id 键按域断言：agents/skills/tools/evals 用 "name"，
    // tasks/schedules/workflows 用 "id"（Op 与 get 都是）。
}

#[test]
fn every_declared_method_is_a_real_registered_name() {
    // 用一个 const 白名单（从 rpc/methods 的 grep 结果抄进测试）
    // 断言 DOMAINS 引用的每个方法名都在其中——防止手滑拼错方法名。
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p elysper-tui --lib domains`
Expected: FAIL（`missing workflows/list`——七条还没加）

- [ ] **Step 3: 实现七条配置**（照 Task 2 的 sessions 条目同款写法；`tasks` 的 `ops` 为空切片）

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p elysper-tui --lib`
Expected: PASS

- [ ] **Step 5: 手工冒烟（七域，刀 6 验收前半）**

Run: 同 Task 6 Step 5 的命令
Expected：侧栏目录分组出现七个域；每个域能**列表**（列头与行渲染正确）、`Enter` **详情**（`<域>/get` 或行摊详情）、该域有的**写操作**先确认再执行（`agents` 删除、`skills` 删除/reload、`tools` 注销、`schedules` tick/删除、`workflows` run/删除、`evals` run/删除）。

- [ ] **Step 6: 格式 / lint / 提交**

```bash
cargo +nightly fmt --all
cargo clippy --all-targets --all-features --tests -p elysper-tui -- -D warnings
git add crates/elysper-tui
git commit -m "feat(tui): 目录七域接入——只加配置表，不加界面"
```

Expected: clippy 0 warning

---

### Task 8: 波次 2 · models / plugins / search / ui + `:json` 通用门

四个**非 `List<T>` 信封**的域（Review Focus #2）按取项指针接入，外加覆盖多字段写操作的 `:json` 通用方法门。

**Files:**
- Modify: `crates/elysper-tui/src/domains.rs`（`DOMAINS` 加四条）
- Modify: `crates/elysper-tui/src/domains/tests.rs`
- Modify: `crates/elysper-tui/src/views/catalog.rs`（`:json` 门 + `query_key` 域的查询输入态 + 无 get 域的 Enter 行摊详情）
- Modify: `crates/elysper-tui/src/views/sidebar.rs`（工作区组标题旁的 🔍 搜索入口，§6.1）
- Modify: `crates/elysper-tui/src/views/tests.rs`

**Interfaces:**
- Consumes: Task 2 的 `rows_pointer` / `query_key` 机制；真实 wire 形状：

| 域 | list 方法（params） | 响应形状与取项 | Enter | 写 |
|---|---|---|---|---|
| `models` | `models/list` `{}` | `ModelsResponse { models[], default_provider, default_model }`（`routes/health/mod.rs:141`）⇒ `rows_pointer: "/models"`；列：provider `/provider` · model `/model` · 窗口 `/context_window` | 行摊详情 | 无 |
| `plugins` | `plugins/list` `{}` | `PluginsResponse { resolved[], skipped[], bundles[] }`（`routes/plugins/mod.rs:87`）⇒ `"/resolved"`；列：名称 `/name` · 类型 `/kind` · 风险 `/risk` · 工具数 `/tools`（长度） | 行摊详情 | 无（只读目录） |
| `search` | `search/all` `{"q": <必填>, "limit"?: n}`（`routes/search.rs:29`） | `List<SearchHit>` ⇒ `"/data"`；列：域 `/domain` · id `/id` · 标题 `/title` · 分 `/score` | 行摊详情 | 无 |
| `ui` | `ui/schema` `{}` | `UiSchemaResponse { schemas[] }`（`routes/ui.rs:15`）⇒ `"/schemas"`；列：slot `/slot` · kind `/kind` | 行摊详情 | 无 |

四条都是 `get_method: None`（这些域没有 get 方法）⇒ `EnterOp::Detail` 落到"把列表行本身摊成详情"分支。`search` 的 `query_key: Some("q")`：打开该域先收查询串（composer 式单行输入，Enter 发 `{"q": …, "limit": 20}`）；侧栏 🔍 与 `:search <词>` 都落到它。

`:json` 门（palette 的一个内置项）：

```rust
// views/catalog.rs 的 palette 分支
// 输入形如 `:json schedules/create {"name":"nightly", ...}`：
// 1. 解析方法名 + params（serde_json::from_str，失败即 status 提示）；
// 2. 弹 Confirm { op_label: "json", method, target: params 摘要 }；
// 3. y 才发 Request::catalog(method, params)——方法名是 &'static str，
//    经 Box::leak 或白名单查表拿（优先查表：DOMAINS 已知方法 ∪ 测试白名单，
//    未知名字 leak 一个 String 的常量并记进 status）；
// 4. 服务器对未知方法回 JSON-RPC error，目录态原样显示。
```

- [ ] **Step 1: 写失败的测试**

```rust
#[test]
fn non_list_envelopes_decode_through_rows_pointers() {   // Review Focus #2
    // models fixture（{"models":[…], "default_provider": …}）⇒ decode_list
    // 用 "/models" 取到行；plugins 用 "/resolved"；ui 用 "/schemas"。
}

#[test]
fn search_domain_prompts_for_a_query_before_listing() {
    // open(search 域) ⇒ 不发请求、query 输入态；
    // 输入 "rust" + Enter ⇒ 恰一条 params == {"q":"rust","limit":20}。
}

#[test]
fn the_json_gate_confirms_before_sending() {
    // palette 输入 `:json evals/create {...}` ⇒ 先 confirm 后发；
    // params 非法 JSON ⇒ 不发、status 提示。
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p elysper-tui --lib`
Expected: FAIL（四域缺失 + `:json` 分支不存在）

- [ ] **Step 3: 实现**（四条配置 + catalog 的查询输入态与 `:json` 分支 + 侧栏 🔍）

- [ ] **Step 4: 跑测试，确认通过**

Run: `cargo test -p elysper-tui --lib`
Expected: PASS

- [ ] **Step 5: 手工冒烟**：`models` / `plugins` / `ui` 三域列表与行详情正确；🔍 搜一个词出跨域命中；`:json skills/reload {}` 经确认执行。

- [ ] **Step 6: 格式 / lint / 提交**

```bash
cargo +nightly fmt --all
cargo clippy --all-targets --all-features --tests -p elysper-tui -- -D warnings
git add crates/elysper-tui
git commit -m "feat(tui): models/plugins/search/ui 接入与 :json 通用门"
```

Expected: clippy 0 warning

---

### Task 9: 波次 2 收口 —— 矩阵、README、全量门禁

**Files:**
- Modify: `docs/host-capabilities.yaml`：`agents` / `skills` / `tools` / `tasks` / `schedules` / `workflows` / `evals` / `models` / `plugins` / `search` / `ui` 的 tui 列全部落位——七目录域与四小域标 `full`（备注列写明"多字段写操作经 `:json` 通用门，无表单"），证据指向 `crates/elysper-tui/src/views/tests.rs` 与 `crates/elysper-tui/src/domains/tests.rs`；若评审认为该降级，改 `adapted` 即可（矩阵是数据不是承诺）
- Modify: `crates/elysper-tui/README.md`（目录域清单 + `:json` 门一段）
- Modify: `CHANGELOG.md`（`[Unreleased]` 一条）

- [ ] **Step 1: 矩阵与文档更新**

- [ ] **Step 2: 矩阵门禁**

Run: `make check-host-capabilities`
Expected: `OK:`；tui 列的缺口（`planned` + `n-a`）降到只剩介质不适用项（刀 6 验收原文），缺口数不超过基线

- [ ] **Step 3: 全量门禁**

Run: `make ci`
Expected: 全绿（`check-test-layout` / `check-public-api-runtime` / `check-publishable` 都过——新公开项无 `tokio::` 路径、无新依赖）

- [ ] **Step 4: 按 crate 分批回归（邻居不受影响）**

```bash
cargo test -p elysper-tui
cargo test -p elysper-gateway
```

Expected: 两个 crate 全部 passed（`elysper-app-server` 未改，不在本轮回归清单）

- [ ] **Step 5: 提交**

```bash
git add docs/host-capabilities.yaml crates/elysper-tui/README.md CHANGELOG.md
git commit -m "feat(tui): 波次 2 收口——目录全域可达，矩阵缺口清到介质项"
```

---

## 附：本计划核对过的事实清单

- 方法名（全部 grep 自 `crates/elysper-app-server/src/rpc/methods/**` 的 `router.register`）：
  `sessions/{list,get,delete,export,fork,events,status,follow,resume,compact,usage,search}`、
  `session/unfollow`、`approval/respond`、
  `chat/{session/create,message/send,cancel,regenerate,feedback,usage,operation}`、
  `agents/{list,create,default,get,delete}`、`skills/{list,create,reload,get,delete}`、
  `tools/{list,register,get,unregister}`、`tasks/{list,get}`、
  `schedules/{list,create,get,patch,delete,tick}`、
  `workflows/{list,create,get,replace,delete,run}`、`evals/{list,create,get,delete,run}`、
  `models/list`、`plugins/list`、`ui/schema`、`search/all`
- params 键：`PageQuery { cursor?, limit?, sort? }`（默认 20 / 上限 100，`api/v1/page_query.rs:32-42`）；`NameParams {"name"}`；`IdParams {"id"}`；`SessionIdParams {"session_id"}`（`rpc/methods/sessions.rs:272-280`）；`sessions/list` 还收 `status?` / `context_id?`（`SessionPageQuery`）；`search/all` 的 `q` 必填
- 响应形状：`List<T> { data, next_cursor, total }`（`api/v1/list.rs:27`）；`SessionSummary`（`routes/sessions/mod.rs:85`）；`SessionUsageResponse`（`routes/follow.rs:1505`）；`ResumeRequest` / `InterruptDecision`（`routes/follow.rs:991-1013`）；`ResumeToken { last_seq, pending, request_snapshot_hash }`（`crates/elysper-harness/src/agent/approval.rs:243`）；`FollowSnapshot { header, rows, cursor, live, context, pending_interrupts, request_snapshot_hash }`（`routes/follow.rs:140`）；七目录域与 models / plugins / ui / search 的行类型（Task 7/8 表中逐一标注文件行号）
- TUI 侧：`RequestKind` 七变体与穷尽 match 只在 `app.rs:476-544` 两处（`transport/tests.rs` / `render/tests.rs` / `app/tests.rs` 只是构造值）；`handle_key` 在 `main.rs` 且审批分支在最前；`elysper-tui` 零 feature、`publish = false`

---

## 执行偏差记录（2026-11-10，刀 5 + 刀 6 实施后）

计划写于刀 2 之前，下列条目按**真实代码**修正；接口与提交以本记录为准。

1. **`RequestKind` 已是 9 变体**（`CreateSession` … `DeleteSession`），不是计划写的
   七变体——`ListSessions` / `DeleteSession` 是刀 2 已落地的侧栏路径。本计划只加了
   `Catalog(&'static str)`。
2. **`handle_key` 在 `app.rs`**（`App::handle_key`），不在 `main.rs`。目录键位由
   `App::handle_key` 的 `Pane::Catalog` 分支委派给 `Catalog::key`；`main.rs` 只负责
   三件事：真 palette 的全局 `Ctrl+k`/`Esc` 路由（`App::handle_palette_key`）、
   `RequestKind::Catalog` 响应拦截（先于 reducer）、以及每帧 `take_catalog_requests()`
   的发送。`viewport` 由 `main` 每帧 `App::set_viewport` 注入（半页步进用）。
3. **目录入口键不在 D30 里**：侧栏 `o` 打开目录（sessions 域），`s` 打开搜索查询
   输入（§6.1 的 🔍 入口）；目录内 `Tab` 切换域。
4. **`:` / `Ctrl+k` 的现状**：`App::handle_key` 里 `Ctrl+k` / 侧栏 `:` 打开的是刀 2 的
   占位 overlay，既有测试
   （`colon_ctrl_k_and_question_open_the_overlays`）钉住该行为，未改。二进制里
   `main` 在派发前拦 `Ctrl+k`（任意面板、无审批时）打开真 palette；目录面板内 `:`
   由 `Catalog::key` 打开真 palette；**composer 里 `:` 仍是输入字符**（打字优先），
   所以"从任何面板 `:` 打开 palette"收窄为 `Ctrl+k`（全局）+ 目录内 `:`。待人工冒烟
   时如认为不可接受，再调整 reducer 的 Ctrl+k 分支并同步该测试。
5. **工作区树仍是 `App.workspaces`**（刀 2 的单一真源，`sessions/list` →
   `apply_sessions_list`），未改为引用 catalog 分页：改成引 catalog 会推翻减少器语义
   与既有测试；catalog 的 sessions 域是**按需**读取的第二视图（计划 Review Focus #5
   的"目录按需拉取"就是它）。两处并存已在 README 说明。
6. **`DOMAINS` 的 12 条在 Task 2 的提交里一次落表**（sessions + 七域 + 四小域）；
   Task 7 的提交补七域的 wire fixture 表测试，Task 8 的提交补搜索入口（侧栏 🔍/`s`）、
   无 `get` 域的行摊详情与 chat palette 测试。行与测试的最终状态与计划一致。
7. **`:json` 未知方法本地拒绝**，不 leak `String`：项目规则禁止 `Box::leak`。计划原文
   "未知名字 leak 一个 String 的常量"改为查 `KNOWN_METHODS` 白名单，未命中即 status
   提示、不发请求（服务器本就会拒绝，少一次往返）。
8. **`ResumeMaterial` 多一个 `last_message_id` 字段**（`chat/feedback` 的客户端标签
   `msg-<n>`）；resume 的三件材料（`cursor` / `pending_interrupts[].call_id` /
   `request_snapshot_hash`）对应 `app.rs` 新增的**三个**基线字段（计划写"两个"——
   pending 也需要留存，否则无法拼 token）。
9. **`sessions/resume` 的可用性**：只有焦点行 == 跟随会话且 `pending` 非空时可用；
   其余情况菜单项给出 status 提示且不弹确认（Review Focus #4 的"置灰"）。
10. **确认模态的键**：`y`/`Enter` 执行、`Esc`/`n` 取消；模态打开时其余键被吞掉，
    不会透传到列表。
11. **`list_params` 只为分页域发 `limit`**：`models` / `plugins` / `ui` 的响应无
    `next_cursor`，params 为 `{}`（对齐 Task 8 的表格）；`search` 发
    `{"q": …, "limit": 20}`，其余分页域 `limit: 50`。
12. **`docs/host-capabilities.yaml` 与 `make check-host-capabilities` 在本仓不存在**
    （刀 4 的产物是 `docs/host-capabilities.md`，没有逐行 tui 列，也没有 YAML 门禁）。
    Task 6 / Task 9 的矩阵更新改落在该 md 的新 §6「TUI 落点」，证据指向
    `crates/elysper-tui/src/{domains,views,app}/tests.rs`；`make ci` 照跑。
13. **目录响应失败**（含网关重启导致的在途失败）落该域的 `ListView::error` /
    status 行，`ready` 后不自动重放目录请求；`r` 手动重拉（Review Focus #5 后半）。
14. **手工冒烟未执行**（无 tty 环境）：留待人工按 Task 6 Step 5 / Task 7 Step 5 /
    Task 8 Step 5 的清单核对。
