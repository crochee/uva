# AGENTS.md

本文件是 agent 在本仓库的唯一规范：**做什么**（§1–§4：仓内硬约束）与**怎么做**（§5：agent 工作流）合流一处。逐 crate 的职责见 `crates/*/README.md`，架构理由见 [`docs/architecture.md`](docs/architecture.md)，门禁命令的完整清单见 `make help`。

> **仓库根的隐藏目录**：`./.agents/` `./.omo/` `./.superpowers/` `./.uva/` `./.trae/` `./.worktrees/` `./.attachments/` `./.scratch/` 是 AI agent / 本地工具链的状态、草稿与会话，不是本项目资产，已在 [`.gitignore`](.gitignore) 显式排除——**不要**把它们的路径写进 commit message、PR 描述或本文件。同理 `./.env` 与 `./config.yaml` 含真实 LLM API 配置，仅本地加载、不入库。
> **例外 — `./.cargo/config.toml` 是项目资产**：CI 与本地一致可达的工具链入口，`.gitignore` 不排除它（也不应排除）。
> **例外 — `./.agents/profile.toml` 是项目资产**：本仓自己的部署 profile 文档层，app server 开机即读它；`.gitignore` 单独放行这一个文件，`.agents/` 其余内容仍排除。

***

# 1. 环境

## 1.1 工作区分层

31 个 workspace member（以 `cargo metadata` 为准），全部在 `crates/`；另有独立 workspace 的 `desktop/` 与前端 `uva-web/`。

| 层 | crate | 职责 |
|---|---|---|
| 基座 | `uva-core` | 纯类型 / trait 基座：Clock、IdGen、CancelToken、Spawner、Registry、schema、错误 |
| 范式 | `uva-provider` `uva-tool` `uva-context` `uva-steering` `uva-session` `uva-skill` | 七个可替换组件的 trait + 默认实现（§3.7） |
| 插件 | `uva-tool-plugins` `uva-policy-plugins` `uva-hook-plugins` | 工具 / 策略 / hook 实现插件族，一模块一 feature |
| 装配 | `uva-kernel` `uva-runtime` | 插件内核（manifest / mount / effect）与配置驱动装配 |
| 运行时 | `uva-events` | run 事件词汇（`AgentEvent` / `SystemEvent` / `WarningKind`）；`uva-harness` 根与 `events` 深路径经 re-export 保路径 |
| 运行时 | `uva-agent` | agent 词汇：`Agent` trait、`AgentInput`、两种收件箱（转向通道）、peer 注册表、子会话写入者来源、prompt manifest；`AgentCaps` 与 `ReActAgent` 仍住 harness，路径经 re-export 保持 |
| 运行时 | `uva-hook` | 拦截契约：`Hook` / `Gate` / 请求与结果 transformer、七个挂载点键、解耦 `HookContext`、`HookPayload`、可撤销 `HookRegistry` 与仲裁、`hooks` 内核插件；harness 根与深路径保持 |
| 运行时 | `uva-harness` | ReAct 循环、seam、`Host` 注册表 |
| 插件 | `uva-tool-task` | `task` 工具：普通 `Tool`，解析同伴 agent、开子会话、内联驱动子 run；依赖 agent/事件词汇，不依赖 harness |
| 宿主 | `uva-app-server` `uva-tui` `uva-gateway` `uva-mcp-server` `desktop/` | 人机入口：唯一允许在公开签名里出现 tokio 的地方 |
| 观测 | `uva-telemetry` | console / file logging、Prometheus metrics、OTel 导出；日志与指标的唯一入口 |
| 适配 | `uva-mcp` | MCP 客户端：stdio / HTTP / in-memory 传输、supervisor 重连，把远端工具注册进本机 `ToolRegistry` |
| 门面 | `uva` | facade：每个部件一个 feature + 精选 `prelude`；只有 `pub use`，零逻辑 |

其余 crate 各占一个领域：`uva-core-tokio`（`uva-core` 那些 runtime-neutral seam 的 tokio 实现）、`uva-approval`（审批词汇与决策函数，harness 冻结面经 re-export 保路径）、`uva-search`（多域检索）、`uva-scheduler`（定时分发）、`uva-eval`（评测）、`uva-attachment`（多模态附件）、`uva-test-support`（确定性假件）。逐 crate 一句话职责见 [`docs/architecture.md`](docs/architecture.md)。

## 1.2 入口

- `uva-app-server` 一个二进制两种模式：`core`（stdio JSON-RPC 2.0 无头内核，stdout 只承载协议帧，日志一律走 stderr）与 `serve`（网关：WebSocket `/rpc` + `/livez` `/readyz` `/version` `/metrics` + 静态前端）。
- `uva-tui` 与桌面壳都经 `uva-gateway` 驱动 `serve`——与浏览器同一条宿主路径；进程监管（拉起、探活、退避重启、回收）在网关 crate。
- 协议与配置项见 [`crates/uva-app-server/README.md`](crates/uva-app-server/README.md)。

## 1.3 观测性

`uva-telemetry` 常驻 console / file logging；`metrics`（Prometheus `gather_text()`）与 `otlp`（OTel exporter + tracer / propagation 模块）是 feature。OTel 端点与采样器走环境变量：`UVA_OTLP_ENDPOINT`（scheme 决定 gRPC / HTTP）、`UVA_OTEL_SAMPLER`（`always_on` / `always_off` / `trace_id_ratio:x`）。见 [`crates/uva-telemetry/README.md`](crates/uva-telemetry/README.md)。

## 1.4 依赖纪律（三条硬规则）

1. **传输按 feature 可选**：HTTP client（`reqwest`）只允许出现在 `uva-provider/{anthropic,openai}`、工具族的 `web`、telemetry 的 `otlp`；其余成员默认零 HTTP。门禁 `make check-mvp-deps`。
2. **运行时按 crate 声明**：每个 crate 的 `tokio` 行只写自己代码用到的 feature，禁止从 workspace 继承 `full`；仅测试用 tokio 的 crate 放 `[dev-dependencies]`；需要脱离运行时的 spawn 一律走 `uva_core::spawn::Spawner`，禁止 `tokio::spawn`。门禁 `make check-no-runtime`。
3. **公开 API 不出现运行时类型**：库 crate 的 `pub` 面不得出现 `tokio` / `async-std` / `smol` 路径；`uva-app-server` 与 `uva-mcp-server` 是应用 / 二进制 crate，豁免。门禁 `make check-public-api-runtime`。

***

# 2. 代码同步规范

- 不主动 push 代码到远程仓库。
- 搜索路径优先级：本地工作空间 > HOME 目录 > 其他目录。

***

# 3. Rust 编码规范

## 3.1 强制要求

- **错误处理**：Rust app 只能用 `anyhow`，Rust lib 只能用 `thiserror`——互不混用。调用方需要分辨失败分支的库错误用 typed enum（`thiserror`），包住别的错误用 `#[from]`（它蕴含 `#[source]`）。
- **依赖版本**：一律 `major.minor`（`1.1.0` → `1.1`），禁止三段或单段。
- **依赖收口**：新增依赖优先复用根 [`Cargo.toml`](Cargo.toml) 的 `[workspace.dependencies]`，crate 内写 `dep = { workspace = true }`，不各自写版本号。
- **MSRV**：workspace `rust-version = "1.98"`；引入依赖前确认其 MSRV 不高于此。
- **公开项有文档**：每个 `pub` 项写清语义；公开行为随实现演进会破坏调用方。
- **库代码不 `unwrap` / `expect`**：返回 `Result`，或在文档化的不变量上 panic；测试代码除外。
- **feature 只增不减**：开启一个 feature 不得移除功能、不得破坏 semver。

## 3.2 代码质量与格式化

- 不用 `dead_code` / `unused` 属性压制警告——新代码没用上就删掉。
- 控制圈复杂度，优先可读性与可维护性；遵循 Rust Style Guide。
- 写完必须执行并修净所有警告与错误：

  ```bash
  cargo +nightly fmt --all
  cargo clippy --all-targets --all-features --tests --all -- -D warnings
  ```

## 3.3 测试与编译规范

- **禁止 `cargo test --workspace`**：按模块分批 `cargo test -p <crate>`（`make test-crates` 逐 crate 跑，失败时只丢一段）。
- 测试选型：单元测试贴代码；不变量用 property test；公开项用 doctest；feature 组合用矩阵。
- 发现磁盘占用率过高时清理构建产物（例如 `cargo clean`），避免 WSL 崩溃。

## 3.4 项目基线配置（单点真相）

下列文件是仓内基线，agent 改前先读文件而不是各自复制；改它们必须在同一变更里更新本文件对应引用，避免规则与配置漂移。

| 文件 | 基线 |
|---|---|
| `rust-toolchain.toml` | 固定 `stable` 通道；`cargo +nightly fmt --all` 是唯一例外（rustfmt 配置要求 nightly） |
| `rustfmt.toml` | edition 2024、`max_width = 80`、`imports_granularity = "Crate"`、`group_imports = "StdExternalCrate"` |
| `clippy.toml` | `cognitive-complexity-threshold = 20`、`warn-on-all-wildcard-imports = true`；测试内允许 `expect` / `unwrap` / `dbg`；禁用 `map_or` / `map_or_else` / `for_each` / `try_for_each` / `mem::forget` / `ptr::read_unaligned` |
| `scripts/harness-shape/clippy.toml` | 形状预算：函数 ≤100 行、嵌套 ≤4 层。**故意不放进根 `clippy.toml`**——写进去会全局启用这两个默认关闭的 lint；只由 `scripts/check-harness-shape.sh` 经 `CLIPPY_CONF_DIR` 注入 |
| `Cargo.toml` | `resolver = "2"`、edition 2024、`rust-version = "1.98"`；release profile（opt-level 3 / thin LTO / codegen-units 1 / strip symbols）不得随意改 |
| `deny.toml` | `cargo-deny` 的许可证 / 来源 / ban 审计入口；安全公告由 `cargo-audit` 单独负责 |
| `.markdownlint.json` | markdown lint 锁，作用于 31 个 crate 的 `README.md` 与 `CONTRIBUTORS.md`（与 `make fmt-check` 互不重叠） |
| `.editorconfig` | 编辑期 LF / UTF-8 / 尾空格 / 末行换行；与 rustfmt / prettier 互补不重叠 |
| `.gitattributes` | git 期 LF 强制 + binary 标记 + linguist 关闭 vendor/generated；`CHANGELOG.md` / `Cargo.lock` 用 `merge = union` |
| `[workspace.lints]`（根 `Cargo.toml`） | rustdoc 棘轮表（`missing_docs` 等）。**当前没有 crate opt-in**（`grep -rln '^\[lints\]' crates/*/Cargo.toml` 为空），所以一个都没在跑；实际约束是 `make check-public-docs` 的计数棘轮 |
| `[workspace.metadata.docs.rs]`（根 `Cargo.toml`） | `all-features = true` + 固定 target，让 docs.rs 渲染 opt-in 契约而不是空模块 |

## 3.5 圈复杂度与代码异味

- **形状预算才是被强制的那条**：`uva-harness` 生产代码不得有函数超过 100 行或嵌套超过 4 层（`make check-harness-shape`）。超出时优先拆分，而不是放宽阈值。
- `cognitive-complexity-threshold = 20` 保留但不再作为拆分理由——该 lint 属 `restriction` 组（默认关闭），且按其自身文档已不再度量"认知复杂度"。
- 禁止通配符导入（`use foo::*;`）；优先 `for` 循环处理副作用；优先 `map(..).unwrap_or(..)`；禁止 `std::mem::forget` 与 `std::ptr::read_unaligned`。
- 写 doc comment 时遵守 `doc-valid-idents` 白名单（`MiB`、`IPv4`、`OAuth`、`PostgreSQL` 等）。

## 3.6 自动化入口（Makefile）

**`make help` 是目标的唯一清单**——本节只记"哪个门禁护什么"与必须同步的规则，不复述目标列表。

收尾必跑：`make fmt-check`、`make lint-rust`、`make doc-check`、`make test-crates`，以及本次改动触及的 `check-*` 棘轮。

| 门禁 | 护住什么 |
|---|---|
| `check-mvp-deps` | 七 feature 最小子集与两个插件族不引入 HTTP / OTel / DB 依赖 |
| `check-no-runtime` | runtime-free crate 的 lib 构建不含 tokio |
| `check-kernel-deps` | `uva-kernel` 的依赖图不含 agent / UI / 传输层 crate |
| `check-hook-external` | 样板 `docs/examples/hook-only-consumer` 的 **normal** 依赖树不含 `uva-harness`（harness 只进 dev-deps），并跑其 dev 测试驱动一次真实工具调用 |
| `check-public-api-runtime` | 库 crate 的公开 API 不出现运行时类型 |
| `check-pub-surface` / `check-pub-surface-strict` | 库 crate 内禁止 `pub use <mod>::*`；范式 crate 的 `pub use` 计数不得增长 |
| `check-frozen-api` | 冻结 crate 的精选公开面变动必须同步 `docs/api-frozen.md` 与基线 |
| `check-public-docs` | 冻结 crate 未文档化 `pub` 项的计数只降不升 |
| `check-harness-shape` / `check-harness-docs` | `uva-harness` 的函数长度 / 嵌套预算与私有项注释预算 |
| `check-plugin-shape` | 插件族单个生产源文件的行数预算 |
| `check-test-layout` | 生产文件内联 `#[cfg(test)]` 测试块的行数上限 |
| `check-clock` | 库代码不得直接调用 `chrono::Utc::now()` |
| `check-stdout` / `check-cli-surface` | 无头核心的 stdout 只承载协议帧；命令行一律 clap |
| `check-host-capabilities` | 能力矩阵只许变好 |
| `check-single-writer` | 会话行只能由该会话的 writer 写 |
| `check-claim-language` | 当前状态文档不出现与代码事实矛盾的绝对化措辞 |
| `check-publishable` / `check-deny` | 每个 crate 可 `cargo package`；许可证 / 来源 / ban 审计通过 |
| `check-web` / `check-web-contract` | 前端类型 / lint / 格式化 / 纯函数单测；`uva-web/src/api/types.ts` 必须逐字节等于服务端 wire types 的重新生成结果 |
| `check-web-methods` | 前端域客户端的 `METHOD` 表里的每个方法名都必须是服务端真正提供的方法（路由注册表 + 通知常量）；服务端有而前端不调的不算错；**读不懂的表也红**（每个含 `const METHOD` 的文件必须至少产出一行；表里读不出的一行——计算值、带插值的模板、计算 key、取值不在 key 那一行——也算读不懂，门禁点名文件与 key；契约清单的每一行都必须是被抽取器读到的一行） |
| `lint-sh` | 对 16 个 `scripts/*.sh` 门禁跑 shellcheck |

**新增 `scripts/*.sh` 门禁必须在同一变更里同步四处**：`Makefile` 的 `check-*` 目标与 `make ci` 的聚合清单、`Makefile` 的 `lint-sh` 清单、`.github/workflows/shellcheck.yml` 的清单、本节的 `lint-sh` 行——两份脚本清单必须一致。

CI（[`.github/workflows/rust-quality.yml`](.github/workflows/rust-quality.yml)）只调用上述 `make` 目标，分 `gates` / `tests` / `examples` / `web` / `bench` 五个 job；测试与示例无需 API key。

## 3.7 架构原则（硬不变量）

两个最高级目标决定下面每一条：**可作为 lib**（`cargo add uva` 是消费端入口，下游永不回指消费端应用）与**乐高式组装**（七大组件各自是可换 trait + 默认实现）。每条的完整理由与对应门禁见 [`docs/architecture.md`](docs/architecture.md)。

- **依赖方向单向**：`uva` → `uva-*` → `uva-core`，永不回指；用 `cargo tree -p uva-core` 验证无环。
- **公开面最小化**：只在 crate 外真的被用到的项才 `pub`（判据是 grep，不是印象）。每个 `uva-*` crate 的 `lib.rs` 是决定公开面的唯一地方，必须显式列名——禁止 `pub use <mod>::*`。
- **facade 零逻辑**：`crates/uva/src/lib.rs` 只有 `pub use`，所有逻辑属于底层 crate。
- **工具实现 = 插件族 crate**：面向 agent 的工具住在 `uva-tool-plugins`（一工具 = 一模块 + 一 feature），范式留在 `uva-tool`；策略插件同理住在 `uva-policy-plugins`，hook 实现同理住在 `uva-hook-plugins`。`task` 是单独的工具 crate `uva-tool-task`（注册进同一 `ToolRegistry`），因为它要带着自己的委派实现且不得依赖 harness。不存在"默认工具集"。
- **插件专属依赖逐插件 optional**：一个工具带什么由它自己的 feature 决定（`web` → `reqwest`、`scheduler` → `uva-scheduler`）；`uva-tool-task` 无 feature，依赖集固定为 agent/事件词汇 + 工具 trait。
- **工具的边界随工具走**：OS 执行策略（`ExecutionPolicy` / `SandboxBackend` / `effective_policy`）住在 `uva-tool-plugins::shell::sandbox`，由 `ShellTool` 自己持有——告诉模型的描述与真正 spawn 的 argv 来自同一个值，不可能漂移。
- **harness 与插件分离**：`uva-harness` 只保留核心循环 + registry 组装；委派是**普通工具**（`uva-tool-task` 的 `task`，注册表分发），不再有专门的委派通道，实现不得回填 harness。
- **不依赖具体运行时**：公开 API 不出现 `tokio::*` / `async_std::*` / `smol::*`（§1.4）。
- **chrono 是唯一的时间库**：墙钟走 `uva_core::Clock`（§3.8）。
- **ULID 是默认 ID 生成器**：走 `uva_core::IdGen`。
- **Registry seam 收口在 `uva-core`**：所有"具名目录 + cursor 分页"的抽象复用 `uva_core::registry::Registry`，不要在自己的 `mod` 里再写一份拷贝。
- **`uva-kernel` 是框架边界，不是 agent**：位于 `uva-core` 与 `uva-harness` 之间，只承载插件适配与生命周期原语。
- **profile HMR 只重挂 diff 的 id**：变更后只对 `added` / `removed` / `remounted` 的行卸载 / 重挂；`hmr = false` 的变更只记录"需要重启"，绝不半重载。
- **UI 扩展点只传 JSON schema**：`ui/schema` 只返回 `version` / `slot` / `kind` / `props`；v1 永不执行远端 JS / CSS。
- **第三方扩展走 MCP server**：动态插件通道（进程插件 / Wasm 组件）已删除，第三方工具与脚本以 **MCP server** 接入——`mcp.json` 的 `mcpServers` 映射按 `command` + `args` 声明，stdio 传输，server 发布的工具注册进同一个 `ToolRegistry`。MCP 不承载的角色（结果改写 / gate / authority 分级）随通道一起消失，不要为第三方另开一条动态插件通道；仓内的结果改写与 gate 实现是 `uva-hook-plugins` 的编译期 feature（`redact` / `traffic-light`），不是运行时插件通道。
- **Optional crates 是 opt-in 扩展**：可选 crate 各自带测试、example 与 facade feature；默认 `uva` 不拉起其中任何一个（`make check-mvp-deps` 断言）。

## 3.8 时间与 ID 的使用准则

- **生产代码**：`SharedClock::system()` 构造一次，把 `Arc<SharedClock>` 传给所有需要的组件；`SharedIdGen::ulid()` 同理。
- **测试代码**：`SharedClock::fixed_at(t)` / `SharedIdGen::sequence()` 注入确定值。
- 新代码禁止直接写 `chrono::Utc::now()` 或 `ulid::Generator::new().generate()`——必须经 trait。需要"当下"的公开构造函数一律提供 `_at(..., now)` 变体，默认形式再走 `SystemClock`。
- 门禁 `make check-clock`：库 crate 生产代码里的 `chrono::Utc::now()` 计数只降不升（基线 0；app crate 与 `#[cfg(test)]` 豁免）。

## 3.9 异步与并发

- **默认同步**：只有真实的并发 I/O 才引入 async。
- **长任务必须有取消路径**：用 `uva_core::CancelToken`，不用布尔标志；`Spawner` 是唯一的"脱离运行时"接缝。
- **不跨 `await` 持有 `MutexGuard`**——guard 不是 `Send`，且会卡住 executor task。
- **不在 runtime worker 上阻塞**：改用 async API、`spawn_blocking` 或 `block_in_place`。
- **通道按语义选**：需要背压用 bounded `mpsc`；一播多用 `broadcast`；只要最新值用 `watch`。

## 3.10 依赖与供应链

- 新增依赖必须通过 `cargo deny check licenses bans sources`（`make check-deny`）与 `cargo audit`。
- `Cargo.lock` 入库，release 产物从它复现。
- 破坏性公开面变更必须同步 `docs/api-frozen.md` 与基线（`make check-frozen-api`）。
- 可发布的 crate 必须能被 `cargo package`（`make check-publishable`）——path 依赖缺版本号这类缺陷只有打包那一刻才暴露。

***

# 4. Web 前端编码规范

## 4.1 基本要求

- 0 lint（无 lint 错误 / 警告）、需要格式化、`npm run build` 通过。
- 代码符合 Web TS / HTML / CSS 编码规范。
- 门禁：`make check-web`（`tsc --noEmit` + `eslint` + `prettier --check` + `vitest` 单元测试，无需浏览器）。

## 4.2 前端验证：vitest 纯函数单测 + Playwright MCP，不写 e2e

**不保留前端 e2e 套件；纯函数单元测试用 vitest（`uva-web/tests/*.test.ts`，node 环境），随 `make check-web` 跑。** 单测覆盖不到的一切——DOM、网络、React 组件、真实渲染——一律用 Playwright MCP 直接驱动浏览器验证。

1. 纯函数（`src/lib/` / `src/api/` 的投影、validator、分页等规则密集逻辑）改动：先在 `uva-web/tests/` 补或改 vitest 用例，`make check-web` 必须绿。
2. 起真实前端（`npm run dev`，必要时 `cargo run -p uva-app-server -- serve`）。
3. 用 MCP 打开页面、执行交互、读取 DOM 与真实渲染结果作为证据。

**MCP 的两个易踩陷阱**（必须处理，否则 chat 页"看起来没反应"）：

- **后台 tab 的 `visibilityState === 'hidden'`**：`useServerHealth` 的 `probe()` 在第一次检查时就 early-return，把整个 app 标成 Offline，输入框与 send 按钮置灰，且无任何错误信息。打开 tab 后在 navigate 前先 patch：

  ```js
  await page.evaluateOnNewDocument(() => {
    Object.defineProperty(document, 'visibilityState',
      { get: () => 'visible', configurable: true });
    Object.defineProperty(document, 'hidden',
      { get: () => false, configurable: true });
  });
  await page.bringToFront();
  ```

  或更直接地 mock `/readyz` 200 + `bringToFront`，效果一样。
- **隐藏 tab 里 `requestAnimationFrame` 永不触发**：页面内 `await new Promise(r => requestAnimationFrame(r))` 会一路挂到 `protocolTimeout`（默认 30 s）。验证 `naturalWidth` 之类的渲染后量应改用 `setTimeout` 或 MCP 提供的 `wait(...)` 助手。

理由：前端 e2e 套件（原 44 个 spec / ~5,200 行）跑一次 13 分钟，且需 `cargo build` + Vite + Chromium 三件套，成本远高于它发现的问题；spec 与实现会同步腐化（曾出现整批 mock 停留在重构前的 wire 形状、长期红灯无人察觉）。这套理由对 e2e 成立，对纯函数单测不成立——vitest 在 node 里跑，无需浏览器，秒级完成。MCP 打在真实应用上，不引入第二份需要维护的契约；MCP 的浏览器是 harness 自带的，与 `uva-web` 的依赖无关。边界论证见 [`docs/adr/0001-frontend-test-policy.md`](docs/adr/0001-frontend-test-policy.md)。

因此：`playwright*.config.ts`、`.github/workflows/e2e.yml` 与 `uva-web/scripts/e2e/` 均已删除，不要再建 e2e 套件；`uva-web/tests/` 只放 vitest 纯函数单测（不装 jsdom / happy-dom，不做组件测试）。

***

# 5. Agent 行为守则

> 本节合并自原 `CLAUDE.md`（Anthropic Claude Code 上游标准的 agent 行为守则）。上游原文本身就写明 "Merge with project-specific instructions as needed."，故与项目规范合流为**单文件**——仓库内不再保留独立的 `CLAUDE.md`，行为守则与项目规范只有这一处真源。

Behavioral guidelines to reduce common LLM coding mistakes. Merge with project-specific instructions as needed.

**Tradeoff:** These guidelines bias toward caution over speed. For trivial tasks, use judgment.

## 5.1 Think Before Coding

**Don't assume. Don't hide confusion. Surface tradeoffs.**

Note: After checking for applicable skills (§5.5), before responding with any solution:

Before implementing:

- State your assumptions explicitly. If uncertain, ask.
- If multiple interpretations exist, present them - don't pick silently.
- If a simpler approach exists, say so. Push back when warranted.
- If something is unclear, stop. Name what's confusing. Ask.

## 5.2 Simplicity First

**Minimum code that solves the problem. Nothing speculative.**

- No features beyond what was asked.
- No abstractions for single-use code.
- No "flexibility" or "configurability" that wasn't requested.
- No error handling for impossible scenarios.
- If you write 200 lines and it could be 50, rewrite it.

Ask yourself: "Would a senior engineer say this is overcomplicated?" If yes, simplify.

## 5.3 Surgical Changes

**Touch only what you must. Clean up only your own mess.**

When editing existing code:

- Don't "improve" adjacent code, comments, or formatting.
- Don't refactor things that aren't broken.
- Match existing style, even if you'd do it differently.
- If you notice unrelated dead code, mention it - don't delete it.

When your changes create orphans:

- Remove imports/variables/functions that YOUR changes made unused.
- Don't remove pre-existing dead code unless asked.

The test: Every changed line should trace directly to the user's request.

## 5.4 Goal-Driven Execution

**Define success criteria. Loop until verified.**

Transform tasks into verifiable goals:

- "Add validation" → "Write tests for invalid inputs, then make them pass"
- "Fix the bug" → "Write a test that reproduces it, then make it pass"
- "Refactor X" → "Ensure tests pass before and after"

For multi-step tasks, state a brief plan:

```text
1. [Step] → verify: [check]
2. [Step] → verify: [check]
3. [Step] → verify: [check]
```

Strong success criteria let you loop independently. Weak criteria ("make it work") require constant clarification.

## 5.5 Skills

**If a skill might apply, you must use it. No negotiation.**

- Subagent tasks: skip this rule
- User instructions (AGENTS.md, GEMINI.md, or the user's own `CLAUDE.md`) take highest priority
- Otherwise, skill rules override default behavior

**Execution order:** receive task → check skills → respond (including clarifications)

1% rule: invoke skill to confirm if there's even 1% chance. Ignore if inapplicable; missing one cannot be recovered.

**Red Flags (you're rationalizing):**

| Thought | Reality |
|---------|---------|
| "Just a simple question" | Questions are tasks. Check skills. |
| "Need more context first" | Skill check precedes clarifications. |
| "Let me explore codebase first" | Skills tell you how to explore. |
| "I know this skill" | Invoke it, don't rely on memory. |
| "Just a small thing" | Check before doing anything. |
| "Skill is overkill" | Simple things become complex. |

**Priority:** Process skills (brainstorming, debugging) → Implementation skills

**Types:** Rigid skills (debugging, TDD) follow exactly; flexible skills adapt principles to context.

***

## 5.6 Task-Centric Execution

**Decompose with Task tool. Dispatch in parallel. Track with verification, not todos.**

### Core Rules

- **NEVER generate Plans** before starting work. Do not write markdown plans, task lists, or step-by-step outlines as part of your response. Planning is implicit in your reasoning.
- **ALWAYS use the Task tool** to decompose complex tasks into independent sub-tasks. The Task tool handles decomposition, context propagation, and result collection.
- **Dispatch independent tasks concurrently**. When two or more tasks have no shared state or sequential dependencies, dispatch them in parallel.
- **Do NOT use TodoWrite to track your own task progress**. The parent agent must not use TodoWrite for self-management. Sub-agents may use TodoWrite internally for their own decomposition, but the parent agent should track progress by awaiting sub-task results and verifying outcomes.

### Parallel Task Dispatch

When multiple tasks are independent:

1. Identify tasks with no shared state or sequential dependencies
2. Dispatch them concurrently using separate Task tool invocations
3. Wait for all results before proceeding to dependent work

Example of independent tasks that should be dispatched in parallel:

- Editing files in different modules with no cross-references
- Running unrelated test suites
- Searching different parts of the codebase for different patterns

Example of dependent tasks that must be sequential:

- Task B reads output from Task A
- Task B modifies a file that Task A will also modify
- Task B depends on a compilation artifact from Task A

### Context in Task Descriptions

When dispatching sub-tasks, include all necessary context in the task description:

- What files to modify and what changes to make
- Any constraints (e.g., "do not modify tests", "preserve existing API")
- Verification criteria (e.g., "run `cargo check` after changes", "ensure tests pass")
- Relevant code snippets if the context is small

### Verification Criteria

Every dispatched sub-task should specify how success is verified:

- "Make the change, then run `cargo test -p <crate>` to confirm"
- "Add the feature, then verify it compiles with `cargo check`"
- "Fix the bug, then confirm the test passes"

***

**These guidelines are working if:** fewer unnecessary changes in diffs, fewer rewrites due to overcomplication, and clarifying questions come before implementation rather than after mistakes.

***

## Agent skills

### Issue tracker

Issues live as markdown files under `.scratch/` in this repo. See `docs/agents/issue-tracker.md`.

### Triage labels

Five canonical triage roles; each label string equals its role name. See `docs/agents/triage-labels.md`.

### Domain docs

Single-context: `GLOSSARY.md` at the repo root plus `docs/adr/`. See `docs/agents/domain.md`.
