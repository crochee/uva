# elysper-web

浏览器入口的 SPA：React + Vite。它与终端、桌面壳渲染**同一套界面契约**
（设计文档 §6.1），与 `elysper-app-server serve` 通过网关的 `/rpc` WebSocket
说 JSON-RPC 2.0——同一份方法名与信封，桌面壳打包的也是这份 `dist`。

## 快速上手

```bash
cd elysper-web
npm ci
npm run dev          # :5173，/rpc 与 /readyz 代理到 :8080 的网关
npm run build        # 产出 dist/（桌面壳与 embed-web 共用这一份）
```

仓库根的 `make dev` 会同时起后端与前端；`make check-web` 跑类型检查
（`tsc --noEmit`）、eslint 与 prettier——**本项目不保留前端 e2e 套件**，
前端行为按 AGENTS.md §4.2 用 Playwright MCP 驱动真实页面验证。

## 与后端的接线

- 所有请求走 JSON-RPC 2.0，方法名与 `crates/elysper-app-server/src/rpc/methods/`
  的注册表一一对应；`src/api/client.ts` 保留旧的 REST 路径**映射表**（设置分区
  仍在用），它不是页面路由。
- 页面路由只有两条：`/chat*`（对话优先）与 `/settings/:section`（设置九分区）；
  其余路径落到 `/chat`。

## UI 树（插槽）

`ui-bundle` 行声明插槽（`chat.sidebar`、`session.detail.extensions` …），服务器在
`ui/schema` 的响应里同时给出 `slots` 与 `schemas`。前端只有**一个取数方**
（`src/components/ui-schema/store.ts`），`SlotRegion` 只读它的快照。

**边界**：组件按 `kind` 渲染，已知 kind 在注册表内，未知 kind 走占位块；
v1 **永不执行远端 JS/CSS**（装载器拒绝 `script` / `stylesheet`）。新增一种 kind
需要在前端注册表加一个渲染器；插槽 id 改名时组件不会静默丢失——它落进
"未放置"分组（`UnplacedSlots`）。
