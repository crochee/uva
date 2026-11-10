# Desktop 入口（Tauri 壳）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 交付设计文档 §8 的刀 3：`desktop/` 独立 workspace 里的 Tauri 2 壳——拉起 `elysper-app-server serve` sidecar、等 `/readyz` 就绪、开窗（`WebviewUrl::External`）、sidecar 死亡则重启并把窗口重定向、窗口关闭则回收 sidecar（无孤儿进程）；外加 `.github/workflows/desktop.yml`（apt 装 webkit 系依赖，跑该 workspace 的 fmt / clippy / test / `cargo deny`）与仓库文档同步。

**Architecture:** 壳不做协议、不做渲染。进程监管（拉起 / 等就绪 / 退避重启 / 回收）全部委托 `crates/elysper-gateway` 的 supervisor（刀 1 产物，跨 workspace 的 path 依赖——两份 lockfile 是已批准的代价，设计 §10 R-6）；窗口在生产里加载 sidecar 的 `base_url`（页面 origin 与 WS origin 同源，D1），在开发里加载 Vite dev server `http://localhost:5173`（代理已就位，前端零改动，D2）。`desktop/` 自带 `[workspace]` 表、**不进** root workspace 的 `members`——`make lint-rust` 的 `--all-features` 因此永远不会编到 GUI 目标（C2 / D3）。同一份前端：`elysper-web/dist` 同步成 `desktop/dist`，同时作为 `frontendDist`、`bundle.resources` 与 `serve --web-dir` 的输入。

**Tech Stack:** Rust stable 通道（`rust-toolchain.toml`；fmt 用 nightly，照 root 先例）、tauri 2.12（feature `wry`；C1 实测 `cargo check` 通过）、tauri-build 2、`crates/elysper-gateway`（path，刀 1）、tokio 1.53（feature `time`）、anyhow 1.0（app 边界）、Vite 8（`elysper-web` 既有构建）、GitHub Actions ubuntu-latest（apt 包名）

**Spec:** `docs/design/2026-11-09-desktop-frontend-tree-design.md` —— §8 的刀 3，§7 的 D1 / D2 / D3 / D4 / D10 / D14，§3 的 C1 / C2 / C5 / C10 / C11

## Global Constraints

- **前置：刀 1 先落地。** 本计划消费 `crates/elysper-gateway` 的公开面
  （`Gateway` / `GatewayOptions` / `GatewayState` / `GatewayError`，签名见
  `docs/design/2026-11-09-gateway-crate-plan.md` 的 Task 2）。**该 crate 目前不存在**
  （2026-11-09 核实：`crates/` 下无此目录）；它没合入之前，Task 1 起的每条 `cargo` 命令都会因
  path 依赖缺失而失败。刀 1 落地时若签名有出入，以实现为准并回头同步本计划。
- **独立 workspace（C2 / D3）**：`desktop/Cargo.toml` 带 `[workspace]` 表
  （`members = ["src-tauri"]`，`resolver = "2"`）；root `Cargo.toml` 的 `members` **一行不改**。
  `desktop/Cargo.lock` **提交入库**（应用 workspace 锁依赖，与根 `Cargo.lock` 立场一致）。
- **依赖白名单（D14）**：壳的直接依赖只有 `tauri`（加 build-dep `tauri-build`）、
  `elysper-gateway`（path）、`tokio`、`serde_json`、`anyhow`。**不引 `tokio-tungstenite`**——
  壳不碰协议，`GatewayClient` 留在 crate 里给下一个宿主。白名单是**上界**：实现结束仍未用到的
  依赖（很可能包括 `serde_json`）不写进 `Cargo.toml`，不预留。
- **错误处理（AGENTS.md §3.1）**：壳是 app ⇒ 只用 `anyhow`；`GatewayError` 在边界
  `map_err` 成带上下文的 `anyhow::Error`。
- **依赖版本 `major.minor`**（AGENTS.md §3.1）：独立 workspace 用不了 `dep.workspace = true`，
  版本直接写在 `desktop/src-tauri/Cargo.toml`（`tauri = "2.12"`、`tokio = "1.53"`、
  `anyhow = "1.0"`）；`elysper-gateway` 走 path 不带版本。tokio feature 按需声明、禁止 `full`：
  壳直接用到的只有 `time`（30s 就绪超时）。
- **不碰前端（D2 / AGENTS.md §4.2）**：`elysper-web/` 一行不改。"重连中"的表达复用既有离线徽标
  （`elysper-web/src/hooks/useServerHealth.ts` + `elysper-web/src/api/rpc.ts` 的
  capped 指数退避重连），壳不加任何窗口级 UI。
- **fmt / lint**：rustfmt 按源文件目录**向上**找配置，`desktop/src-tauri/src/*` 会命中仓库根
  `rustfmt.toml`——**不要**在 `desktop/` 里新建 fmt 配置。fmt 在 `desktop/` 目录下跑
  `cargo +nightly fmt --all`（只覆盖本 workspace）；clippy 照 root `make lint-rust` 的原样命令
  `cargo clippy --all-targets --all-features --tests --all -- -D warnings`。
- **测试按 crate 分批**（AGENTS.md §3.3）：`cargo test -p elysper-desktop --lib`；
  禁止 `cargo test --workspace`。壳的测试只测纯函数（选项解析、候选顺序、URL 决策）——
  `elysper-gateway` 的 `Launcher` 缝是 `pub(crate)`，壳从外面注入不了假进程；
  端到端行为按设计文档刀 3 的验收做**手工冒烟**（Task 3 Step 8-11）。
- **本机可跑**（C1）：Arch + `webkit2gtk-4.1` 2.54.2 已装、WSLg `DISPLAY=:0`。CI 跑
  ubuntu-latest，用 apt 的包名（`libwebkit2gtk-4.1-dev` 等），与本机 pacman 无关。
- **不 push 远程仓库。**

## Review Focus

规格里没有测试覆盖、最可能咬人的六处：

1. **frontendDist 目录不存在 ⇒ 编译期就停**。`tauri.conf.json` 的 `frontendDist` 指向
   `../dist`（构建产物副本）；目录缺失时 tauri 的 context codegen 无法嵌入资产。因此
   **每条** cargo 命令（check / clippy / test / fmt 之外都要编译）之前必须先跑
   `make -C desktop frontend`，CI 的步骤顺序相同。〔2026-11-10 复现并修订：同类编译期
   输入还有 `bundle.externalBin`——tauri-build 2.7.4 无条件校验并拷贝之，缺文件即
   `resource path binaries/... doesn't exist`（本机已复现）；已把 externalBin 移入
   打包期 overlay（Task 1 Step 3），基础配置只剩 `frontendDist` / `resources` 两个
   dist 编译期输入。〕
2. **dev 模式的端口是写死的**。Vite 代理目标硬编
   `process.env.ELYSPER_BACKEND_PORT || '8080'`（`elysper-web/vite.config.ts:6-7`），而
   生产端口策略是 `port = 0`（D4）。dev 分支若也用 0，Vite 会代理到一个没人监听的端口，
   页面永远离线 ⇒ dev（debug 构建）绑 `ELYSPER_BACKEND_PORT`（缺省 8080），release 才用 0。
3. **端口换了窗口必须重定向**。D10：重启优先复用原端口，bind 失败则重选 ⇒ 新一代 `Ready`
   的 `base_url` 可能换端口。只看"supervisor 活着"不比对 URL，窗口会停在一个已死端口上
   （白屏 / 永远离线）⇒ `states()` 的每条 `Ready` 都与窗口当前 URL 比对，不同则
   `WebviewWindow::navigate`。
4. **窗口关闭 ⇒ 孤儿 sidecar**。最后一个窗口关闭后应用默认直接退出；忘了 `shutdown()` 则
   `elysper-app-server` 连同它监管的 core 全部存活 ⇒ `RunEvent::Exit` 里显式
   `gateway.shutdown()`（`Gateway` 的 `Drop` 再兜一层），冒烟用 `pgrep` 验证无孤儿。
5. **`cargo deny` 第一次吃进 gtk / webkit 系 crate**。桌面 lockfile 带 `webkit2gtk-sys` /
   `soup3-sys` / `gtk-sys` 一族，root `deny.toml` 的 license 白名单未必全认。失败时的处置是
   **逐个审后加 `deny.toml` 条目**（这正是 C10 要补的审），不是删检查、不是 `--allow` 绕过。
6. **root 门禁必须保持全绿**（R1）。验收之一：root 的 `cargo metadata` workspace 成员里
   **没有** desktop（Task 1 Step 9 用命令证明），`make ci` 不受影响。

---

### Task 1: 独立 workspace 骨架与 dist 同步

**Files:**
- Create: `desktop/Cargo.toml`
- Create: `desktop/Makefile`
- Create: `desktop/src-tauri/Cargo.toml`
- Create: `desktop/src-tauri/build.rs`
- Create: `desktop/src-tauri/tauri.conf.json`
- Create: `desktop/src-tauri/tauri.bundle.conf.json`（打包期 overlay：externalBin 只在
  `make bundle` 时叠加——tauri-build 在编译期校验 externalBin，见 Review Focus #1 修订）
- Create: `desktop/src-tauri/src/main.rs`
- Create: `desktop/src-tauri/src/lib.rs`
- Create: `desktop/src-tauri/icons/`（`cargo tauri icon` 生成，入库）
- Create: `desktop/Cargo.lock`（首次 `cargo check` 生成，入库）
- Create: `desktop/app-icon.png`（图标源图，入库，一次性）
- Modify: `.gitignore`（追加两行，见 Step 6）

**Interfaces:**
- Consumes: `elysper-web` 的构建（`npm ci && npm run build`，输出 `elysper-web/dist`——
  Vite 缺省 outDir，`elysper-web/vite.config.ts` 无覆盖）；刀 1 的 `crates/elysper-gateway`
- Produces: 能 `cargo check` 的壳 crate `elysper-desktop`（窗口逻辑到 Task 3 才有）

- [ ] **Step 1: 写 workspace 根 `desktop/Cargo.toml`**

```toml
# The desktop shell's own workspace.
#
# Deliberately NOT a member of the repository root workspace (design
# D3 / C2): root `make lint-rust` runs `cargo clippy --all-features
# --all`, which would compile this GUI target — and webkit2gtk would
# become a prerequisite for every contributor running `make ci`. The
# accepted cost is a second lockfile and a second feature resolution
# (design §10 R-6); `desktop/Cargo.lock` is committed, matching the
# repo's position that application workspaces lock their deps.
[workspace]
members = ["src-tauri"]
resolver = "2"
```

- [ ] **Step 2: 写壳 crate 的 `desktop/src-tauri/Cargo.toml` 与 `build.rs`**

```toml
[package]
name = "elysper-desktop"
description = "The Tauri desktop shell: supervise the gateway sidecar and host its webview"
# Application crate behind a cross-workspace path dependency on a
# `publish = false` crate; nothing here belongs on a registry.
publish = false
version = "0.1.0"
edition = "2021"

[dependencies]
# The whole point of the shell is to own nothing: process supervision
# (spawn `serve`, wait for /readyz, backoff restart, reap) lives in
# the shared gateway crate (design D14). Cross-workspace path dep —
# see the workspace root comment for the accepted cost.
elysper-gateway = { path = "../../crates/elysper-gateway" }
# `wry` is the webview backend C1 verified locally (2.12.5).
tauri = { version = "2.12", features = ["wry"] }
# App boundary: errors bubble as context strings, nothing to match.
anyhow = "1.0"
# `tokio` and `serde_json` are on the approved whitelist (design
# knife 3) but stay out until something uses them: the supervisor
# already enforces D10's 30s readiness budget (`GatewayError::NotReady`),
# so a shell-side `tokio::time::timeout` would be a second clock, and
# nothing here speaks JSON.

[build-dependencies]
tauri-build = "2.0"
```

```rust
// desktop/src-tauri/build.rs
fn main() {
    tauri_build::build()
}
```

- [ ] **Step 3: 写 `tauri.conf.json` 与最小 `main.rs` / `lib.rs`**

```json
{
  "$schema": "https://schema.tauri.app/config/2",
  "productName": "Elysper",
  "version": "0.1.0",
  "identifier": "dev.elysper.desktop",
  "build": {
    "frontendDist": "../dist",
    "devUrl": "http://localhost:5173"
  },
  "app": {
    "windows": []
  },
  "bundle": {
    "active": true,
    "targets": "all",
    "icon": [
      "icons/32x32.png",
      "icons/128x128.png",
      "icons/128x128@2x.png",
      "icons/icon.icns",
      "icons/icon.ico"
    ],
    "resources": { "../dist": "dist" }
  }
}
```

要点（都来自已核实的 Tauri 2 语义）：

- `app.windows` 留空——窗口在 setup 里拿到 `base_url` 后**编程创建**（Task 3），不吃配置里的
  静态 URL。
- `frontendDist: "../dist"` 相对 `tauri.conf.json` 解析，即 `desktop/dist`（dist 副本，
  Step 5 产出）。
- `bundle.resources` 的 map 形式：key = 源路径（相对本配置文件），value = `$RESOURCE` 下的
  目标 ⇒ 打包后 `resource_dir()/dist` 就是 `serve --web-dir` 的运行期输入。
- `bundle.externalBin` **移入 `tauri.bundle.conf.json` overlay**（2026-11-10 修订：tauri-build
  2.7.4 在**编译期**无条件校验并拷贝 `externalBin`——缺文件即 `resource path
  binaries/elysper-app-server-<triple> doesn't exist`，本机已复现；留在基础配置会迫使
  每条 cargo 命令、包括 CI 的 fmt/clippy/test，先构建真 sidecar）。overlay 只在手工打包时
  叠加：`make bundle` = `frontend + sidecar` 后 `cargo tauri build --config
  tauri.bundle.conf.json`。打包输入文件名必须带 target triple
  （`binaries/elysper-app-server-x86_64-unknown-linux-gnu`，`make sidecar` 产出）；
  打包器把它拷到主程序旁时**去掉 triple 后缀**。该目录 gitignore（Step 6）。
- `devUrl` 记录 D2 的开发期契约（窗口开 5173，Vite 代理 `/rpc` 与 `/readyz`）。

```rust
// desktop/src-tauri/src/main.rs
//! The binary entry: thin, so the logic stays testable in the lib.
// Hides the extra console window on Windows release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    elysper_desktop::run()
}
```

```rust
// desktop/src-tauri/src/lib.rs
//! The desktop shell.
//!
//! Four jobs and nothing else (design knife 3):
//!
//! 1. start the sidecar (`elysper-app-server serve`) via the shared
//!    gateway supervisor and wait for `/readyz`;
//! 2. open one window at the gateway's `base_url` (external URL, so
//!    the page origin and the WebSocket origin are the same);
//! 3. when the sidecar dies, the supervisor restarts it — and if the
//!    new generation lands on a different port, navigate the window;
//! 4. when the window closes, reap the sidecar (no orphans).
//!
//! The shell speaks no protocol: JSON-RPC belongs to the gateway and
//! the SPA. The webview shows the same `elysper-web/dist` the
//! browser does.

#![forbid(unsafe_code)]

pub fn run() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running the desktop shell");
}
```

（Task 3 会重写 `run()`；此步的最小版本只求编译通过。）

- [ ] **Step 4: 生成图标（一次性，本机）**

准备一张 ≥1024×1024 的正方形 PNG 占位图存为 `desktop/app-icon.png`，然后：

Run: `cargo install tauri-cli --version "^2.0" --locked`（已装则跳过）
Expected: `cargo-tauri` 安装到 `~/.cargo/bin`（已装时 cargo 报 `binary already exists`，忽略）

Run: `cd desktop && cargo tauri icon app-icon.png`
Expected: 输出 `App icons generated to .../desktop/src-tauri/icons` 一类清单
（32x32.png、128x128.png、128x128@2x.png、icon.icns、icon.ico 等）。这些文件**入库**；
`app-icon.png` 也入库（换正式品牌图时可复现）。

- [ ] **Step 5: 写 `desktop/Makefile`（dist 怎么进桌面壳的唯一入口）**

```make
# The desktop shell's workspace-local entry points. The ROOT Makefile
# must NOT grow desktop targets: it runs --all-features --all over
# the root workspace only, and desktop/ is outside it by design (C2).
SHELL := /bin/bash
.DEFAULT_GOAL := help

help: ## List targets
	@grep -E '^[a-zA-Z_-]+:.*?## ' $(MAKEFILE_LIST) | awk 'BEGIN {FS = ":.*?## "}; {printf "  %-12s %s\n", $$1, $$2}'

frontend: ## Build elysper-web and sync its dist into desktop/dist
	cd ../elysper-web && npm ci && npm run build
	rm -rf dist && cp -R ../elysper-web/dist dist

sidecar: ## Copy the root-workspace app-server binary as the Tauri sidecar
	cargo build --manifest-path ../Cargo.toml -p elysper-app-server --release
	mkdir -p src-tauri/binaries
	cp ../target/release/elysper-app-server "src-tauri/binaries/elysper-app-server-$$(rustc -vV | sed -n 's/host: //p')"

bundle: frontend sidecar ## Package the release bundle (needs cargo-tauri; sidecar via overlay)
	cd src-tauri && cargo tauri build --config tauri.bundle.conf.json

check: frontend ## cargo check over this workspace only
	cargo check --all

test: frontend ## Unit tests, one crate (never --workspace)
	cargo test -p elysper-desktop --lib

lint: frontend ## clippy with the root make lint-rust's exact flags
	cargo clippy --all-targets --all-features --tests --all -- -D warnings

fmt: ## Format (root rustfmt.toml applies via upward search)
	cargo +nightly fmt --all

fmt-check: ## Check formatting without writing
	cargo +nightly fmt --all --check

deny: ## License / ban / source check for THIS workspace's lockfile (C10)
	cargo deny --workspace check licenses bans sources

run: frontend ## Dev run (debug build; window opens the Vite dev server)
	cargo run -p elysper-desktop

.PHONY: help frontend sidecar check test bundle lint fmt fmt-check deny run
```

`frontend` 是"dist 进桌面壳"的答案：**每次 cargo 之前**先 `npm run build`
（`tsc --noEmit && vite build`，产物 `elysper-web/dist`）再 `cp -R` 成 `desktop/dist`。
同一份副本服务三个消费者：`frontendDist`（编译期嵌入）、`bundle.resources`（打包期拷贝）、
`serve --web-dir`（运行期由壳传给 sidecar）。

- [ ] **Step 6: 追加 `.gitignore`**

在文件末尾（`crates/elysper-app-server/web-dist/` 那段之后）加：

```gitignore
# Desktop shell build inputs: the Tauri sidecar staging copy (the
# bundler wants a target-triple suffix) and tauri's generated schemas.
# `desktop/dist` is already covered by the repo-wide `dist` pattern.
desktop/src-tauri/binaries/
desktop/src-tauri/gen/
```

- [ ] **Step 7: 构建前端副本**

Run: `cd desktop && make frontend`
Expected: `npm ci` 安装清单 → `vite v8.3.1 building for production...` → `✓ built in …s`。
然后 `test -f desktop/dist/index.html && echo synced` 输出 `synced`。

- [ ] **Step 8: 首次编译，生成 lockfile**

Run: `cd desktop && cargo check`
Expected: 首次拉全 tauri / wry / tao 链 + gateway 传递树，数分钟，结束 `Finished \`dev\` profile [unoptimized + debuginfo] target(s) in …s`。
若报 `frontendDist` / asset 相关错误 ⇒ Step 7 没做或路径不对（回看 Review Focus #1）。

- [ ] **Step 9: 证明 root workspace 没吃进 desktop**

Run: `cargo metadata --no-deps --format-version 1 | jq -r '.workspace_members[]' | grep desktop || echo "not a root member: OK"`
Expected: `not a root member: OK`（root 的 44 个成员里没有 desktop ⇒ root 的
`make lint-rust` / `make ci` 编不到 GUI 目标，C2 成立）。

- [ ] **Step 10: 提交**

```bash
git add desktop .gitignore
git status --short   # 人工确认：desktop/Cargo.lock 在列，desktop/dist 与 binaries/ 不在
git commit -m "feat(desktop): Tauri 壳的独立 workspace 骨架与 dist 同步"
```

---

### Task 2: 选项解析与 sidecar 资源定位（纯函数）

**Files:**
- Create: `desktop/src-tauri/src/options.rs`
- Create: `desktop/src-tauri/src/tests.rs`
- Modify: `desktop/src-tauri/src/lib.rs`（挂 `mod options; mod tests;`）

**Interfaces:**
- Consumes: TUI 的 CLI 先例（`crates/elysper-tui/src/main.rs:54-64`：`--directory` / `--profile` /
  `--app-server` + env `ELYSPER_APP_SERVER`）；`serve` 的参数面
  （`crates/elysper-app-server/src/main.rs:126-152`）
- Produces: `Options`（壳的全部输入）与三个纯解析函数；`GatewayOptions` 的组装在 Task 3

不引 clap：白名单五个依赖，四个旗标不值得第六个。解析是纯函数（`&[String]` 进、
`Result` 出），便于单测。

```rust
// desktop/src-tauri/src/options.rs 的公开面
pub struct Options {
    /// 工作区目录（透传 `serve --directory`，D10：默认进程当前目录）。
    pub directory: PathBuf,
    pub profile: Option<PathBuf>,
    pub bundle: Option<PathBuf>,
    pub patch: Option<PathBuf>,
    /// `--app-server` 或 env `ELYSPER_APP_SERVER`。
    pub app_server: Option<PathBuf>,
    /// `--web-dir`：显式覆盖 dist 的定位。
    pub web_dir: Option<PathBuf>,
    /// `--dev-url`：调试构建里窗口加载的地址（缺省 http://localhost:5173，D2）。
    pub dev_url: Option<String>,
}

/// 已知的环境输入，测试可注入。
pub struct Env {
    pub app_server: Option<String>,
    pub backend_port: Option<String>,
}

impl Options {
    pub fn parse_from(args: &[String], env: &Env) -> Result<Self, String>;
}

/// 二进制定位顺序（D10）：flag/env → 主程序同目录（打包产物里
/// externalBin 落在主程序旁且不带 triple 后缀）→ PATH。
pub fn app_server_candidates(flag: Option<&Path>, exe_dir: Option<&Path>) -> Vec<PathBuf>;

/// dist 定位顺序：`--web-dir` → `resource_dir/dist`（打包产物）→
/// `../../elysper-web/dist`（dev：`cargo run` 的 cwd 是 `desktop/src-tauri`）。
pub fn web_dir_candidates(flag: Option<&Path>, resource_dist: Option<&Path>) -> Vec<PathBuf>;

/// 取第一个"存在"的候选（`exists` 注入，测试不用碰磁盘）。
pub fn first_existing<'a>(candidates: &'a [PathBuf], exists: impl Fn(&Path) -> bool) -> Option<&'a PathBuf>;

/// dev（debug 构建）绑 Vite 代理的目标端口；release 用 0（D4，由 supervisor 选）。
pub fn gateway_port(is_release: bool, backend_port: Option<&str>) -> Result<u16, String>;
```

- [ ] **Step 1: 写失败的测试**

`desktop/src-tauri/src/tests.rs`：

```rust
use crate::options::{Env, Options, app_server_candidates, first_existing, gateway_port, web_dir_candidates};
use std::path::PathBuf;

#[test]
fn flags_parse_and_defaults_hold() {
    let options = Options::parse_from(
        &["--directory".into(), "/tmp/ws".into(), "--profile".into(), "p.toml".into()],
        &Env { app_server: None, backend_port: None },
    )
    .expect("parses");
    assert_eq!(options.directory, PathBuf::from("/tmp/ws"));
    assert_eq!(options.profile, Some(PathBuf::from("p.toml")));
    assert!(options.app_server.is_none());
}

#[test]
fn an_unknown_flag_is_rejected() {
    let error = Options::parse_from(&["--nope".into()], &Env { app_server: None, backend_port: None }).expect_err("rejected");
    assert!(error.contains("--nope"));
}

#[test]
fn the_app_server_resolution_order_is_flag_then_exe_dir_then_path() {
    let flag = PathBuf::from("/opt/server");
    let exe_dir = Some(PathBuf::from("/usr/lib/elysper"));
    let candidates = app_server_candidates(Some(&flag), exe_dir.as_deref());
    assert_eq!(candidates[0], flag);
    assert_eq!(candidates[1], PathBuf::from("/usr/lib/elysper/elysper-app-server"));
    assert_eq!(candidates[2], PathBuf::from("elysper-app-server"));
}

#[test]
fn the_web_dir_resolution_order_is_flag_then_resource_then_dev_checkout() {
    let flag = PathBuf::from("/tmp/dist");
    let resource = PathBuf::from("/usr/lib/elysper/resources/dist");
    let candidates = web_dir_candidates(Some(&flag), Some(&resource));
    assert_eq!(candidates[0], flag);
    assert_eq!(candidates[1], resource);
    assert!(candidates[2].ends_with("elysper-web/dist"));
}

#[test]
fn first_existing_picks_the_first_candidate_that_exists() {
    let candidates = vec![PathBuf::from("/a"), PathBuf::from("/b"), PathBuf::from("/c")];
    let hit = first_existing(&candidates, |p| p == Path::new("/b")).expect("some");
    assert_eq!(hit, PathBuf::from("/b"));
}

#[test]
fn dev_binds_the_vite_proxy_port_and_release_binds_zero() {
    assert_eq!(gateway_port(true, None).unwrap(), 0);
    assert_eq!(gateway_port(false, None).unwrap(), 8080);
    assert_eq!(gateway_port(false, Some("8081")).unwrap(), 8081);
    assert!(gateway_port(false, Some("not-a-port")).is_err());
}
```

（`Env` 一律显式构造，测试不碰进程环境；`app_server_candidates` 的 PATH 项是相对名，
不测存在性——顺序本身就是被测行为。）

- [ ] **Step 2: 跑测试，确认失败**

Run: `cd desktop && make test`
Expected: FAIL（`unresolved import crate::options`）——同时验证了 `make test` 会先跑
`frontend`（Review Focus #1 的顺序保障）。

- [ ] **Step 3: 实现 `options.rs`**

要点：

- `parse_from` 手写循环：认 `--directory` / `--profile` / `--bundle` / `--patch` /
  `--app-server` / `--web-dir` / `--dev-url`（每个跟一个值）、`--help`（打印用法后返回
  `Err` 让上层退出）、未知旗标报错并回显旗标名。env 只兜 `app_server`
  （`ELYSPER_APP_SERVER`，与 TUI 同名同义）。
- `app_server_candidates` / `web_dir_candidates` 只**排顺序**不做 IO；IO（`Path::exists`、
  `std::env::current_exe()`、`app.path().resource_dir()`）留给调用方在 Task 3 组装。
- `gateway_port`：`is_release == true` → `Ok(0)`；否则解析 `backend_port`
  （`.parse::<u16>()`），`None` → `8080`，解析失败 → `Err`（带原值）。
- 全部函数注释说明顺序依据（D10 / D2 / vite.config.ts 的代理目标）。

- [ ] **Step 4: 跑测试，确认通过**

Run: `cd desktop && make test`
Expected: PASS（`test result: ok. 6 passed; 0 failed`）

- [ ] **Step 5: 格式与 lint**

Run: `cd desktop && make fmt && make lint`
Expected: 均无 warning / error（clippy 结束无输出，退出码 0）

- [ ] **Step 6: 提交**

```bash
git add desktop/src-tauri desktop/Cargo.lock
git commit -m "feat(desktop): 壳的选项解析与 sidecar 资源定位"
```

---

### Task 3: supervisor 接线与窗口生命周期

**Files:**
- Modify: `desktop/src-tauri/src/lib.rs`（重写 `run()`，挂窗口与状态监听）
- Create: `desktop/src-tauri/src/window_nav.rs`（纯决策函数）
- Modify: `desktop/src-tauri/src/tests.rs`（补 `next_window_url` 的测试）

**Interfaces:**
- Consumes: 刀 1 的 `Gateway::start(GatewayOptions) -> BoxFuture<'static, Result<Gateway, GatewayError>>`、
  `Gateway::base_url(&self) -> &str`、`Gateway::states(&self) -> futures::channel::mpsc::UnboundedReceiver<GatewayState>`、
  `Gateway::shutdown(&self)`；`GatewayOptions` 字段
  `executable / bind / port / directory / profile / bundle / patch / web_dir`；
  `GatewayState::{Starting, Ready { base_url, ws_url }, Restarting { attempt }, Failed { reason }}`
  （签名引自 `docs/design/2026-11-09-gateway-crate-plan.md` Task 2）
- Produces: 可运行的桌面壳（设计刀 3 验收的行为全部落在这一个 Task）

- [ ] **Step 1: 写失败的测试（URL 决策）**

`window_nav.rs` 的公开面只有一个函数；`tests.rs` 追加：

```rust
use crate::window_nav::next_window_url;

#[test]
fn a_ready_with_a_new_port_navigates_the_window() {
    let url = next_window_url(Some("http://127.0.0.1:8080/"), None, "http://127.0.0.1:8081/");
    assert_eq!(url.as_deref(), Some("http://127.0.0.1:8081/"));
}

#[test]
fn a_ready_with_the_same_url_does_nothing() {
    assert_eq!(next_window_url(Some("http://127.0.0.1:8080/"), None, "http://127.0.0.1:8080/"), None);
}

#[test]
fn a_dev_window_stays_on_the_dev_server() {
    // dev 会话里窗口在 Vite 上；sidecar 换端口由代理与前端重连消化（D2）。
    assert_eq!(next_window_url(Some("http://localhost:5173/"), Some("http://localhost:5173/"), "http://127.0.0.1:8081/"), None);
}

#[test]
fn a_first_ready_without_a_window_url_navigates() {
    assert_eq!(next_window_url(None, None, "http://127.0.0.1:8080/").as_deref(), Some("http://127.0.0.1:8080/"));
}
```

```rust
// desktop/src-tauri/src/window_nav.rs 的公开面
/// 新一代 `Ready` 到达时窗口是否需要换地址。
/// `current` = 窗口当前加载的 URL（壳自己记账）；`dev_url` = 本会话的
/// 开发地址（若窗口正在显示它则不动，D2）；`base_url` = 新的网关地址。
/// 返回 `Some(url)` 表示要 `WebviewWindow::navigate`。
pub fn next_window_url(current: Option<&str>, dev_url: Option<&str>, base_url: &str) -> Option<String>;
```

Run: `cd desktop && make test`
Expected: FAIL（`unresolved import crate::window_nav`）

- [ ] **Step 2: 实现 `window_nav.rs`**

三条规则按序：`current == dev_url` → `None`；`current == Some(base_url)` → `None`；
其余（含 `current == None`）→ `Some(base_url.into())`。

Run: `cd desktop && make test`
Expected: PASS（`10 passed`——Task 2 的 6 条 + 本步 4 条）

- [ ] **Step 3: 实现 `lib.rs` 的 `run()`**

骨架（Tauri 2 语义全部已核实：`setup` 里 `tauri::async_runtime::block_on`、
`webview::WebviewWindowBuilder` + `WebviewUrl::External`、`WebviewWindow::navigate`、
`RunEvent::Exit`、`Manager::manage` / `try_state` / `get_webview_window` / `path()`）：

```rust
pub fn run() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let env = options::Env {
        app_server: std::env::var("ELYSPER_APP_SERVER").ok(),
        backend_port: std::env::var("ELYSPER_BACKEND_PORT").ok(),
    };
    let options = options::Options::parse_from(&args, &env)
        .unwrap_or_else(|usage| { eprintln!("{usage}"); std::process::exit(2); });

    tauri::Builder::default()
        .setup(move |app| {
            // 1. 定位 sidecar 与 dist（纯函数排顺序，这里做 IO）。
            let exe_dir = std::env::current_exe().ok().and_then(|p| p.parent().map(Path::to_path_buf));
            let executable = options::first_existing(
                &options::app_server_candidates(options.app_server.as_deref(), exe_dir.as_deref()),
                |p| p.is_file(),
            ).cloned().context("no elysper-app-server found: pass --app-server or add it to PATH")?;
            let resource_dist = app.path().resource_dir().ok().map(|d| d.join("dist"));
            let web_dir = options::first_existing(
                &options::web_dir_candidates(options.web_dir.as_deref(), resource_dist.as_deref()),
                |p| p.is_dir(),
            ).cloned().context("no web dist found: run `make -C desktop frontend` or pass --web-dir")?;

            // 2. 组装 GatewayOptions 并限时等待就绪（D10：30s）。
            let is_release = !cfg!(debug_assertions);
            let port = options::gateway_port(is_release, env.backend_port.as_deref())?;
            let gateway_options = elysper_gateway::GatewayOptions {
                executable, bind: "127.0.0.1".into(), port,
                directory: options.directory.clone(), profile: options.profile.clone(),
                bundle: options.bundle.clone(), patch: options.patch.clone(),
                web_dir: Some(web_dir),
            };
            let gateway = tauri::async_runtime::block_on(async {
                tokio::time::timeout(READY_TIMEOUT, elysper_gateway::Gateway::start(gateway_options))
                    .await
                    .map_err(|_| anyhow!("gateway not ready within 30s (design D10)"))??
            }).map_err(|error| anyhow!("failed to start the gateway sidecar: {error}"))?;

            // 3. 开窗：release 加载 base_url；debug 加载 dev 地址（D2）。
            let dev_url = options.dev_url.clone().or_else(|| (!is_release).then(|| DEV_URL.to_string()));
            let url = dev_url.clone().unwrap_or_else(|| gateway.base_url().to_string());
            tauri::webview::WebviewWindowBuilder::new(
                app, "main".to_string(),
                tauri::WebviewUrl::External(url.parse().context("window url")?),
            ).title("Elysper").build().context("failed to open the window")?;

            // 4. 记账 + 状态监听：新一代 Ready 且端口变了 → navigate。
            let current = std::sync::Mutex::new(url);
            app.manage(GatewayHandle(gateway));
            let handle = app.handle().clone();
            let mut states = app.state::<GatewayHandle>().0.states();
            tauri::async_runtime::spawn(async move {
                use futures::StreamExt;
                while let Some(state) = states.next().await {
                    let elysper_gateway::GatewayState::Ready { base_url, .. } = state else { continue };
                    let Some(next) = window_nav::next_window_url(
                        current.lock().unwrap().as_deref(), dev_url.as_deref(), &base_url,
                    ) else { continue };
                    if let Some(window) = handle.get_webview_window("main") {
                        if let Ok(parsed) = next.parse() {
                            let _ = window.navigate(parsed);
                            *current.lock().unwrap() = Some(next);
                        }
                    }
                }
            });
            Ok(())
        })
        .build(tauri::generate_context!())
        .expect("error while building the desktop shell")
        .run(|app, event| {
            if let tauri::RunEvent::Exit = event {
                if let Some(handle) = app.try_state::<GatewayHandle>() {
                    handle.0.shutdown(); // 显式回收；Gateway 的 Drop 再兜一层
                }
            }
        });
}
```

配套常量与类型（同文件）：

```rust
/// 就绪等待上限（设计 D10）。
const READY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);
/// 开发期窗口地址（D2）：Vite dev server，已代理 `/rpc`（ws）与 `/readyz`。
const DEV_URL: &str = "http://localhost:5173";

/// Tauri managed state：让 Exit 事件够得着 supervisor。
struct GatewayHandle(elysper_gateway::Gateway);
```

实现注意：

- `GatewayOptions` 是刀 1 的字段面；若刀 1 落地时构造方式不同（例如提供了
  `GatewayOptions::new(executable, directory)` + builder 式 `with_*`），按实际签名组装，
  语义不变。
- `bind` 固定 `127.0.0.1`（桌面壳没有容器场景，`serve --bind` 的 env 覆盖不需要）。
- setup 闭包返回 `anyhow::Error` 时 Tauri 会把应用带崩——`setup` 的 `Box<dyn Error>` 返回
  与 `anyhow::Error` 兼容（`anyhow::Error: Into<Box<dyn Error>>`），错误信息会进 panic 输出；
  这是可接受的 v1 行为（设计未要求错误窗口）。
- **不要**在壳里读网关 stdout 或解析日志取端口——`Gateway::base_url()` 就是端口真源（D4）。
- **2026-11-10 实施修订（骨架与刀 1 实际公开面的差异，以实现为准）**：
  1. 刀 1 的 supervisor **自带** 30s 就绪预算（`READY_TIMEOUT` → `GatewayError::NotReady`），
     壳不再包第二层 `tokio::time::timeout` ⇒ 直接依赖清单里**没有 tokio**（白名单是上界）。
  2. 白名单（D14）不含 `futures`，而骨架里 `use futures::StreamExt` 需要它——TUI 为此
     加了直接依赖，桌面壳不加：状态监听改为普通线程 + `UnboundedReceiver::try_recv()`
     （固有方法，100ms 轮询）。`try_next` 已 deprecated 且区分 `Empty`/`Closed` 需要点名
     futures 的错误类型；sender 与 managed state 同寿命，`Closed` 实际不会先于进程退出到达。
  3. 窗口 URL 记账（骨架里的 `Mutex<Option<String>>`）只有监听线程读写 ⇒ 线程**独占**
     该状态，无锁（也符合本仓 parking_lot 规则与白名单的双重约束）。
  4. `GatewayOptions` 字段全 pub：按 struct 字面量组装（同语义）。
  5. `web_dir_candidates` 追加 `../../elysper-web/dist`：`make run`（cwd=desktop/）与
     `cargo tauri dev`（cwd=src-tauri/）两种工作目录都要命中 dev checkout。

- [ ] **Step 4: 编译与单测**

Run: `cd desktop && make check && make test && make lint`
Expected: `Finished` / `test result: ok. 10 passed` / clippy 0 warning

- [ ] **Step 5: 格式化**

Run: `cd desktop && make fmt && make fmt-check`
Expected: 均无 diff

- [ ] **Step 6: 冒烟（dev 链路，两个终端）**

终端 1：`cd elysper-web && npm run dev`
终端 2：

```bash
cargo build -p elysper-app-server
cd desktop && ELYSPER_APP_SERVER=../target/debug/elysper-app-server cargo run -p elysper-desktop
```

Expected（人工核对，本机 C1 环境）：窗口打开并加载 `http://localhost:5173`，会话列表可见
（Vite 把 `/rpc` 代理到 sidecar 的 8080）；另开终端
`curl -s http://127.0.0.1:8080/readyz` 返回 `{"status":"ok"}`。

- [ ] **Step 7: 冒烟（release 链路，不经 Vite）**

```bash
cargo build -p elysper-app-server --release
cd desktop && ELYSPER_APP_SERVER=../target/release/elysper-app-server cargo run -p elysper-desktop --release
```

Expected（人工核对）：窗口直接加载 sidecar 的 `base_url`（`http://127.0.0.1:<port>/`，
端口由 supervisor 选定），同一份 SPA；发一轮对话、审批在窗口内应答（设计刀 3 验收）。

- [ ] **Step 8: 冒烟（sidecar 死亡 → 重启 → 前端自愈）**

壳运行中：

```bash
pkill -f 'elysper-app-server serve' && sleep 8
```

Expected（人工核对，对应设计刀 3 验收）：窗口内出现**既有**离线徽标
（`useServerHealth` 轮询失败），数秒内 supervisor 以 1s→5s 退避拉起新一代
（优先复用原端口 ⇒ 窗口无需重定向），前端 WS 重连后徽标恢复。若端口被占而换新端口，
窗口应自动 `navigate`（Review Focus #3）。

- [ ] **Step 9: 冒烟（无孤儿进程）**

关闭桌面窗口后：

```bash
pgrep -af elysper-app-server || echo "no orphans"
```

Expected: `no orphans`（`RunEvent::Exit` → `shutdown()` 生效；若仍有进程，回查 Step 3 的
`.run(...)` 回调与 `GatewayHandle` 的挂载）。

- [ ] **Step 10: 提交**

```bash
git add desktop/src-tauri desktop/Cargo.lock
git commit -m "feat(desktop): 接线网关 supervisor，开窗、重定向与回收"
```

---

### Task 4: CI workflow（apt 依赖 + fmt / clippy / test / cargo deny）

**Files:**
- Create: `.github/workflows/desktop.yml`

**Interfaces:**
- Consumes: `desktop/Makefile` 的 `frontend` / `fmt-check` / `lint` / `test` / `deny` 目标；
  仓库根 `deny.toml`（唯一许可证策略真源）
- Produces: desktop workspace 的独立 CI 入口（root `make ci` 一行不改）

- [ ] **Step 1: 写 workflow**

```yaml
name: desktop

# The desktop shell's own gates. desktop/ is deliberately outside the
# root workspace (design C2/D3): root `make lint-rust`'s --all-features
# must not compile GUI targets, so THIS workflow carries fmt / clippy /
# test for the shell — plus `cargo deny`, because the root
# `make check-deny` only sees the root workspace's lockfile (C10).
# The apt package names are Ubuntu's; contributors on Arch install
# webkit2gtk-4.1 via pacman (C1) and run `make -C desktop lint`.

on:
  push:
    branches: [master]
    paths:
      - "desktop/**"
      - "crates/**"
      - "elysper-web/**"
      - "Cargo.toml"
      - "deny.toml"
      - ".github/workflows/desktop.yml"
  pull_request:
    branches: [master]
    paths:
      - "desktop/**"
      - "crates/**"
      - "elysper-web/**"
      - "Cargo.toml"
      - "deny.toml"
      - ".github/workflows/desktop.yml"
  workflow_dispatch:

concurrency:
  group: desktop-${{ github.ref }}
  cancel-in-progress: true

env:
  CARGO_TERM_COLOR: always
  RUST_BACKTRACE: 1

jobs:
  shell:
    name: fmt + clippy + test + cargo deny
    runs-on: ubuntu-latest
    timeout-minutes: 40
    steps:
      - name: Checkout
        uses: actions/checkout@v4

      # Tauri's Linux build needs the webkit2gtk stack. These are the
      # apt (Ubuntu) names from the design doc's knife 3 scope; the
      # shell crate is the only place in the repo that needs them.
      - name: Install webkit system dependencies
        run: |
          sudo apt-get update
          sudo apt-get install -y \
            libwebkit2gtk-4.1-dev \
            libjavascriptcoregtk-4.1-dev \
            libsoup-3.0-dev \
            libgtk-3-dev \
            librsvg2-dev \
            patchelf

      - name: Install stable toolchain
        uses: dtolnay/rust-toolchain@stable

      - name: Install nightly toolchain (rustfmt.toml requires nightly fmt)
        uses: dtolnay/rust-toolchain@nightly
        with:
          components: rustfmt

      - name: Setup Node.js
        uses: actions/setup-node@v4
        with:
          node-version: 20
          cache: npm
          cache-dependency-path: elysper-web/package-lock.json

      # tauri's codegen embeds frontendDist at compile time, so the
      # dist copy must exist before ANY cargo command (Review Focus
      # #1). This mirrors `make -C desktop frontend`.
      - name: Build the web front-end and sync the dist copy
        run: |
          cd elysper-web && npm ci && npm run build
          rm -rf ../desktop/dist && cp -R dist ../desktop/dist

      - name: Cache Cargo
        uses: actions/cache@v4
        with:
          path: |
            ~/.cargo/registry
            ~/.cargo/git
            desktop/target
          key: ${{ runner.os }}-cargo-desktop-${{ hashFiles('desktop/Cargo.lock') }}
          restore-keys: |
            ${{ runner.os }}-cargo-desktop-
            ${{ runner.os }}-cargo-

      - name: cargo fetch
        working-directory: desktop
        run: cargo fetch

      - name: fmt
        working-directory: desktop
        run: cargo +nightly fmt --all --check

      - name: clippy
        working-directory: desktop
        run: cargo clippy --all-targets --all-features --tests --all -- -D warnings

      - name: test
        working-directory: desktop
        run: cargo test -p elysper-desktop --lib

      # Same install pattern as rust-quality.yml. The check runs over
      # the DESKTOP lockfile (the root one never sees tauri's tree),
      # with the repo's single deny.toml as the policy (C10).
      - name: Install cargo-deny
        run: cargo install --locked cargo-deny

      - name: cargo deny (licenses / bans / sources)
        run: cargo deny --manifest-path desktop/Cargo.toml --config deny.toml --workspace check licenses bans sources
```

要点：

- `paths` 过滤包含 `crates/**`：桌面壳经 path 依赖编到 `elysper-gateway` →
  `elysper-app-server` → facade 的整棵树，root 侧的 crate 改动同样影响桌面构建。
- `cargo deny` 的三个全局选项（`--manifest-path` / `--config` / `--workspace`）必须写在
  子命令 `check` **之前**（cargo-deny 的参数布局，2026-11-09 本机 `cargo deny --help` 核实）。
- **不**在 CI 里跑 `cargo tauri build`（打包产物不是本轮验收，且多平台 bundling 超出
  ubuntu-latest 的职责）；`tauri build` 的手工入口在 Task 1 的 `make sidecar` +
  `cargo tauri build`。

- [ ] **Step 2: 本机校验 YAML 与 action 语义**

Run: `actionlint .github/workflows/desktop.yml`
Expected: 无输出（0 问题；本机已装 actionlint，2026-11-09 核实）

Run: `python3 -c "import yaml; yaml.safe_load(open('.github/workflows/desktop.yml'))"`
Expected: 无输出（YAML 可解析；本机 pyyaml 可用）

- [ ] **Step 3: 本机跑一次 deny（吃进 gtk/webkit 树）**

Run: `cd desktop && make deny`
Expected: 两种结局之一——
(a) `licenses / bans / sources` 三节全绿（`ok` / `0 errors`）；
(b) license 白名单第一次见到 gtk 系 crate 报 `license not allowed`，输出里逐个点名 crate。
  处置（Review Focus #5）：逐个查证许可（`cargo deny --manifest-path desktop/Cargo.toml list licenses`），
  属于 OSI 常见宽松许可的加进根 `deny.toml` 的 `[licenses] allow`（本变更一并提交），
  拿不准的停下来问人——**不许**用 `--allow` 旗标绕过或删检查。

- [ ] **Step 4: root 门禁不受影响**

Run: `make check-deny && cargo metadata --no-deps --format-version 1 | jq -r '.workspace_members[]' | grep desktop || echo "not a root member: OK"`
Expected: deny `ok`；`not a root member: OK`

- [ ] **Step 5: 提交**

```bash
git add .github/workflows/desktop.yml deny.toml
git commit -m "ci(desktop): 桌面壳独立门禁（webkit 依赖 + fmt/clippy/test/cargo deny）"
```

（`deny.toml` 仅在 Step 3 走到结局 (b) 时才有 diff。）

---

### Task 5: 仓库同步（gateway README · AGENTS §1/§3.6 · CHANGELOG）

**Files:**
- Modify: `crates/elysper-gateway/README.md`
- Modify: `AGENTS.md`（§1 环境段 + §3.6 门禁表）
- Modify: `CHANGELOG.md`（`[Unreleased]`）

**Interfaces:**
- Consumes: 前四个任务的全部产物
- Produces: 入库文档与现状一致（`make check-claim-language` 与人工 review 的依据）

- [ ] **Step 1: `crates/elysper-gateway/README.md` 补桌面壳一节**

在"怎么用"之后加一小节（标题 `## The desktop shell`，英文，照该 README 既有风格）：

- 桌面壳（`desktop/`，Tauri 2，独立 workspace）只消费 `supervisor`：`Gateway::start` →
  `base_url()` → 开窗；它**不用** `client`，因此也不依赖 `tokio-tungstenite`（D14）。
- 壳的二进制定位与端口策略指向 `desktop/src-tauri/src/options.rs` 的模块文档。
- 壳的门禁在 `.github/workflows/desktop.yml`，不在 root `make ci`。

- [ ] **Step 2: `AGENTS.md` §1 加 desktop 条目**

在刀 1 加的 `elysper-gateway` 条目之后（若刀 1 未合入则在 `elysper-tui` 条目
`AGENTS.md:20` 之后）加一条，格式照邻居：

> - [desktop/](desktop/) Tauri 2 桌面壳（**独立 workspace**，不是 root 成员，D3/C2）：只做四件事——
>   经 `crates/elysper-gateway` 的 supervisor 拉起 `elysper-app-server serve` sidecar、等
>   `/readyz`、开窗（`WebviewUrl::External(base_url)`）、sidecar 死亡重启并把窗口重定向 /
>   窗口关闭回收（无孤儿）。壳不讲协议（不引 `tokio-tungstenite`，D14）；webkit 系系统依赖
>   因此不进 `make ci`；其 fmt / clippy / test / cargo deny 在 `.github/workflows/desktop.yml`。
>   入口 `make -C desktop help`。

- [ ] **Step 3: `AGENTS.md` §3.6 门禁表加一行**

在 `make check-deny` 那行之后加：

> | `desktop`（CI job） | 桌面壳独立 workspace 的 fmt / clippy(`-D warnings`) / test / `cargo deny`（`--manifest-path desktop/Cargo.toml --config deny.toml`，补 C10：root 的 `check-deny` 只看 root lockfile）；**不进** `make ci`——`--all-features` 不得把 GUI 目标编进每个贡献者的门禁（C2）；入口 `.github/workflows/desktop.yml`（ubuntu-latest，apt 装 `libwebkit2gtk-4.1-dev` 等；本机 Arch 用 pacman 装 `webkit2gtk-4.1`，C1） |

本计划**不新增** `scripts/*.sh` 门禁脚本，故 `make lint-sh` 与 `shellcheck.yml` 的两份清单
**不动**（AGENTS.md §3.6 的同步义务不触发）。

- [ ] **Step 4: `CHANGELOG.md` 的 `[Unreleased]` 加一条**

在 `[Unreleased]` 下现有的 `### Added` 小节里加（沿用该节的粗体导语风格）：

```markdown
- **Desktop 入口（刀 3）**：新增 `desktop/`（Tauri 2 壳，独立 workspace、不进 root
  workspace，webkit 系系统依赖因此不进 `make ci`）——经 `crates/elysper-gateway` 拉起
  `elysper-app-server serve` sidecar、等 `/readyz`、开窗加载与浏览器同一份
  `elysper-web/dist`；sidecar 死亡自动重启并重定向窗口，窗口关闭回收 sidecar。配套
  `.github/workflows/desktop.yml`（apt 装 webkit 依赖 + fmt / clippy / test +
  `cargo deny`，补齐 root `cargo deny` 看不到桌面 lockfile 的缺口）。
```


- [ ] **Step 5: 核对 root 门禁与文档一致性**

Run: `make check-claim-language`
Expected: `OK:`（新加的措辞没有绝对化的假声明）

Run: `cd desktop && make fmt-check && make lint && make test`
Expected: 三条全绿（文档改动不该碰代码，这是确认不是修复）

- [ ] **Step 6: 提交**

```bash
git add crates/elysper-gateway/README.md AGENTS.md CHANGELOG.md
git commit -m "docs: 登记桌面壳与其独立 CI 门禁"
```

---

## 附：与设计文档的对齐清单

| 设计锚点 | 本计划的落点 |
|---|---|
| C2（`--all-features` 不编 GUI） | Task 1 Step 1 的 workspace 隔离 + Step 9 的 `cargo metadata` 证明 |
| C10（deny 只覆盖 root） | Task 4 的 `cargo deny --manifest-path desktop/Cargo.toml --config deny.toml` |
| D1（sidecar = `serve`，壳不讲协议） | Task 3 Step 3 只用 `Gateway::start` / `base_url()` |
| D2（一套前端、壳不碰前端） | 全程无 `elysper-web` 改动；dev 走 Vite 代理（Task 3 Step 6） |
| D3（三段代码位置） | `desktop/` 独立 workspace（Task 1），path 依赖 `../../crates/elysper-gateway` |
| D4（端口由宿主选） | release `port = 0` 交给 supervisor；dev 绑 `ELYSPER_BACKEND_PORT`/8080（Review Focus #2） |
| D10（二进制定位 / 30s / 退避 / 端口复用） | Task 2 的候选顺序 + Task 3 的 `READY_TIMEOUT` 与 `next_window_url` |
| D14（壳不引 `tokio-tungstenite`） | Task 1 Step 2 的依赖清单（白名单上界） |
| 刀 3 验收（离线徽标自愈、无孤儿） | Task 3 Step 8 / Step 9 的手工冒烟 |
