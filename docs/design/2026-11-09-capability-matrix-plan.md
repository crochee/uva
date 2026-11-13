# 能力矩阵（刀 4 · 从 profile 推导）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 产出一份**可复核**的入库矩阵文档 `docs/host-capabilities.md`：15 个能力域（会话 · 对话 ·
插件目录 · 模型 · Agent · 技能 · 命令 · MCP · Hooks · 计划 · 任务 · 工作流 · 评测 · 搜索 · UI 扩展）
每个域写清四件事——**落在哪个宿主界面** / **数据来自哪里** / **谁拥有它**（哪个 profile 行）/
**缺行时的行为**。矩阵的每一行都追溯到**一个 profile 行或一个目录约定**（"解析后的 profile 的
反推"，D26 / D27），并由一个**一次性**核对流程完成"行 → 域 → 界面"的对账与人工签核。
**只出文档与核对**：不改任何 Rust / TS 代码、不新增 crate、不新增门禁、不动核心与内核。

**Architecture:** 一份 tracked 文档 + 一个 throwaway 脚本。文档四层结构：§2「行 → 域 → 界面」
总表（行全集 = `native_profile()` 的 22 个内置行 + `wasm` 可选行 + `[ui]` 段的 `ui.*` 行，
`provides` / `inject` 列以 `uva-app-server --dump-config` 的真实输出为证）；§3「目录约定 → 域」
（`.agents/` 两层布局 + `mcp.json` + 运行时状态目录）；§4 每域四要素小节；§5 一次性核对记录。
核对脚本放 **`/tmp/uva-knife4-audit.sh`（用完即弃、不入库）**：构造一个**合成部署**
（隔离 `HOME`，防 home patch 泄入），跑两份 `--dump-config`（全量 24 行 / 精简 5 行），做三面
双向对账（dump 行 ↔ 文档表、目录约定 ↔ 文档表、文档引用的方法名 ↔ 服务器注册表），打印
"行 → 域 → 界面"对照表供人工签核。**不入 `scripts/`**：本轮没有新增门禁脚本，AGENTS.md §3.6
的"`make lint-sh` 清单 + `shellcheck.yml` + §3.6 表"三处同步**不触发**（D15 已明示"本轮没有
新增门禁脚本"）。

**Tech Stack:** Markdown（入库）+ bash（一次性核对）+ 既有 CLI `uva-app-server --dump-config`
（确定性 dump：稳定行序、逐深度排序的对象键，见 `crates/uva-app-server/src/main.rs:103-108`
与 `src/profile/mod.rs:454-507`）+ grep / find。零新依赖。

**Spec:** `docs/design/2026-11-09-desktop-frontend-tree-design.md` —— §6.1（能力域 → 落点表、
设置分区表）、§7 的 D15 / D17 / D19 / D20 / D26 / D27 / D32 / D38 / D39 / D40、§8 的刀 4、
§11 用户拍板第 8 / 11 / 13 条；AGENTS.md §3.6（lint-sh 三处同步规则——本轮不触发）。

## Global Constraints

- **刀 4 只出文档 + 核对**：不引入任何新的运行时机制；不改 `crates/**`、`uva-web/**`、
  `desktop/**`、`Makefile`、`.github/**`、`scripts/**`、`AGENTS.md`。核心一行不动（D38 前提）。
- **唯一新建文件**是 `docs/host-capabilities.md`；唯一修改文件是 `CHANGELOG.md`（`[Unreleased]`）。
  一次性脚本在 `/tmp/`，**用完即弃**（`rm -rf /tmp/uva-knife4 /tmp/uva-knife4-audit.sh`），
  绝不落进仓库。
- **矩阵不是手写愿望清单**：每行必须能追溯到一个 profile 行（以 `--dump-config` 输出或
  `native_profile()` 源码为证）或一个目录约定（以 `crates/uva-core/src/layout.rs` 为证）。
  写不出来的行不许出现在矩阵里。
- **事实以仓库为准，不照抄设计文档**：设计文档 §6.1 与仓库事实存在**三处漂移**（`.agents/providers`
  目录不存在 / Hooks 无目录约定 / 命令无独立数据源），矩阵必须记仓库事实并显式列出漂移
  （Task 4 Step 3）。
- 方法名引用一律写成反引号全形 `` `域/动词` ``（如 `` `sessions/list` ``）——核对脚本按这个
  形状从文档提取并到注册表里验证存在性。
- `--dump-config` **没有** native 兜底：`main.rs:173-181` 直接 dump `inputs.resolve()` 的结果，
  native 兜底（`Some(profile) if !profile.core.is_empty() => …, _ => native_profile()`）只在
  boot 路径（`src/state/app_state/boot.rs:242-243`）。所以**空 profile 的 dump 是空表**，
  矩阵的"内置行全集"以 `native_profile()`（`src/composition/kernel/mod.rs:153-177`）为准。
- CHANGELOG 的 `[Unreleased]` 段在 `make check-claim-language` 的扫描范围内
  （AGENTS.md §3.6）：措辞避免绝对化断言，提交前跑一次该目标。
- 不 push 远程仓库；提交信息只含 `docs/host-capabilities.md` 与 `CHANGELOG.md`。

## Review Focus

下面五处是最容易写错的地方，每个都在对应任务里钉上可执行的核对。

1. **矩阵变成第二份真相**。钉子：Task 5 的三面双向对账——dump 出的每个行 id 必须在矩阵 §2
   表里，矩阵 §2 表的每个行 id 必须在 dump 里；目录约定同理。
2. **照抄设计文档的路径**。§6.1 写"模型 ← `.agents/providers`"——仓库里**没有**这个目录
   （provider 来自 `model-adapter` 行 config → `.agents/config.toml` `[providers]` → 环境变量，
   `src/main.rs:63-77`）。§6.1 写"Hooks ← plugin 排布"——`.agents` 布局里**没有** hooks 目录
   （hook 是挂载的插件行在 harness `Host` 上的注册，`crates/uva-harness/src/agent/host/mod.rs:16-21`）。
   钉子：Task 4 Step 3 的漂移表 + Task 5 的目录约定对账（合成树里**不造** hooks 目录，矩阵
   也**不写** hooks 路径）。
3. **两类"缺行"语义混为一谈**。boot 必需行（`agent-loop` / `model-adapter`）缺 → **boot 直接
   失败**（"the agentLoop service is unavailable after mount … the profile must include the
   `agent-loop` row"，`src/state/app_state/boot.rs:445-450`；llm 缺 → "the llm service is
   unavailable; remount the model-adapter row"，`src/state/sessions.rs:351-357`）；其余行缺 →
   **不挂载、能力与界面同消**（D32，README `:181-183`）。矩阵每域必须写明自己属于哪一类。
4. **两个"任务"不是一个东西**。`.agents/tasks/<name>/task.md` 是**计划域**的声明文件
   （`src/state/app_state/tasks.rs:5-9`：declaration vs runtime）；`tasks/list` / `tasks/get`
   方法域扫的是**会话日志里的 delegation 记录**（`src/routes/tasks.rs:1-34`，`subagent_enter` /
   `subagent_exit` 头尾对）。矩阵的两个域必须分开落。
5. **dump ≠ mount 校验**。`--dump-config` 不跑 `build_plugins`：未知 id 的行照样 dump（打出
   空 `provides` / `inject`），装载失败是 boot 的事（README `:181-183`）。核对脚本的结论只覆盖
   "行的存在性与归属"，**不**宣称"该 profile 能装起来"。

---

### Task 1: 事实基线复核（只读）

**Files:**
- Read only: `crates/uva-app-server/src/composition/kernel/mod.rs`、`src/main.rs`、
  `src/rpc/methods/**`、`src/routes/**`、`crates/uva-core/src/layout.rs`

**Interfaces:**
- Consumes: 无
- Produces: 本计划"事实基线"仍然成立的确认（计划与执行之间仓库可能已漂移）

- [ ] **Step 1: 确认内置行全集还是 22 个**

Run: `sed -n '/const ENTRIES: \&str; 22/,/^    \];/p' crates/uva-app-server/src/composition/kernel/mod.rs | grep -c '^\s*"'`
Expected: `22`。若不是 22，**停下**：以新的 `native_profile()` 全集为准重排本计划的行清单
（Task 3 的合成 profile 与 §2 表同步改），其余任务结构不变。

- [ ] **Step 2: 确认方法注册表的规模**

Run: `grep -c 'router\.register("' crates/uva-app-server/src/rpc/methods/agents.rs crates/uva-app-server/src/rpc/methods/automation.rs crates/uva-app-server/src/rpc/methods/chat.rs crates/uva-app-server/src/rpc/methods/misc.rs crates/uva-app-server/src/rpc/methods/sessions.rs`
Expected:

```
crates/uva-app-server/src/rpc/methods/agents.rs:14
crates/uva-app-server/src/rpc/methods/automation.rs:19
crates/uva-app-server/src/rpc/methods/chat.rs:7
crates/uva-app-server/src/rpc/methods/misc.rs:12
crates/uva-app-server/src/rpc/methods/sessions.rs:14
```

（合计 66；矩阵引用的任何 `域/动词` 都要能在这里面 grep 到。）

- [ ] **Step 3: 确认 dump 路径没有 native 兜底**

Run: `sed -n '173,181p' crates/uva-app-server/src/main.rs`
Expected: 看到 `if args.dump_config { let profile = inputs.resolve()?; … print!(…dump_profile…); return Ok(()); }`——
dump 的是**层序解析结果**（bundle → profile → home patch → CLI overlay），没有 `native_profile()` 兜底。

- [ ] **Step 4: 确认目录约定的三常量**

Run: `grep -n 'PLUGIN_MANIFEST\|MCP_CONFIG\|AGENTS_DIR' crates/uva-core/src/layout.rs | head -6`
Expected: 前三行为 `50:pub const PLUGIN_MANIFEST: &str = "plugin.json";`、
`53:pub const MCP_CONFIG: &str = "mcp.json";`、`56:pub const AGENTS_DIR: &str = ".agents";`
（其余命中是 `AGENTS_DIR` 的使用点）。

---

### Task 2: 创建矩阵文档骨架

**Files:**
- Create: `docs/host-capabilities.md`

**Interfaces:**
- Consumes: 设计文档 §6.1（落点表）、§7（D15 / D17 / D19 / D26 / D27 / D32）
- Produces: 文档骨架（§1–§5 的标题与表格头；域小节留占位）

- [ ] **Step 1: 写骨架**

`docs/host-capabilities.md` 初始内容（占位部分标 `<!-- 待填 -->`）：

```markdown
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
<!-- 待填（Task 4 Step 3） -->

## 2. 行 → 域 → 界面（profile 行反推）

| profile 行 | 能力域 | 宿主界面落点 | provides / inject（dump 实测） | 缺行时的行为 |
| --- | --- | --- | --- | --- |
<!-- 待填：22 内置行 + wasm + ui.* 示例行，共 24 行（Task 3） -->

## 3. 目录约定 → 域（plugin 排布与运行时状态）

| 目录 / 文件约定 | 域 | 规则（优先级 / 合并） | 现状证据 |
| --- | --- | --- | --- |
<!-- 待填（Task 4 Step 1） -->

## 4. 能力域分节（15 域 × 四要素）

<!-- 每域一节，格式：
### 4.N <域>（<key>）
- **宿主界面落点**：…
- **数据来源**：…（profile 行或目录约定，逐字可追溯）
- **谁拥有它**：…（行 id）
- **缺行时的行为**：…（boot 失败 / 不挂载（D32）/ 空态 / 隐藏）
- **现状证据**：`文件:行`
-->

## 5. 一次性核对记录

<!-- 待填（Task 5）：日期、命令、输出摘要、签核 -->
```

- [ ] **Step 2: 确认骨架可被后续核对脚本解析**

Run: `grep -c '^## ' docs/host-capabilities.md && grep -c '待填' docs/host-capabilities.md`
Expected: `5` 与 `4`（五个二级标题；四处 `待填` 占位——§1 漂移表 / §2 表 / §3 表 / §5 记录。
§4 的格式说明是注释块，不带 `待填` 字样）。

---

### Task 3: 「行 → 域 → 界面」总表 + dump 取证

**Files:**
- Modify: `docs/host-capabilities.md`（§2 表）
- 临时（不入库）: `/tmp/uva-knife4/`（合成部署与两份 dump）

**Interfaces:**
- Consumes: `uva-app-server --dump-config --directory <path>`（全局 flag，`main.rs:103-108`）；
  profile TOML 形状（`hmr` + `[core] entries` + `[ui] entries`，`src/profile/model.rs:220-234`）
- Produces: §2 表的 24 行（行 id + 域 + 落点 + dump 实测的 provides/inject + 缺行行为）

- [ ] **Step 1: 造合成部署（全量行 + 全部目录约定形态）**

```bash
WORK=/tmp/uva-knife4
rm -rf "$WORK" && mkdir -p "$WORK/home" "$WORK/mini" \
  "$WORK/full/.agents/skills/demo" "$WORK/full/.agents/agents/reviewer" \
  "$WORK/full/.agents/tasks/daily" "$WORK/full/.agents/plugins/acme"
printf -- '---\nname: demo\ndescription: demo skill\n---\n\nDemo body.\n' \
  > "$WORK/full/.agents/skills/demo/SKILL.md"
printf -- '---\nname: reviewer\ndescription: reviews things\n---\n\nReviews.\n' \
  > "$WORK/full/.agents/agents/reviewer/agent.md"
printf -- '---\nname: daily\nschedule: "0 9 * * *"\nprompt: standup\n---\n' \
  > "$WORK/full/.agents/tasks/daily/task.md"
printf '{ "name": "acme", "version": "0.1.0" }\n' \
  > "$WORK/full/.agents/plugins/acme/plugin.json"
printf '{ "mcpServers": { "fs": { "command": "mcp-server-fs", "args": [] } } }\n' \
  > "$WORK/full/.agents/mcp.json"
cat > "$WORK/full/profile.toml" <<'EOF'
hmr = false

[core]
entries = [
  { id = "tool-registry", runtime = "native", config = {} },
  { id = "tool-read", runtime = "native", config = {} },
  { id = "tool-write", runtime = "native", config = {} },
  { id = "tool-shell", runtime = "native", config = {} },
  { id = "tool-todo", runtime = "native", config = {} },
  { id = "tool-web", runtime = "native", config = {} },
  { id = "model-adapter", runtime = "native", config = {} },
  { id = "memory", runtime = "native", config = {} },
  { id = "search", runtime = "native", config = {} },
  { id = "session-store", runtime = "native", config = {} },
  { id = "sandbox", runtime = "native", config = {} },
  { id = "subagent-backend", runtime = "native", config = {} },
  { id = "context-compactor", runtime = "native", config = {} },
  { id = "approval", runtime = "native", config = {} },
  { id = "skills", runtime = "native", config = {} },
  { id = "agent-loop", runtime = "native", config = {} },
  { id = "mcp", runtime = "native", config = {} },
  { id = "eval", runtime = "native", config = {} },
  { id = "scheduler", runtime = "native", config = {} },
  { id = "schedule-tool", runtime = "native", config = {} },
  { id = "attachments", runtime = "native", config = {} },
  { id = "process-plugins", runtime = "native", config = {} },
  { id = "wasm", runtime = "wasm", config = { component = "echo.wat" } },
]

[ui]
entries = [
  { id = "ui.slot.chat.sidebar", runtime = "ui-bundle", config = { version = 1, components = [ { slot = "chat.sidebar", kind = "panel", props = { title = "Notes" } } ] } },
]
EOF
cat > "$WORK/mini/profile.toml" <<'EOF'
hmr = false

[core]
entries = [
  { id = "model-adapter", runtime = "native", config = {} },
  { id = "agent-loop", runtime = "native", config = {} },
  { id = "session-store", runtime = "native", config = {} },
  { id = "skills", runtime = "native", config = {} },
  { id = "mcp", runtime = "native", config = {} },
]
EOF
```

说明：`wasm` 行照 `composition/kernel/tests.rs:362-374` 的既有形状写
（`{ component = "echo.wat" }`，manifest 无宿主也可读）；`ui` 行照 `src/ui/schema.rs:93-100`
的示例（`ui.` 前缀 + `ui-bundle` + `components[].slot`）。

- [ ] **Step 2: 跑全量 dump 并断言形状**

```bash
export HOME=/tmp/uva-knife4/home   # 隔离 home patch（~/.agents/profile.toml）
unset UVA_HOME_PROFILE
cargo run -q -p uva-app-server -- --dump-config --directory /tmp/uva-knife4/full \
  > /tmp/uva-knife4/full.json
grep -c '"id":' /tmp/uva-knife4/full.json
grep -c '"runtime": "native"' /tmp/uva-knife4/full.json
grep -c '"runtime": "wasm"' /tmp/uva-knife4/full.json
grep -c '"runtime": "ui-bundle"' /tmp/uva-knife4/full.json
grep -c '"provides":' /tmp/uva-knife4/full.json
```

Expected: `24` / `22` / `1` / `1` / `24`（每行都打 `provides`——无工厂的行打空数组，
`src/profile/mod.rs:523-580`）。

- [ ] **Step 3: 跑精简 dump 并断言"缺行 = dump 缺行"**

```bash
cargo run -q -p uva-app-server -- --dump-config --directory /tmp/uva-knife4/mini \
  > /tmp/uva-knife4/mini.json
grep -o '"id": "[^"]*"' /tmp/uva-knife4/mini.json | sed 's/"id": "//;s/"$//'
grep -c '"id": "eval"' /tmp/uva-knife4/mini.json || echo "eval 缺行（预期）"
```

Expected: 第一条列出恰好 5 个 id（`model-adapter` / `agent-loop` / `session-store` / `skills` /
`mcp`）；第二条 `grep` 退出码 1 并打印 `eval 缺行（预期）`——**profile 没写的行不出现在
解析结果里**，这就是 D32 在 profile 层的证据。

- [ ] **Step 4: 填 §2 表**

把 `full.json` 里每个 id 的 `provides` / `inject` 抄进 §2 表（**以 dump 输出为准**，不凭记忆）。
行 id 用反引号放第一列，**一个 id 一行、共 24 行**（合并行会让 Task 5 的逐 id 双向对账漏配，
不许用）；域与落点按下面的事实基线填（同族行的域 / 落点列重复填写即可）：

| profile 行 | 能力域 | 宿主界面落点 |
| --- | --- | --- |
| `tool-registry` | 支撑（`tools` 服务，技能 / MCP / 搜索的注入目标） | 无直接界面 |
| `tool-read` / `tool-write` / `tool-shell` / `tool-todo` / `tool-web` | 工具（对话执行的支撑面，不单列域） | 对话流内的工具调用呈现 |
| `model-adapter` | 模型 | 设置 · 模型 |
| `memory` | 支撑（steering / 记忆，**不进设置面**，D24） | 无直接界面 |
| `search` | 搜索 | 工作区组标题旁的 🔍 |
| `session-store` | 会话 | 主界面 · 工作区树 |
| `sandbox` | 支撑（`tool-shell` 的执行策略） | 无直接界面 |
| `subagent-backend` | 任务 | 设置 · 自动化 |
| `context-compactor` | 支撑（对话上下文） | 无直接界面 |
| `approval` | 对话（审批在对话流应答） | 主界面 · 对话流 |
| `skills` | 技能（+ 命令） | 设置 · 技能与命令 |
| `agent-loop` | 对话（+ 内置默认 agent，D22） | 主界面 · 对话流 |
| `mcp` | MCP | 设置 · MCP |
| `eval` | 评测 | 设置 · 自动化 |
| `scheduler` / `schedule-tool` | 计划 | 设置 · 自动化 |
| `attachments` | 支撑（附件） | 对话流内的附件呈现 |
| `process-plugins` | 插件目录（进程插件） | 设置 · 插件 |
| `wasm`（可选行） | 插件目录（wasm 插件） | 设置 · 插件 |
| `ui.slot.chat.sidebar`（`ui.*` 示例行，可选） | UI 扩展 | UI 树插槽（`chat.sidebar`） |

缺行行为列：`agent-loop` 写"boot 失败（`state/app_state/boot.rs:445-450`）"；
`model-adapter` 写"llm 服务缺 → 会话创建报错（`state/sessions.rs:351-357`）"；
其余内置行写"不挂载，能力与界面同消（D32）"；`wasm` / `ui.*` 写"可选行：没有就没有该插件 /
插槽（`ui/schema` 空 components，插槽区域不渲染）"。

- [ ] **Step 5: 断言 §2 表行数**

Run: `grep -cE '^\| `[a-z][a-z0-9.-]*` \|' docs/host-capabilities.md`
Expected: `24`（§2 表的数据行 = dump 的全部 id：22 个内置行 + `wasm` + `ui.slot.chat.sidebar`，
一个 id 一行。上表里五个 `tool-*` 与 `scheduler` / `schedule-tool` 的"合并展示"只是本计划的
紧凑写法，落表时拆成独立行、域 / 落点列重复填写）。

---

### Task 4: 「目录约定 → 域」表 + 15 域四要素小节 + 漂移记录

**Files:**
- Modify: `docs/host-capabilities.md`（§1 漂移表、§3 表、§4 的 15 个小节）

**Interfaces:**
- Consumes: `crates/uva-core/src/layout.rs`（布局契约）、Task 3 的事实基线
- Produces: 矩阵的主体内容

- [ ] **Step 1: 填 §3 目录约定表**

按下表填（字符串逐字用左列形状——Task 5 的对账按字面 grep）：

| 目录 / 文件约定 | 域 | 规则 | 现状证据 |
| --- | --- | --- | --- |
| `.agents/skills/<name>/SKILL.md` | 技能 | 四源优先级：项目直定义 > 项目插件包 > 全局直定义 > 全局插件包；legacy `~/.uva` 只读、仅 skills 域读 | `crates/uva-core/src/layout.rs:84-114`、`crates/uva-app-server/src/routes/skills.rs:3-4` |
| `.agents/agents/<name>/agent.md`（+ 可选 `config.json`） | Agent | 同上四源；frontmatter + 正文合成一个 `AgentDescriptor` | `crates/uva-agents/src/discovery.rs:1-11`、`crates/uva-agents/README.md:5-8` |
| `.agents/tasks/<name>/task.md` | 计划 | 声明文件（what / how often / prompt）；运行时状态在 ScheduleStore | `crates/uva-app-server/src/state/app_state/tasks.rs:5-9` |
| `.agents/mcp.json` | MCP | `mcpServers` 映射、仅 stdio；插件包内以 `<plugin>/<server>` 命名空间 | `crates/uva-core/src/layout.rs:242-275`、`crates/uva-app-server/README.md:134-135` |
| `.agents/plugins/<name>/plugin.json` | 插件目录 | 带 manifest 的目录才算插件包（同构子树，可带 `skills/` / `agents/` / `tasks/` / `mcp.json`） | `crates/uva-core/src/layout.rs:277-301`、`routes/plugins/mod.rs:60-131` |
| `.agents/config.toml`（`[providers]`） | 模型 | provider 回退输入（`model-adapter` 行 config 优先） | `crates/uva-app-server/src/main.rs:63-77`、`src/state/app_state/boot.rs:763-794` |
| `~/.agents/profile.toml` | （装配层）home patch | 层序里排在 profile 文档之后、CLI overlay 之前 | `src/profile/merge.rs:217-231` |
| `<workspace>/profile.toml` | （装配层）profile 文档 | `--profile` 缺省时的默认位置 | `src/main.rs:225-237` |
| `<workspace>/sessions/` | 会话（运行时状态） | durable 真源，不在 `.agents`（布局明文：runtime state 不进定义目录） | `src/state/app_state/boot.rs:137-139`、`crates/uva-core/src/layout.rs:32-37` |

- [ ] **Step 2: 填 §4 的 15 个小节**

每域一节（`### 4.N <域>（<key>）`），五要素清单。内容按下面的事实基线转写
（证据指针若已漂移，以当次读到的为准并更新行号）：

| 域 | 宿主界面落点 | 数据来源 | 拥有者（行） | 缺行时的行为 |
| --- | --- | --- | --- | --- |
| 会话 | 主界面 · 工作区树（默认工作区常驻，D35）+ 会话详情 | `<workspace>/sessions/`（运行时状态） | `session-store` | 行缺 → 会话存取消失（D32）；目录空 → 树空态 |
| 对话 | 主界面 · 对话流 + 待办进度 + 输入框；审批在流内应答 | 会话 transcript 行（同上 store） | `agent-loop`（默认 agent，D22）+ `approval` + `context-compactor` | `agent-loop` 缺 → **boot 失败**（`boot.rs:445-450`） |
| 插件目录 | 设置 · 插件 | profile 全部行 + `.agents/plugins/<name>/plugin.json` + `[plugins]` composition（resolved / skipped） | 装配本身；`process-plugins`（进程插件）、`wasm`（wasm 插件） | 无插件包 → `plugins/list` 的 `bundles` 空数组（空态） |
| 模型 | 设置 · 模型 | `model-adapter` 行 config（权威）→ `.agents/config.toml` `[providers]` → 环境变量 | `model-adapter` | 卸载 → agent-loop 停摆、建会话报错（`sessions.rs:351-357`） |
| Agent | 设置 · 智能体（内置默认 agent 不可停用） | `.agents/agents/<name>/agent.md` + `config.json`（四源）；内置默认 agent 来自 `agent-loop` 行 | `agent-loop`；目录定义经 boot 收进注册表 | 目录空 → 只剩内置 `agent`（`routes/agents.rs:44-48` 保护名） |
| 技能 | 设置 · 技能与命令 | `.agents/skills/<name>/SKILL.md`；boot 会 seed 默认技能（`state/app_state/boot.rs:171-177`） | `skills`（inject `tools`，注册 `skill` 工具——D38 / D40） | 行缺 → 服务与工具一并撤销（两贡献皆可逆，`uva-skill/src/plugin.rs:1-46`）；目录空 → 空列表 |
| 命令 | 设置 · 技能与命令（与技能同区） | **现状无独立数据源**——技能的 slash-command 扩展位（`uva-skill/src/skill.rs:123-126`） | `skills`（同技能） | 现状即无独立行；分区命令区显示空态并注明"命令随技能提供" |
| MCP | 设置 · MCP | `.agents/mcp.json`（两层 + 插件包命名空间） | `mcp` | 行缺 → MCP 工具不进注册表（D32）；文件缺 → 无 server（空态） |
| Hooks | 设置 · Hooks（运行方式 + 运行日志；不从别处导入，D24） | **无目录约定**——挂载的插件行在 harness `Host` 上注册（六条 pass 链 + 五个观察点） | 各提供 hook 的插件行（steering 面） | 无注册 → 分区空态；行禁用 → 其 hook 随 Registration 撤销（`host/mod.rs:644-674`） |
| 计划 | 设置 · 自动化（`[手动新建] [在对话中创建]` + 三标签 + 空态） | 声明 `.agents/tasks/<name>/task.md` + 运行时 ScheduleStore | `scheduler` + `schedule-tool` | 行缺 → 计划不跑（D32）；声明目录空 → 只剩运行时条目 |
| 任务 | 设置 · 自动化 | 会话日志扫描的 delegation 记录（`subagent_enter` / `subagent_exit`，`routes/tasks.rs:1-34`） | `subagent-backend`（发布 `subagents` 服务） | 无 delegation → 空态；行缺 → 委派能力消失 |
| 工作流 | 设置 · 自动化 | AppState **内存** `WorkflowSpec` registry（`routes/workflows.rs:1-17`；重启即失） | 无专属行——`workflow_host` 经 AppState 解析 agent 注册表（记录为已知缺口） | 注册表空 → 空态 |
| 评测 | 设置 · 自动化 | AppState 内存 `EvalSuite` registry（`routes/evals.rs:1-9`） | `eval`（runner） | 行缺 → `evals/run` 不可用；注册表空 → 空态 |
| 搜索 | 工作区组标题旁的 🔍（跨域动作） | 六域 fan-out（tool / mcp / skill / memory / agent / session），同一 Registry 服务模型侧 `search` 工具 | `search`（自建会话索引 + 注入 tools / memory） | 行缺 → `search/all` 与 `search` 工具同消（D32） |
| UI 扩展 | UI 树插槽（`chat.sidebar` / `session.detail.extensions`…；插槽 id 与宿主无关，D25） | profile `[ui]` 段行（`ui.` 前缀 + `ui-bundle` + `{ version, components: [{ slot, kind, props }] }`） | `ui-bundle` 行本身（装载器消费） | 无行 → `ui/schema` 空 components，插槽区域不渲染（隐藏）；未知 kind → 占位 |

每节另起一行**现状证据**指向具体文件（上表"证据"列未展开的按 Task 1 复核结果补）。
注意命令域**不引用任何方法名**（注册表里没有 `commands/*`——这是矩阵要记录的事实，不是遗漏）。

- [ ] **Step 3: 填 §1 漂移表**

| # | 设计文档说法 | 仓库事实 | 矩阵采用 |
| --- | --- | --- | --- |
| 1 | §6.1：模型分区内容来源 `.agents/providers` | 无此目录；provider 来自 `model-adapter` 行 config → `.agents/config.toml` `[providers]` → 环境变量 | 仓库事实（§4 模型节） |
| 2 | §6.1：Hooks 内容来源 "plugin 排布" | `.agents` 布局无 hooks 目录；hook 是插件行在 harness `Host` 上的注册 | 仓库事实（§4 Hooks 节） |
| 3 | §6.1：设置分区含"命令" | 无独立命令数据源 / 方法域；命令是技能的 slash-command 扩展位 | 并入技能与命令分区，现状标注 |
| 4 | §6.1：自动化含"模板" | 无任何模板数据源（设计期概念） | 分区模板区记"现状无数据源，空态" |

- [ ] **Step 4: 断言 15 个小节齐全**

Run: `grep -cE '^### 4\.[0-9]+ ' docs/host-capabilities.md`
Expected: `15`。

Run: `grep -c '待填' docs/host-capabilities.md`
Expected: `1`（只剩 §5 的一次性核对记录）。

---

### Task 5: 一次性核对（临时脚本 + 人工签核 + 记录 + 弃置）

**Files:**
- Modify: `docs/host-capabilities.md`（§5 核对记录）
- 临时（用完即弃，**不入库**）: `/tmp/uva-knife4-audit.sh`、`/tmp/uva-knife4/`

**Interfaces:**
- Consumes: Task 3 / Task 4 的文档内容；`--dump-config`；`router.register` 注册表
- Produces: §5 的核对记录（日期 / 命令 / 输出摘要 / 签核）

**为什么脚本不入 `scripts/`**：AGENTS.md §3.6 与 `shellcheck.yml:45-60` 规定新增
`scripts/*.sh` **门禁**必须在同一变更里同步 `make lint-sh` 清单、workflow 清单与 §3.6 表。
刀 4 明确不新增门禁（D15："本轮没有新增门禁脚本"），为一次性对账引入三处同步是维护面净增。
故脚本放 `/tmp/`，跑完即弃。

- [ ] **Step 1: 写核对脚本到 `/tmp/uva-knife4-audit.sh`**

```bash
#!/usr/bin/env bash
# 刀 4 一次性核对 —— 用完即弃，不入库（本轮无新门禁，AGENTS.md §3.6 三处同步不触发）。
# 三面对账：dump 行 ↔ 矩阵 §2 表；目录约定 ↔ 矩阵 §3 表；矩阵引用的方法名 ↔ 注册表。
set -euo pipefail

REPO=$(git rev-parse --show-toplevel)
DOC=$REPO/docs/host-capabilities.md
WORK=/tmp/uva-knife4
fail=0

# --- 1. 合成部署（若 Task 3 的产物还在则复用，否则重建） ---
if [ ! -f "$WORK/full/profile.toml" ]; then
  echo "!! 合成部署不存在，先跑计划 Task 3 Step 1"; exit 2
fi
export HOME=$WORK/home
unset UVA_HOME_PROFILE

cd "$REPO"
cargo run -q -p uva-app-server -- --dump-config --directory "$WORK/full"  > "$WORK/full.json"
cargo run -q -p uva-app-server -- --dump-config --directory "$WORK/mini"  > "$WORK/mini.json"

# --- 2. 行 ↔ 文档 双向对账 ---
grep -o '"id": "[^"]*"' "$WORK/full.json" | sed 's/"id": "//;s/"$//' | sort -u > "$WORK/rows.txt"
while IFS= read -r id; do
  grep -qF "| \`$id\` |" "$DOC" \
    || { echo "MISS(doc): dump 行 \`$id\` 不在矩阵 §2 表"; fail=1; }
done < "$WORK/rows.txt"
grep -oE '^\| \`[a-z][a-z0-9.-]*\` \|' "$DOC" | sed 's/^| `//;s/` |$//' | sort -u > "$WORK/doc-rows.txt"
while IFS= read -r id; do
  grep -qxF "$id" "$WORK/rows.txt" \
    || { echo "MISS(dump): 矩阵 §2 行 \`$id\` 不在 dump 里"; fail=1; }
done < "$WORK/doc-rows.txt"

# --- 3. 目录约定 ↔ 文档（合成树里逐一存在 + 文档逐一提到） ---
for f in .agents/skills/demo/SKILL.md .agents/agents/reviewer/agent.md \
         .agents/tasks/daily/task.md .agents/mcp.json .agents/plugins/acme/plugin.json; do
  [ -f "$WORK/full/$f" ] || { echo "MISS(tree): 合成树缺 $f"; fail=1; }
done
for p in '.agents/skills/<name>/SKILL.md' '.agents/agents/<name>/agent.md' \
         '.agents/tasks/<name>/task.md' '.agents/mcp.json' \
         '.agents/plugins/<name>/plugin.json'; do
  grep -qF "$p" "$DOC" || { echo "MISS(doc): 目录约定 $p 不在矩阵 §3 表"; fail=1; }
done

# --- 4. 方法名存在性（通知名不在注册表，显式豁免） ---
grep -oE '\`[a-z]+(/[a-z-]+)+\`' "$DOC" | tr -d '\`' | sort -u \
  | grep -vxF -e session/event -e anthropic/messages/event -e ready \
  > "$WORK/methods.txt" || true
while IFS= read -r m; do
  grep -rq "router.register(\"$m\"" "$REPO/crates/uva-app-server/src/rpc/methods/" \
    || { echo "MISS(registry): 矩阵引用的方法 \`$m\` 不在服务器注册表"; fail=1; }
done < "$WORK/methods.txt"

# --- 5. 打印「行 → 域 → 界面」对照（人工签核的输入） ---
echo "=== 行 → 域 → 界面（dump 全量 $(wc -l < "$WORK/rows.txt") 行）==="
while IFS= read -r id; do
  grep -F "| \`$id\` |" "$DOC" | head -1
done < "$WORK/rows.txt"
echo "=== 精简部署（5 行）：缺行 = dump 缺行（D32 在 profile 层的证据）==="
grep -o '"id": "[^"]*"' "$WORK/mini.json" | sed 's/"id": "//;s/"$//'

if [ "$fail" -eq 0 ]; then
  echo "OK: 行 / 目录 / 方法三面全部对上（$(wc -l < "$WORK/rows.txt") 行、$(wc -l < "$WORK/methods.txt") 个方法名）"
else
  echo "FAIL: 见上方 MISS 行"
fi
exit "$fail"
```

（§2 表**一个 id 一行**是硬约束：Step 2 的 `doc-rows` 逐 id 提取，合并行会让双向对账漏配。
`ui.slot.chat.sidebar` 是 `ui.*` 的**示例**行 id——矩阵以它代表"任意 `ui.` 前缀行"，前缀规则
本身在 §4 UI 扩展节写明。）

- [ ] **Step 2: 跑核对**

Run: `bash /tmp/uva-knife4-audit.sh`
Expected: 末行 `OK: 行 / 目录 / 方法三面全部对上（24 行、<N> 个方法名）`，退出码 0。
任何 `MISS(...)` 行都表示矩阵与事实脱节——**先修矩阵再重跑**，不许改脚本迁就矩阵。
（`<N>` 以实际输出为准，抄进 §5。）

- [ ] **Step 3: 人工签核并把记录写进 §5**

对照脚本第 5 段打印的"行 → 域 → 界面"逐行目检（落点与 §6.1 的分区一致、域归属与
`provides` / `inject` 相容，例如 `skills` 行应 `inject` `tools`）。然后把骨架 §5 的
`<!-- 待填 -->` 占位**替换**为下面内容（标题已在骨架里，不重复抄）：

```markdown
## 5. 一次性核对记录

- **日期**：2026-11-XX（执行日）
- **方法**：临时脚本 `/tmp/uva-knife4-audit.sh`（已弃置，未入库）——合成部署（隔离
  `HOME`）→ 两份 `--dump-config` → 行 / 目录约定 / 方法名三面双向对账 → 打印对照表目检。
- **结果**：`OK: 行 / 目录 / 方法三面全部对上（24 行、<N> 个方法名）`；精简部署确认
  "缺行 = dump 缺行"。
- **签核**：<执行者>（逐行目检 §2 对照表与 §6.1 落点一致）
- **再核对**：本文件是反推文档——profile 布局或目录约定变更后应重跑一次核对（重建合成
  部署的配方在 `docs/design/2026-11-09-capability-matrix-plan.md` Task 3/5）。
```

- [ ] **Step 4: 弃置临时产物**

Run: `rm -rf /tmp/uva-knife4 /tmp/uva-knife4-audit.sh && ls /tmp/uva-knife4* 2>&1 || true`
Expected: `ls: cannot access '/tmp/uva-knife4*': No such file or directory`（脚本与合成
部署已删；仓库里本来就没有它们的痕迹）。

- [ ] **Step 5: 确认矩阵没有未填占位**

Run: `grep -c '待填' docs/host-capabilities.md`
Expected: `0`。

---

### Task 6: 收口（CHANGELOG + 范围检查 + 提交）

**Files:**
- Modify: `CHANGELOG.md`（`[Unreleased]`）

**Interfaces:**
- Consumes: Task 1–5 的全部产物
- Produces: 一个只含文档的提交

- [ ] **Step 1: 加 CHANGELOG 条目**

在 `## [Unreleased]`（`CHANGELOG.md:13`）之后、现有第一个 `###` 小节之前插入：

```markdown
### Added (the capability matrix: every domain traced to a profile row)

- **`docs/host-capabilities.md`**：15 个能力域 → 界面落点 / 数据来源 / 拥有行 / 缺行行为
  的矩阵，行从解析后的 profile（`--dump-config`）与 `~/.agents` 目录约定反推，附一次性
  核对记录与设计文档的漂移记录；不新增门禁（刀 4 只出文档）。
```

- [ ] **Step 2: 确认本刀造成的改动只有两个文件**

Run: `git status --porcelain | grep -E 'CHANGELOG\.md|^..\s?docs/'`
Expected: ` M CHANGELOG.md` 与 `?? docs/host-capabilities.md` 必在；至多多出本计划文件自身
一行（`?? docs/design/2026-11-09-capability-matrix-plan.md`，由主线入库，**不属**本刀执行者的
提交范围）。工作树里的本机杂物（`.playwright-mcp/` 等）不匹配本模式、不纳入断言；若 `docs/`
下多出**其它**新文件，说明越界了。

- [ ] **Step 3: 确认零代码 / 零门禁改动**

Run: `git diff --exit-code Makefile AGENTS.md scripts/ .github/ crates/ uva-web/ Cargo.toml && echo CLEAN`
Expected: `CLEAN`（门禁清单、workflows、源码全部未动——lint-sh 三处同步自然不触发）。

- [ ] **Step 4: 跑受影响的门禁**

Run: `make check-claim-language`
Expected: 通过（exit 0，无 `FAIL` 行）——`[Unreleased]` 在该门禁的扫描范围（AGENTS.md §3.6）。

- [ ] **Step 5: 提交**

```bash
git add docs/host-capabilities.md CHANGELOG.md
git commit -m "docs: 能力矩阵——15 域从解析后的 profile 反推，附一次性核对记录"
```

（不 push。）

---

## 附：本计划已核对的关键事实（执行时若漂移，以仓库为准）

| 事实 | 位置 |
| --- | --- |
| 内置行全集 = 22（`native_profile()`） | `crates/uva-app-server/src/composition/kernel/mod.rs:153-177` |
| `wasm` 行注册工厂但不在内置集（opt-in） | `composition/kernel/mod.rs:343-349` |
| native 兜底只在 boot，不在 dump | `src/main.rs:173-181` vs `src/state/app_state/boot.rs:242-243` |
| dump 形状（`hmr` / `core` / `ui`；每行 `id`/`runtime`/`capabilities`/`config`/`provides`/`inject`） | `src/profile/mod.rs:454-507`、`:510-591` |
| profile TOML 形状（`hmr` + `[core] entries` + `[ui] entries`） | `src/profile/model.rs:220-234` |
| home patch 层（`~/.agents/profile.toml` / `UVA_HOME_PROFILE`） | `src/profile/merge.rs:217-231` |
| `agent-loop` 缺 → boot 失败的报错原文 | `src/state/app_state/boot.rs:445-450` |
| `model-adapter` 卸载 → 建会话报错原文 | `src/state/sessions.rs:351-357` |
| 行缺 = 不挂载（省略内置 id 即禁用；未知 id 失败；空解析挂内置集） | `crates/uva-app-server/README.md:181-183` |
| `.agents` 布局三常量（`plugin.json` / `mcp.json` / `.agents`）与四源优先级 | `crates/uva-core/src/layout.rs:49-56`、`:18-30` |
| `Domain` = Skills / Agents / Tasks（目录名）；legacy 仅 skills 读 | `crates/uva-core/src/layout.rs:84-114` |
| `mcp.json` 命名空间（`<plugin>/<server>`） | `crates/uva-core/src/layout.rs:242-275` |
| MCP 仅 stdio | `crates/uva-app-server/README.md:134-135` |
| skills 行 inject `tools`、注册 `skill` 工具、两贡献皆可逆 | `crates/uva-skill/src/plugin.rs:1-46` |
| 命令 = 技能的 slash-command 扩展位（现状无独立面） | `crates/uva-skill/src/skill.rs:123-126` |
| boot seed 默认技能 | `src/state/app_state/boot.rs:171-177` |
| agent 定义（`agent.md` + `config.json`、四源） | `crates/uva-agents/src/discovery.rs:1-11` |
| 内置保护名 `agent` 不可删 | `src/routes/agents.rs:44-48` |
| `models/list` 来自 provider catalogue；provider 回退链 | `src/rpc/methods/misc.rs:34-36`、`src/main.rs:63-77`、`src/state/app_state/boot.rs:763-794` |
| `plugins/list` = composition + `bundles`（origin project/global） | `src/routes/plugins/mod.rs:60-131` |
| 计划域双源（`task.md` 声明 + ScheduleStore 运行时） | `src/state/app_state/tasks.rs:5-9` |
| 任务域 = 会话日志 delegation 扫描 | `src/routes/tasks.rs:1-34` |
| 工作流 = AppState 内存 registry | `src/routes/workflows.rs:1-17` |
| 评测 = AppState 内存 registry | `src/routes/evals.rs:1-9` |
| 搜索 = 六域 fan-out（tool/mcp/skill/memory/agent/session） | `src/routes/search.rs:1-47`、`src/rpc/methods/misc.rs:47-51` |
| Hooks = `Host::hook` 注册（六 pass 链 + 五观察点、可撤销） | `crates/uva-harness/src/agent/host/mod.rs:16-21`、`:644-674` |
| `[ui]` 行约束（`ui.` 前缀、`ui-bundle`、components 形状、拒可执行载荷） | `crates/uva-app-server/src/ui/schema.rs:88-123` |
| 会话目录 `<workspace>/sessions` | `src/state/app_state/boot.rs:137-139` |
| 方法注册表规模 66（agents 14 / automation 19 / chat 7 / misc 12 / sessions 14） | `grep -c 'router.register("' crates/uva-app-server/src/rpc/methods/*.rs` |
| lint-sh 三处同步规则；本轮 11 个门禁脚本清单不变 | `AGENTS.md` §3.6、`.github/workflows/shellcheck.yml:45-60` |
| CHANGELOG `[Unreleased]` 在 claim-language 扫描范围；docs/ 不在 markdownlint 范围 | `AGENTS.md` §3.6（check-claim-language 行）、`.markdownlint.json` |
