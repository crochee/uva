# uva-tui

终端投影：一个 ratatui + crossterm 的 TUI，通过 `uva-gateway` 驱动
`uva-app-server serve` 网关——与浏览器、桌面壳**同一条宿主路径**：
同一个 `/rpc` WebSocket、同一个网关监管者、同一份 JSON-RPC 2.0 契约。

> 这是应用 crate（`publish = false`）：它依赖 `publish = false` 的
> `uva-gateway`，不是 lib 消费者的入口。分层见
> [`src/transport.rs`](src/transport.rs)（网关客户端的领域适配：方法映射、
> id 关联、全量通知订阅、重连后重发 follow、活性探针）、
> [`src/app.rs`](src/app.rs)（纯 reducer）、[`src/render.rs`](src/render.rs)
> （只画）、[`src/main.rs`](src/main.rs)（clap + 终端守卫 +
> `tokio::select!`）。

## 三条宿主路径同源

```text
crates/uva-tui ─┐
浏览器（uva-web）─┼─▶ serve 的 /rpc WebSocket ─▶ core（无头）
桌面壳（desktop）  ─┘     （网关监管 core、按 id 中继帧）
```

TUI 不再自己 spawn `core`、不再自己分帧：进程监管（spawn、`/readyz` 探针、
1s→5s 退避重启、退出回收）都在 `uva-gateway` 里。TUI 保留的是自己的
领域层——`RequestKind` ↔ 方法名映射、每个请求一个 task 的 id 关联
（`Response` 事件始终带着它的 `RequestKind`）、catch-all 通知订阅、以及
"连接是新的"就重发 `sessions/follow`（core 重启的 `ready` 广播、网关
重启、断线后探针首次应答）。网关只广播 core 启动时的 `ready`，晚到的
连接收不到——所以传输层的 `health/ping` 探针负责合成初始 `Ready`。

## 运行

```sh
# 先构建 server，再让 TUI 拉起它（默认从 PATH 找 `uva-app-server`）
cargo build -p uva-app-server
UVA_APP_SERVER=target/debug/uva-app-server \
  cargo run -p uva-tui -- --directory /path/to/workspace

# 或附着到已在跑的网关（浏览器与终端同服；不 spawn、不监管）
cargo run -p uva-app-server -- serve --port 8080 &
cargo run -p uva-tui -- --attach http://127.0.0.1:8080
```

`--profile <PATH>` 原样传给网关（`serve --profile <path>`）。
`--app-server <PATH>` 与 `UVA_APP_SERVER` 等价。附着模式下网关死掉：
TUI 显示网关不可达并持续重连，**不会**自己重启网关。

## 键位

| 场景 | 键 |
| :-- | :-- |
| 列表（侧栏 / 目录） | `j`/`k` 移动 · `gg`/`G` 首尾 · `Ctrl+d`/`Ctrl+u` 半页 · `/` 搜索 · `n`/`N` 下一个/上一个匹配 |
| 打开 / 操作 | `Enter` 打开 · `y` 复制 id · `x`（`dd`）删除条目 · `Esc` 取消 |
| 全局 | `:` / `Ctrl+k` 命令面板 · `?` 快捷键帮助 · `Ctrl+b` 折叠侧栏 · `Ctrl+c` 退出 |
| 侧栏 | `Tab` 设置导航 · `i` 聚焦输入框 · `r` 刷新会话列表 · `o` 打开目录 · `s` 搜索 |
| 输入框 | `Enter` 发送（没有会话时先创建） · `Esc` 回列表 |
| 审批 | 提示打开时 `y` 允许 / `n` 拒绝（优先于列表的 `y`） |

## 目录浏览（波次 1：会话 + 对话全域）

侧栏工作区树下的「目录 (o)」分组列出已接的域；`o` 进入目录面板
（`Pane::Catalog`）——侧栏变成域列表，主区是该域的通用列表。每域的
列、取项指针、`get` 方法与操作表都在 [`src/domains.rs`](src/domains.rs)
的 `DOMAINS` 里：新增一个域只加一条配置，不加界面。

- `Enter` 打开：`sessions` 域走既有的 `sessions/follow` 路径进对话流；
  其余域调 `<域>/get`，没有 `get` 的域把列表行本身摊成键值详情。
- `:` / `Ctrl+k` 打开命令面板：目录面板列出当前域的操作（含「详情 /
  用量 / 运行状态 / 导出 / fork / resume / 压缩 / 删除」），对话面板列出
  `regenerate` / `feedback-up` / `feedback-down` / `search <词>`。
- 写操作先确认：`x` / `dd` 与 palette 里的 fork / resume / 压缩 / 删除都先弹
  确认模态，`y` / `Enter` 执行、`Esc` / `n` 取消。审批提示打开时 `y`/`n`
  仍然先答审批——该分支在 reducer 里排在最前，既有语义未动。
- `resume` 的请求体由 `sessions/follow` 基线拼成：
  `cursor` / `pending_interrupts[].call_id` / `request_snapshot_hash` 三个
  字段来自基线（`ResumeMaterial`），无挂起中断时会话不予 resume。
- `feedback-up` / `feedback-down` 的目标是**本轮**回答那一行的序号（行里的 `seq`）：
  轮次从操作员最后一次提问起算——评的是这一轮的答案，更早一轮的答案属于别的轮次；
  本轮只写了思考内容时没有可评价的答案，不给目标。web 的投影按同一条边界拆气泡，
  两个界面指认同一行。
- 目录响应在 `main` 里**先于** reducer 拦下交给 `Catalog`，不会覆盖连接
  状态行；网关重启后目录请求不自动重放，`r` 手动重拉（目录是按需拉取，
  不是订阅）。

多字段写操作（create / patch / register）走 `:json` 通用门：
`Ctrl+k`（目录面板里也可直接 `:`）输入 `json <method> <params>`，例如
`json schedules/create {"name":"nightly","kind":"cron",…}`。合法 JSON 才弹确认，
`y` 发送；方法名必须在注册表白名单里（`src/domains.rs` 的 `KNOWN_METHODS`，
与服务器 `router.register` 对账），未知名字本地拒绝、不发请求。

## 目录域清单（波次 2）

| 域 | 列表方法 | 详情 | 写操作（先确认） |
| :-- | :-- | :-- | :-- |
| 智能体 | `agents/list` | `agents/get` | 删除 |
| 技能 | `skills/list` | `skills/get` | 删除 · 重载 |
| 工具 | `tools/list` | `tools/get` | 注销 |
| 任务 | `tasks/list` | `tasks/get` | —（服务器只注册 list/get） |
| 计划 | `schedules/list` | `schedules/get` | 触发 · 删除 |
| 评测 | `evals/list` | `evals/get` | 运行 · 删除 |
| 模型 | `models/list`（`/models`） | 行摊详情 | — |
| 搜索 | `search/all`（`q` 必填） | 行摊详情 | — |
| UI | `ui/schema`（`/schemas`） | 行摊详情 | — |

「模型 / 搜索 / UI」的响应不是 `List<T>` 信封，`DomainSpec.rows_pointer`
声明各自的取项指针（服务器 wire 未改）。搜索入口有两处：侧栏工作区组标题旁的
🔍（`s` 打开查询输入）与 `Ctrl+k` 里的 `search <词>`。

## UI 树（插槽）

终端也有一棵 UI 树（设计 D25）：每代 core `ready` 后取一次 `ui/schema`，
`chat.sidebar` 渲染为对话右侧列、`session.detail.extensions` 渲染为
transcript 下方的块；两者都只在有组件时占位，空树时布局与插槽出现前逐项
相同。kind 注册表各宿主各写一份（D18）：`panel` / `list` / `key-value` 三种
照常画，未知 kind 显示 `? <kind>` 占位行；声明了但本构建不放置的插槽收进
那个块的一行 `unplaced: …` 摘要。新增一个 UI 插件 = 一行 profile + 一份组件
数据，终端代码零改动。

**边界（别把"零改动"读成"永远零改动"）**：零改动成立的前提是组件用**已注册的
kind**（`panel` / `list` / `key-value`）；新增一种 kind 需要在终端的 kind 注册表
（`src/render/ui_slots.rs`）加一个渲染器。插槽 id（`chat.sidebar` 等）两宿主各写
一份字面量——服务器改名时，终端不会静默丢组件：它落进 `unplaced: …` 摘要，
浏览器落进"未放置"分组，两边都看得见。

## 转录一行条目的截断规则

工具的调用与结果各渲一行（`tool call: <名字> <参数>`、`<名字>: ok|failed <输出>`），
参数与输出是载荷，一行放不下全部——放多少按仓内既有约定取：**48 个字符**（与
`views::catalog` 确认弹层的一行参数摘要同一个限值），按字符而非字节截断（用
`uva_core::text::truncate_chars`，仓内唯一的截断原语，永不在码点中间下刀），
截断时补一个 `…`（TUI 自己的截断点用的标记，`views::catalog` 的参数摘要同一个；
仓内其它截断点各有自己的标记）。多行载荷先折叠成单行（空白归一），
崩溃修复 closer 的错误码不带载荷，只进状态：`shell: failed (TOOL_NOT_STARTED)`。

一条消息的多个文本段按空格隔开（`part one part two`）——那些段是一条消息的几块，
不是粘在一起的一个词；工具输出的那一行同样如此。

## 分层与测试

- **transport**：`Driver::launch`（spawn `serve`）与 `Driver::attach`
  （连已在跑的网关）是生产路径；测试用一个进程内 axum WebSocket 服务器
  说同样的 JSON-RPC 信封（`src/tests.rs` 的 `FakeGateway`），因此不需要
  tty、真实进程或真实模型。服务器断开 ⇒ `Disconnected`；重连后探针应答
  ⇒ 合成新 `Ready` 并重发 follow。
- **app**：`App::apply(TransportEvent) -> Vec<Action>` 是纯函数式投影；
  `Action::request()` 是请求工厂，`approval/respond` 的
  `approval_id` / `decision` 原样序列化。
- **render**：`render::draw` 只读 `App`；测试用
  `ratatui::backend::TestBackend` 断言缓冲区文本，不需要 tty。

真实进程冒烟测试不在 CI 里跑。手动冒烟见计划文档
`docs/design/2026-11-09-tui-gateway-alignment-plan.md` Task 3。
