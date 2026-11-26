# 能力矩阵：域 → 界面落点（从 profile 推导）

> 状态：刀 4 产物（2026-11-09）。本文件是**解析后的 profile 的反推**（D26 / D27）：
> 每一行都能追溯到一个 profile 行或一个目录约定，不是手写的愿望清单。
> 设计锚点：`docs/design/2026-11-09-desktop-frontend-tree-design.md` §6.1、§7（D15 / D17 /
> D19 / D20 / D26 / D27 / D32 / D38 / D39 / D40）、§8 刀 4。
> 逻辑原型：`docs/prototypes/host-capabilities-model.prototype.html`（五条规则的可执行版）。

## 1. 这份矩阵怎么读

- **行的唯一真源**：解析后的 profile（`uva-app-server --dump-config` 的输出）与
  `.agents/` 目录约定（`crates/uva-core/src/layout.rs`）。本文件不发明行。
- **四要素**：宿主界面落点 / 数据来源 / 拥有者（profile 行）/ 缺行时的行为。
- **缺行的两类语义**：boot 必需行缺 → 启动失败；其余行缺 → 不挂载，能力与界面同消（D32）。
- **范围注记**：设置面的「账户 / 通用」分区不是能力域（无能力行：账户是本机身份、通用是
  壳行 config），不在本矩阵；「工具」不单列域——工具是插件行提供的服务投影（D38 服务契约），
  经对话流呈现。

### 与设计文档 §6.1 的漂移记录

| # | 设计文档说法 | 仓库事实 | 矩阵采用 |
| --- | --- | --- | --- |
| 1 | §6.1：模型分区内容来源 `.agents/providers` 目录 | 无此目录；provider 来自 `model-adapter` 行 config → `.agents/config.toml` 的 `[providers]` → 环境变量 | 仓库事实（§4.3 模型节） |
| 2 | §6.1：Hooks 内容来源「plugin 排布」 | `.agents` 布局无 hooks 目录；hook 是 `uva_hook::HookRegistry` 上的注册，`hooks` 行发布 `Host` 携带的同一张表 | 仓库事实（§4.8 Hooks 节） |
| 3 | §6.1：设置分区含「命令」 | 无独立命令数据源与方法域；命令是技能的 slash-command 扩展位 | 并入「技能与命令」分区，现状标注（§4.6 命令节） |
| 4 | §6.1：自动化分区含「模板」 | 无任何模板数据源（设计期概念） | 模板区记「现状无数据源，空态」（§4.9 计划节附注） |

## 2. 行 → 域 → 界面（profile 行反推）

| profile 行 | 能力域 | 宿主界面落点 | provides / inject（dump 实测） | 缺行时的行为 |
| --- | --- | --- | --- | --- |
| `tool-registry` | 支撑（`tools` 服务，技能 / MCP / 搜索的注入目标） | 无直接界面 | provides tools · inject — | 不挂载，能力与界面同消（D32） |
| `tool-read` | 工具（对话执行的支撑面，不单列域） | 对话流内的工具调用呈现 | provides — · inject tools | 不挂载，能力与界面同消（D32） |
| `tool-write` | 工具（对话执行的支撑面，不单列域） | 对话流内的工具调用呈现 | provides — · inject tools | 不挂载，能力与界面同消（D32） |
| `tool-edit` | 工具（对话执行的支撑面，不单列域） | 对话流内的工具调用呈现 | provides — · inject tools | 不挂载，能力与界面同消（D32） |
| `tool-job` | 工具（对话执行的支撑面，不单列域） | 对话流内的工具调用呈现 | provides — · inject tools | 不挂载，能力与界面同消（D32） |
| `tool-glob` | 工具（对话执行的支撑面，不单列域） | 对话流内的工具调用呈现 | provides — · inject tools | 不挂载，能力与界面同消（D32） |
| `tool-goal` | 工具（对话执行的支撑面，不单列域） | 对话流内的工具调用呈现 | provides — · inject tools | 不挂载，能力与界面同消（D32） |
| `tool-grep` | 工具（对话执行的支撑面，不单列域） | 对话流内的工具调用呈现 | provides — · inject tools | 不挂载，能力与界面同消（D32） |
| `tool-lsp` | 工具（对话执行的支撑面，不单列域） | 对话流内的工具调用呈现 | provides — · inject tools | 不挂载，能力与界面同消（D32） |
| `tool-shell` | 工具（对话执行的支撑面，不单列域） | 对话流内的工具调用呈现 | provides — · inject tools | 不挂载，能力与界面同消（D32） |
| `tool-todo` | 工具（对话执行的支撑面，不单列域） | 对话流内的工具调用呈现 | provides — · inject tools | 不挂载，能力与界面同消（D32） |
| `tool-web` | 工具（对话执行的支撑面，不单列域） | 对话流内的工具调用呈现 | provides — · inject tools | 不挂载，能力与界面同消（D32） |
| `model-adapter` | 模型 | 设置 · 模型 | provides llm, llm.default · inject — | boot 失败（llm / llm.default 缺，`src/state/app_state/boot.rs:341-355`）；运行期撤销 → 建会话报错（`src/state/app_state/sessions.rs:351-357`） |
| `memory` | 支撑（steering / 记忆，不进设置面，D24） | 无直接界面 | provides memory · inject — | 不挂载，能力与界面同消（D32） |
| `search` | 搜索 | 工作区组标题旁的 🔍 | provides search · inject tools, memory | 不挂载，能力与界面同消（D32） |
| `session-store` | 会话 | 主界面 · 工作区树 | provides sessions · inject — | 不挂载，能力与界面同消（D32） |
| `sandbox` | 支撑（`tool-shell` 的执行策略） | 无直接界面 | provides sandbox · inject — | 不挂载，能力与界面同消（D32） |
| `tool-task` | 任务 | 设置 · 自动化 | provides — · inject tools | 不挂载，能力与界面同消（D32） |
| `context-compactor` | 支撑（对话上下文） | 无直接界面 | provides compaction · inject llm.default | 不挂载，能力与界面同消（D32） |
| `approval` | 对话（审批在对话流应答） | 主界面 · 对话流 | provides approval · inject — | 不挂载，能力与界面同消（D32） |
| `skills` | 技能（+ 命令） | 设置 · 技能与命令 | provides skills · inject tools | 不挂载，能力与界面同消（D32） |
| `hooks` | Hooks | 设置 · Hooks | provides hooks · inject — | 不挂载，外部拦截注册服务缺席（默认策略仍在 `Host` 的同一张表上，D32） |
| `agent-loop` | 对话（+ 内置默认 agent，D22） | 主界面 · 对话流 | provides agentLoop · inject llm, llm.default, tools, sessions | boot 失败（`src/state/app_state/boot.rs:445-450`） |
| `mcp` | MCP | 设置 · MCP | provides mcp · inject — | 不挂载，能力与界面同消（D32） |
| `eval` | 评测 | 设置 · 自动化 | provides eval · inject — | 不挂载，能力与界面同消（D32） |
| `scheduler` | 计划 | 设置 · 自动化 | provides scheduler · inject — | 不挂载，能力与界面同消（D32） |
| `schedule-tool` | 计划 | 设置 · 自动化 | provides — · inject scheduler, tools | 不挂载，能力与界面同消（D32） |
| `attachments` | 支撑（附件） | 对话流内的附件呈现 | provides attachments · inject — | 不挂载，能力与界面同消（D32） |
| `ui.slot.chat.sidebar` | UI 扩展（`ui.*` 示例行，任意 `ui.` 前缀行同此） | UI 树插槽（`chat.sidebar`） | provides — · inject — | 可选行：无行 → `ui/schema` 空 components，插槽区域不渲染（隐藏） |

## 3. 目录约定 → 域（plugin 排布与运行时状态）

| 目录 / 文件约定 | 域 | 规则（优先级 / 合并） | 现状证据 |
| --- | --- | --- | --- |
| `.agents/skills/<name>/SKILL.md` | 技能 | 四源优先级：项目直定义 > 项目插件包 > 全局直定义 > 全局插件包；legacy `~/.uva` 只读、仅 skills 域读 | `crates/uva-core/src/layout.rs:84-114`、`crates/uva-app-server/src/routes/skills.rs:3-4` |
| `.agents/agents/<name>/agent.md`（+ 可选 `config.json`） | Agent | 同上四源；frontmatter + 正文合成一个 `AgentDescriptor` | `crates/uva-core/src/agent_def/discovery.rs:1-11`、`crates/uva-core/src/layout.rs:192-240` |
| `.agents/tasks/<name>/task.md` | 计划 | 声明文件（what / how often / prompt）；运行时状态在 ScheduleStore | `crates/uva-app-server/src/state/app_state/tasks.rs:5-9` |
| `.agents/mcp.json` | MCP | `mcpServers` 映射、仅 stdio；插件包内以 `<plugin>/<server>` 命名空间 | `crates/uva-core/src/layout.rs:242-275`、`crates/uva-app-server/README.md:132-135` |
| `.agents/plugins/<name>/plugin.json` | 插件目录 | 带 manifest 的目录才算插件包（同构子树，可带 `skills/` / `agents/` / `tasks/` / `mcp.json`）；包内定义按 `<plugin>/<name>` 命名空间 | `crates/uva-core/src/layout.rs:277-301` |
| `.agents/config.toml`（`[providers]`） | 模型 | provider 回退输入（`model-adapter` 行 config 优先） | `crates/uva-app-server/src/state/app_state/boot.rs:53-58` |
| `~/.agents/profile.toml` | （装配层）home patch | 层序里排在 profile 文档之后、CLI overlay 之前（`UVA_HOME_PROFILE` 可指到别处） | `crates/uva-app-server/src/profile/merge.rs:233-247` |
| `<workspace>/.agents/profile.toml` | （装配层）profile 文档 | `--profile` 缺省时的默认位置（工作区的配置层，与 `.agents/config.toml` 同层） | `crates/uva-app-server/src/main.rs:136-151`、`crates/uva-app-server/src/profile/merge.rs:215-230` |
| `<workspace>/sessions/` | 会话（运行时状态） | durable 真源，不在 `.agents`（布局明文：runtime state 不进定义目录） | `crates/uva-app-server/src/state/app_state/boot.rs:137-140`、`crates/uva-core/src/layout.rs:32-37` |

## 4. 能力域分节（13 域 × 四要素）

### 4.1 会话（sessions）

- **宿主界面落点**：主界面 · 工作区树（默认工作区常驻，D35）+ 会话详情。
- **数据来源**：`<workspace>/sessions/`（运行时状态，durable 真源，不进 `.agents`）。
- **谁拥有它**：`session-store` 行（provides sessions）。
- **缺行时的行为**：行缺 → 会话存取与工作区树同消（D32）；目录空 → 树空态。
- **现状证据**：`crates/uva-app-server/src/state/app_state/boot.rs:137-140`、
  `crates/uva-core/src/layout.rs:32-37`、`crates/uva-app-server/src/rpc/methods/sessions.rs`
  （14 个 `sessions/*` 方法，`sessions/list` / `sessions/get` / `sessions/search` 等）。

### 4.2 对话（chat）

- **宿主界面落点**：主界面 · 对话流 + 待办进度 + 输入框；审批在流内应答。
- **数据来源**：会话 transcript 行（`session-store` 的会话日志）；审批由 `approval` 服务应答。
- **谁拥有它**：`agent-loop` 行（provides agentLoop；内置默认 agent，D22）+ `approval` 行
  （provides approval）+ `context-compactor` 行（provides compaction）。
- **缺行时的行为**：`agent-loop` 缺 → **boot 失败**（`boot.rs:445-450`）；`approval` /
  `context-compactor` 缺 → 不挂载，审批应答 / 上下文压缩能力与界面同消（D32）。
- **现状证据**：`crates/uva-app-server/src/state/app_state/boot.rs:445-450`、
  `crates/uva-app-server/src/rpc/methods/chat.rs`（8 个 `chat/*` 方法，
  `chat/message/send` / `chat/message/stream` / `chat/regenerate` /
  `chat/cancel` 等）。

### 4.3 模型（models）

- **宿主界面落点**：设置 · 模型。
- **数据来源**：`model-adapter` 行 config（权威）→ `.agents/config.toml` 的 `[providers]` →
  环境变量（`OPENAI_API_KEY` / `ANTHROPIC_API_KEY`）。**没有 `.agents/providers` 目录**
  （漂移记录 #1）。
- **谁拥有它**：`model-adapter` 行（provides llm, llm.default）。
- **缺行时的行为**：boot 失败（llm / llm.default 服务缺，`boot.rs:341-355`）；运行期撤销 →
  会话创建报错（`sessions.rs:351-357`）。
- **现状证据**：`crates/uva-app-server/src/main.rs:63-77`、
  `crates/uva-app-server/src/state/app_state/boot.rs:763-796`、
  `crates/uva-app-server/src/rpc/methods/misc.rs:34-36`（`models/list`）。

### 4.4 Agent（agents）

- **宿主界面落点**：设置 · 智能体（内置默认 agent 不可停用）。
- **数据来源**：`.agents/agents/<name>/agent.md`（+ 可选 `config.json`，四源）；内置默认
  agent 来自 `agent-loop` 行（D22）。
- **谁拥有它**：`agent-loop` 行；目录定义经 boot 收进注册表。
- **缺行时的行为**：目录空 → 只剩内置 `agent`（`PROTECTED_AGENTS` 保护名）。
- **现状证据**：`crates/uva-core/src/agent_def/discovery.rs:40-80`、
  `crates/uva-app-server/src/routes/agents.rs:44-48`。

### 4.5 技能（skills）

- **宿主界面落点**：设置 · 技能与命令。
- **数据来源**：`.agents/skills/<name>/SKILL.md`（四源；legacy `~/.uva` 仅 skills 域读）；
  boot 会 seed 默认技能（`boot.rs:171-177`，已装技能不覆盖）。
- **谁拥有它**：`skills` 行（provides skills · inject tools，注册 `skill` 工具——D38 / D40）。
- **缺行时的行为**：行缺 → 服务与 `skill` 工具一并撤销（两贡献皆可逆）；目录空 → 空列表。
- **现状证据**：`crates/uva-skill/src/plugin.rs:1-46`、
  `crates/uva-app-server/src/state/app_state/boot.rs:171-177`、
  `crates/uva-app-server/src/routes/skills.rs:3-4`。

### 4.6 命令（commands，现状无独立方法域）

- **宿主界面落点**：设置 · 技能与命令（与技能同区）。
- **数据来源**：**现状无独立数据源**——技能正文的 slash-command 扩展位
  （`uva-skill/src/skill.rs:121-127`）。
- **谁拥有它**：`skills` 行（同技能）。
- **缺行时的行为**：现状即无独立行；分区命令区显示空态并注明「命令随技能提供」。
- **现状证据**：`crates/uva-skill/src/skill.rs:121-127`；方法注册表（71 个）里没有
  `commands` 前缀——本节刻意不引用任何命令方法名，这是矩阵要记录的事实，不是遗漏。

### 4.7 MCP（mcp）

- **宿主界面落点**：设置 · MCP。
- **数据来源**：`.agents/mcp.json`（两层 + 插件包命名空间 `<plugin>/<server>`，仅 stdio）。
- **谁拥有它**：`mcp` 行（provides mcp）。
- **缺行时的行为**：行缺 → MCP 工具不进注册表（D32）；文件缺 → 无 server（空态）。
- **现状证据**：`crates/uva-core/src/layout.rs:242-275`、
  `crates/uva-app-server/README.md:132-135`。

### 4.8 Hooks（hooks）

- **宿主界面落点**：设置 · Hooks（运行方式 + 运行日志；不从别处导入，D24）。
- **数据来源**：**无目录约定**——只有一张表（ADR 0028）：部署的
  `uva_hook::HookRegistry` 承载四条拦截 trait 与 prompt sections，十二个挂载点
  （六个 pass 点、五个观察点 `before_run` / `after_run` /
  `before_provider_call` / `on_error` / `on_compaction`、`prompt/sections`）。
  boot 先建它，`Host::default_policy(workspace, registry)` 把默认策略的 logging
  hook 以 `InProcess` 写进去，`hooks` 行把同一实例发布为服务（外部/插件 crate 的
  注册，句柄 drop 即撤）。没有第二个注册面。
- **谁拥有它**：`Host`（`hook_registry()` 是循环的读取口）与 `hooks` 行
  （provides hooks，发布同一实例；`agent-loop` 不注入它，表随 host 到达每次运行）。
- **缺行时的行为**：无注册 → 分区空态；`hooks` 行缺 → 外部注册服务缺席，但默认
  策略仍在（表随 host 进入运行）；行禁用 → 只撤下 `hooks` 服务（外部注册的入口
  消失），表仍归 `Host` 所有并随 host 进入每次运行，表内各行各自跟随自己的句柄，
  不随本行禁用而撤销。
- **现状证据**：`crates/uva-hook/src/registry.rs:141-165`、
  `crates/uva-hook/src/plugin.rs:63-94`、
  `crates/uva-app-server/src/composition/kernel/mod.rs:575-578`、`:983-995`、
  `crates/uva-harness/src/seams/mod.rs:494-544`、
  `crates/uva-harness/src/agent/host/mod.rs:194-215`、`:382-425`。

### 4.9 计划（schedules）

- **宿主界面落点**：设置 · 自动化（`[手动新建] [在对话中创建]` + 三标签 + 空态）。
- **数据来源**：声明 `.agents/tasks/<name>/task.md`（what / how often / prompt）+ 运行时
  ScheduleStore（boot 按 name 同步进 store）。附注：分区的「模板」区现状无数据源，空态
  （漂移记录 #4）。
- **谁拥有它**：`scheduler` 行（provides scheduler）+ `schedule-tool` 行
  （inject scheduler, tools——模型侧建计划入口）。
- **缺行时的行为**：行缺 → 计划不跑（D32）；声明目录空 → 只剩运行时条目。
- **现状证据**：`crates/uva-app-server/src/state/app_state/tasks.rs:1-12`、
  `crates/uva-app-server/src/rpc/methods/automation.rs`（`schedules/list` /
  `schedules/create` / `schedules/tick` 等 6 个）。

### 4.10 任务（tasks）

- **宿主界面落点**：设置 · 自动化。
- **数据来源**：会话日志扫描的 delegation 记录（`subagent_enter` / `subagent_exit` 头尾对，
  全部以 child 会话日志为准；parent 侧只有带 child id 的 `tool_result`）。
- **谁拥有它**：`tool-task` 行（把 `task` 工具注册进 `tools`；普通工具，注册表分发）。
- **缺行时的行为**：无 delegation → 空态；行缺 → 委派能力消失（D32）。
- **现状证据**：`crates/uva-app-server/src/routes/tasks.rs:1-34`（`tasks/list` /
  `tasks/get`）。

### 4.11 评测（evals）

- **宿主界面落点**：设置 · 自动化。
- **数据来源**：AppState 内存 `EvalSuite` registry（重启即失）。
- **谁拥有它**：`eval` 行（provides eval，runner）。
- **缺行时的行为**：行缺 → `evals/run` 不可用（D32）；注册表空 → 空态。
- **现状证据**：`crates/uva-app-server/src/routes/evals.rs:1-9`（`evals/list` /
  `evals/run` 等 5 个）。

### 4.12 搜索（search）

- **宿主界面落点**：工作区组标题旁的 🔍（跨域动作）。
- **数据来源**：六域 fan-out（tool / mcp / skill / memory / agent / session），同一
  Registry 服务模型侧 `search` 工具。
- **谁拥有它**：`search` 行（provides search · inject tools, memory）。
- **缺行时的行为**：行缺 → `search/all` 与 `search` 工具同消（D32）。
- **现状证据**：`crates/uva-app-server/src/routes/search.rs:1-16`、
  `crates/uva-app-server/src/rpc/methods/misc.rs:47-51`。

### 4.13 UI 扩展（ui）

- **宿主界面落点**：UI 树插槽（`chat.sidebar` / `session.detail.extensions` …；插槽 id 与
  宿主无关，D25——web / TUI / desktop 三宿主由同一 UI 装载器装这棵树）。
- **数据来源**：profile `[ui]` 段行（`ui.` 前缀 + `ui-bundle` runtime）。
  行的 `config` 声明插槽（`slots = [{ id, cardinality }]`，`cardinality`
  取 `one` / `many`，缺省 `many`）并向先声明的插槽注册组件
  （`components = [{ slot, kind, props }]`）；`ui/schema` 回
  `{ slots: [{id, cardinality, owner}], schemas: [{version, components}] }`。
  拒可执行载荷（`script` / `stylesheet` / `css` …）。
- **谁拥有它**：`ui-bundle` 行本身（装载器消费；无核心工厂，dump 实测 provides / inject
  均为空）。装配是 Effect 语义：声明与注册各持可撤销句柄，drop 即回滚；
  向未声明插槽注册 / 重复声明 / `one` 槽超基数在激活阶段失败。
- **缺行时的行为**：无行 → `ui/schema` 空 components，插槽区域不渲染（隐藏）；未知 kind →
  占位。
- **现状证据**：`crates/uva-app-server/src/ui/schema.rs`（wire 类型与错误）、
  `crates/uva-app-server/src/ui/tree.rs`（插槽表、可撤销句柄、激活入口）。

## 5. 一次性核对记录

- **日期**：2026-11-10（执行日）。
- **方法**：临时脚本 `/tmp/uva-knife4-audit.sh`（已弃置，未入库）——合成部署（隔离
  `HOME`，防 home patch 泄入）→ 两份 `--dump-config`（全量 22 行 / 精简 5 行）→ 行 /
  目录约定 / 方法名三面双向对账 → 打印对照表目检。全量 dump 形状断言：22 id /
  21 native / 1 ui-bundle / 22 provides（进程插件行与 wasm 行随动态插件通道一起删除）。
- **结果**（脚本真输出摘录）：

  ```text
  OK: 行 / 目录 / 方法三面全部对上（22 行、19 个方法名）
  === 精简部署（5 行）：缺行 = dump 缺行（D32 在 profile 层的证据）===
  model-adapter
  agent-loop
  session-store
  skills
  mcp
  ```

  19 个方法名全部在注册表（66 个 `router.register`）中：`chat/message/send` /
  `evals/run` / `models/list` / `schedules/tick` / `search/all` /
  `sessions/list` / `tasks/list` / `ui/schema` 等；精简部署里
  `eval` 缺行 → dump 无此行（`grep -c '"id": "eval"'` 为 0）。
- **签核**：刀 4 执行者（omp 编码代理）——逐行目检 §2 对照表：落点与设计 §6.1 分区一致
  （会话 → 工作区树、模型 / 技能 / MCP / Hooks / 自动化 → 设置分区、搜索 → 工作区组旁
  🔍、`ui.*` → UI 树插槽），域归属与 `provides` / `inject` 相容（`skills` 行 inject
  `tools`、`schedule-tool` 行 inject `scheduler, tools`、`agent-loop` 行 inject 全五服务）。
- **再核对**：本文件是反推文档——profile 布局或目录约定变更后应重跑一次核对（重建合成
  部署的配方在 `docs/design/2026-11-09-capability-matrix-plan.md` Task 3 / Task 5）。

## 6. TUI 落点（刀 5 / 刀 6 执行后）

本仓的矩阵没有逐行 `tui` 列（刀 4 产物是分区文档，不是带列的表格）；本节是刀 5 / 刀 6
落地的可追溯记录：域 → TUI 状态 → 证据。状态只有三档（与刀 4 计划同义）：`full` =
该域全部方法面可从 TUI 到达、`adapted` = 主路径到达但方法面有缺口（缺口写明）、
`n/a` = 介质不适用。

| 域 | TUI 状态 | 说明 | 证据 |
| --- | --- | --- | --- |
| 会话（`sessions`） | adapted | 目录面板：`list` / `get` / `usage` / `status` / `export` / `fork` / `resume` / `compact` / `delete`；写操作先确认。`sessions/search` 由「搜索」域承载（`search/all`）；`sessions/events` 无界面落点（`sessions/follow` 已给全量行） | `crates/uva-tui/src/domains.rs`（`sessions` 条目）、`crates/uva-tui/src/views/tests.rs` |
| 对话（`chat`） | adapted | `chat/session/create` / `chat/message/send` / `chat/cancel` 走 reducer 既有路径；`chat/regenerate` / `chat/feedback` 在命令面板；`chat/usage` / `chat/operation` 未接（无界面落点） | `crates/uva-tui/src/app/tests.rs`、`crates/uva-tui/src/views/tests.rs` |
| Agent（`agents`） | full | `list` / `get` / `delete`（确认）；`create` 经 `:json` 通用门（无表单） | `crates/uva-tui/src/domains.rs`、`crates/uva-tui/src/domains/tests.rs` |
| 技能（`skills`） | full | `list` / `get` / `delete`（确认）/ `reload`（确认）；`create` 经 `:json` | 同上 |
| 工具（`tools`） | full | `list` / `get` / `unregister`（确认）；`register` 经 `:json` | 同上 |
| 任务（`tasks`） | full | `list` / `get`（服务器只注册这两个，无写操作） | 同上 |
| 计划（`schedules`） | full | `list` / `get` / `tick`（确认）/ `delete`（确认）；`create` / `patch` 经 `:json` | 同上 |
| 评测（`evals`） | full | `list` / `get` / `run`（确认）/ `delete`（确认）；`create` 经 `:json` | 同上 |
| 模型（`models`） | full | `list`（取项 `/models`）+ 行摊详情；只读 | 同上 |
| 搜索（`search`） | full | `search/all`（`q` 必填）+ 行摊详情；侧栏 `s` 与 `:search <词>` 两个入口 | 同上 |
| UI 扩展（`ui`） | full | `ui/schema`（取项 `/schemas`）+ 行摊详情；只读 | 同上 |
| 配置（`profile`） | adapted | `profile/list` / `profile/get` / `profile/set` / `profile/create` / `profile/delete` 经 `:json` 通用门可达（写入走事务与四态回执）；**未**接目录面板（`profile/list` 的返回不是 `List<T>` 信封，需要取项适配） | `crates/uva-app-server/src/rpc/methods/names.rs`（profile 段，`:json` 通用门按它放行）、`crates/uva-app-server/src/rpc/methods/profile.rs` |

缺口说明：`create` / `patch` / `replace` / `register` 是多字段写，TUI 不提供表单，经
`:json` 通用门可达（R-10 允许先只读）；若评审认为该降级，把对应行改 `adapted` 即可。本文
§6 是刀 5 / 刀 6 的落地记录，profile 或方法面变化后随 §5 的再核对一起更新。

刀 7 新增 `profile/list|get|set|create|delete` 方法面与自管 MCP 的 `profile`
工具。TUI 侧：方法名表（`crates/uva-app-server/src/rpc/methods/names.rs`）已收录
这五个方法（`:json` 通用门按它放行），但
`profile/list` 的返回不是 `List<T>` 信封，未接目录面板——因此本表的 `profile`
行是 `adapted`（可达但无专用视图）。设置页的写回交互由浏览器侧承担
（`uva-web/src/api/settings-profile.ts` 绑同一组方法）。

## 7. 门禁（矩阵棘轮）

`make check-host-capabilities`（脚本 `scripts/check-host-capabilities.sh`）把本文档变成
可机械核验的棘轮：

1. 每行必须有状态：`full` / `adapted` / `n/a` / `planned`（其它值即失败）；
2. `planned` 行必须带锚点（说明里写清"什么会让它变成真"），`n/a` 行必须写理由；
3. `full` / `adapted` 行引用的证据文件必须在磁盘上；
4. 矩阵点名的方法必须在服务器注册表（`crates/uva-app-server/src/rpc/methods/`）里存在；
5. 缺口数（`planned` + `n/a`）不得超过 `scripts/host-capabilities/baseline`（当前基线 0）。

反向验证（改坏任一格 ⇒ 门禁红）：把某格改成无锚点的 `planned`、删掉一列、把证据指向不存在的
文件、在矩阵里编一个服务器没有的方法、让缺口数回升——五种情形都会让门禁失败。

基线只能**单独一次提交**地抬高（与 `check-plugin-shape` 同一纪律）：矩阵退化是显式决定，
不是漂移。
