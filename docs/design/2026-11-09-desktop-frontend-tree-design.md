# 投影第二波：统一宿主路径 + 宿主等价 + Desktop 入口 + 浏览器插件树 + spec 收口

日期：2026-11-09
状态：待批准（用户 2026-11-09 拍板：Desktop 用 Tauri；同一套前端代码服务 web 与 desktop；
TUI 保留终端原生 UI，但传输并轨到网关）
参考：
- Axiom 原文（`docs/superpowers/specs/2026-11-08-axiom-harness-reference.md`，**本地规划层，不入库**）
  §7.2 / §7.4 / §8.2 / §12 阶段六、阶段七
- DeepSeek Harness 的对应做法（**行 = 插件 + 配置**、层序、变更结果四态、装配回滚）：
  - <https://dshplugin.app/guides/dsh-plugins-bundles-profiles>（Plugin / Bundle / Profile 与层序）
  - <https://dshx.dev/en/docs/internals/architecture>（插件树按层装配、失败回滚、`application` 四态）
- 迁移设计（`docs/superpowers/specs/2026-11-08-uva-plugin-spine-design.md`，**本地规划层，不入库**）
  §7 裁定记录、§8 分阶段落地

> 本篇是**入库**的设计文档（`docs/design/` 是本仓被跟踪的设计文档目录）。
> `openspec/` 与 `docs/superpowers/` 被 `.gitignore:41-42` 排除，是本地的规划 / spec 层
> （仓库 0484f5c6 的既定立场：规划产物留在本地）。它们照常同步更新，但不构成入库产物。

## 1. 意图与动机

Axiom 终态图有三种投影。uva 今天的状态：

| 形态 | 核心树 | 投影树 | 状态 |
|---|---|---|---|
| 无头 `core` | ✅ | ❌（A5 的正确表现） | 已落地 |
| TUI | ✅ | 进程内固定 widget（无加载器） | 已落地（迁移设计阶段 5），但**自建了一条传输** |
| Web | ✅ | 浏览器端 **schema 渲染降级** | 已落地，但投影树是"半棵" |
| Desktop | ✅ | — | **缺** |

三件事因此欠着：

1. **宿主路径不统一**。今天 TUI 自己 spawn `core`、自己实现 Content-Length stdio 传输、
   自己管 1s→5s 退避与重发 `sessions/follow`（`crates/uva-tui/src/transport.rs`，726 行），
   而 web / 桌面走的是网关。同一份协议契约被两套传输实现各写一遍，core 的监管者有两个。
   本轮把 TUI 并轨到网关：**三个宿主，一个协议面，一个 core 监管者**。
2. **Desktop 入口**（Axiom §7.4 / 阶段六）——迁移设计 N2 把它排除为"独立后续项目"，本轮做掉。
3. **浏览器端投影树**（Axiom §8.2 / 阶段七可选）——今天前端只认服务器下发的 JSON schema
   （`panel` / `list` / `key-value` 三种 kind，未知 kind 走占位），`slot` 只是个字符串，
   SPA 里**没有真正的插槽区域**；服务器侧明确拒绝任何可执行载荷（`ui/schema.rs` 拒
   `script` / `stylesheet` / `javascript` / `css`）。本轮升级成真的插槽系统，
   并把同源 JS 执行作为**显式开关**后的第二级能力。

第四件事是收脏活：本地 openspec 层里 `openspec/specs/web-projection/spec.md` 仍写着
"MUST NOT 新增 TUI 或 Desktop 入口"与"`uva-app-server` 与 `uva-web` 之外不得出现
表现层 crate"，而 `crates/uva-tui` 已经在仓库里——**两条**禁令都已被违反，
本方案还会再违反一次。刀 0 把它们收干净。

第五件事是用户追加的硬要求：**三个宿主做等价实现**。并轨只统一了"怎么到达 core"，
没统一"能做什么"——今天 SPA 覆盖 13 个域 / 54 个方法，TUI 覆盖 7 个方法 / 3 块界面。
本轮把"等价"变成可度量的属性（能力矩阵 + 棘轮），并用通用渲染器把 TUI 补到同一能力集。

"等价"的两层含义在本轮是分开的：**架构对等**（同一协议面、同一个 core 监管者、
各自最优的渲染栈）与**能力面等价**（同一套核心能力在三个宿主上都可达）。
**不追求共享呈现代码**——共享止于协议契约与能力矩阵（D18）。

**核心一行不动。** 本轮所有改动都在投影层与传输层：核心的 Fiber / Effect / Service / Event
语义、profile 装配、Wasm 通道、seam 链都不进本轮的范围（A1 / A3 / A4 保持）。
"大插件管一族小插件"（D36）用**服务契约**表达（DSH 同款，D38）——**不需要** kernel 新增
父子装配机制。

## 2. 目标 / 非目标 / 反目标

**目标**

- **G1 宿主路径统一**：TUI / 浏览器 / 桌面 WebView 三个宿主全部经**同一个网关**
  （`uva-app-server serve`）到达**同一个 core**，协议面只有一份 JSON-RPC。
- **G2 一套前端**：浏览器标签页与桌面 WebView 跑**同一份** `uva-web` 构建产物，
  同一套 JS，不存在"桌面专用前端"。
- **G3 Desktop 入口**：一个 Tauri 2 桌面应用，内含窗口 + WebView。
- **G4 真插槽系统**：前端的 `slot` 从字符串升级为声明式插槽（基数 `one` / `many`、作用域、
  owner props、子插槽、dispose 级联），SPA 划出真实插槽区域。
- **G5 同源插件执行（默认关）**：允许 UI 插件以同源 `import()` 的形式提供代码，
  由部署逐行显式开启；关闭时保持今天"永不执行远端 JS"的保证。
- **G6 spec 收口**：本地 openspec 层的两条禁令改成现状陈述，并新增 `desktop-projection`
  与 `frontend-plugin-tree` 两份能力 spec；入库文档同步（AGENTS.md / CHANGELOG / 本篇）。
- **G7 三个宿主能力面对齐**：TUI 与 web / desktop 覆盖**同一套核心能力**（13 个域），
  由一份**能力矩阵**度量并被 CI 棘轮。桌面因为复用同一份 `dist` 天然对齐；
  TUI 通过**通用渲染器 + 每域适配表**补齐（不逐页移植）。
- **G8 一套界面 + 一处真源**：三个宿主是**同一套界面**的两种渲染（web/desktop 同一份 SPA，
  TUI 是终端渲染）；能力与能力的**定义**只来自 **plugin 排布**（读目录的手段），
  **启用与参数一律内敛到 profile**（没有 settings 概念）；变更走**内置 MCP**，
  **装得起来才生效**（事务性）。
- **G9 本轮的终点**：**架构收敛**（内核可插拔、界面统一、能力对等）为主目标，
  **一个能给别人用/演示的桌面应用**为验收；因此与目标无关的机制性工作**砍掉**
  （前端插件树 / 同源 JS / CI 棘轮门禁，见 D33 / D27）。

**非目标**

- **N1** 不改核心（Fiber / Effect / Service / Event 语义、装配、seam 链、Wasm 通道、
  profile 层序一律不动）；不为任何具体插件在核心里开特例（A1 / A3 / A4 保持）。
- **N2** 不做跨源插件加载（永不）、不做 `eval`、不在本轮引入 CSP（见 D11 的缺口记录）。
- **N3** 不做桌面端专属 UI，也不改 TUI 的终端渲染层（桌面与浏览器同一份前端；TUI 仍是 ratatui）。
- **N4** 不做远程分发 / 插件市场；插件资产只从页面同源的静态服务加载。
- **N5** 不重写 SPA 的页面结构；插槽只新增区域，不替换既有页面布局。
- **N6** 不建前端自动化测试套件（AGENTS.md §4.2 家规，见 D7）。
- **N7** 不删 TUI 的 ratatui 界面，也不把它降级成"打开浏览器"的启动器。
- **N8** 不追求"逐像素同款界面"：终端与 DOM 是两种介质，**等价 = 能力可达**，
  呈现各自最优（`adapted` 是合法终态，见 D17）。
- **N9** 不建**共享呈现层**：不把领域投影上移服务器、不引入跨语言共享运行时、
  不让 TUI 复用 SPA 的界面代码（DOM/CSS 在终端里不成立，见 D18）。
- **N10** 设置页**不做**「规则与记忆」分区（steering / memory 不进设置面）；
  Hooks **不**从别处的配置文件导入（定义一律来自 plugin 排布，见 D24）。

**反目标**

- **R1** 不让 root workspace 的任何门禁变红（`make ci` 必须保持绿，见 §3 的 `--all-features` 约束）。
- **R2** 不引入第二个前端传输实现（前端仍然只有一条 WS 通路，见 D2）。
- **R3** 不留"桌面另有一套前端 / 另有一套协议 / TUI 另有一套传输"的影子路径。
- **R4** 不在两个宿主里各复制一份"拉起网关 + 等就绪 + 重启"的逻辑（逻辑住在共享 crate）。

## 3. 约束

**仓库硬约束（继续全部生效）**

- AGENTS.md §3：错误处理（lib 用 `thiserror`、app 用 `anyhow`）、依赖版本 `major.minor`、
  tokio feature 按需声明、`Clock` / `IdGen` trait、禁止 `pub use mod::*`。
- `make ci` 的全部棘轮：`check-kernel-deps` / `check-mvp-deps` / `check-pub-surface-strict` /
  `check-clock` / `check-stdout` / `check-public-docs` / `check-test-layout` /
  `check-harness-shape` / `check-harness-docs` / `check-plugin-shape` / `check-claim-language` /
  `check-deny` / `check-publishable` / `check-single-writer` / `check-public-api-runtime` /
  `check-frozen-api`。
- `uva-kernel` 依赖图不得出现任何 agent / UI / 传输层 crate。
- AGENTS.md §4.2：**本项目不保留任何前端自动化测试套件**，验证一律用 Playwright MCP
  驱动浏览器；纯函数按 §4.2 的写法在页面内 `await import('/src/lib/foo.ts')` 断言。

**本轮新增的硬约束（实测事实）**

- **C1 本机是 WSL2 + Arch Linux；GUI 目标已可编（2026-11-09 实测）**：
  `/etc/os-release` = `Arch Linux`（rolling，包管理器 **pacman，没有 apt**）。
  装 `webkit2gtk-4.1` 前 `pkg-config --exists webkit2gtk-4.1` 为空；装成 **2.54.2**
  （Arch `extra` 仓库；镜像滞后时先 `pacman -Syy` 再装，`gtk3` / `libsoup3` / `librsvg` /
  `gcc` / `make` / `openssl` / `curl` / `wget` / `file` 本来就在位）。
  **实测**：一个只依赖 `tauri 2.12.5`（feature `wry`）的临时 crate 在本机 `cargo check`
  **通过**（75s，含 `webkit2gtk` / `soup3` / `tao` / `tauri` 全链）⇒ **桌面壳在本机可以编、
  可以跑**（`/mnt/wslg` 在、`DISPLAY=:0`）。
  ⇒ C1 的约束从"编不过"降级为"需要 `webkit2gtk-4.1`"；D3 把壳放在 root workspace 之外的理由
  随之改为：**不让 webkit 系统依赖成为每个贡献者跑 `make ci` 的前提**
  （`make lint-rust` 的 `--all-features` 会编到 GUI 目标）。
  CI 侧不变：workflow 跑在 `ubuntu-latest` 上，用 apt 的包名。
- **C2 `--all-features` 会编到 GUI 目标**：`make lint-rust` =
  `cargo clippy --all-targets --all-features --tests --all`。只要 Tauri 依赖待在 root
  workspace 成员里，**任何没装 `webkit2gtk-4.1` 的机器**跑 `make ci` 必红（本机已装，见 C1；
  但 CI 的 `ubuntu-latest` 与其他贡献者的机器未必有）。
- **C3 前端 WS 硬连同源**：`uva-web/src/api/rpc.ts:269` 直接
  `ws(s)://${window.location.host}/rpc`，没有可插拔 transport 层。
- **C4 前端无测试框架，且 CI 的 web job 固定 Node 20**：`package.json` 只有 vite / react /
  typescript / eslint / prettier；`.github/workflows/rust-quality.yml:177` 是 `node-version: 20`
  （Node 20 没有 TS 类型剥离）；`tsconfig.json` 的 `include` 是 `["src"]` 且没有 `@types/node`
  → 在 `src/` 下放 `*.test.ts` 会同时炸 `tsc --noEmit` 与 CI 的 Node 版本。
  **这三条合起来直接否掉"前端单测"这条路**（D7）。
- **C5 网关已是完整投影面**：`serve` 提供 `/rpc`（WS）、`/livez`、`/readyz`（core 宣布
  `ready` 后才 200）、`/version`、`/metrics`，其余路径 fallback 到 SPA 静态文件
  （`crates/uva-app-server/src/gateway.rs:594`），并以 1s→5s 退避重启 core。
  网关的 WS 对帧是**透明中继**：浏览器发什么文本，就原样转给 core 的 stdin，按 id 回帧
  （`gateway.rs` 的 `forward_upstream`）。⇒ **任何 JSON-RPC 客户端（浏览器、Rust TUI）都能
  用同一套方法名与信封**，这正是 TUI 并轨的技术前提。
- **C6 规划层不入库**：`openspec` 与 `docs/superpowers` 在 `.gitignore:41-42`。
  入库产物落 `docs/design/`、`AGENTS.md`、`SEAMS.md`、`CHANGELOG.md`、`crates/*/README.md`、
  `scripts/`、`Cargo.toml`、CI。
- **C7 公开面 runtime-free 门禁扫全部 crate**：`scripts/check-public-api-runtime.sh` 的扫描式是
  `find crates -name '*.rs' -path '*/src/*'`，豁免只有 `uva-app-server` 与
  `uva-mcp-server`。新 crate 的 `pub` 面出现 `tokio::…` 路径 → `make ci` 红。
  （`uva-tui` 是正例：它的 lib 面刻意不带 tokio 类型，`CoreWriter` / `CoreReader` 是
  `pub(crate)`。）
- **C8 `make ci` 不含 `check-web`**：前端门禁是独立 CI job（`rust-quality.yml` 的 web job）。
- **C9 前端已有动态 import**：`App.tsx:20-61` 有 19 处 `React.lazy(() => import(...))`
  路由分包，`Markdown.tsx:24-32` 等另有 4 处。任何"前端没有动态 import"的断言都不成立。
- **C10 `cargo deny` 只覆盖 root workspace**：桌面壳的依赖树在 root workspace 之外，
  许可证 / 来源策略需要单独审（D10）。
- **C11 WS 客户端依赖已在 lockfile 里**：`tokio-tungstenite 0.29.0` 已随 `axum` 的 `ws` feature
  进入 `Cargo.lock`（`Cargo.lock:4462`）⇒ 直接依赖它**不引入新下载、不产生版本冲突**，
  且与 `Cargo.toml:170` 的 `axum = { version = "0.8", features = ["ws"] }` 同源。
- **C12 TUI 今天的传输是自建的一整套**：`crates/uva-tui/src/transport.rs`（726 行）
  自己 spawn `core`、自己分帧（消费 `uva_app_server::rpc::framing`）、自己做 id 关联、
  自己管 1s→5s 退避与"新一代 `ready` 后重发 `sessions/follow`"；
  `app.rs`（884 行 reducer）与 `render.rs`（ratatui 绘制）与传输无关，**不需要动**。

## 4. 现状与差距（2026-11-09 侦察事实）

**TUI 侧**

- 三层：`transport`（进程 + 分帧 + id 配对 + 重启）、`app`（纯 reducer：
  `App::apply(TransportEvent) -> Vec<Action>`）、`render`（`render::draw` 只读 `App`，
  测试用 `TestBackend` 断言缓冲区文本）。
- 键位只有三组（`Enter` 发送、`y`/`n` 审批、`Ctrl-C`/`Esc` 退出），覆盖面比 SPA 窄得多
  （SPA 有会话列表、工具、技能、计划、Agent、任务、工作流、评测等页面）。
- 它 spawn 的是 `core`（stdio），**不经过网关**；因此 core 的监管者有两个（TUI 一个、网关一个），
  协议契约有两份实现（TUI 的 stdio 分帧一份、网关的 WS 中继一份）。
- `crates/uva-tui` 是 workspace 成员、`publish = false`、bin + lib；
  `transport::Driver::connect` 是 `pub(crate)` 的可注入 spawn 缝（生产 `spawn_process`，
  测试用 `tokio::io::duplex` 假装 core）。

**协议词表（三个宿主共用，本轮"等价"的度量单位）**

- 服务器的方法注册表是**唯一真源**：`crates/uva-app-server/src/rpc/methods/**`
  逐个 `router.register("<域>/<动作>", …)`。SPA 的 `api/client.ts` 把 REST 形状的路径映射到
  这些方法（`client.ts:222` 一行一个），**54 个方法、13 个域**：
  `sessions`(10) / `workflows`(6) / `schedules`(6) / `skills`(5) / `evals`(5) / `chat`(5) /
  `agents`(5) / `tools`(4) / `tasks`(2) / `models`(1) / `plugins`(1) / `search`(1) / `ui`(1)。
- 命名高度规整：`<域>/list` / `<域>/get` / `<域>/create` / `<域>/delete` + 少量动词
  （`sessions/fork`、`chat/regenerate`、`schedules/tick`、`workflows/run`、`evals/run`…）。
- **列表响应是统一信封**：`List<T> { data: Vec<T>, next_cursor: Option<String>, total: Option<u64> }`
  （`crates/uva-app-server/src/api/v1/list.rs:27`），所有 v1 列表端点共用
  （cursor 编解码走 `uva::core::cursor`）。
  ⇒ **一个通用列表渲染器 + 一张每域列配置**就能吃掉全部目录域，不必逐页移植（见 D16）。

**Desktop 侧**

- `serve` 的 CLI：`--bind`（默认 127.0.0.1）、`--port`（默认 8080）、`--web-dir`，
  以及全局的 `--directory` / `--profile` / `--bundle` / `--patch`
  （`crates/uva-app-server/src/main.rs:126` 起、`:190-201` 组装 `ServeOptions`）。
- **没有**任何 `/api/v1/*` HTTP 面：`ui/schema` 是 WebSocket 上的 JSON-RPC 方法
  （`src/rpc/methods/misc.rs:40`），前端的 `api/client.ts:222` 把 `'GET /api/v1/ui/schema'`
  映射到该方法。`src/routes/**` 与 `src/api/**` 是方法体，不是 HTTP 路由。
- `serve` 是同一个二进制的**子命令**，不是独立可执行文件（`main.rs:112`）。
- 仓库里**没有** Tauri / wry / tao 的任何痕迹（`grep -E '\btauri\b|\bwry\b|\btao\b' crates/ uva-web/ Cargo.toml` 无命中）。

**浏览器插件树侧**

- 服务器：`[ui]` 段 → `ui/schema.rs::load_ui_manifest`，每行必须 `ui.` 前缀 +
  `ui-bundle` runtime + `config = { version, components: [{ slot, kind, props }] }`；
  顶层出现可执行字段（`script` / `stylesheet` / `javascript` / `css`）直接
  `UiError::ExecutablePayload` 拒绝。
- 前端：`api/ui-schema.ts` 取一次，`components/ui-schema/registry.ts` 的 kind 表只有
  `panel` / `list` / `key-value`，未知 kind 走 `FALLBACK_KIND` 占位；
  `UiSchemaPanel` **已经**接受可选 `slot` prop 并按 slot 过滤
  （`UiSchemaPanel.tsx:18`、`:26`），但调用点 `SessionDetailPage.tsx:545` 是
  `<UiSchemaPanel />`（不传 slot ⇒ 不过滤），且**每个实例各取一次** schema
  （`useEffect` 里的 `uiSchemaApi.get`）。
- `chat.sidebar` 这个 slot id 已被服务器侧的示例与测试占用
  （`src/routes/ui.rs:67,80`、`src/ui/schema.rs:95,98`）；前端对应的是会话侧栏
  `components/chat/SessionSidePanel.tsx` 的 `<aside className="nt-chat-panel">`
  （不是主导航 `components/layout/Sidebar.tsx`）。
- **没有**任何插件清单 / 微内核 / 插槽注册表的痕迹（动态 import 只有 C9 说的路由分包）。

**spec 侧**

- 本地 `openspec/specs/` 现有 10 份能力 spec（`web-projection` / `plugin-kernel` /
  `plugin-runtime` / `capability-plugins` / `builtin-plugin-set` / `agent-loop-plugin` /
  `profile-assembly` / `headless-rpc` / `wasm-plugin-runtime` / `performance-budget`）。
- `web-projection/spec.md` 的《Requirement: 不引入 TUI 与 Desktop》有**两条**禁令：
  (a) "本变更 MUST NOT 新增 TUI 或 Desktop 入口"；(b) "`uva-app-server` 与
  `uva-web` 之外不得出现表现层 crate"。两条都已被 `crates/uva-tui` 违反。
- **入库文档没有同类矛盾**：`AGENTS.md` / `SEAMS.md` / `README.md` / `MINIMAL.md` 里
  没有"无 TUI / 无 Desktop"的陈述（`README.md:527` 只把 "a TUI / desktop shell" 列为
  可能的宿主）。所以刀 0 的入库部分只是把新形态写进去，不需要"纠错"。

## 5. 方案取舍

### 5.1 桌面链路

| 方案 | 结论 | 理由 |
|---|---|---|
| **R1 复用 serve 网关做桌面 sidecar** | **选定** | 前端零改动（`serve` 与 SPA 同源，WS 天然同源；dev 侧 Vite 已代理 `/rpc` ws 与 `/readyz`）；core 仍是唯一真相，网关只按 id 转帧，A4/A5 不变；复用现成的 readyz / 退避重启 / 静态文件服务 |
| R2 壳直连 core 的 stdio JSON-RPC（Axiom 原味） | 落选 | 前端要抽 transport 层并背第二套传输（C3），代码量与故障面大一档；换来的只是"壳自己握着 core 帧"这一形式 |
| R3 不做原生壳 | 落选 | 用户 2026-11-09 明确要 Tauri |

R1 与 Axiom §7.4 的偏离（"Tauri IPC ↔ stdio JSON-RPC"）按迁移设计 D1 的先例处理：
**以代码事实为准，偏离写进本设计与裁定记录**。Axiom 文档是参考，不是合同。

### 5.2 TUI 并轨路线（用户 2026-11-09 选定）

| 路线 | 结论 | 理由 |
|---|---|---|
| **T1 保留终端原生 UI，传输并轨到网关** | **选定** | 三个宿主共用同一个网关、同一个 core 监管者、同一份协议契约；终端原生体验（SSH、无图形环境）保住；顺带得到 `--attach`（挂到已在跑的服务器，浏览器与终端同服） |
| T2 TUI 降级为"统一前端的启动器" | 落选 | 终端用户需要浏览器，SSH 体验退化；删掉约 1500 行已工作的终端代码 |
| T3 删掉 TUI | 落选 | 丢掉「SSH 里就有终端原生 Agent UI」这个已有能力 |

T1 与 Axiom §7.2 的偏离（Axiom 写的是"TUI 以 stdio JSON-RPC 直连 core"）同样按代码事实为准：
本轮的核心诉求是**统一协议面与统一 core 监管**，直连 core 恰好是二者分叉的根源。

**用户追加的约束（2026-11-09）**：三个宿主要**等价实现**，不是"web 完整 + TUI 精简"。
落法（用户选定路线 a）：**能力矩阵 + 通用渲染器增量补齐**——

- "等价"的度量单位是**服务器方法词表**（13 个域 / 54 个方法），不是"页数"。
- TUI 的补齐走**通用列表 / 通用详情 + 每域一张列配置表**（数据，不是 19 套界面），
  借 `List<T>` 统一信封把目录域一次吃掉。
- "等价"由一份 tracked 的**能力矩阵**度量并被 CI 棘轮（见 D15 / D17）。

## 6. 目标架构（终态）

```
┌────────────────────────────────────────────────────────────────────┐
│ 三个宿主（各自独立编译、独立部署）                                 │
│  crates/uva-tui   ratatui + crossterm（终端原生 UI 保留）       │
│  uva-web + 浏览器  React SPA（桌面与浏览器同一份 dist）        │
│  desktop/             Tauri 2 壳 + WebView（同一份 dist）           │
└───────────────┬────────────────────────────────────────────────────┘
                │ 同一个协议面：JSON-RPC 2.0 信封
                │ （浏览器/桌面走 WS，终端走 WS —— 同一个 /rpc 端点）
┌───────────────▼────────────────────────────────────────────────────┐
│ serve 网关（既有，本轮不改它的转发语义）                          │
│  /rpc /livez /readyz /version /metrics + SPA fallback               │
│  监管 core（1s→5s 退避）、把帧按 id 中继给 core                    │
└───────────────┬────────────────────────────────────────────────────┘
                │ Content-Length 分帧的 stdio
┌───────────────▼────────────────────────────────────────────────────┐
│ core（无头）— 本轮一行不动                                        │
│  kernel 脊柱 / native 插件 / wasm 通道 / profile HMR                │
└────────────────────────────────────────────────────────────────────┘

共享 crate（root workspace 成员，publish = false）
  crates/uva-gateway
    ├─ supervisor.rs  拉起 / 附着网关、等 /readyz、退避重启、退出回收
    └─ client.rs      网关 WS 上的 JSON-RPC 客户端（请求 / 通知 / 重连）
        ↑ 被 crates/uva-tui 与 desktop/ 壳共用

能力矩阵（tracked，CI 棘轮）——"三个等价"的度量与守门
  docs/host-capabilities.yaml
    行 = 服务器方法词表的 13 个能力域；列 = tui / web / desktop
    格 = full · adapted · n-a(附理由) · planned(附锚点)
  规则：每行三列必有值 · planned 必须带锚点 · full/adapted 必须带存在的证据文件
        缺口数只降不升 · 引用的方法名必须在服务器注册表里存在

三个宿主的界面
  crates/uva-tui   通用列表 / 通用详情 / 对话 / 审批（+ 每域列配置表）
  uva-web + 浏览器  React SPA（19 页）+ microkernel（纯 TS）
  desktop/             Tauri 2 壳 + WebView（同一份 dist ⇒ 与浏览器天然等价）
    microkernel/：slots.ts · registry.ts · container.ts
    SlotRegion（React 纯投影，useSyncExternalStore 订阅不可变快照）
    插槽实例：session.detail.extensions · chat.sidebar · …
```

## 6.1 共享界面契约

三个宿主是**同一套界面**的两种渲染：web 与 desktop 是同一份 SPA，终端是它在终端里的渲染。
下面是这套界面的骨架（原型：`docs/prototypes/shared-interface.prototype.html`，
由 `crates/uva-tui/examples/tui-layouts-prototype.rs` 生成，终端那半是真 ratatui 输出）。

**外壳（两个宿主一致）**

| 区域 | 内容 |
|---|---|
| 侧栏 | 品牌 + `HARNESS` 徽标 · **新会话** · **工作区**（树：工作区 → 会话；组标题旁一个跨域的 🔍）· 左下角用户（**设置入口**）· **可折叠成图标栏**（`Ctrl/Cmd+B`，与 SPA 现有 rail 同义） |
| 顶栏 | 面包屑 + 服务器状态徽标 + 命令面板提示 |
| 主区 | 对话优先：标题栏 · 对话流 · 待办进度 · prompt 建议 · 输入框 |

**对话流**

| 元素 | 契约 |
|---|---|
| 用户消息 | **右对齐的气泡**（终端用底色块 + 右对齐实现） |
| 思考块 | 输出中**展开**（边流边滚）；输出完成**自动折叠**成一行 `已思考 >` |
| 助手回答 | markdown（粗体引导词 + `——` + 说明的编号列表是本仓自己的风格） |

**输入区（自上而下）**：待办进度（`n/m 完成` + 逐项 ✔/▸/·；**没有 todo 时不占位置**）→
输入框。**输入框用 bash 的 prompt 修饰**（`❯ <工作区路径> `，可带上下文标记），
**不做** prompt 建议条（D31，推迟）。输入框底部一行：`+` / 工作区范围 / 模型 / 发送。

**快捷键（Vim 优先，VSCode 次之，D30）**

| 场景 | 键 |
|---|---|
| 列表 | `j/k` 移动 · `gg`/`G` 首尾 · `Ctrl+d`/`Ctrl+u` 半页 · `/` 搜索 · `n`/`N` 下一个/上一个匹配 |
| 打开 | `Enter` 打开 · `y` 复制 id · `x`（`dd`）删除条目 · `Esc` 取消 |
| 全局 | `:` 命令面板 · `?` 快捷键帮助 · `Ctrl+b` 折叠侧栏 · `Ctrl+k` 命令面板 |
| 对话 | 思考块输出中展开、完成后自动折叠（D31 之外的行为约束） |

**工作区**：默认工作区**常驻**（不建工作区也能对话，会话落在默认栏里，D35）。

**设置面**（从左下角用户进入；侧栏**整个变成设置导航**：用户卡 → `Ctrl+F 搜索` → 分区列表）

设置页的内容是**解析后的 profile 的反推**（D26）：它显示的就是 core 将要装配的那棵树；
每行带**启用开关**，界面只改**启用与参数**，**没有 settings 文件**（D20）；
创建 / 删除能力**走内置 MCP 通道**（D28），界面不直接写文件。

| 分区 | 内容来源（装配进 profile 的行） | 界面职责 |
|---|---|---|
| 账户 | — （本机） | 用户 / 订阅 / 凭据 |
| 通用 | — （壳自己的 UI 行） | 主题 / 密度 / 字号 / 快捷键：记在 profile 里壳那一行的 config |
| **插件** | `.agents/plugins` | 插件行的启用与参数（入口从对话界面搬到这里） |
| 模型 | `model-adapter` 行 config → `.agents/config.toml` 的 `[providers]` → 环境变量（**无 `.agents/providers` 目录**，2026-11-10 对账） | provider 行的启用 |
| 智能体 | 内置 + `.agents/agents` | **内置默认 agent**（整个 harness 的运转方式，不可停用）+ 附加 agent 行 |
| 技能与命令 | `.agents/skills`（命令**无独立数据源**：技能的 slash-command 扩展位） | skill 行的启用（正文在 plugin 排布里） |
| MCP | `mcp.json` | server 行的启用 |
| Hooks | plugin 排布（**无目录约定**：hook 是挂载的插件行在 harness `Host` 上的注册） | hook 行的启用 + 运行方式 + 运行日志（**不从别处导入配置**） |
| 自动化 | plugin 排布（模板**现状无数据源**，空态） | 自动化 / 执行历史（标题 + `[手动新建] [在对话中创建]` + 三标签 + 空态） |

> 域清单与每域四要素（落点 / 数据源 / 归属 / 缺行行为）以
> [`docs/host-capabilities.md`](../host-capabilities.md) 为唯一真源——它是从 profile dump 反推
> 并三面核对过的；本表是它的摘要。

**UI 树（两个宿主共用，D25）**

`ui-bundle` 行声明**插槽**（`chat.sidebar`、`session.detail.extensions` …）；
**插槽 id 与宿主无关**：浏览器渲染成 DOM、终端渲染成 ratatui。UI 装载器给**两个宿主**都装
这棵树 ⇒ **终端也有一棵 UI 树**，因此 TUI 可扩展（新增一个 UI 插件 = 一行 profile + 一份
插件数据，两个宿主同时拿到）。

**能力域 → 落点**（13 个域全覆盖）

| 落点 | 域 |
|---|---|
| 主界面（对话） | 会话 · 对话 |
| 设置 · 插件 / 模型 / 智能体 / 技能与命令 / MCP / Hooks / 自动化 | 插件目录 · 模型 · Agent · 技能 · 命令 · MCP · Hooks · 计划 · 任务 · 工作流 · 评测 |
| 工作区组旁的 🔍 | 搜索 |
| UI 树（插槽） | UI 扩展：`ui-bundle` 行声明插槽，**两个宿主共用插槽 id**（D25） |

## 7. 关键决策（我的裁量，可推翻）

- **D1 桌面链路 = `serve` sidecar。** Tauri 壳 spawn
  `uva-app-server serve --bind 127.0.0.1 --port <port> --web-dir <dist>`，
  等 `/readyz` 200（该 200 的语义就是"core 已宣布 ready"）后让 WebView 打开
  `http://127.0.0.1:<port>/`。壳**不**讲 JSON-RPC——协议由网关讲，壳只管生命周期。
- **D2 一套前端，两个宿主，且刀 3 完全不碰前端。** 生产：WebView 与浏览器都加载 sidecar
  提供的同一份 `dist`，页面 origin 与 WS origin 同源；开发：Tauri dev 的 WebView 指向
  `http://localhost:5173`，Vite 已经代理 `/rpc`（ws）与 `/readyz`
  （`uva-web/vite.config.ts:16-30`），前端**零改动**即可连通。
  因此**不需要**运行时端点覆盖缝（`window.__UVA_RPC_URL__` 一类）——那是多余的第二条真源。
- **D3 代码位置三段。**
  - `crates/uva-gateway/`：**共享 crate**，root workspace 成员、`publish = false`、
    零 GUI 依赖，只装"拉起 / 附着网关 + 等就绪 + 退避重启 + 回收"与"网关 WS 上的
    JSON-RPC 客户端"。TUI 与桌面壳共用它（R4）。
  - `crates/uva-tui/`：既有成员，**传输层换掉**（D13），reducer 与渲染层不动。
  - `desktop/`：Tauri 壳，**自带 `[workspace]`、不进 root workspace**，配独立 CI job 装
    webkit 依赖后跑 fmt / clippy / test。理由是 C2：留在 root workspace 里，
    `--all-features` 会把 GUI 目标编进 `make ci` ⇒ **每个**跑 `make ci` 的人都要先装
    webkit 系统库（本机已装，见 C1；但这是贡献者的普遍负担，不该由架构买单）。
    `desktop/` 以 path 依赖 `../crates/uva-gateway`（跨 workspace 的 path 依赖：两份
    lockfile，两套 feature 解析——已知代价，见 §10 R-6）。
- **D4 端口由宿主选。** 用 `TcpListener::bind("127.0.0.1:0")` 取一个空闲端口再传给
  `serve`（bind → 读端口 → 关闭 → 使用，竞态窗口极小）；`serve` 的 `--port` 语义不变。
  不解析网关日志取端口（fragile），不改网关的绑定契约。
- **D5 插槽的单一真源在前端。** **〔已作废，见 D33：不再有插槽系统〕** SPA（壳）声明插槽区域与基数；服务器只投递"某插槽里放什么"
  的内容；向**未声明**插槽投递 = 拒绝并记录（D9 的记录方式），不静默丢弃。
  **基数词表 = `one` | `many`**（刻意不叫 `list`：渲染器 kind 里已经有 `list`，两个轴撞名
  会读错）。v1 不引入 `keyed`（wire 上 `UiComponent` 只有 `slot` / `kind` / `props`，
  没有条目键；引入 `keyed` 就要改 wire，而 D5 承诺不改）。
- **D6 2b 默认关，且开关在**行级**。** **〔已作废，见 D33：不再有同源 JS 执行〕** `[ui]` 段的 wire 结构是
  `WireSection { entries }`（`src/profile/model.rs:231`）**没有** `deny_unknown_fields`
  ⇒ 写 `[ui] allow_code = true` 会被 serde **静默忽略**；而段级开关要改 `Profile`
  结构体，会打断 4+ 处结构体字面量与 `dump_profile` 的 golden。
  故开关落在**每一行**的 `config.allow_code`（复用 `Entry.config`，不改任何结构体）：
  **每行显式开启**，比"一个部署级总开关"更保守。未开启的行携带 `module` 字段时，
  仍按今天的 `UiError::ExecutablePayload` 拒绝。
- **D7 前端不建测试层。** AGENTS.md §4.2 的家规是"不保留任何前端自动化测试套件"，
  且 §4.2 已经给出纯函数的验证方式（页面内 `await import('/src/lib/foo.ts')` 断言）。
  微内核是纯 TS、不 import React，正好落在这条路上。C4 的三条（Node 20 / `tsc --noEmit`
  的 `include` / 无 `@types/node`）也各自否掉了"加测试文件"的每条路。
  验收改为**Playwright MCP 驱动的页面内断言清单**（见 §8 刀 7）。
- **D8 spec 收口落两处。** 本地 openspec 层（`web-projection` 的**两条**禁令都改，新增
  `desktop-projection` / `frontend-plugin-tree`）+ 入库文档（本篇、AGENTS.md、CHANGELOG、
  各 crate README）。
- **D9 微内核的形状与生命周期。** **〔已作废，见 D33：不再有前端微内核〕** 应用级单例（`src/microkernel/index.ts` 导出一个
  store），插件在应用启动、拿到清单后实例化，dispose 只在应用卸载时发生（页面级路由切换
  不销毁微内核——否则插槽内容在切页后消失）。插槽条目有**两类**且共存于同一个注册表：
  - `schema` 类：服务器通过 `ui/schema` 投递（`kind` 选渲染器）；
  - `code` 类：插件的 `setup(ctx)` 注册（刀 5 才有）。
  冲突规则：`one` 槽位第二个注册被拒并记录；`many` 槽位按注册顺序共存，
  `schema` 类在前、`code` 类在后。拒绝记录走 `console.warn`（前端没有别的日志通道，
  不为此新建一套）。
- **D10 桌面壳的二进制定位与生命周期参数。**
  - 二进制：优先用 Tauri 打包的 sidecar 资源（`externalBin`，文件名带 target triple）；
    其次 `--app-server` / `UVA_APP_SERVER` / PATH（照 TUI 先例
    `crates/uva-tui/src/main.rs:113`）；最后 `current_exe()` 同目录兜底。
  - 默认工作区：进程的当前目录（与 TUI 一致）；profile / bundle / patch 从命令行透传给
    `serve`。
  - 就绪超时：30s 内 `/readyz` 未 200 → 判定启动失败并报错（不再无限等）。
  - 重启退避：壳对网关用 1s→5s（与网关对 core 的退避同形）。两层退避是叠加的，
    语义上可接受（壳只负责把网关拉回来）。
  - 端口策略：重启时**优先复用原端口**；bind 失败（端口被占）则重选并让 WebView 重新导航。
    判据是子进程快速退出 + stderr 里的 bind 失败信息；判定不了就当作"启动失败"重试一次。
- **D11 CSP 不在本轮。** **〔已作废，见 D33：不再加载远端 JS〕** 仓库今天**没有任何 CSP**（`grep Content-Security-Policy` 在
  `crates/uva-app-server/src` 与 `uva-web/index.html` 均零命中），所以 D2 的
  "不需要 `unsafe-eval`"是"没有 CSP 可放宽"，不是"有 CSP 且不需放宽"。
  本轮不引入 CSP（会动静态响应面、可能影响现有前端），**记录为已知缺口**：
  2b 打开后没有 CSP 背书，靠逐行开关 + 同源约束兜底。补 CSP 另立一轮。
- **D12 `crates/uva-gateway` 的形态。** **lib**（不是 bin）：公开面 futures-based
  （`futures::future::BoxFuture`、`futures::channel::mpsc`），**不带任何 `tokio::` 路径**
  （C7），内部用 tokio（照 `uva-tui` 的分层：内部 tokio、公开面干净）。
  它必须同时进 root `Cargo.toml` 的 `members` 与 `scripts/check-publishable.sh` 的 `ORDER`
  （后者要求与成员集合**完全一致**）。
- **D13 TUI 并轨到网关。** `crates/uva-tui` 的 `transport` 层换成
  `crates/uva-gateway` 的 client：
  - **默认**：TUI 自己 spawn `uva-app-server serve`（保持"一条命令"的体验），
    连 `ws://127.0.0.1:<port>/rpc`；
  - **`--attach <url>`**：附着到已在跑的网关（浏览器与终端同服），不 spawn、不监管进程。
  - 协议面不变：TUI 今天用的方法名（`chat/session/create`、`sessions/follow`、
    `chat/message/send`、`approval/respond`、`chat/cancel`、`session/unfollow`、
    `sessions/events`）与信封形状都不变，只是从 stdio 帧搬到 WS 文本帧（C5 的透明中继）。
  - **被删掉的逻辑**：自造的分帧（`rpc::framing` 的消费）、自己管 core 的 1s→5s 退避重启、
    "新一代 core 就绪"的重启编排——core 的监管归网关。
  - **保留的逻辑**：`App::apply` reducer、ratatui 渲染、键位、以及"收到 `ready` 就重发
    `sessions/follow`"——因为网关重启 core 时会丢弃订阅并广播 `ready`
    （`gateway.rs` 的 `drop_subscriptions` / `broadcast`），客户端必须自己重建订阅。
    这条从"我重启了我的子进程"变成"网关换了代"，触发源变了，行为不变。
  - **依赖变化**：`tokio-tungstenite 0.29`（已在 lockfile，C11）；对
    `uva-app-server` 的**库**依赖可以摘掉（不再消费 `rpc::framing`）——TUI 需要的是
    server **二进制**，按今天 README 的方式提供（先构建 / PATH 查找）。
  - **测试形态**：传输测试从"duplex 假装 core"改成"进程内 axum WS 服务器说 JSON-RPC 信封"
    （axum 已在 workspace，C11）+ supervisor 的可注入 spawn 缝（假 spawn，不起真进程）。
    reducer 与渲染测试不动。
- **D14 共享 crate 的边界。** `crates/uva-gateway` 提供两件东西：
  `supervisor`（进程）与 `client`（协议）。桌面壳只需要 `supervisor` 的
  `base_url()`，TUI 两者都要。壳**不**用 `client`（它不碰协议）——但 crate 里留着，
  因为"下一个宿主"多半需要它；不用就不引依赖（Tauri 壳不引 `tokio-tungstenite`）。
- **D15 能力矩阵：一份 tracked 的数据 + 一次性核对（非 CI 棘轮，见 D27）。**
  - 文件：`docs/host-capabilities.yaml`（入库）；**行从解析后的 profile 推导**（D27），
    不手工维护；本轮**核对一次**并把结果记进文档，**不做成 CI 棘轮**。
  - 行 = 服务器方法词表的 13 个域（`sessions` / `chat` / `agents` / `skills` / `tools` /
    `tasks` / `schedules` / `workflows` / `evals` / `models` / `plugins` / `search` / `ui`）；
    列 = `tui` / `web` / `desktop`。
  - 格的取值：`full`（能力完整且呈现等价）/ `adapted`（能力可达，呈现按介质调整）/
    `n-a`（该介质不适用，**必须写理由**）/ `planned`（未实现，**必须带锚点**：issue 号或
    代码里的 TODO 锚点）。
  - 门禁规则：①每行三列都有值（不许留空）；②`planned` 必须带锚点；
    ③`full` / `adapted` 必须指向**磁盘上存在**的证据文件（测试或文档）；
    ④缺口数（`planned` + `n-a` 的格子）不得超过基线；⑤矩阵里引用的方法名必须在
    服务器的方法注册表里存在（`grep 'router.register("' crates/uva-app-server/src/rpc/methods/`）。
  - 配套的「新增 `scripts/*.sh` 门禁要同步 `make lint-sh` 与 shellcheck workflow」
    （AGENTS.md §3.6）在本轮**不触发**——本轮没有新增门禁脚本。
- **D16 TUI 补齐走通用渲染器，不逐页移植。** TUI 侧新增：
  - `views/list.rs`：**通用列表**——吃 `List<T>` 信封（`data` + `next_cursor`），
    按"每域列配置"渲染成表格，支持翻页（cursor）。
  - `views/detail.rs`：**通用详情**——吃单对象，渲染成键值表。
  - `views/chat.rs` / `views/approval.rs`：已有的对话与审批面（保留）。
  - `domains.rs`：**每域一张列配置表**（数据，不是代码）：域 → list 方法、item 字段路径、
    显示列、detail 方法。目录域（agents / skills / tools / tasks / schedules / workflows /
    evals）因此只加一张表，不加界面。
  - 导航：一棵域树（13 个域）+ 每个域的 list / detail 两级视图 + 现有的对话视图。
- **D17 "等价"的定义与允许的终态。** 等价 = **同一套核心能力在三个宿主上都可达**，
  不是像素一致。`adapted` 是合法终态（例如详情在终端是键值表、在浏览器是卡片）；
  `n-a` 只允许用于介质本身不适用的能力，且必须写理由；`planned` 必须带锚点。
  **不允许**的终态：无锚点的 `planned`、无证据的 `full`、缺口数回升。
- **D18 不共享呈现代码，只共享契约。** 用户 2026-11-09 明确：**不做"把领域投影上移到
  服务器"那一刀**（曾提议把会话折叠上移，让 SPA 与 TUI 共用一份 Rust 投影），也**不引入
  跨语言共享运行时**（TUI 内嵌 JS 引擎执行前端模块那一类）。
  - 共享的止于三样：**JSON-RPC 方法词表**、**`List<T>` 统一信封**、**能力矩阵**。
  - 每个宿主用**自己的语言与渲染栈**实现同一份契约：SPA 是 DOM/React，TUI 是 ratatui，
    桌面是 WebView 里的同一份 SPA。
  - **接受的代价（明写）**：会话行词汇的解释会有两份——`uva-web/src/lib/session-to-messages.ts`
    （TS，1244 行）与 TUI 的 Rust reducer（`crates/uva-tui/src/app.rs`，884 行）。
    这是**有意的重复**：换来的是每个宿主对自己的呈现完全自主（终端不需要为了复用去迁就
    DOM 抽象），且**架构保持简洁**——共享层只有协议与矩阵，没有共享呈现层这个新概念。
  - 字段级漂移（服务器改了某域的字段、某个宿主没跟上）**不被机制拦住**，只靠 wave 的
    手工冒烟与矩阵里的证据指针发现。能力级的漂移（少了一个能力）由矩阵棘轮拦住。
- **D19 profile 是最终产物，plugin 排布是它的装配输入。** core 是无头 harness，它**只认解析
  后的 profile**——那就是插件树的组装入参。所以"能力"与"界面效果"最终**都是 profile 的行**
  （能力行 / `ui-bundle` 行）；**plugin 排布**（`.agents` 布局 / `mcp.json`）是**读取文件目录的
  手段与约定**，用它识别各类能力**数据**（mcp / agent / skill / tool / hooks / 自动化），
  装配仍以 profile 为基础（见 D29 的术语统一）。
  **一行 = 一个插件的内容**（对齐 DSH：profile 行就是插件 + 它的配置）：
  - 行里放**小配置**（插件自己的少量参数）；
  - **大配置不进 profile**，由**插件自己管理**（自己的文件 / 自己的状态）——
    DSH 的同一做法：settings 是一个**插件提供的服务**，不是一个文件概念；
  - **插件按约定自己读 plugin 排布**：skill 插件读 `skills/`（怎么读由它自己实现），
    mcp 插件读 `mcp.json`，依此类推——**profile 不复制这些数据**。
  **uva 现有的 `{ id, runtime, config }` 行已经是这个形状**，本轮改的是语义而非格式。
- **D20 没有 settings 一说。** 启用与参数**没有独立的设置面**：每行自带启用位与该插件的
  **小配置**；**大配置与细粒度启用归插件自己管**——
  例如"哪些 skill 启用"由 **skill 插件自己实现并标识**（它自己的数据/状态），
  **不进 profile**（否则 profile 会被撑爆）。
  profile 的**粗粒度**启用位（这一行插件装不装）**直接决定插件树按需装载什么**：
  关掉的行不进 Fiber 树 ⇒ **能力与界面同时消失**（D32）。
  `settings.yaml` 这个文件**作废**（原 D20 / D28 一并作废）。
- **D21 动态变更走 MCP 通道，且是事务性的。** 内置 MCP（暂名 `uva-settings`）暴露
  `profile/list` · `profile/get` · `profile/set`（以及创建/删除能力行的动作）：
  1. 先按新 profile **试装**（`mount_all` 本来就是全组前置校验 + 失败全组回滚）；
  2. **装得起来** ⇒ 提交（写 profile、刷新界面），并广播"目录变了"；
  3. **装不起来** ⇒ **报错并保持变更前的状态**（不写 profile、不动已挂载的树）。
  写入是**两阶段**的：先落临时文件 → 试装 → 成功才替换正式文件。
- **D22 内置默认 agent 是整个 harness 的运转方式。** 有一个**内置、默认、不可停用**的 agent
  定义（`agent-loop` 插件的默认 agent），整个 agent harness 靠它运转；plugin 排布里的 agent
  是**附加项**，由 profile 行的 `enabled` 控制。
- **D23 插件入口从对话界面搬进设置页。** 对话界面的侧栏只剩：新会话 · 工作区树 · 搜索 ·
  用户（设置入口）。「插件」与「自动化」是设置分区。
- **D24 设置页不做「规则与记忆」；Hooks 不从别处导入。** steering / memory 能力保留在核心里，
  但**不进设置面**；Hooks 的定义只来自 plugin 排布，不提供"导入别处配置"的通道。
- **D25 终端与浏览器各有一棵 UI 树，由 UI 装载器负责。**（原 D33 反转）
  `ui-bundle` 行声明**插槽**（`chat.sidebar` 之类）；**插槽 id 与宿主无关**，浏览器渲染成 DOM、
  终端渲染成 ratatui。UI 装载器给**两个宿主**都装这棵树 ⇒ **TUI 因此可扩展**
  （新增一个 UI 插件 = 一行 profile + 一份插件数据，两个宿主同时拿到）。
  **UI 树不是浏览器专属**——它属于"投影"这一侧，跨宿主共享同一份插槽契约。
- **D26 界面是 profile 的反推。** 设置页按域分组显示**解析后的 profile 行**与其 `enabled`，
  而不是另建一份"设置真相"——界面与 core 看到的东西**天然一致**。
- **D27 能力矩阵从 profile 推导，不手工维护。** 矩阵的**行 = 解析后的 profile 的行**
  （按域分组只是展示方式），列 = 宿主可达性。这样矩阵不会变成"第二份真相"。
  **门禁降级**：矩阵是**文档 + 一次性核对**，不做成 CI 棘轮。
- **D28 能力目录可以创建 / 删除，但走内置 MCP 通道下发。** 界面不直接写文件：
  "创建能力"与"删除能力"都是一次 MCP 调用（MCP 写 plugin 排布的数据文件 + 写 profile 行，
  再试装）。
- **D29 术语统一：插件 = cordis 插件；plugin 排布 = 读目录的手段。**
  - **插件**（cordis plugin）= 插件树里的单元：核心树的 Fiber，或 UI 树的 UI 插件。
  - **plugin 排布** = `.agents` 目录约定（读文件、识别数据），**不再承载"插件"概念**。
  - **能力数据** = 排布里识别出来的 mcp / agent / skill / tool / hooks / 自动化定义。
- **D30 快捷键用 Vim 优先（VSCode 次之）。** 列表视图：`j/k` 移动、`h/l` 左右、`gg`/`G` 首尾、
  `Ctrl+d`/`Ctrl+u` 半页、`/` 搜索、`n`/`N` 下一个/上一个匹配、`:` 命令面板、`?` 快捷键帮助、
  `Esc` 取消、`x`（或 `dd`）删除条目、`Enter` 打开、`y` 复制 id、`Ctrl+b` 折叠侧栏。
- **D31 输入框按 bash 的 prompt 修饰，不要 prompt 建议条。** 输入框显示
  `❯ <工作区路径> ` 这样的提示符（bash 风格），可带上下文标记；**"输入框上方的 prompt 建议"
  本轮不做**（推迟）。
- **D32 "禁用"的语义。** 禁用 = **该行不挂载** = **能力与界面同时消失**（D20 的直接推论）。
- **D33 profile 变更 → 界面刷新。** core 重挂后经既有通知通道（`session/event` 一类）发出
  "目录变了"信号，界面收到即刷新；进行中的 turn 用旧句柄完成（与 profile HMR 语义一致）。
- **D34 旧的 19 个页面删除，以新原型为准。** 新 IA = 对话优先 + 设置分区；旧的表格页
  （工具 / 技能 / Agent / 评测 / 工作流 / 任务 / 计划…）与其路由一并删除。
- **D36 插件可以管理子插件（插件层级）。** 一个**大插件**可以按约定**装载并管理一族小插件**：
  例如 **tool 插件管着 skill 插件**（同族的都这么玩）。子插件由父插件在**装配期**拉起，
  启用与否由**父插件自己实现并标识**（不进 profile），因此 profile 保持小而可读。
  这与 DSH 一致：大插件提供一族能力，小插件只是它的一个实现细节。
- **D37 变更结果四态（对齐 DSH 的 `application`）。** 一次变更的结果是四者之一：
  **applied**（已生效，界面刷新）· **restart-required**（需要重启才生效，界面提示）·
  **overridden**（被更高层覆盖，未生效）· **failed**（装不起来 ⇒ **报错并保持变更前**）。
  界面与 MCP 都按这四态回报，不出现"改了但不知道有没有生效"。
- **D38 族用服务契约表达，不用父子挂载（照 DSH）。** DSH 的插件树是**平的**：
  "All layers are **flattened into one list** and applied once"；"Row order carries no load
  semantics (activation is service-availability driven)"。族靠两件事表达：
  **包 + 子路径**（`name: '@deepseek-ai/dsh-plugin-manager/tools'`）与**服务契约**
  （`inject: ['tools']`，未声明的服务访问被 context 的 Proxy 拒绝 ⇒ 能力安全）。
  ⇒ uva 同款：**tool 插件提供工具注册表服务**，skill 插件 `inject` 它并注册自己的 skill；
  **不需要** kernel 新增"父插件挂子树"，**核心一行不动**成立。
- **D39 启用位可以由插件判定（照 DSH 的 `disabled: !!js …`）。** 行上的启用位不只是
  `true` / `false`，还可以指向**插件提供的判定**（例如
  `enabled = { service = "skill-registry", check = "is_enabled", arg = "foo" }`）。
  这让"哪些 skill 启用由 skill 插件自己实现和标识"**不必把每个 skill 写成 profile 行**。
- **D40 skill 先当"数据"，要成为插件时再显式加行。** DSH 的 skill 是 `SKILL.md` 数据，
  由一个 `dsh-skill-filesystem` 插件扫描目录并注册成 tool。uva 同款：
  skill 插件按约定读 `skills/`、自己判定启用、注册成工具；哪天某个 skill 需要自己的服务与
  生命周期，再**显式加一行**升级成插件（profile 行 = 一个插件 + 小配置）。
- **D35 默认工作区常驻。** 对话的主界面若不设定工作区，会话放在一个**常打开的默认栏**
  （默认工作区）里——不要求用户先建工作区才能用。

## 8. 分阶段落地

**刀序与依赖**（九刀；每刀一次 PR 级收口，`make ci` 全绿）：

```
刀 0 spec 收口 ──┬─▶ 刀 1 共享 crate ──┬─▶ 刀 2 TUI 并轨 ──▶ 刀 4 矩阵 ──▶ 刀 5 wave1 ──▶ 刀 6 wave2
                 │                    │
                 │                    └─▶ 刀 3 Desktop（Tauri 壳）
                 │
                 └─▶ 刀 7 profile 启用位 + 事务性变更 + 内置 MCP ──┬─▶ 刀 8 UI 树与插槽契约
                                                               └─▶ 刀 9 SPA 按新原型重排 IA
```

- 刀 1 是刀 2 / 刀 3 的共同前置；刀 4 是刀 5 / 刀 6 的前置（矩阵先行，缺口才可度量）。
- 刀 3（Desktop）与刀 5 / 刀 6（TUI 补齐）之间无依赖，可并行。
- 刀 7（启用位 + 事务性变更 + 内置 MCP）是刀 8 / 刀 9 的数据源。
- 刀 8 的插槽契约**两个宿主共用**：浏览器与终端各渲染一次（终端因此可扩展）。
- 刀 5 / 刀 6 每完成一波，能力矩阵的缺口数下降一格 → 基线随之下调。

### 刀 0 · spec 收口

**本地 openspec 层（不入库，仓库既定立场）**

- `web-projection` 的《Requirement: 不引入 TUI 与 Desktop》两条禁令都改：
  (a) 改为现状陈述（TUI 已落地且本轮并轨到网关；Desktop 是本轮新增的第三个宿主）；
  (b) 表现层 crate 白名单扩为 `uva-app-server` / `uva-web` / `uva-tui` /
  `crates/uva-gateway` / `desktop`。
- 新增 `desktop-projection` spec：sidecar 契约、生命周期、单一前端、崩溃隔离。
- 新增 `frontend-plugin-tree` spec：插槽系统、dispose 级联、代码执行开关的默认与边界。
- 新增（或并入）一段 TUI 并轨的 Requirement：三个宿主共用网关与协议面、core 监管单一归属。

**入库文档**

- 本篇 `docs/design/2026-11-09-desktop-frontend-tree-design.md`。
- `AGENTS.md` §1（环境段：三个宿主的关系、新增 `crates/uva-gateway`、桌面壳）、
  §3.6（若新增门禁目标，按该节规则同步表格）、§4.2（补一句"纯 TS 微内核同样按页面内
  `import()` 断言验证"，避免下一轮又来一遍同样的争论）。
- `CHANGELOG.md` 的 `[Unreleased]`。
- `crates/uva-gateway/README.md`、`crates/uva-tui/README.md`（改写传输层说明）、
  `crates/uva-desktop/README.md`（若保留该目录）。

**验收**：本地 openspec 层不再有任何一条与代码矛盾的禁令；入库文档说清三个宿主的关系。

### 刀 1 · 共享 crate `crates/uva-gateway`

**范围**

- `SidecarOptions` → 更名 `GatewayOptions { executable, bind, port, directory, profile,
  bundle, patch, web_dir }`。
- `supervisor`：`GatewaySupervisor::start(options) -> BoxFuture<'static, Result<Gateway, GatewayError>>`，
  `base_url() -> &str`，`ws_url() -> &str`，`states() -> futures::channel::mpsc::UnboundedReceiver<GatewayState>`
  （`Starting` / `Ready` / `Restarting` / `Failed`），`shutdown()` 同步发信号；
  spawn 缝可注入（照 TUI 的 `Driver::connect`），测试用假 spawn，**不起真进程**。
- 就绪探针：内部用 `tokio::net::TcpStream` 手写最小 HTTP GET（读 `GET /readyz HTTP/1.1`
  的状态行），**不引 HTTP 客户端**。
- `client`：网关 WS 上的 JSON-RPC 客户端——按 id 关联请求与响应、通知扇出、断线退避重连。
  公开面 futures-based，**不带 `tokio::` 路径**（C7 / D12）。
- 依赖：`tokio`（按需 feature）、`futures`、`serde_json`、`thiserror`、`tokio-tungstenite`（C11）。

**验收**

- `cargo test -p uva-gateway` 绿，且**不起真进程**：假 spawn 覆盖
  "ready 后 resolve" / "网关死亡后按退避重启" / "Drop 回收子进程" / "端口被占时重选"；
  client 侧覆盖"请求按 id 关联" / "通知扇出" / "断线后重连"。
- `make ci` 绿（新成员进 `members` 与 `check-publishable.sh` 的 `ORDER`）。

### 刀 2 · TUI 并轨

**范围**

- `crates/uva-tui/src/transport.rs` 瘦身：换成 `uva-gateway` 的 client +
  supervisor，TUI 只保留自己的领域层（`RequestKind` → 方法名、"收到 `ready` 重发 follow"、
  断线后的 UI 提示）。
- 传输并轨**不动** reducer 与渲染的**既有语义**（C12：今天 `app.rs` / `render.rs` 与传输无关）；
  §6.1 的界面骨架是**新增**视图模块与**新增**状态字段，不改既有行的语义。
- **界面按 §6.1 的契约**：终端侧的侧栏（工作区树 + 可折叠 rail）、对话流（右对齐气泡 +
  思考块两态）、待办进度 + 输入框（bash 的 `❯` 修饰）；设置面（侧栏变设置导航 + 八个分区）。
  现有对话视图保留，**新增**工作区树列与设置导航；**不做** prompt 建议条（D31 推迟）。
- CLI：`--attach <url>`（附着到已在跑的网关）、`--app-server`（server 二进制路径）、
  `--directory`、`--profile` 保持；删掉"直连 core"的任何残留开关。
- 测试：传输测试改用进程内 axum WS 服务器（说 JSON-RPC 信封）；reducer / 渲染测试不动。
- README 改写：说明"三条宿主路径同源"、如何 `--attach`、如何自建 server。

**验收**

- `cargo test -p uva-tui` 绿。
- 手工冒烟：`cargo build -p uva-app-server && UVA_APP_SERVER=target/debug/uva-app-server
  cargo run -p uva-tui -- --directory .` → 起一轮对话、审批用 `y`/`n` 应答；
  再用 `--attach ws://127.0.0.1:8080/rpc` 挂到已在跑的 `serve` 上，行为一致。
- 回归：core 的 stdio 契约仍由 `crates/uva-app-server/tests/stdio_roundtrip.rs` 覆盖
  （它 spawn 二进制并逐帧驱动，不受 TUI 改动影响）。

### 刀 3 · Desktop 入口

**范围**

1. `desktop/`（新，独立 workspace）：Tauri 2 壳。职责只有四件——拉起 sidecar、等 ready、
   开窗（`WebviewUrl::External(base_url)`）、sidecar 死亡则重启并重定向窗口、窗口关闭则
   回收 sidecar。`tauri.conf.json` 的 `frontendDist` 指向 `uva-web/dist` 的副本
   （作为 Tauri resource，同时作为 `--web-dir` 的输入）。
   壳的公开依赖只有 `tauri` + `uva-gateway`（path）+ `tokio` + `serde_json` + `anyhow`；
   **不引 `tokio-tungstenite`**（D14）。
2. CI：新增 `.github/workflows/desktop.yml`（ubuntu-latest，apt 装
   `libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev libsoup-3.0-dev libgtk-3-dev
   librsvg2-dev patchelf`），跑该 workspace 的 fmt / clippy(`-D warnings`) / test，
   **并跑一次 `cargo deny check licenses bans sources`**（补 C10 的缺口）。
3. 仓库同步：`crates/uva-gateway/README.md`、AGENTS.md §1 / §3.6、CHANGELOG。

**验收**

- `make ci` 保持绿（桌面壳不进 root workspace）。
- 手工（本机已装 `webkit2gtk-4.1`，见 C1）：起桌面壳 → 窗口打开 → 发一轮对话 → 审批在窗口内应答 →
  杀掉 sidecar → 窗口内出现**既有**的离线徽标与重连（`useServerHealth` + `rpc.ts` 的重连
  逻辑，前端零改动）→ 自动恢复。

### 刀 4 · 能力矩阵（从 profile 推导 · 文档 + 一次性核对）

**范围**

- `docs/host-capabilities.yaml`：13 行（域）× 3 列（宿主）。初值按**今天的真实状态**填：
  web / desktop 多为 `full`；tui 的 `sessions` / `chat` 为 `adapted` 或 `full`，
  其余域为 `planned`（带锚点）。
- `scripts/check-host-capabilities.sh` + `scripts/host-capabilities/baseline`（记**缺口数**）：
  按 D15 的五条规则校验，打印缺口数并与基线比较。
- `make check-host-capabilities` + 进 `make ci`；**同步** `make lint-sh` 的清单与
  `.github/workflows/shellcheck.yml`（AGENTS.md §3.6：两份清单必须一致）。

**验收**

- 每个格都有值；`planned` 都带锚点；`full` / `adapted` 的证据文件都在磁盘上；
  矩阵引用的方法名都能在服务器注册表里 grep 到。
- 反向验证：故意把某个格改成无锚点的 `planned`、或删掉一列、或让缺口数回升 → 门禁失败。
- 缺口数 ≤ 基线（初版基线 = 今天的真实缺口数）。

### 刀 5 · TUI 补齐 wave 1：会话 + 对话全域

**范围**

- `crates/uva-tui/src/views/list.rs`（**通用列表**：吃 `List<T>` 信封，按列配置渲染，
  支持 cursor 翻页）、`views/detail.rs`（**通用详情**：单对象 → 键值表）、
  `domains.rs`（**每域一张列配置表**：域 → list 方法、item 字段路径、显示列、detail 方法）。
- wave 1 覆盖 `sessions`（list / get / status / usage / export / fork / resume / compact /
  delete）与 `chat`（create / send / cancel / regenerate / feedback）。
- 导航：域树 → list / detail 两级 + 现有对话视图。

**验收**

- `cargo test -p uva-tui` 绿；通用列表 / 详情渲染器用 `TestBackend` 断言缓冲文本。
- 手工：TUI 能列出会话、打开一个会话、看用量、fork / resume / compact / 删除。
- 矩阵的 `sessions` / `chat` 两行在 tui 列从 `planned` 变为 `full` / `adapted`，缺口数下降。

### 刀 6 · TUI 补齐 wave 2：通用目录七域

**范围**

- 目录域（agents / skills / tools / tasks / schedules / workflows / evals）各加**一张列配置表**，
  **不加界面**；`models` / `plugins` / `search` / `ui` 按同一机制接入。
- 写操作（create / delete / run / tick）在终端里先给确认提示再执行。
- 若某域的响应形状与 `List<T>` 信封不一致，在适配表里声明自己的取项路径
  （不改服务器 wire）。

**验收**

- 每个目录域在 TUI 里能列表 + 详情 +（该域有的）写操作。
- 矩阵这七行在 tui 列全部 ≥ `adapted`；缺口数降到只剩介质不适用项。

### 刀 7 · profile 的启用位 + 事务性动态变更 + 内置 MCP

**范围**

1. **profile 行自带 `enabled`**（D20）：每一行（能力行 / `ui-bundle` 行）多一个启用位与
   自己的参数；`enabled = false` 的行**不进 Fiber 树** ⇒ 能力与界面同时消失（D32）。
   **不引入 settings 文件**，也不引入全局面；插件的私有设置由插件自己管（D20）。
   启用位**可以由插件判定**（D39）：`enabled = { service = "...", check = "...", arg = "..." }`
   ⇒ skill 插件自己回答"这个 skill 启用吗"，**不必把每个 skill 写成 profile 行**。
   **不引入 `group` 字段**（DSH 同款）：族靠服务契约 + 行表达（D38）。
2. **事务性变更**（D21）：内置 MCP（暂名 `uva-settings`）暴露
   `profile/list` · `profile/get` · `profile/set` + 能力行的创建 / 删除动作。流程：
   写临时文件 → **试装**（`mount_all` 全组校验 + 失败全组回滚）→ 成功才**替换正式文件**
   并广播"目录变了"；失败则**报错并保持变更前的状态**（文件不换、树不动）。
3. **创建 / 删除能力走 MCP**（D28）：界面上的"创建/删除能力"是一次 MCP 调用
   （MCP 写 plugin 排布的数据文件 + 写 profile 行 + 试装），界面不直接写文件。
4. **profile 变更 → 界面刷新**（D33）：经既有通知通道（`session/event` 一类）广播，
   界面收到即刷新能力目录；进行中的 turn 用旧句柄完成。

**验收**

- 把一行 `enabled` 置 false → 该能力不可用 ∧ 该界面区块不渲染（同一行，两种表现）。
- MCP 写一个**装不起来**的 profile ⇒ 报错，**profile 文件没变**，已挂载的树没变，
  界面仍显示变更前的目录。
- MCP 写一个装得起来的 profile ⇒ 提交、刷新、下一 turn 用新装配结果。
- MCP 创建一条能力 ⇒ 排布里出现对应**数据文件**（`.agents/agents/<name>/` 或
  `.agents/skills/<name>/`），且**承载它的聚合行**在 profile 里存在并启用（技能由 `skills`
  行承载；**agent 没有独立的 profile 行**——它由目录发现，不是一行插件）。
  ⇒ 验收：数据文件在盘 + 聚合行在 profile 里可见。（2026-11-10 修订：初稿写的是
  "profile 里出现对应行"，那假设了 per-agent 行；实际的能力目录模型是"文件 + 聚合行"。）

### 刀 8 · UI 树与插槽契约（UI 装载器 + 浏览器 + 终端）

**范围**

1. **UI 树由 UI 装载器装，两个宿主都消费**（D25）：`ui-bundle` 行声明**插槽**
   （`chat.sidebar`、`session.detail.extensions` 之类）；**插槽 id 与宿主无关**，
   浏览器渲染成 DOM、终端渲染成 ratatui。
2. **UI 装载器**：装配期读 profile 的 `ui-bundle` 行，为**当前宿主**装一棵 UI 树
   （浏览器树 = JS 模块；终端树 = 终端可渲染的 UI 插件）；向未声明插槽投递 = 拒绝并记录。
3. **浏览器侧**：插槽区域声明 + 渲染（`SlotRegion`），未知 kind 走既有占位。
4. **终端侧**：同一批插槽 id 在终端里渲染成 ratatui 部件 ⇒ **TUI 因此可扩展**
   （新增一个 UI 插件 = 一行 profile + 一份插件数据，两个宿主同时拿到）。

**验收**

- 同一行 `ui-bundle` 在浏览器与终端里都能拿到（插槽 id 相同、呈现不同）。
- 向未声明插槽投递 ⇒ 拒绝 + 记录（两个宿主一致）。
- 终端里新增一个 UI 插件 ⇒ 不用改终端代码就能多出一块界面。

### 刀 9 · SPA 按新原型重排信息架构

**范围**（原型：`docs/prototypes/shared-interface.prototype.html`，**以它为准**）

1. **删除旧的 19 个页面**（D34）：`pages/` 下的表格页（工具 / 技能 / Agent / 评测 /
   工作流 / 任务 / 计划…）与其路由一并删除，能力目录改由设置分区承载。
2. 侧栏：品牌 + HARNESS 徽标 · **新会话** · **工作区**（树：工作区 → 会话；**默认工作区
   常驻**，D35）· 组标题旁的跨域搜索 · 左下用户（设置入口）· **可折叠成图标栏**（`Ctrl+B`）。
3. 主区（对话优先）：标题栏 · 对话流（用户**右对齐气泡** · 思考块**输出中展开、输出完自动
   折叠** · 助手 markdown 回答）· **待办进度** · **输入框（bash 风格 prompt 修饰，
   `❯ <路径> `，不做 prompt 建议条）**（D31）。
4. 设置面：侧栏**整个变成设置导航**（用户卡 · `Ctrl+F 搜索` · 分区列表）；分区内容
   **从解析后的 profile 反推**（D26），每行带启用开关；创建 / 删除能力**走 MCP**（D28）；
   **「规则与记忆」不做**（D24）。
5. 快捷键按 **Vim 优先**（D30）：`j/k` 移动 · `gg`/`G` 首尾 · `Ctrl+d`/`Ctrl+u` 半页 ·
   `/` 搜索 · `n`/`N` 跳转 · `:` 命令面板 · `?` 帮助 · `Esc` 取消 · `x` 删除 · `Enter` 打开 ·
   `y` 复制 id · `Ctrl+b` 折叠侧栏。

**验收**

- 旧页面与其路由全部删除（无残留入口），新壳按原型呈现。
- Playwright MCP：工作区树可展开/切换（默认工作区常驻）、折叠 rail 生效、待办可见、
  输入框是 bash 风格提示符、设置分区可切且开关经 MCP 写回 profile。
- `make check-web` 绿；`npm run build` 通过。

## 9. 留给 builder 的

- Tauri 2 的窗口创建与 `WebviewUrl::External` 的具体写法、`tauri.conf.json` 的
  `frontendDist` / `externalBin` / resource 布局细节。
- `crates/uva-gateway` 的公开面切分（哪些 `pub`、哪些 `pub(crate)`）——
  判据是"crate 外真的被用"，由 builder 按 AGENTS.md §3.7 实测 grep 决定；
  硬约束是公开面不带 `tokio::` 路径（C7）。
- TUI 的 `RequestKind` → 方法名映射是否保留在 TUI 里、还是下沉到共享 crate 的 client
  （倾向前者：方法名是 TUI 的领域词汇）。
- 刀 5 的 boot 清单方法名与落点（`src/rpc/methods/` 下的新模块 vs 既有模块加方法）——
  两者都可行，builder 自选并说明；注意它是 JSON-RPC 方法，前端要在 `api/client.ts`
  的路由映射表加一行。
- 桌面壳的图标 / 打包元数据（`tauri.conf.json` 的 bundle 段）。
- 桌面端的 macOS / Windows 打包配置（本轮 CI 只跑 Linux 构建，不出安装包）。
- 能力矩阵的**初值**：13 个域 × 3 列的逐格判定（哪些是 `full`、哪些是 `adapted`、
  哪些 `n-a`、哪些 `planned`）——这是一份**判断**，由 builder 按当天代码事实填，
  但必须与 D17 的规则一致（`n-a` 要写理由，`planned` 要带锚点）。
- TUI 通用列表 / 详情渲染器的**终端交互约定**：翻页键、光标移动、进入详情与返回、
  写操作的确认提示——沿用 `crates/uva-tui/src/render.rs` 已有的键位风格，不另造一套。
- 每域列配置表的字段名（哪些字段当列、哪些当详情键）——以服务器实际响应为准，
  不新增服务器字段。

## 10. 风险

- **R-1 桌面壳的构建依赖**：本机已装 `webkit2gtk-4.1`（C1），GUI 目标实测可编可跑；
  但**其他机器 / 贡献者**若要在本地跑桌面壳，同样需要它。缓解：壳的逻辑全部落在
  `crates/uva-gateway`（root workspace 内，无 GUI 依赖，任何机器都能测）；
  Tauri 壳保持薄，编译与运行由独立的 desktop CI job 覆盖。
- **R-2 脱离 root workspace 的门禁缺口**：`crates/uva-gateway` 与 `crates/uva-tui`
  在 root workspace 内（root 门禁全覆盖）；`desktop/` 在外（自带 job 覆盖
  fmt / clippy / test / cargo-deny）。缓解：desktop job 与 rust-quality 并列，缺它则 CI 红。
- **R-3 前端出现第二个"真源"**：界面自己维护一份"能力清单"会与 profile 漂移。
  缓解：D26 的规则——设置界面是**解析后 profile 的反推**，不另建清单；profile 变更经
  通知通道告知前端（D31）。
- **R-4 2b 打开新的执行面**：同源 JS 一旦可执行，`[ui]` 行就变成代码注入面。
  缓解：默认关 + 逐行开关 + 同源路径 + 禁 eval + 装配期拒绝（不静默忽略）。
- **R-5 没有 CSP 背书**（D11）：2b 打开后，页面里能跑的 JS 只受"同源 + 开关"约束，
  没有响应头的第二道防线。已知缺口，本轮不补。
- **R-6 跨 workspace 的 path 依赖**（D3）：`desktop/` → `../crates/uva-gateway`
  意味着两份 lockfile、两套 feature 解析。缓解：该 crate 依赖面小
  （tokio / futures / serde_json / tokio-tungstenite）。
- **R-7 TUI 并轨的行为差异**：TUI 今天的 core 监管是自己做的，并轨后由网关做——
  "core 崩了"的恢复路径变了（网关重启 core → 广播 `ready` → TUI 重发 follow）。
  缓解：D13 明确保留"收到 `ready` 重发 follow"这条；刀 2 的手工冒烟覆盖"杀 core 后 TUI 恢复"。
  另外：网关重启 core 时会**丢弃订阅并让在途请求失败**（`drop_subscriptions`），
  TUI 必须能重新建立会话跟随——今天的 TUI 走的是自己的重启路径，不会遇到这个语义。
- **R-8 TUI 覆盖面比 SPA 窄**：并轨后 TUI 与浏览器共用协议，但 TUI 的界面仍只有
  会话 / composer / 审批三块。本轮不扩 TUI 的界面（不扩范围），但"统一"带来的收益
  （协议面单一、core 监管单一、可 attach 多客户端）是真的。
- **R-9 `make ci` 时长与依赖**：新增 desktop job 增加 CI 时间；`--all-features` 的
  clippy 不再覆盖桌面壳（它在别的 workspace），属已知缺口，由 desktop job 补。
- **R-10 TUI 补齐是本波最大的一块**：13 个域、54 个方法；通用渲染器把它压成
  "4 个界面 + 13 张表"，但**写操作**（create / delete / run / tick / compact / fork）
  在终端里要各自设计交互与确认流程，这是最容易拖慢的部分。
  缓解：wave 1 / wave 2 分开交付，矩阵棘轮让进度可见；写操作允许先只读（`adapted`）
  再补写。
- **R-11 矩阵退化成"填表游戏"**：只填不做的格子会让门禁失去意义。
  缓解：D15 的五条规则——`planned` 必须带锚点、`full` 必须有磁盘上的证据、
  缺口数只降不升、方法名必须在服务器注册表里存在。
- **R-12 两套前端的语义漂移**：SPA 改了某域的字段，适配表没跟上 → TUI 显示空白。
  缓解：适配表引用的是**服务器方法名**（矩阵规则 ⑤ 强制它们存在）；
  字段级漂移靠 wave 的手工冒烟发现（矩阵的证据指针指向那些冒烟脚本）。
- **R-13 有意保留的重复**（D18）：会话行词汇的解释在 TS 与 Rust 各一份。
  这不是缺陷而是取舍——**能力级**漂移由矩阵棘轮拦住，**字段级**漂移只靠冒烟发现。
  缓解：TUI 的波次交付时同步更新矩阵里的证据指针（指向那次冒烟脚本），
  字段语义变更时把两个宿主的适配表一起改（改动的字段集中在 `domains.rs` 与
  `session-to-messages.ts` 两处，可数）。
- **R-14 profile 的写入是事务性的新能力**：今天 profile 只读 + HMR watch，写路径不存在。
  缓解：**两阶段**（临时文件 → 试装 → 成功才替换）；失败则报错并保持变更前
  （文件不换、树不动）；写入器**单点**（界面与内置 MCP 共用）；写完的重挂语义与 profile 一致
  （进行中的 turn 用旧句柄完成）。
- **R-15 内置 settings MCP 是自配置面**：agent 能改自己的启用状态。
  缓解：只允许改**启用开关与参数**（D21），不允许改定义、不允许改 profile；
  每次写都进事件日志（审计），可被 `hooks` 观察到。

## 11. 用户已拍板（2026-11-09）

1. Desktop 用 **Tauri**（Tauri 2 + WebView）。
2. **同一套前端代码服务 web 与 desktop**（G2 / D2）。
3. **TUI 保留终端原生 UI，传输并轨到网关**（§5.2 的 T1 / D13）。
4. 2b 默认关 + 逐行显式开关（D6），2a 先独立交付。
5. **三个宿主要做等价实现**（不是"web 完整 + TUI 精简"）：落法 = **能力矩阵 + 通用渲染器
   增量补齐**（D15 / D16 / D17），TUI 按 wave 补到与 web / desktop 同能力集。
6. **三个宿主是同一套界面**（不是三套等价界面）：web/desktop 同一份 SPA，TUI 是终端渲染。
7. **用户消息与 web 一致**（右对齐气泡）；**思考块**输出中展开、输出完自动折叠。
8. **搜索**放工作区组标题旁（跨域动作）；**UI 扩展**目录归「插件」、渲染在对话壳。
9. **plugin 排布是定义的唯一来源**：智能体 / 技能与命令 / MCP / Hooks / 自动化 / 模型都由它定义；
   **没有 settings**，启用与参数**内敛到 profile**；**插件入口从对话界面搬进设置页**；
   变更走**内置 MCP**且**装得起来才生效**；**内置默认 agent** 是整个 harness 的运转方式
   （不可停用）；设置页**不做「规则与记忆」**（D19–D25）。
10. **本轮终点** = 架构收敛为主目标、桌面应用为验收 ⇒ 与目标无关的机制性工作砍掉
    （前端插件树 / 同源 JS / CI 棘轮门禁）（G9 / D33 / D27）。
11. **能力矩阵从 profile 推导**，13 域退化为展示分组；门禁降级为**文档 + 一次性核对**（D27）。
12. **"同一套界面"按能力对齐 + 同一套骨架执行**，不做终端逐页镜像 SPA（D17 / D32）。
13. **没有 settings 文件**：启用与参数一律在 profile 里，各插件自己管自己的设置（D20）。
14. **agent 改设置走审批 + 审计**（D29）。
15. **"禁用" = 该行不挂载 = 能力与界面同时消失**（D30）。
16. **profile 变更经通知通道告知前端**，前端刷新能力目录（D31）。
17. **旧的 19 个页面删除**，以新原型为准（D32）。

## 12. 原型与分支

一次性原型在分支 **`prototype/interface-contract`**（不进 master）：

| 文件 | 是什么 |
|---|---|
| `docs/prototypes/host-capabilities-model.prototype.html` | 能力矩阵与对等模型（逻辑原型；五条规则的可执行版） |
| `docs/prototypes/shared-interface.prototype.html` | 共享界面契约：web 形态 + 终端渲染并排（两边共用状态） |
| `crates/uva-tui/examples/tui-layouts-prototype.rs` | 生成上面那个页面的源；终端那半是**真 ratatui 输出** |
| `docs/prototypes/*.png` | 各状态截图 |

原型的**结论**已折进本篇（§6.1 界面契约、D15–D25）；原型本身只留在分支上。

## 13. 评审结论的处理（builder check, 2026-11-09）

本篇已按 builder check 的 19 条结论修订。要点：

| 编号 | 结论 | 处理 |
|---|---|---|
| Q1 | `openspec/` 与 `docs/superpowers/` 不入库 | 本篇移到 `docs/design/`；刀 0 拆成"本地 openspec 层"+"入库文档"两段（C6 / D8） |
| Q2 | AGENTS.md §4.2 禁的是**任何**前端测试套件 | D7 改为不建测试层，验收走 MCP 页面内断言；刀 0 在 AGENTS.md §4.2 补一句 |
| Q3 | CI web job 固定 Node 20 | 同 Q2 一并消解（不写 `.ts` 测试文件） |
| Q4 | `tsc --noEmit` 的 `include: ["src"]` + 无 `@types/node` | 同 Q2 一并消解 |
| Q5 | 前端已有 19 处动态 import | §4 补 C9；刀 8 的 grep 验收改成"清单为空 ⇒ 加载器未被调用" |
| Q6 | `web-projection` 有**两条**禁令 | 刀 0 两条都改 |
| Q7 | `[ui]` 段没有放标量的地方（且无 `deny_unknown_fields`） | D6 改为行级 `config.allow_code`，不动 `Profile` 结构体 |
| Q8 | `check-public-api-runtime` 扫全部 crate | D12 要求新 crate 公开面 runtime-free（照 `uva-tui`） |
| Q9 | 没有 `/api/v1/*` HTTP 面，`ui/schema` 是 JSON-RPC 方法 | §4 澄清；刀 8 的 boot 清单定为 JSON-RPC 方法 |
| Q10 | `UiSchemaPanel` 已有 slot 过滤，但每实例各取一次 | 刀 7 §3 补"store 是唯一取数方"的重构 |
| Q11 | 桌面壳逃过 `cargo deny` | 刀 3 的 CI job 加一次 `cargo deny`（C10） |
| Q12 | 插件模块放哪、"同源"相对谁 | D2 / 刀 8 §2：相对页面 origin 的路径 + 页面同源的静态服务（生产 web-dir，dev Vite public/）；CSP 缺口记入 D11 |
| Q13 | 微内核生命周期、两类条目共存、keyed 的键、命名撞名 | D5（基数 `one`/`many`、v1 无 keyed）+ D9（单例、两类共存、`console.warn`） |
| Q14 | 壳从哪找 `uva-app-server` | D10：sidecar 资源 → flag/env/PATH → `current_exe()` 同目录 |
| Q15 | 共享 crate 的形态 | D12：lib（futures 公开面 / 内部 tokio）；members 与 `ORDER` 同步 |
| Q16 | Vite 已代理 `/rpc`，覆盖缝不必要 | D2 删除覆盖缝；刀 3 完全不碰前端 |
| Q17 | 桌面壳的生命周期参数 | D10：默认工作区 = cwd、就绪超时 30s、1s→5s 退避、端口优先复用 |
| Q18 | "重连中"用哪个既有表达 | 刀 3 验收改用既有离线徽标 + 重连逻辑（前端零改动） |
| Q19 | 插槽 id 词法与 `chat.sidebar` 归属 | 刀 7 §4：`chat.sidebar` = 会话侧栏（`SessionSidePanel`），并说明 `[ui]` 行是 deployment-wide |

---

## 14. 落地结果（2026-11-10，十刀全部交付）

十刀按 §8 落地，共 60 个提交（`90850620..HEAD`），根 `make ci` exit=0。

| 刀 | 落点 | 证据 |
|---|---|---|
| 0 spec 收口 | 本地 openspec 三层（`web-projection` 白名单 / `desktop-projection` / `shared-interface`） | 本地层不入库（`.gitignore:41`） |
| 1 网关 crate | `crates/uva-gateway`（supervisor + client，公开面 futures-only） | 16 测试；`make check-public-api-runtime` 免豁免 |
| 2 TUI 并轨 | `crates/uva-tui/src/transport.rs` 重写 + `views/` + Vim 键位 | 104 测试；真 provider 一轮对话；杀网关 5s 内报 `offline`；退出零孤儿 |
| 3 桌面入口 | `desktop/`（独立 workspace，Tauri 2 壳）+ `.github/workflows/desktop.yml` | 杀 sidecar 自愈；关壳零孤儿；actionlint 过；含 `cargo deny` |
| 4 能力矩阵 | `docs/host-capabilities.md`（15 域四要素 + 24 行双向对账） | 方法名对照 66 个注册方法全中；4 条漂移如实记录 |
| 5+6 TUI 补齐 | `domains.rs` + `views/{list,detail,catalog}.rs`（12 域） | 矩阵 §6 的 tui 列达 `adapted`/`full` |
| 7 profile 生命周期 | `Entry.enabled` 三态 + `apply_resolved` 事务四态 + 内置 MCP `profile` 工具 + `profile/*` | `crates/uva-kernel/src` **零改动**；WS 冒烟 `profile/set → profile/changed` |
| 8 UI 树与插槽 | `ui/tree.rs` 装载器 + `ui/schema` 顶层 `slots` + 浏览器 `SlotRegion` + 终端 ratatui | 双宿主真冒烟（页面断言 + 终端缓冲文本） |
| 9 SPA 重排 | 对话优先路由 + 设置九分区 + 删 17 旧页 + Vim 层 | `check-web` 绿；真页面验证四态回执 |

**§9 留给 builder 的九项**均已定案：Tauri 窗口写法与 `tauri.conf.json` 布局（刀 3，
`externalBin` 走打包 overlay）；网关公开面切分（刀 1，`pub` 仅四类，判据是 crate 外真被用）；
`RequestKind → 方法名` 留在 TUI（方法名是 TUI 领域词汇）；boot 清单落既有路由模块
（不新增 `rpc/methods` 模块）；桌面图标与 bundle 元数据（刀 3，`npx @tauri-apps/cli icon`）；
macOS/Windows 打包（本轮 CI 只跑 Linux）；矩阵初值（刀 4，三面核对）；列表/详情键位沿用
既有风格（刀 5+6）；列配置以服务器实际响应为准（不新增服务器字段）。

**§10 风险的处置**：R-1/R-2/R-6/R-9/R-11 由布局本身消解（壳薄、逻辑在 root workspace、
desktop 自带 job）；R-3 由 D26+D31 兜住（设置面是 profile 的反推 + 变更通知）；R-4/R-5 已知
（v1 纯数据、无 CSP，本轮不补）；R-7 由刀 2 的"ready 后重发 follow"保留并实测；
R-8/R-10 的前提已变（TUI 覆盖从三块扩到 12 域目录 + 对话优先骨架）。

**验证期抓到的四个真缺陷**（均已修 + 回归钉）：
1. attach 模式网关静默死亡时 TUI 永远 `connected`——探针错误匹配过窄，且 client 的
   `request` 在断连时挂起等重连；补 `GatewayClient::connected()`（client 自己知道）。
2. 文档层缺 `config` 键被解析成 `{}`（显式清空）而非 `null`（无意见）→ 第二次
   `profile/set` 恒报 `restart-required`（整个装配被判重挂）。
3. 配置内部的 JSON `null` 键不可被 TOML 表达 → 往返即变更 → boot 物化行反复重挂；
   diff 改比“TOML 视图”（递归丢弃 null 值键）。
4. 同值重复写仍广播 `profile/changed` → 订阅方无谓重读；no-op（diff 为空）不再广播。

**验收期的缺口闭合**（设计审计 14 条，逐一处置）：
- G3 能力创建的语义按上条修订对齐实现（数据文件 + 聚合行，而非 per-agent 行）。
- G4 能力矩阵补上棘轮门禁（`scripts/check-host-capabilities.sh` + 基线 + `make ci`），
  五种反向验证（无锚点 planned / 删列 / 缺口回升 / 证据不存在 / 编造方法）都会红。
- G5 矩阵与 TUI 的 `KNOWN_METHODS` 补 `profile/*`（`:json` 通用门可达）。
- G7 网关的指定端口被占时改选空闲端口（此前只会退避重试同一端口）。
- G8 UI 装载器拒绝一行时写 `tracing::error!` 记录（此前只有错误上抛）。
- G9/G10 终端的 kind 注册表与两宿主的 slot id 字面量：边界写进 README
  （新 kind 需终端渲染器；slot id 漂移会落进"未放置"分组，不是静默丢失）。
- G12 桌面壳的 `dist` 随前端重新同步（`make -C desktop frontend`）。
- G13 root CI 的 web job 也跑 `npm run build`（此前只有 desktop job 构建）。
