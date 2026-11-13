# SPA 按新原型重排信息架构 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 把 `uva-web` 的信息架构从"九个并列目录页"重排成设计文档 §6.1 的**对话优先**骨架：主界面只剩会话 / 对话，能力目录搬进设置分区，侧栏换成"品牌 → 新会话 → 工作区树 → 用户"，对话流与输入区按新原型（`docs/prototypes/shared-interface.prototype.html`）重排，旧页面全部删除（D34）。

**Architecture:** 路由收成三条：`/chat`（含 `/chat/:sessionId`、`/chat/:sessionId/agent/:agentName`）、`/settings/:section`、兜底重定向 `/chat`。侧栏（`Sidebar.tsx`）变成双模式组件——chat 模式渲染工作区树、settings 模式整根替换为设置导航（用户卡 → `Ctrl+F` 搜索 → 分区列表）。设置面**不建数据模型**：八个分区的行来自既有目录方法（`plugins/list` / `models/list` / `agents/list` / `skills/list` / `schedules/list` / `workflows/list` / `tasks/list` / `evals/list`），启用位的读写集中在一个 profile 缝（`src/api/settings-profile.ts`，对接刀 7 的 `profile/list` / `profile/get` / `profile/set`，wire 名待核）；**没有 settings 文件**（D20）。对话流与输入区的改动都在既有 `ChatPage.tsx` / `ChatMessageView.tsx` 内：用户消息加右对齐气泡、思考块加"输出中展开 / 完成自动折叠"两态、输入框上方加待办进度条（复用既有 `deriveTodos`）、输入框加 bash 风格 `❯` 修饰。**传输层一行不动**（C3：`api/rpc.ts` 的同源 WS）。

**Tech Stack:** TypeScript 6.0（`tsc --noEmit`）、React 19、react-router-dom 7、@radix-ui/themes 3、Vite 8；**不新增任何依赖**（`package.json` 不动）。

**Spec:** `docs/design/2026-11-09-desktop-frontend-tree-design.md` —— §6.1 共享界面契约，§8 的刀 9；§7 的 D20 / D21 / D22 / D23 / D24 / D26 / D28 / D30 / D31 / D32 / D34 / D35；§3 的 C3 / C4 / C8 / C9。原型：`docs/prototypes/shared-interface.prototype.html`（**以它为准**）。

## Global Constraints

- **不写任何前端自动化测试**（AGENTS.md §4.2）：不新增 `*.test.ts`、不建 `uva-web/tests/`、不建 `playwright*.config.ts`、不装 playwright 依赖。验证 = `make check-web` + `cd uva-web && npm run build` + Playwright MCP 驱动真实页面（Task 8 给出逐条 URL / 点击 / DOM 断言）。
- **前端没有 Node 类型**（C4）：`src/` 下不得出现 `import 'node:*'`、`process.*`（`vite.config.ts` 是仓库既有例外）。
- **WS 传输不动**（C3）：`src/api/rpc.ts` 一行不改。新数据调用一律在 `src/api/client.ts` 的 `ROUTES` 映射表加行（REST 形状 → RPC 方法名），或者像 `pluginsApi` 一样包在 `src/api/` 下的模块里。
- **无新依赖、无新 icon 库**：图形符号沿用仓库现状（emoji 字形 + 文本，见 `Sidebar.tsx:10-13` 的既有注释立场）。
- **刀 7 是数据源**：启用位 / MCP / Hooks / 模板的行数据来自刀 7 的 profile 面。wire 方法名（设计文档 D21 写的是 `profile/list` / `profile/get` / `profile/set`，经内置 MCP `uva-settings`）**待核**——所有调用集中写在 `src/api/settings-profile.ts` 一个文件里，刀 7 定名后只改这一处。
- **没有 settings 文件**（D20）：设置面只改**启用与参数**；创建 / 删除能力走 MCP 通道（D28），界面不直接写文件。
- **不做** prompt 建议条（D31 推迟）：原型里的 `.chips`（"总结这段代码 / 写个测试 / …"）**不搬**。
- **不做**「规则与记忆」分区（D24）；Hooks 分区不从别处导入配置。
- **默认 agent 不可停用**（D22）：`AgentDetail.protected === true` 的行开关渲染为锁定态。
- 每个任务收口时：`make check-web` 必须 0 错误 0 警告（tsc + eslint + prettier --check）。
- 提交前 `make check-web` + `cd uva-web && npm run build` 双绿；Rust 侧零改动（本刀不碰 `crates/`）。
- 不 push 远程仓库。

## Review Focus

规格暗示但没有测试覆盖、最可能咬人的六处。每条都在对应任务里钉上验证步骤。

1. **删除不彻底 = 幽灵入口**：旧路由除了 `App.tsx` 还有四个引用面——`CommandPalette.tsx:19-28` 的 `NAV_COMMANDS`、`useKeyboardShortcuts.ts:18-28` 的 `SHORTCUTS`、`Header.tsx:29-35` 的 `PAGE_LABEL`、`ChatMessageView.tsx:456` 与 `SessionSidePanel.tsx:149` 的 `/sessions/${id}` 深链。少改一处，点进去就是死路由。Task 6 逐个点名并配 residue grep。
2. **思考块的"输出中"判定**：不能用"segment 是否最后一段"猜——判定必须复用 `ChatMessageList` 已有的 `msg.isStreaming === true && msg.role === 'assistant' && msg.status === 'working'`（`ChatMessageView.tsx:769-771` 的 `data-streaming` 同款）。完成后**自动折叠**，但用户手动展开的权利保留。
3. **待办进度"没有 todo 时不占位置"**：`deriveTodos` 返回 `null` 时必须**一个 DOM 节点都不渲染**（不能渲染空框占位）；有 todo 时 `n/m 完成` 的 m 用 `items.length`，不是 `total` 之外的猜测。
4. **折叠 rail 的可达性**：现有 Sidebar 折叠时 label 留在 DOM、`title` 补 tooltip（`Sidebar.tsx:83` 的 `title={collapsed ? item.label : undefined}`）；新的工作区树折叠态必须沿用同一约定，否则屏幕阅读器直接丢导航。
5. **设置面数据缝分散**：任何分区组件都不得直接 fetch profile；全部经 `src/api/settings-profile.ts`。刀 7 改名时只动一个文件，否则八个分区各改一遍。
6. **Playwright 两个已知陷阱**（AGENTS.md §4.2）：后台 tab 的 `visibilityState === 'hidden'` 会让 `useServerHealth` 把整个 app 标 Offline；隐藏 tab 里 `requestAnimationFrame` 永不触发。Task 8 的每条验证都先打 visibility 补丁、等待一律用文本/selector 而非 rAF。

---

### Task 1: 设置面骨架——`/settings` 路由 + 分区注册表 + 分区壳

**Files:**
- Create: `uva-web/src/pages/SettingsPage.tsx`
- Create: `uva-web/src/pages/SettingsPage.css`
- Create: `uva-web/src/settings/sections.ts`
- Create: `uva-web/src/settings/RowCard.tsx`
- Create: `uva-web/src/settings/EnableToggle.tsx`
- Modify: `uva-web/src/App.tsx`（加两条路由）
- Modify: `uva-web/src/components/layout/Header.tsx`（`PAGE_LABEL` 加 `settings`）

**Interfaces:**
- Consumes: react-router-dom 的 `useParams` / `Navigate`；`@radix-ui/themes`（既有依赖）。
- Produces: `SETTINGS_SECTIONS: ReadonlyArray<SettingsSection>`（id / label / icon / description / source），供 Task 2 的分区组件与 Task 3 的侧栏设置导航共用；`/settings/:section` 路由。

`sections.ts` 的注册表是 §6.1 表的直译（账户由侧栏用户卡承载，分区列表是八个）：

```ts
export interface SettingsSection {
  id: 'account' | 'general' | 'plugins' | 'models' | 'agents' | 'skills' | 'mcp' | 'hooks' | 'automation';
  label: string;        // 通用 / 插件 / 模型 / 智能体 / 技能与命令 / MCP / Hooks / 自动化
  icon: string;         // ⚙ 🧩 ◈ 🤖 ⌘ 🔌 🪝 ⏰（沿用原型的字形）
  description: string;  // 一句话职责，照 §6.1 表的"界面职责"列
  source: string;       // 来源说明，照 §6.1 表的"内容来源"列
}
```

- [ ] **Step 1: 写 `sections.ts` 注册表**

九个条目按 §6.1 表逐行落：`account`（来源"—（本机）"，职责"用户 / 订阅 / 凭据"）、`general`（来源"壳自己的 UI 行"，职责"主题 / 密度 / 字号 / 快捷键"）、`plugins`（来源 `.agents/plugins`，职责"插件行的启用与参数（入口从对话界面搬到这里，D23）"）、`models`（来源 `.agents/providers`）、`agents`（来源"内置 + `.agents/agents`"）、`skills`（来源 `.agents/skills`）、`mcp`（来源 `mcp.json`）、`hooks`（来源"plugin 排布"）、`automation`（来源"plugin 排布"，职责含 `[手动新建] [在对话中创建]` + 三标签 + 空态）。**没有**「规则与记忆」条目（D24）。

- [ ] **Step 2: 写 `RowCard.tsx` 与 `EnableToggle.tsx`**

`RowCard` 照原型的 `.srow`：图标 + 名称（可带 badge）+ 描述 + 右侧动作区。`EnableToggle` 是受控开关（`checked` / `onChange` / `locked?: boolean`）：`locked === true` 渲染为置灰并带 `title="内置 · 不可停用"`（D22），不触发 `onChange`。样式写在 `SettingsPage.css`（BEM 前缀 `nt-settings__`，变量沿用 `styles/tokens.css`）。

- [ ] **Step 3: 写 `SettingsPage.tsx` 壳**

读 `useParams<{ section: string }>()`；未命中注册表时 `<Navigate to="/settings/general" replace />`。body 渲染：分区标题（顶栏 `esc 关闭` 的退出按钮 → `navigate('/chat')`）+ 分区说明卡（照原型 `.card` 的"来源"卡：来源路径 + "设置页是解析后的 profile 的反推"一句话 + `⟳ 重新载入` 按钮）。本 Task 只挂 `general` 之外的分区为"骨架占位卡"（标题 + 来源说明），Task 2 逐个替换成真数据。

- [ ] **Step 4: 挂路由**

`App.tsx` 在 `/chat/:sessionId/agent/:agentName` 之后加：

```tsx
<Route path="/settings" element={<Navigate to="/settings/general" replace />} />
<Route
  path="/settings/:section"
  element={
    <Suspense fallback={<RouteFallback testId="page-fallback-settings" />}>
      <SettingsPage />
    </Suspense>
  }
/>
```

`SettingsPage` 用 `lazy()` 分包（照 `App.tsx:20-60` 既有写法）。`Header.tsx` 的 `PAGE_LABEL` 加 `settings: 'Settings'`。

- [ ] **Step 5: 门禁**

Run: `make check-web`
Expected: `tsc --noEmit` 无输出、`eslint .` 无输出、`prettier --check .` 无错误（exit 0）。

---

### Task 2: 八个分区接真数据 + profile 缝

**Files:**
- Create: `uva-web/src/api/settings-profile.ts`
- Create: `uva-web/src/settings/sections/GeneralSection.tsx`
- Create: `uva-web/src/settings/sections/PluginsSection.tsx`
- Create: `uva-web/src/settings/sections/ModelsSection.tsx`
- Create: `uva-web/src/settings/sections/AgentsSection.tsx`
- Create: `uva-web/src/settings/sections/SkillsSection.tsx`
- Create: `uva-web/src/settings/sections/McpSection.tsx`
- Create: `uva-web/src/settings/sections/HooksSection.tsx`
- Create: `uva-web/src/settings/sections/AutomationSection.tsx`
- Create: `uva-web/src/settings/sections/AccountSection.tsx`
- Delete: `uva-web/src/components/plugins/DeploymentPlugins.tsx`
- Delete: `uva-web/src/components/plugins/DeploymentPlugins.css`
- Modify: `uva-web/src/settings/sections.ts`（每分区挂组件引用）

**Interfaces:**
- Consumes（既有，全部核对过）:
  - `pluginsApi.list()` → `GET /api/v1/plugins` → `plugins/list`（`src/api/plugins.ts:50`；`ResolvedPlugin { name, kind: 'static'|'remote', authority, risk, tools: string[] }` + `SkippedPlugin { name, reason }`）
  - `listModels()` → `GET /api/v1/models` → `models/list`（`src/api/chat-stream.ts:224`；`ModelEntry { provider, model, context_window, supports_tools, supports_streaming }`，`ModelsResponse { models, default_provider, default_model }`）
  - `api.get('/api/v1/agents')` → `agents/list`（行类型 `AgentDetail`，含 `protected: boolean`、`tools: string[]`、`modelHint?`）；`api.get('/api/v1/agents/default')` → `agents/default`（`DefaultAgentResponse`）
  - `api.get('/api/v1/skills')` → `skills/list`（行形状照 `SkillsPage.tsx:33` 的本地 `Skill` 接口搬到 `src/api/types.ts`）
  - `schedulesApi.list` / `workflowsApi.list` / `tasksApi.list` / `evalsApi.list`（`src/api/{schedules,workflows,tasks,evals}.ts`，全部走 `client.ts` 的 `List<T>` 信封）
  - `useTheme()`（`src/hooks/useTheme.ts:103`，`Theme = 'light' | 'dark' | 'system'`）
- Produces: `settingsProfile` 模块（`listRows(section)` / `setRowEnabled(rowId, enabled)`），八个分区共用；分区组件注册进 `sections.ts`。

- [ ] **Step 1: 写 profile 缝 `src/api/settings-profile.ts`**

唯一允许接触刀 7 面的文件。形状：

```ts
export interface ProfileRow {
  id: string;            // 行标识（刀 7 的行 id）
  label: string;
  description?: string;
  enabled: boolean | { service: string; check: string; arg?: string }; // D39：可由插件判定
  badge?: string;        // 如「内置」
}
export const settingsProfile = {
  listRows(section: SettingsSectionId, signal?: AbortSignal): Promise<ProfileRow[]>;
  setRowEnabled(rowId: string, enabled: boolean, signal?: AbortSignal): Promise<void>;
};
```

wire 调用写**待核**占位：按 D21 预期是 `profile/list` / `profile/set`，但经 WS 路由还是经 MCP 桥由刀 7 决定——本文件顶部留 `// TODO(knife-7): 对表刀 7 的最终方法名，只改本文件` 注释，方法体在刀 7 合并前抛 `new Error('profile surface lands with knife 7')`。分区组件里 `try/catch` 该错误 → 显示"启用位等待 profile 面（刀 7）"的行内提示，**行本身照常渲染**（目录数据不依赖它）。

- [ ] **Step 2: 插件分区（含工具）**

`DeploymentPlugins.tsx` 的 resolved / skipped 双列渲染**搬进来**（该文件与它的 css 删除，内容并入）：每行 `RowCard`（名称 + `kind`/`authority`/`risk` 描述行 + `tools.join(' ')`）+ `EnableToggle`（走 profile 缝）。`skipped` 行渲染为置灰 + reason，无开关。工具域不单列——`ResolvedPlugin.tools` 已含每个插件的工具名（`src/api/plugins.ts:30-33`），§6.1 落点表的"插件目录"由本分区承载。

- [ ] **Step 3: 模型分区**

`listModels()` 的行按 `provider` 分组：组标题 = provider 名，行 = `model`（描述 = `context_window` / `supports_tools` / `supports_streaming`），`default_provider + default_model` 命中的行加 `badge="默认"`。开关走 profile 缝。

- [ ] **Step 4: 智能体分区**

`agents/list` 行 + `agents/default` 的名字标 `badge="默认"`。`protected === true` 或 名字 === 默认 agent 的行：`EnableToggle locked`（D22：内置默认 agent 不可停用）。描述行 = `description`（`tools.length` 个工具、`modelHint`）。

- [ ] **Step 5: 技能与命令分区**

`skills/list` 行：名称 + 描述（正文在 plugin 排布里，这里只开关——照 §6.1 表）。行顶部加 `⟳ 重新载入` 调 `POST /api/v1/skills/reload`（`ReloadSkillsResponse { count }`，toast 显示 count）。

- [ ] **Step 6: MCP / Hooks 分区**

两者行数据**只**来自 profile 缝（`mcp.json` 行 / plugin 排布的 hook 行，D24：不从别处导入配置）。刀 7 未合并时渲染来源卡 + "行数据等待 profile 面"空态；合并后 `listRows('mcp')` / `listRows('hooks')` 直接出行。Hooks 分区额外两行只读设置：Hooks 命令运行方式 + 查看运行日志按钮（照原型；日志面没有现成 RPC——按钮渲染但 `disabled` + `title="运行日志面随刀 7 提供"`，**不造假数据**）。

- [ ] **Step 7: 自动化分区**

标题 + `[手动新建]`（`Modal` 表单 → `schedulesApi.create`，字段照 `CreateScheduleRequest { name, description?, kind: JobKind, payload, next_fire_at? }`）+ `[⏱ 在对话中创建]`（`navigate('/chat')`）。三标签：
- **已配置**：`schedulesApi.list`（`ScheduleSummary { id, name, kind, status, next_fire_at }`，复用 `src/components/schedules/ScheduleCard.tsx` 渲染）+ `workflowsApi.list`（`WorkflowSummary`）。
- **执行历史**：`tasksApi.list`（`TaskDelegation`，最新在前，行链到 `/chat/<childSessionId>`）+ `evalsApi.list`（`EvalSuite` 行，名称 + 用例数）。
- **任务模板**：空态卡（"尚未配置自动化任务" + `从模板创建 →` 按钮 disabled + title 说明模板面来自 plugin 排布）。

- [ ] **Step 8: 通用分区 + 账户分区**

`GeneralSection`：主题（三段选择 `light|dark|system`，直接调既有 `useTheme().setTheme`）、密度（`紧凑|舒适`）、字号（数字档位）——密度/字号写 `localStorage` 键 `uva.density` / `uva.fontSize`（照 `uva.theme` 的既有模式，`src/hooks/useTheme.ts:5`），生效方式是给 `<html>` 设 `data-density` 与 `--font-size`（`index.css` 加两行变量消费）；快捷键行 = "18 个"只读计数 + 按钮打开 Task 7 的快捷键帮助。源真归属是 profile 壳行的 config（D26）——在组件注释里标 `// TODO(knife-7): 换到壳行 config`。
`AccountSection`：本机用户卡（"本机 · 无订阅 · 凭据只存在本机"——仓库没有账户体系，不造登录面）+ 凭据卡 = `listModels()` 按 provider 聚合的只读行（provider 名 + "N 个模型可用"）。凭据的**写**界面不做（没有面）。

- [ ] **Step 9: 注册 + 清占位**

`sections.ts` 每个条目挂 `component` 引用；删掉 Task 1 的占位卡逻辑。`SettingsPage.css` 补齐分区布局（`.nt-settings__row` 等）。

- [ ] **Step 10: 门禁 + 构建**

Run: `make check-web`
Expected: exit 0（tsc / eslint / prettier 全绿）。
Run: `cd uva-web && npm run build`
Expected: `tsc --noEmit` 通过后 `vite build` 产出 `dist/`，无 chunk 报错。

---

### Task 3: 新侧栏——工作区树 + 新会话 + 跨域搜索 + 用户入口 + 折叠 rail

**Files:**
- Modify: `uva-web/src/components/layout/Sidebar.tsx`（整根重写，`NAV_ITEMS` 删除）
- Modify: `uva-web/src/components/layout/Sidebar.css`
- Create: `uva-web/src/hooks/useWorkspaceTree.ts`
- Create: `uva-web/src/components/layout/WorkspaceSearch.tsx`
- Modify: `uva-web/src/components/layout/Header.tsx`（移除 `ThemeToggle`——主题入口已搬进设置·通用）

**Interfaces:**
- Consumes: `useSidebarCollapse()`（`Ctrl/Cmd+B` 折叠，`uva.sidebarCollapsed` 持久化——**不动**）；`useCursorList<T>('/api/v1/sessions')`（`sessions/list`，`List<SessionSummary>` 信封，`refresh()` 可用）；`api.get('/api/v1/search?q=&limit=')` → `search/all`（`SearchHit { domain, id, title, score, preview? }`，`SEARCH_DOMAINS` 六域）。
- Produces: 双模式 `Sidebar`（`mode: 'workspace' | 'settings'` 由当前路径推导）；`useWorkspaceTree()` → `{ root, sessions, loading, refresh }`；`WorkspaceSearch`（🔍 弹层）。

- [ ] **Step 1: 写 `useWorkspaceTree.ts`**

`useCursorList<SessionSummary>('/api/v1/sessions', { limit: 50, sort: '-created_at' })` 取列表；根节点固定为"默认工作区"（**常驻**，D35——服务器只有一个 `workspace_root`，没有工作区 RPC，树根是前端常量）。子节点 = `parent_session_id === undefined` 的会话（sub-agent 会话不进树——它们经对话流里的委派卡到达，`ChatMessageView.tsx:440-458`）。标签用 `shortId(session.id)`（`src/lib/short-id.ts`）。`window` focus 时 `refresh()`。

- [ ] **Step 2: 重写 `Sidebar.tsx` 的 workspace 模式**

照原型 `sidebar()`（`shared-interface.prototype.html:198-224`）自上而下：
1. 品牌行：`◐` logo + `uva` + `HARNESS` 徽标 + 折叠按钮（沿用 `data-testid="sidebar-toggle"`、`aria-expanded` / `aria-controls` / `aria-label` 既有约定）。
2. `新会话` 按钮（`data-testid="new-session"`）→ `<NavLink to="/chat">`——`/chat` 无会话 id，`ChatPage` 首条消息时经 `createChatSession()` 铸 id（`ChatPage.tsx:783-788`），无需新逻辑。
3. `工作区` 组标题 + 右侧动作区 `🔍 ⇅ 📁`：🔍 = `WorkspaceSearch` 触发钮；⇅ / 📁 渲染为 disabled + `title="多工作区面随后续刀次提供"`（单工作区是现状事实，不做假下拉）。
4. 树：根节点（🗂 + 默认工作区，caret ▾/▸）→ 会话子节点（缩进）。会话节点 `NavLink to={/chat/${id}}`，`active` 态高亮；`data-testid={session-node-${id}}`。
5. 左下用户钮（`data-testid="user-entry"`，`🧑 dev`）→ `/settings/account`。

折叠 rail（`.nt-sidebar--collapsed` 既有类）：品牌只留 logo、新会话变 `+`、树只留工作区图标 + caret、用户只留 🧑；**label 全部留在 DOM**（`Sidebar.tsx:83` 的既有约定：折叠时 `title` 补 tooltip、label 节点不卸载）+ `title` tooltip。

- [ ] **Step 3: settings 模式（侧栏整个变成设置导航）**

`useLocation().pathname.startsWith('/settings')` 时渲染：用户卡（🧑 头像 + `dev` + `免费` badge，链 `/settings/account`）→ `Ctrl+F 搜索` 输入框（`data-testid="settings-search"`，过滤下面的分区列表）→ 八个分区 `NavLink`（`/settings/<id>`，`data-testid={settings-section-<id>}`，图标 + label）。数据来自 Task 1 的 `SETTINGS_SECTIONS`。

- [ ] **Step 4: 写 `WorkspaceSearch.tsx`**

🔍 点击展开的浮层：输入框（`data-testid="search-input"`）+ 结果列表。输入防抖 200ms 调 `GET /api/v1/search?q=${encodeURIComponent(q)}&limit=20`（与 `SessionsPage.tsx:222` 同款调用）。命中行的落点：`domain === 'session'` → `/chat/${hit.id}`；`tool` / `mcp` / `skill` / `memory` / `agent` → 对应设置分区（`tool|skill → /settings/skills 或 /settings/plugins`：tool→`/settings/plugins`、skill→`/settings/skills`、agent→`/settings/agents`、mcp→`/settings/mcp`、memory→不跳转，行内显示 preview）。每行渲染 `domain` 徽标 + `title` + `preview?`。`Esc` 关闭。

- [ ] **Step 5: Header 收口**

`Header.tsx` 删 `ThemeToggle` import 与渲染（主题入口在设置·通用）；`PAGE_LABEL` 收成 `{ chat: 'Chat', settings: 'Settings' }`（其余五个 key 随 Task 6 删页后失义，本步先留——Task 6 统一清）。

- [ ] **Step 6: 门禁**

Run: `make check-web`
Expected: exit 0。

---

### Task 4: 对话流——右对齐气泡 + 思考块两态 + 标题栏

**Files:**
- Create: `uva-web/src/lib/thinking-state.ts`
- Modify: `uva-web/src/components/chat/ChatMessageView.tsx`
- Modify: `uva-web/src/pages/ChatPage.css`
- Modify: `uva-web/src/pages/ChatPage.tsx`（标题栏）

**Interfaces:**
- Consumes: `ChatMessageViewItem { role, segments, status?, isStreaming? }`（`ChatMessageView.tsx:714-723`）；`MessageSegment.type === 'thinking'`（`src/api/chat-message.ts:272` 起）。
- Produces: `thinkingIsLive(msg: ChatMessageViewItem): boolean`（纯函数，页面内可 `await import('/src/lib/thinking-state.ts')` 断言，AGENTS.md §4.2 第 3 条）；`.nt-chat__bubble` 用户气泡；思考块两态。

- [ ] **Step 1: 写 `thinking-state.ts` 纯函数**

```ts
export function thinkingIsLive(msg: {
  isStreaming?: boolean;
  status?: string;
  role: string;
}): boolean {
  return msg.isStreaming === true && msg.role === 'assistant' && msg.status === 'working';
}
```

判定式与 `ChatMessageView.tsx:769-771` 的 `data-streaming` 完全一致（Review Focus 2）。

- [ ] **Step 2: `ChatMessageList` 下传 live 态**

消息行组件里对**当前 live 消息**的 segments 传 `live={thinkingIsLive(msg)}`；`SegmentView` 接收 `live?: boolean`。**只影响 thinking 段**：`useState(live)` 作初值（输出中展开），`useEffect(() => { if (!live) setExpanded(false); }, [live])`（完成自动折叠）；用户手动 `setExpanded(true)` 后不被覆盖（effect 只在 `live` 翻转时触发）。其余 collapsible 段（`tool_call` / `tool_result` / `tool_block`）行为不变（默认收起）。

- [ ] **Step 3: 思考块两态样式与文案**

展开态（live）：标签 `▾ 已思考（输出中…）` + 边流边滚（容器 `max-height` + `overflow auto`，自动滚底）；折叠态：一行 `已思考 >`（`data-testid="thinking-block"`，可点开，展开后显示既有 `thinkingPreview` 逻辑 `ChatMessageView.tsx:674-676`）。CSS 进 `ChatPage.css`（`.nt-chat__segment--thinking.nt-chat__segment--live` 高亮色，照原型 `.think.live` 用 accent）。

- [ ] **Step 4: 用户消息右对齐气泡**

`ChatMessageView.tsx` 里 `msg.role === 'user'` 的消息：`message-content` 内包一层 `<div className="nt-chat__bubble" data-testid="user-bubble">`（文本段 + 附件 chips 进气泡；meta 行留在气泡外）。CSS：`margin-left: auto; max-width: 72%; background: var(--bubble, var(--bg-secondary)); border-radius: 12px; padding: 8px 13px;`（token 用 `styles/tokens.css` 既有变量，缺则在 tokens.css 加 `--bubble`）。助手消息不动（markdown 全宽，`Markdown.tsx` 不改）。

- [ ] **Step 5: 标题栏**

`ChatPage.tsx` 顶部 chip 区（`nt-chat__usage-chip` / 面板开关那一行，`ChatPage.tsx:1595-1606`）重排成 `.nt-chat__titlebar`：左侧会话标题（`shortId(sessionId)`，无会话时"新会话"）+ 状态 pill；右侧 usage chip + `进度与产物` 面板开关（两者都保留既有 testid `usage-chip` / `session-panel-open`）。

- [ ] **Step 6: 门禁**

Run: `make check-web`
Expected: exit 0。

---

### Task 5: 输入区——待办进度 + bash 风格 prompt + 底部一行

**Files:**
- Create: `uva-web/src/components/chat/TodoProgress.tsx`
- Create: `uva-web/src/components/chat/PromptPrefix.tsx`
- Modify: `uva-web/src/pages/ChatPage.tsx`
- Modify: `uva-web/src/pages/ChatPage.css`

**Interfaces:**
- Consumes: `deriveTodos(messages)` → `PanelTodoState { items: PanelTodoItem[], completed, total } | null`（`src/lib/session-panel-data.ts:100`；`ChatPage.tsx:1505` 已算好 `panelTodos`）；`PanelTodoItem.status ∈ 'pending' | 'in_progress' | 'completed' | 'cancelled' | string`。
- Produces: `.nt-chat__todo-strip`（`data-testid="todo-progress"`，无 todo 时**零节点**）；`.nt-chat__prompt-prefix`（`data-testid="prompt-prefix"`）；composer 底部一行重排。

- [ ] **Step 1: 写 `TodoProgress.tsx`**

`props: { todos: PanelTodoState | null }`；`todos === null || todos.items.length === 0` 时 `return null`（**不占任何位置**，Review Focus 3）。有 todo 时：头行 `待办` 标签 + 计划名 + 右侧 `{completed}/{items.length} 完成`；条目行图标映射 `completed → ✔`（划线）、`in_progress → ▸`（accent 色）、`pending → ·`、`cancelled → ✕`（划线）。数据直接用 Task 4 已有的 `panelTodos`（与右侧面板同源，`ChatPage.tsx:1505` 注释已声明"panel 与 inline 不能 disagree"）。

- [ ] **Step 2: 写 `PromptPrefix.tsx`**

`❯ <工作区路径> ` 修饰条，`props: { workspace: string; context?: string }`。渲染：`❯` accent 色 + 工作区名 + 可选上下文标记（如选中会话的 agent 名）。**工作区路径现状**：服务器没有任何面暴露 `workspace_root`（`GET /version` 只回构建身份，`gateway.rs:711-715`；`SessionSummary` 无 workspace 字段）——先显示常量 `默认工作区`（与树根同名，D35），组件注释标 `// TODO(server): 工作区路径面就绪后换真值`。**不做 prompt 建议条**（D31：原型 `.chips` 不实现）。

- [ ] **Step 3: 重排 composer**

`ChatPage.tsx` 输入区（`textarea` 在 `:1801`、`nt-chat__composer-bar` 在 `:1817`）改为：`TodoProgress`（在最上）→ `PromptPrefix` + `textarea`（同一行内左侧修饰，textarea 的 `data-testid="chat-input"`、auto-grow、`handleKeyDown` 全部不动）→ 底部一行从左到右：`+` 附件钮（既有 attach `<label>`，改为圆形 `+` 图标样式）→ 工作区范围标签（`🕐 默认工作区`，非交互，title 说明单工作区现状）→ agent 选择器（既有，`/chat/:id/agent/:name` 路由联动保留）→ 模型选择器（既有 `listModels` 驱动）→ 发送钮（既有 `data-testid="send-button"`，右对齐圆形）。附件预览列表（`pending-attachments`）位置不动。

- [ ] **Step 4: CSS**

`ChatPage.css` 加 `.nt-chat__todo-strip`（照原型 `.todo`：圆角卡、`✔` 行划线）、`.nt-chat__prompt-prefix`（等宽字体、accent 的 `❯`）、composer 底部行 flex 布局（`justify-content: space-between`，发送钮 `margin-left: auto`）。

- [ ] **Step 5: 门禁**

Run: `make check-web`
Expected: exit 0。

---

### Task 6: 删旧页与路由收口（D34）

**Files:**
- Delete: `uva-web/src/pages/` 下 17 个文件（见 Step 1 清单）
- Delete: `uva-web/src/components/ui/ListToolbar.tsx`
- Delete: `uva-web/src/components/ui/MetadataTable.tsx`
- Delete: `uva-web/src/components/ui/metadataCells.tsx`
- Delete: `uva-web/src/hooks/useListFilter.ts`
- Modify: `uva-web/src/App.tsx`
- Modify: `uva-web/src/components/ui/CommandPalette.tsx`
- Modify: `uva-web/src/hooks/useKeyboardShortcuts.ts`
- Modify: `uva-web/src/components/layout/Header.tsx`
- Modify: `uva-web/src/components/chat/ChatMessageView.tsx`
- Modify: `uva-web/src/components/chat/SessionSidePanel.tsx`

**Interfaces:**
- Consumes: Task 1–5 已就位（能力目录有新家，删除不失联）。
- Produces: 路由表收成 `/chat*` + `/settings*`；无任何死链接。

- [ ] **Step 1: 删 17 个页面文件**

`uva-web/src/pages/` 今天共 19 个文件；保留 `ChatPage.tsx` / `ChatPage.css`（Task 1 已加 `SettingsPage.tsx` / `SettingsPage.css`）。删除：

```
AgentsPage.tsx  AgentDetailPage.tsx  ToolsPage.tsx  ToolDetailPage.tsx
SkillsPage.tsx  SkillDetailPage.tsx  SessionsPage.tsx  SessionDetailPage.tsx
SessionDetailPage.css  SchedulesPage.tsx  ScheduleDetailPage.tsx
WorkflowsPage.tsx  WorkflowDetailPage.tsx  EvalsPage.tsx  EvalDetailPage.tsx
TasksPage.tsx  TaskDetailPage.tsx
```

（"设计文档 D34 的 19 个页面" = `pages/` 目录现状 19 个文件；删 17 留 2。）

- [ ] **Step 2: `App.tsx` 收路由**

删 16 处 `lazy()` import 与对应 `<Route>`；路由表终态：

```tsx
<Route path="/" element={<Navigate to="/chat" replace />} />
<Route path="/chat" element={<ChatPage />} />
<Route path="/chat/:sessionId" element={<ChatPage />} />
<Route path="/chat/:sessionId/agent/:agentName" element={<ChatPage />} />
<Route path="/settings" element={<Navigate to="/settings/general" replace />} />
<Route path="/settings/:section" element={…SettingsPage…} />
<Route path="*" element={<Navigate to="/chat" replace />} />
```

- [ ] **Step 3: 连带删仅被旧页使用的组件**

`ListToolbar.tsx`（8 个删除页用）、`MetadataTable.tsx` + `metadataCells.tsx`（AgentDetail/SkillDetail 用）、`useListFilter.ts`（ListToolbar 用）。**保留**：`ScheduleCard.tsx` + `describeScheduleKind.ts`（Task 2 自动化分区用）、`visual.tsx` / `visual-utils.ts` / `EmptyState.tsx` / `SkeletonList.tsx`（`UiSchemaPanel.tsx` 用）、`useCursorList.ts`（`CommandPalette.tsx` 用）、`api/{schedules,workflows,tasks,evals,plugins}.ts`（分区用）、`lib/session-to-messages.ts`（`ChatPage.tsx` 也用）。

- [ ] **Step 4: 四个引用面收口（Review Focus 1）**

- `CommandPalette.tsx:19-28` `NAV_COMMANDS` 换成：`Go to Chat (g c)` → `/chat`、`New Session` → `/chat`、`Go to Settings (g s)` → `/settings/general`、八个分区各一条（`Open Settings · <label>` → `/settings/<id>`）。palette 的搜索动作（`GET /api/v1/search`）保留。
- `useKeyboardShortcuts.ts:18-28` `SHORTCUTS` 换成 `{ key: 'c', path: '/chat' }` + `{ key: 's', path: '/settings/general' }`（g 前缀机制不动）。
- `Header.tsx:29-35` `PAGE_LABEL` 收成 `{ chat: 'Chat', settings: 'Settings' }`。
- `ChatMessageView.tsx:456` 与 `SessionSidePanel.tsx:149`：`/sessions/${id}` → `/chat/${id}`。

- [ ] **Step 5: `ui/schema` 调用点搬家**

旧 `SessionDetailPage.tsx:545` 的 `<UiSchemaPanel />`（不传 slot）删页后失联——挪进 `SessionSidePanel.tsx` 的 `<aside className="nt-chat-panel">` 底部（三个 section 之后）渲染一次。`components/ui-schema/*` 与 `api/ui-schema.ts` **不动**（刀 8 的插槽契约会整体接管，本刀只保调用点活着）。

- [ ] **Step 6: residue 检查**

Run: `grep -rn "ToolsPage\|SkillsPage\|AgentsPage\|SessionsPage\|SchedulesPage\|WorkflowsPage\|EvalsPage\|TasksPage\|DetailPage" uva-web/src`
Expected: 无命中（exit 1）。
Run: `grep -rn "to={\`/sessions/" uva-web/src`
Expected: 无命中（exit 1）。
Run: `grep -rn "ListToolbar\|MetadataTable\|useListFilter\|DeploymentPlugins" uva-web/src`
Expected: 无命中（exit 1）。

- [ ] **Step 7: 门禁 + 构建**

Run: `make check-web`
Expected: exit 0。
Run: `cd uva-web && npm run build`
Expected: 构建通过；输出里不再有 tools / skills / agents / sessions / schedules / workflows / evals / tasks 的页面 chunk（只剩 chat / settings / vendor）。

---

### Task 7: Vim 优先快捷键层（D30）

**Files:**
- Create: `uva-web/src/hooks/useListNavigation.ts`
- Create: `uva-web/src/components/ui/ShortcutHelp.tsx`
- Modify: `uva-web/src/hooks/useKeyboardShortcuts.ts`
- Modify: `uva-web/src/components/ui/CommandPalette.tsx`
- Modify: `uva-web/src/components/layout/Sidebar.tsx`（树节点接光标态）
- Modify: `uva-web/src/settings/sections.ts` 注册的消费方（分区列表接光标态）

**Interfaces:**
- Consumes: `isEditableTarget`（`useKeyboardShortcuts.ts:50` 导出）；`useSidebarCollapse`（Ctrl/Cmd+B 已有）。
- Produces: `useListNavigation({ itemIds, onOpen, onDelete, onCopyId, containerRef })` → `{ cursor, setCursor }`；`?` 帮助浮层；`:` 打开命令面板。

- [ ] **Step 1: 写 `useListNavigation.ts`**

处理 `j` / `k`（±1）、`gg`（首，双击 g 600ms 窗口）/ `G`（尾）、`Ctrl+d` / `Ctrl+u`（±半页，按容器可见行数）、`Enter`（`onOpen(cursorId)`）、`x`（`onDelete(cursorId)`）、`y`（`onCopyId(cursorId)`，`navigator.clipboard.writeText`，失败静默——照 `ChatMessageView.tsx` CopyButton 的既有降级）。事件挂 window，`isEditableTarget` 时跳过；光标项 `scrollIntoView({ block: 'nearest' })`。消费者：侧栏工作区树（chat 模式）与设置分区列表。

- [ ] **Step 2: 树与分区列表接光标**

`Sidebar.tsx` 工作区树：节点加 `data-cursor={id === cursor}`（样式 = 既有 active 高亮同款）；`onOpen` = `navigate('/chat/' + id)`；`onDelete` = 确认 `Modal` → `DELETE /api/v1/sessions/:id`（`sessions/delete`，toast 照 `SessionsPage.tsx:118` 文案）→ `useWorkspaceTree().refresh()`；`onCopyId` = 复制完整 session id。设置模式：光标走分区列表，`onOpen` = `navigate('/settings/' + id)`（无删除/复制）。

- [ ] **Step 3: `:` 打开命令面板 + `?` 帮助 + `Esc`**

`CommandPalette.tsx` 的 window 监听（`:205-216`）扩一条：收到 `window` 自定义事件 `uva:palette-open` 时 `setOpen(true)`。`useKeyboardShortcuts.ts` 加全局键：`:`（非 editable）→ `window.dispatchEvent(new CustomEvent('uva:palette-open'))`；`?`（Shift+/）→ 打开 `ShortcutHelp`。`ShortcutHelp.tsx`：`Modal` 浮层，列出本刀全部键位（j/k/gg/G/Ctrl+d/u/Enter/x/y/`/`/n/N/:/?/Esc/Ctrl+B/g c/g s/Ctrl+K），数据是一份本地常量表（同一份表供设置·通用的"快捷键 N 个"计数）。`Esc` 语义：面板开→关（Modal 已处理）、搜索开→关（Step 4）、设置页→`navigate('/chat')`（`SettingsPage` 顶栏既有退出钮同款）。

- [ ] **Step 4: `/` 与 `n` / `N` 搜索跳转**

`/`（非 editable）→ focus 当前模式的搜索入口：chat 模式 focus `WorkspaceSearch` 的 `search-input` 并展开；settings 模式 focus `settings-search`。`n` / `N`：`WorkspaceSearch` 持有命中列表的光标索引，`n` 下一个 / `N` 上一个（循环），命中行高亮 + `Enter` 打开。

- [ ] **Step 5: 门禁**

Run: `make check-web`
Expected: exit 0。

---

### Task 8: Playwright MCP 验证 + 门禁收口

**Files:**
- Modify: `CHANGELOG.md`（`[Unreleased]`）

**Interfaces:**
- Consumes: 真实后端 `cargo run -p uva-app-server -- serve --port 8080`（Vite 代理 `/rpc` / `/readyz` / `/livez` → `:8080`，`vite.config.ts` 已配）；无 provider key 时后端回落 `DemoEchoProvider`（`src/state/app_state/mod.rs:93`），真实对话轮可跑。
- Produces: 验证记录（会话产物 / PR 描述）；CHANGELOG 条目。

**每条验证前的固定前奏**（AGENTS.md §4.2 两个陷阱）：

1. `browser_run_code_unsafe` 执行（等价 `evaluateOnNewDocument` 的补丁，必须在 navigate 前装）：

```js
await context.addInitScript(() => {
  Object.defineProperty(document, 'visibilityState', { get: () => 'visible', configurable: true });
  Object.defineProperty(document, 'hidden', { get: () => false, configurable: true });
});
```

2. 每次 `browser_navigate` 后 `browser_tabs` 选中 + 前景化；等待一律用 `browser_wait_for` 的 text / textGone 或 selector，**不用** `requestAnimationFrame`。

- [ ] **Step 1: 起服务**

```bash
cargo run -p uva-app-server -- serve --port 8080 --web-dir uva-web/dist
```

（另一个终端）`cd uva-web && npm run dev`（Vite 默认 `http://localhost:5173`）。
Expected: serve 日志出现 core `ready`；Vite 输出 `Local: http://localhost:5173/`。

- [ ] **Step 2: 壳与侧栏（chat 模式）**

打开 `http://localhost:5173/chat`。断言（`browser_snapshot` / `browser_find`）：
- 品牌：`uva` 文本与 `HARNESS` 徽标同时存在（`[data-testid="sidebar-brand"]`）。
- `[data-testid="new-session"]` 存在且可点（点后 URL 仍是 `/chat`）。
- `[data-testid="workspace-tree"]` 内有"默认工作区"根节点（D35 常驻）；已有会话时子节点 `[data-testid^="session-node-"]` ≥ 1。
- 左下 `[data-testid="user-entry"]` 存在。

- [ ] **Step 3: 折叠 rail**

`browser_press_key` `Control+b`。断言：`#nt-sidebar-nav`（或 sidebar 根节点）class 含 `nt-sidebar--collapsed`；再按一次恢复。窄视口（`browser_resize` 到 700×800）时折叠态强制、toggle 钮消失（`[data-testid="sidebar-toggle"]` 不存在）。

- [ ] **Step 4: 对话流一轮（右对齐气泡 + 助手 markdown）**

在 `[data-testid="chat-input"]` 输入 `hello` → `browser_click` `[data-testid="send-button"]`。断言：
- `[data-testid="message-user"]` 内 `[data-testid="user-bubble"]` 存在；`browser_run_code_unsafe` 里 `getComputedStyle(document.querySelector('[data-testid=\"user-bubble\"]')).marginLeft === 'auto'`（右对齐）。
- `[data-testid="message-assistant"]` 回答为 markdown 渲染（`<p>` / `<code>` 等元素，非纯文本）。
- 顶栏 `[data-testid="usage-chip"]` 仍在。

- [ ] **Step 5: 思考块两态（纯函数断言 + 有思考段时的 DOM 断言）**

- 纯函数：`browser_run_code_unsafe` 里 `await page.evaluate("import('/src/lib/thinking-state.ts').then(m => [m.thinkingIsLive({isStreaming:true,role:'assistant',status:'working'}), m.thinkingIsLive({isStreaming:false,role:'assistant',status:'working'}), m.thinkingIsLive({isStreaming:true,role:'user',status:'working'})])")` → `[true, false, false]`。
- DOM（依赖 provider 是否吐 thinking 段）：若本轮流式期间快照到 `[data-testid="thinking-block"]`，断言流中它展开（内容可见）、turn 结束后折叠成一行 `已思考 >`。echo provider 不吐 thinking 时，此 DOM 断言记"provider 不适用"跳过（判定函数已由上行覆盖）。

- [ ] **Step 6: 输入区（待办 + prompt 修饰）**

- 无 todo：`browser_find` `todo-progress` → **0 命中**（不占位）。
- 有 todo 的派生：`browser_run_code_unsafe` 里 `await page.evaluate("import('/src/lib/session-panel-data.ts').then(m => m.parseTodoItems(JSON.stringify({todos:[{id:'1',content:'a',status:'completed'},{id:'2',content:'b',status:'in_progress'}]})))")` → 两项、`completed === 1`。
- prompt 修饰：`[data-testid="prompt-prefix"]` 文本以 `❯` 开头且含 `默认工作区`；底部一行可见 `+`、模型选择器、`[data-testid="send-button"]`；**页面上不存在** prompt 建议条（`browser_find` `总结这段代码` → 0 命中，D31）。

- [ ] **Step 7: 设置面**

点 `[data-testid="user-entry"]` → URL `/settings/account`。断言：
- 侧栏整根变设置导航：用户卡 + `[data-testid="settings-search"]` + 八个 `[data-testid^="settings-section-"]`；chat 模式的树不在 DOM。
- 依次点 `settings-section-plugins` / `-models` / `-agents` / `-skills`：各分区渲染真行（插件行含工具名；模型行按 provider 分组；`agents` 分区默认 agent 行的开关为锁定态）。
- `settings-section-agents` 里 `Ctrl+F`（`browser_press_key` `Control+f`）后输入 `模型` → 分区列表过滤（或浏览器查找拦截时改用直接 focus `settings-search` 再输入——以 `settings-search` 路径为准）。
- `settings-section-automation`：三标签可见，`已配置` 出 schedules/workflows 行；`手动新建` 打开 Modal。
- MCP / Hooks 分区显示"等待 profile 面（刀 7）"行内提示（不空白、不假数据）。
- 点开关（插件行）：toast / 行内提示表明走 profile 缝（刀 7 未合并时是明确的等待提示，不是静默失败）。

- [ ] **Step 8: 快捷键抽查**

- `/` → `[data-testid="search-input"]` 获得 focus（`browser_run_code_unsafe` 查 `document.activeElement` 的 testid）；输入 `session` 出现命中行（`search/all` 真数据）；`n` / `N` 移动高亮；`Enter` 跳 `/chat/:id`。
- 树上 `j` / `k` 移动 `data-cursor`；`y` 后 `navigator.clipboard.readText()` 等于完整 session id；`x` 弹确认 Modal。
- `?` 打开快捷键帮助；`Esc` 关闭；设置页 `Esc` 回 `/chat`；`:` 打开命令面板。

- [ ] **Step 9: 旧页死链抽查**

`browser_navigate` 依次 `http://localhost:5173/tools`、`/sessions`、`/evals/anything`。断言：三条都重定向到 `/chat`（URL 栏 + snapshot 无目录表格）。

- [ ] **Step 10: 门禁与收尾**

Run: `make check-web`
Expected: exit 0（tsc + eslint + prettier 全绿）。
Run: `cd uva-web && npm run build`
Expected: 构建通过（刀 9 验收的两条命令）。

`CHANGELOG.md` 的 `[Unreleased]` 加一条：

```markdown
- **SPA 信息架构重排（对话优先）**：主界面收成会话 / 对话 + 设置分区，旧目录页全部删除；
  侧栏换成工作区树（默认工作区常驻）+ 跨域搜索 + 用户入口，可 `Ctrl/Cmd+B` 折叠；
  对话流用户消息右对齐气泡、思考块输出中展开完成后自动折叠；输入区带待办进度与
  bash 风格 `❯` 提示符；设置面是解析后 profile 的反推（无 settings 文件）。
```
