# 终端用服务端的 wire 类型

「终端是线上的客户端，不是服务端 crate 的客户端」——这是这条线开始时
终端的立场。终端隔着一条 `/rpc` WebSocket 与服务端说话，线上形状就是
契约，终端按线上形状**抄一份自己的 DTO** 读，两边各写各的；服务端 crate
是「服务端的事」，终端只消费它的线上字节。

抄一份的账已经付过两次：

- **页大小**：终端的目录面板自己写死 `limit: 50`，服务端从没见过这个
  数——它的默认是 20。终端每页要 50 行，服务端每页给 20 行，而「一页
  多少行」这件事在两个地方各有一个主人。
- **活动投影的工具调用**：终端抄的 live 帧只读 `text` / `reasoning` /
  `running`，服务端的活动投影里的 `tool_calls` **整个被丢掉**——跑着的
  工具调用在终端上不可见，而浏览器看得见。

第三次是方法名（终端留着自己的方法名副本），第四次是 follow 基线（终端
手抄了一份 `FollowSnapshot`）。四次都是同一个病：**形状有两个主人**，
谁先改谁赢，而编译期抓不到任何一次。

本 ADR 裁定：终端与服务端之间的形状**归服务端一个主人**——终端直接用
服务端的类型读线上的帧，终端不留副本。

## 决定

### 1. wire 类型归服务端

终端读线上帧时用服务端的类型，抄写的那份删净：

- follow 基线与帧：`uva_app_server::routes::follow::{FollowSnapshot,
  FollowEvent, SessionHeader, ContextMeter}`；
- 活动投影：`uva_app_server::session::controller::{LiveTurn,
  LiveToolCall}`；
- schema 树：`uva_app_server::routes::ui::UiSchemaResponse`；
- 目录列表的信封：`uva_app_server::api::List<T>`；
- 会话列表与会话状态：`uva_app_server::routes::sessions::SessionSummary`、
  `uva_app_server::session::status::SessionStatus`。

代价是服务端 crate 的这些类型必须**能被客户端解回来**——所以本线给它们
补了 `Deserialize` 派生（见第 6 节）。这是「形状归一个主人」的另一半：
形状要能往返，编解码的两端必须是同一份定义。

### 2. 依赖边及其真实代价（实测）

新增的边是 `crates/uva-tui` → `crates/uva-app-server`（应用 crate 依赖
应用 crate，两个都是 `publish = false`）。这条边的真实代价是**实测**的，
不是推出来的：

- **只链接 wire 类型**：`nm -C` 扫 debug 构建的 `uva-tui` 二进制，
  `uva_app_server` 符号 **1116** 个；`axum::routing` **0** 个、
  `reqwest::` **0** 个、`AppState` **0** 个。服务端的**运行时没有被拉
  进来**——被拉进来的是 wire 类型本身（数据结构与它们的派生实现）。
- **体积**：release 构建、同一棵树、同一份依赖版本，只换终端这一侧——
  这条边本身让终端的 release 二进制从 **3,024,080** 字节涨到
  **3,090,800** 字节（+66,720 字节，+2.2%）；整条线（方法名归一、类型
  归一、页大小对齐、活动投影入屏）累计到 **3,202,816** 字节（+5.9%）。
  涨的是 wire 类型自己的代码，不是第二份服务端。

旧的 Cargo.toml 理由（「网关已经链了服务端（它监管 `serve` 二进制），
所以这条边不增加链接体积」）是**假的**：网关对 `uva_app_server` 的源码
引用数是 **0**——它把 `serve` 当**子进程**拉起，消费的是 CLI 契约。
`crates/uva-gateway/Cargo.toml` 里那条依赖源码无引用，是既存的无用清单
边；本轮不动它，交维护者决定。

### 3. 方法名的唯一出处

请求方法名的唯一出处是
[`rpc::methods::names`](../../crates/uva-app-server/src/rpc/methods/names.rs)：
64 个 `pub const`（`SESSIONS_LIST`、`AGENTS_LIST`…）加一张
`pub const ALL`。终端直接引用那些常量（`names::SESSIONS_LIST`），不留
副本——名字拼错在编译期就红。

**通知**方法名另在
[`rpc::notify`](../../crates/uva-app-server/src/rpc/notify.rs)
（`session/event` / `profile/changed` / `approval/request`）与 Anthropic
兼容事件（`routes/messages/mod.rs`）——**不在** `names.rs` 那张表里：
通知没有注册表可读，它们是**推**出去的，登记的是「推什么」不是「答什么」。

三个集合必须相等，服务端测试把它们钉在一起：路由注册的 ⊆ `ALL`、
`ALL` ⊆ 路由注册的、**文件里每个 `pub const` 都在 `ALL` 里**。第三条
是本线补的：没有它，一个人加一个常量 `FOO` 而不注册、不加进 `ALL`，
服务端测试全绿，而两个门禁脚本会把 `FOO` 当成服务端**提供**的方法。

两个门禁脚本（`check-web-methods` / `check-host-capabilities`）因此改读
**注册**而不是**声明**：`grep -o 'router\.register(names::[A-Z_]*'` 再
回查常量值。判定方向不动——前端调用的每个方法名都必须是服务端真正提供
的方法，服务端有而前端不调的不算错。

### 4. 解不开的帧：整帧不应用 + 状态行

形状归一个主人之后，「形状漂了」有了统一的契约：**解不开 = 整帧不应用
+ 状态行**，不是半应用、不是静默降级。

- follow 帧 / 基线 / 会话列表 / 聊天答复 / schema 树：解不开的那一帧
  **整个不应用**，状态行写明「unreadable」。半应用的状态比没有状态更
  坏：它看起来像一个真的投影。
- 目录列表页：信封（`List<T>`）解不开 = **报出来**——状态行写明解析失
  败。此前是静默显示 0 行、无游标、无总数，「形状漂了」和「这个域没东
  西」长得一模一样。兜底（按指针读）只留给 **bespoke 域**
  （`models/list` / `ui/schema`）——那两个域本来就没有可解的信封。

### 5. 四条用户可见的变化

1. **页大小 50 → 服务端默认（20）**：目录域列表与侧栏会话列表都改问服
   务端的默认（`api::v1::DEFAULT_LIMIT` / `routes::search::DEFAULT_LIMIT`）。
2. **活动投影里的工具调用现在入屏**：服务端发的 `tool_calls` 不再被丢
   弃，跑着的工具调用在终端上看得见（此前只有浏览器看得见）。
3. **漂移的 follow 帧从「半应用」变为「整帧不应用 + 状态行」**：解不开
   的帧不再改出一半状态。
4. **删掉的死路径 `sessions/events`**：终端那一侧的读法与服务端从来对不
   上（服务端答的是对象，终端当数组读），也没有调用点——路径连同它
   唯一的 DTO 一起删净，源码门记着它不回来。

### 6. 公开面的增长

服务端为了让客户端能解回来，公开面增长了一批（逐项见
[`docs/api-frozen.md`](../api-frozen.md) 的「Recorded shape changes」）：

- 新增 `rpc::methods::names` 模块（64 个 `pub const` + `pub const ALL`）；
- 新增 `rpc::notify::ApprovalRequestNotification`（含 `request()`）；
- 新增公开方法 `routes::chat::Delivery::as_wire`、
  `routes::ui::UiSchemaResponse::components_in`；
- 可见性放宽：`routes::search::DEFAULT_LIMIT`（私有 → `pub`）；
- 签名变更：`routes::follow::FollowEvent::payload()` 由 `-> Value` 改为
  `-> Result<Value, serde_json::Error>`（并去掉 `#[must_use]`）；
- 字段类型变更：`session::controller::LiveToolCall::status` 由
  `&'static str` 改为 `String`；
- 新增 `Deserialize` 派生：`routes::follow::{SessionHeader, ContextMeter,
  FollowSnapshot, FollowEvent}`、`routes::chat::{CreateSessionResponse,
  SendMessageResponse, WireAttachment}`、`routes::sessions::SessionSummary`、
  `session::status::SessionStatus`、`ui::schema::{UiSchema, UiComponent}`、
  `ui::tree::SlotInfo`。

**线上字节不变**：`LiveToolCall::status` 的类型变更是 Rust 侧的（`&'static
str` → `String`），序列化出来还是同一个字符串。变的是「客户端能不能把
它解回来」。

### 7. 本轮不采纳的两条建议

- **源码门太脆**（`the_terminal_asks_for_no_method_whose_answer_it_cannot_read`）：
  这道门是**文本扫描**，会误伤注释（一条提到 `sessions/events` 的注释就
  会让它红），覆盖面也只能靠正则猜。改法（编译期穷尽 `match`，让形状
  的分叉在类型上就发生）是更大的改动，本轮不做。
- **`REDUCER_KINDS` 手维护 9 项**：reducer 的请求种类是一张手维护的表，
  加一种 kind 就要记得改表。同上，留到下一轮。

## 参考

- [`crates/uva-app-server/src/rpc/methods/names.rs`](../../crates/uva-app-server/src/rpc/methods/names.rs)：
  方法名的唯一出处（64 个常量 + `ALL`）。
- [`crates/uva-app-server/src/rpc/methods/tests.rs`](../../crates/uva-app-server/src/rpc/methods/tests.rs)：
  三个集合相等的钉子（注册 ⊆ 表 ⊆ 声明）。
- [`crates/uva-tui/src/domains.rs`](../../crates/uva-tui/src/domains.rs)：
  目录域的表格与信封解码（解不开要报出来）。
- [`crates/uva-app-server/src/routes/tests/follow.rs`](../../crates/uva-app-server/src/routes/tests/follow.rs)：
  三个 follow 帧的字节级钉子（帧有两个读者，中间没有门）。
- [ADR-0024](0024-session-resolution-in-one-module.md)：同期的相邻决定——
  会话解析归一个模块；本 ADR 管的是**终端与服务端之间的形状**。

## Considered Options

- **终端继续抄 DTO，只把抄的那几处补齐**：被否。抄一份的账已经付过两次，
  补齐只是把下一次漂移推迟。形状要么有一个主人，要么有两个。
- **把 wire 类型抽成第三个 crate（两边都依赖它）**：被否。那是第三个主人
  ——类型住在那里，编解码的规则还要再抄一份；而且服务端本来就在写这些
  形状，让它拥有它们更省一件事。
- **终端在运行时按 JSON 路径读（不解类型）**：被否。那正是「半应用」的
  来源：读不到字段就当它没有，于是服务端的字段在终端上凭空消失
  （`tool_calls` 就是这样没的）。
- **让网关把服务端的类型转译一遍再给终端**：被否。多一个转译层，多一
  个能漂移的地方；浏览器走的是同一条 `/rpc` 路径，它解的就是服务端的帧。
- **把 follow 帧也加进契约目录（`api/contract.rs`）**：留到下一轮。帧有两
  个读者（终端类型化解码 + 浏览器手写解析），本线先给三个变体加了字节级
  钉子；进契约目录意味着 `types.ts` 也要跟着变，是另一条线的体量。

## Consequences

- **调用方可见**：CHANGELOG「TUI 的 wire 形状归一个主人」一条记录第 5 节
  的四条用户可见变化与第 2 节的依赖边代价。
- **模块面**：`uva-tui` 的 `domains::decode_list` 现在返回 `Result`（信封
  解不开是错误，不是空页）；`rpc::methods::names` 成为方法名的唯一出处。
- **测试面**：帧形状在 `routes/tests/follow.rs` 逐字钉住；方法名表的完备
  性在 `rpc/methods/tests.rs` 钉住；目录页的解码失败在
  `uva-tui/src/views/tests.rs` 钉住（状态行，不是 0 行）。
- **门禁面**：`check-web-methods` 与 `check-host-capabilities` 的取数源从
  「声明」改成「注册」；判定方向不动。
