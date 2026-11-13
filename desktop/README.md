# uva desktop

桌面壳：一个 Tauri 2 外壳，把 `uva-app-server serve` 作为 sidecar 拉起，
然后打开**与浏览器入口同一份**的 SPA。壳只做四件事——拉起 sidecar、等
`/readyz`、开窗、sidecar 死亡时重启并把窗口重定向；所有业务能力都经由
网关的 `/rpc` WebSocket 获得。

## 快速上手

```bash
# 前置：webkit2gtk-4.1（Arch: pacman -S webkit2gtk-4.1；Ubuntu/CI 见下方 apt 清单）
cd desktop
make frontend          # 构建前端并把 uva-web/dist 同步到 desktop/dist（一切 cargo 命令前必跑）
make run               # debug 起窗（dev 指向 Vite 5173，release 直连网关 base_url）
```

需要指到 server 二进制时：`UVA_APP_SERVER=/path/to/uva-app-server`，
否则按 `--app-server` / `PATH` 顺序解析。

## 目标

| 目标 | 作用 |
|---|---|
| `make frontend` | `npm run build` 并把 `uva-web/dist` 拷进 `desktop/dist`（tauri codegen 编译期就要它存在） |
| `make sidecar` | 把 root workspace 的 `uva-app-server` 二进制复制为 sidecar |
| `make run` | debug 起窗 |
| `make check` / `test` / `lint` / `fmt-check` / `deny` | 本 workspace 的门禁 |
| `make bundle` | 打 release 包（`frontend` + `sidecar` 后经 overlay 注入 `externalBin`） |

## 结构

- `src-tauri/` —— 壳 crate：`options.rs`（纯函数选项解析，无 clap）、
  `window_nav.rs`（窗口导航规则）、`run()`（supervisor 接线与生命周期）。
  依赖白名单：`tauri` + `uva-gateway`（path）+ `tokio` + `serde_json` + `anyhow`。
- `dist/` —— `make frontend` 的产物（单一前端：与浏览器同一份，构建一次两处投递）。
- 本 workspace **独立于 root workspace**（webkit 系统依赖不应成为每个贡献者跑
  `make ci` 的前提）；CI 在 `.github/workflows/desktop.yml`
  （ubuntu-latest，apt 装 `libwebkit2gtk-4.1-dev libjavascriptcoregtk-4.1-dev
  libsoup-3.0-dev libgtk-3-dev librsvg2-dev patchelf`），并单独跑一次
  `cargo deny check licenses bans sources`。

## 行为契约

- sidecar 崩溃：壳经 `uva-gateway` 退避重启；窗口内出现 SPA 既有的离线徽标并自动恢复。
- 关闭窗口：sidecar 与其 `core` 子进程全部回收，无孤儿进程。
- 同一份 `dist`：桌面与浏览器零差别（`frontendDist` 与 `serve --web-dir` 同源）。
