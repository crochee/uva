# 刀 C · 前端线上契约 —— Implementation Plan

> 决策见 [`docs/adr/0003-frontend-domain-clients.md`](../adr/0003-frontend-domain-clients.md) 与
> [`docs/adr/0001-frontend-test-policy.md`](../adr/0001-frontend-test-policy.md)。

## Global Constraints

- **Node 版本**：CI 的 web job 固定 Node 20（`.github/workflows/rust-quality.yml`:174），`npm ci`（`:185`）。`vitest@5` 的 engines 是 `^22.12.0 || ^24 || >=26`——**在 CI 上装不上**，必须钉 `vitest@4`。`jsdom@31` 同理装不上（`^22.22.2 || ^24.15 || >=26`）。
- **不装 DOM 环境**：本切面只测纯函数，vitest 默认环境就是 `node`，`jsdom` 不需要。
- **测试文件不进 `src/`**：`tsconfig.json` 的 `include` 是 `["src"]` 且没有 `@types/node`，在 `src/` 下放 `*.test.ts` 会同时炸 `tsc --noEmit` 与 CI 的 Node 版本（Node 20 没有 TS 类型剥离）——这个坑已经记在 `docs/design/2026-11-09-desktop-frontend-tree-design.md`:146-149。
- **§4.2 是明文规则**：AGENTS.md §4.2「不保留任何前端自动化测试套件」。本切面**改写**它，并同步所有写着同一句话的地方。
- **依赖纪律**：新增依赖进 `uva-web/package.json` 的 `devDependencies`，锁文件必须一起更新（`npm ci` 在两者不同步时会拒绝运行）。
- **新增 shell 门禁的同步面**：AGENTS.md §3.6 —— 新增 `scripts/*.sh` 必须在同一变更里同步三处（`Makefile` 的 `lint-sh` 清单、`.github/workflows/shellcheck.yml` 的清单、AGENTS.md §3.6 的 `lint-sh` 行），外加 `Makefile` 目标与 `make ci`。

## Review Focus

1. 迁移是不是**按域**的原子改动——一个域一个客户端 + 它的调用点，而不是一个大 diff。
2. 生成的类型是不是**真从 Rust 来**——手写的镜像一旦又出现，门禁应当立刻红。
3. 纯函数测试是不是真的**纯**——不碰 DOM、不碰网络、不碰 React。

---

### Task 1: 前端纯函数测试的骨架

- `uva-web/package.json` 加 `"test": "vitest run"` 与 `vitest@^4` 一个 devDependency（**不加 jsdom**），锁文件同步。
- `uva-web/vitest.config.ts`：`environment: 'node'`，`include: ['tests/**/*.test.ts']`。
- `uva-web/tests/` 目录（**只放单元测试**）——`tsconfig.json` 的 `include` 保持 `["src"]`，所以 `tsc --noEmit` 扫不到它们。
- `Makefile` 的 `check-web` 目标加上 `cd $(WEB_DIR) && npx vitest run`。

**验证**：`cd uva-web && npm ci && npm test` 绿；`make check-web` 绿。

---

### Task 2: 覆盖第一批纯函数

`src/` 里共 34 个纯导出函数（11 个文件）。第一批取规则最密的两块：

- `src/lib/session-to-messages.ts` 的 9 个导出——日志行 → 消息的投影，要处理多种行方言与工具回声。
- `src/settings/receipt.ts` 的 2 个——四态回执，四种状态不许塌缩成「保存成功」。

**验证**：每个导出至少一条断言；变异测试式地确认断言真的会红（例如把四态里的一态改错，测试必须失败）。

---

### Task 3: `useCursorList` 抽出纯核

`src/hooks/useCursorList.ts`:16 import 了 `react`——它是 hook，node 环境里没有渲染器，测不了。把它可测的两段纯逻辑抽出来：

- 「把一次响应并进 `items`」的规则。
- 「请求在飞 / 没有 `next_cursor` 时 `loadMore` 是 no-op」的守卫。

抽成 `src/lib/cursor-list.ts` 的两个纯函数，hook 调用它们。

**验证**：纯函数进覆盖清单；`useCursorList` 本身不进。

---

### Task 4: 四个 frame validator 独立成模块

`src/api/chat-stream.ts` 的 `readRowFrame`（`:497`）、`readInterrupts`（`:521`）、`readBaseline`（`:559`）、`readLiveTurn`（`:602`）全是模块私有的，**现在 import 不到**。它们是约 150 行防御性解析，解析失败返回 `null`、调用方静默丢帧。

挪进 `src/api/follow-frames.ts` 并导出，接口是 `readBaseline(data: unknown): FollowSnapshot | null` 这一类。`chat-stream.ts` 随之变薄。

**验证**：四个 validator 各有断言（合法帧、缺字段帧、错类型帧）；`chat-stream.ts` 的公开接口不变。

---

### Task 5: 按域的类型化客户端，取代路径表

`src/api/client.ts` 的 45 行 `ROUTES` 表 + `resolveRoute` 全部删除。`resolveRoute` 从不抛错而是回退到**更短**的模板，于是 `GET /api/v1/sessions/abc` 答的是 `sessions/list`、按名字取的 GET 返回列表、`POST /api/v1/evals/x/run` 调的是 `evals/create`。14 行表项没有任何调用点会用到。

按域推进，每个域一个类型化客户端，形状照 `src/api/settings-profile.ts`（`METHOD` 常量表 `:71-78`、直接用 `rpc.request` `:25`、零错误包装）：

|域|调用点|方法数|
|---|---|---|
|schedules|`api/schedules.ts`|6|
|evals|`api/evals.ts`|5|
|sessions|`api/chat-stream.ts`、`Sidebar.tsx`、`CommandPalette.tsx`、`useWorkspaceTree.ts`、`ChatPage.tsx`|7|
|agents|`AgentsSection.tsx`、`AutomationSection.tsx`、`ChatPage.tsx`、`CommandPalette.tsx`|4|
|chat|`api/chat-stream.ts`、`ChatPage.tsx`|5|
|search|`WorkspaceSearch.tsx`、`CommandPalette.tsx`|2|
|skills|`SkillsSection.tsx`|2|
|tasks|`api/tasks.ts`|2|
|models / ui|`api/chat-stream.ts`、`api/ui-schema.ts`|2|

`client.ts` 的删除是**最后一步**——那时它是唯一的 REST 词汇。

**验证**：每域一个可评审的原子改动；`client.ts` 删完后 `make check-web` 与 `npm run build` 绿；`grep -rn "/api/v1" uva-web/src` 只剩注释与类型里的说明文字。

---

### Task 6: 生成的类型

Rust 侧：

- 给 24 个响应类型 derive `JsonSchema`（`List<T>` 已有 derive，`api/v1/list.rs`:26）。
- **给 `profile/*` 五个方法补 Rust 响应类型**——它们现在返回裸 `serde_json::Value`（`state/app_state/profile_apply.rs`:75、:109`，`rpc/methods/profile.rs`:39-75`），没有类型可 derive。补上 `ProfileRow` / `ProfileChange` / `ProfileEnableCheck` 之类。
- 一个**手写清单** + `schema_for!` 逐个导出，产出一份 JSON Schema。清单就是「类型名 → RPC 方法 → TS 名字」的那份目录，一处显式列出。

Node 侧：`json-schema-to-typescript` 生成 `uva-web/src/api/types.ts`。

**验证**：生成产物提交入库；`scripts/check-web-contract.sh`（Rust 导出 → Node 生成到临时目录 → diff 仓内文件）不一致时退出 1；`make check-web-contract` 与 `make check-web` 都进 `make ci`。

---

### Task 7: 改写 §4.2 与所有写着同一句话的地方

AGENTS.md §4.2 改成「不保留前端 e2e 套件；纯函数单元测试用 vitest，随 `check-web` 跑」。同步清单（可行性核查逐处找出来的）：

- `AGENTS.md` §4.2（`:189-216`）
- `DEPLOYMENT.md`:82-98
- `CONTRIBUTING.md`:37-38
- `Makefile`:63-64（`test` 目标的帮助文本）
- `.github/workflows/rust-quality.yml`:188-191（注释）
- `docs/design/2026-11-09-desktop-frontend-tree-design.md`:95、:123-124、:146-149、:436-440、:984
- `docs/design/2026-11-09-spa-ia-plan.md`:15
- `docs/design/2026-11-07-harness-v5-closure-design.md`:19、:97、:99
- `docs/design/2026-10-25-harness-v3-plan.md`:86、:943、:1009
- `docs/design/2026-10-25-writer-unification-plan.md`:816、:839

`CHANGELOG.md` 的 R72 记录是**历史**，不动。

**验证**：`grep -rn "本项目不保留任何前端自动化测试套件\|no automated test suite\|前端有 no test runner" --include='*.md' --include='Makefile' --include='*.yml' .` → 除 §4.2 改写后的措辞外无残留。

---

### Task 8: 验收

1. `make check-web` / `make check-web-contract` / `npm run build` 绿。
2. `make lint-sh` 绿（新增的 `scripts/check-web-contract.sh` 在清单里）。
3. `make check-frozen-api` 绿——Rust 侧**不新增** crate 根公开名字。
4. 前端冒烟：`npm run dev` + `cargo run -p uva-app-server -- serve`，MCP 驱动真实页面走一遍聊天与设置（§4.2 的 MCP 流程仍然有效，它没有被单元测试取代）。
