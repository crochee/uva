# 无头 harness 的业界对齐：逆向 dsh 与 omp 的缺口账本

日期：2026-11-26
状态：进行中（本文件是账本；每刀的落地与验收写在自己的小节里）
取代：无
被取代：无

---

## 1. 意图与范围

上一轮（[`2026-11-25-acp-headless-harness-design.md`](2026-11-25-acp-headless-harness-design.md)）
逆向了 **dsh / pi / codex**，把本仓的 ACP 面做到与三者同一台机器上的同一种行为。

本轮把参考集换成 **dsh（DeepSeek Harness）** 与 **omp（oh-my-pi）**，问的是同一个问题：
**本仓的无头 agent harness（核心 JSON-RPC 面 + ACP 面 + 网关面）离业界先进的形态还差什么。**

范围限定在**无头面**：核心事件流、会话持久化、回合机器、审批、取消、委派控制、
控制面协议（帧、游标、订阅、能力协商）。前端（TUI / 浏览器 / 桌面壳）只在
「同一套核心事件流」的意义上出现，不在本轮的对账范围里。

不在范围：模型质量、提示词工程、成本优化、供应商覆盖面。

---

## 2. 逆向方法

两个参考仓都拉到本地，逐行为定位——问「这个 harness 在哪里做 X」，
答案是一个 `path:line`，再读那一段实现：

```bash
git clone --depth 1 https://github.com/deepseek-ai/deepseek-harness /tmp/dsh
git clone --depth 1 https://github.com/can1357/oh-my-pi /tmp/omp
```

dsh 是 Cordis 插件 monorepo（TypeScript），无头控制面在
`packages/bundle/headless`（`src/startup.ts` + `src/index.ts` +
`src/json-stream.ts`）；omp 是 Rust 核心 + TypeScript 产品层（~80k 行 Rust），
无头控制面在 `packages/coding-agent/src/modes/rpc`（13.6k 行）与
`packages/agent/src/agent-loop.ts`。

证据规则：**每条结论都带 `path:line`**；实现是事实，文档只是意图。

---

## 3. 行为对账表

| 维度 | dsh | omp | 本仓 | 判定 |
|---|---|---|---|---|
| 回合机器 | turn → step → 模型 → 工具批（`packages/core/agent-loop/src/agent.ts:203`） | `agent_start` → turn → message → 工具批 → `turn_end`（`packages/agent/src/agent-loop.ts:1153`） | 同形（`crates/uva-harness/src/agent/re_act/loop/mod.rs:300`） | 对齐 |
| 工具批并发 | 默认并行，**只有显式 `true` 才并行**，其余独占（`packages/core/tools/src/pipeline.ts:100-113`） | `shared` / `exclusive`，独占串行（`packages/agent/src/agent-loop.ts:4036-4076`） | 按 `Tool::mode()` 分桶（`crates/uva-harness/src/agent/re_act/loop/step/tools/mod.rs:162`） | 对齐（声明式 vs 声明式） |
| 结果提交序 | 按模型顺序提交，不按完成顺序（`packages/core/tools/src/batch.ts:135-146`） | 同（`agent-loop.ts:4204-4265` 合成结果） | 按 LLM 调用顺序（`…/step/tools/mod.rs:126`） | 对齐 |
| 取消时未跑的调用 | 合成错误结果，日志仍可回放（`packages/core/tools/src/batch.ts:250-260`） | 合成结果，`aborted`/`skipped`/`length` 各有文案（`agent-loop.ts:4204-4265`） | 未开始的调用**不跑**（取消检查在工具流的 drain 里，`…/step/tools/route.rs:168`），已跑完的结果照旧 | 对齐 |
| 日志即真源 | `session.deriveMessages()` 从日志派生（`agent.ts:203`） | `session-entries.ts` 逐条追加 | 审批 / 压缩 / resume 全是日志折叠 | 对齐（本仓更强：单写者） |
| 单写者 | 进程内 + `flock` 跨进程（`packages/session/session-persistence-jsonl/src/lease.ts:1-24`） | 进程租约 `~/.omp/run/session-owners/<id>`，发现别人在写就**换新文件**（`docs/session.md:648-651`） | 进程内单写者 + **`owner.lock` 跨进程所有权**（切 9）：写路径首次追加时 flock 认领、拒绝**失败关闭**（不换文件——本仓的会话 id 就是身份） | 对齐 |
| 崩溃修复 | 断尾截断 + 开回合补closer（`packages/core/session/src/repair.ts:59-105`） | 追加不 fsync，落盘即返回（`packages/coding-agent/src/session/session-storage.ts:66-76`） | `crates/uva-session/src/repair/mod.rs` 合成 `tool_result` / `step_end` / `turn_end`；**断尾截断已落（切 27）**：写者在第一次写之前切掉日志末尾那半行，读者也容忍它 | 对齐 |
| 审批闸门 | `tools/pre-execute` 水瀑，先于任何工具体（`packages/core/tools/src/pipeline.ts:196-215`） | 扩展包装器再查一次**真正要执行的参数**（`packages/coding-agent/src/extensibility/extensions/wrapper.ts:374-389`） | 五级否决流水线（`…/loop/guard.rs:47-85`） | 对齐 |
| 审批**超时** | 工具级 `timeoutMs` → `TOOL_TIMEOUT` 结构化错误（`packages/guard/timeout-policy/src/index.ts:26`） | **没有超时**：要么有人回答，要么回合被 abort（`wrapper.ts:495-500`） | **已落**（切 1）：策略带 `timeout_secs`，控制器到期走同一条 resume 路径，模型看到「没人回答」的拒绝 | 对齐 |
| 审批可分辨的拒绝 | `allowed-once / rejected / cancelled / unavailable` 四态（`packages/interaction/user-approval/src/types.ts:31-34`） | 拒绝文案分工具策略 / 用户策略（`packages/coding-agent/src/tools/approval.ts:591-602`） | `resolved / cancelled / rejected`（`crates/uva-approval/src/approval.rs:305-309`） | 对齐（无「超时」态，随 G1 一起补） |
| 审批策略来源 | `permission-presets` 写 `setApprovalPolicy`（`packages/interaction/permission-presets/src/index.ts:180-250`） | `tools.approvalMode` + 每工具策略 + 每目标授权（`docs/approval-mode.md`） | **已落**（切 1）：`[approval]` 配置段的策略经 `BootExtras.approval` 发布（`crates/uva-app-server/src/config/server/mod.rs:185-219`） | 对齐 |
| 压缩 | 摘要指令作为**最后一条 user 消息**追加 ⇒ 复用 KV cache（`packages/compaction/compaction-basic/src/summarizer.ts:74-79`） | 缓存友好的裁剪：只裁「省得够多且结果不太小」的（`packages/agent/src/compaction/pruning.ts`） | **同形**（见「切 6」）：`SummaryInput` 携带会话自己的 system prompt 与工具 schema，摘要请求 = `[system, …shadowed…, user(指令)]`——上一个被路由请求的真前缀；裁剪到 `[tool result pruned]` 保持不变 | 对齐 |
| 签名思维块 | 前缀摘要变了就**剥掉**签名思维块，否则整请求被拒（`packages/llm/llm-pi-ai/src/prefix-binding.ts:31-93`） | 同类机制（`prefixDroppedThinkingBlocks` 会话状态） | **已落**（切 4）：压缩点之前的消息失去签名（`…/re_act/transcript.rs:235`），压缩后的请求不再带作废签名 | 对齐 |
| 原生 replay state | `ReplayEnvelope` 按块对齐，装配丢块就丢信封（`packages/llm/llm/src/assembler.ts:130-152`） | `ProviderPayload` 不透明载荷（`packages/ai/src/types.ts:1494`） | **字段存在但没人写**：`replay_state` 全仓只有 `None`（`crates/uva-provider/src/types/completion/mod.rs:42,76`） | **缺口 G2**：声称已对账的能力是死的 |
| 原始 delta 留存 | 压缩的记录列表，可无损展开（`packages/llm/llm/src/assistant-stream.ts:99-181`） | transcript 存消息，不存 delta | 词汇有 `assistant_chunk`，**没有生产者**；且有测试钉住「每回合一行」（`crates/uva-harness/src/agent/re_act/tests/lifecycle/one_row_per_turn.rs:285`） | 有意不同（记录级 vs 行级），**不算缺口** |
| 插件树 | entry 模型 / group / 按 id diff / 三态审计（`packages/preset/agent-preset-registry/src/mount.ts:115-152`） | — | 同一套（`crates/uva-kernel/src/mount.rs:216-238`） | 对齐 |
| 插件树**审计报告** | `auditRows` → `{failed, pending}` 两张表（`mount.ts:115-152`） | — | **已落**（切 7）：`profile/audit` 返回不能用的行 + 原因（`failed` / `pending` + 缺的 key），dsh 语义 | 对齐 |
| 委派控制面 | `interrupt()` 鉴权到祖先链（`packages/subagent/subagent/src/manager.ts:196-233`） | RPC：`get_subagents` / `cancel_subagent` / `steer_subagent`，子代理记录按字节增量读（`packages/coding-agent/src/modes/rpc/rpc-subagents.ts:74-107`） | 机器有（`crates/uva-tool-task/src/control/mod.rs:389` 的 `kill`、`registry`），**但没有无头门** | **缺口 G8（本轮切）** |
| 控制面协议 | 自有 JSON-RPC stdio server（`packages/sdk/server/src/server.ts:609-667`） | 自有 JSONL：~90 条命令、协议协商、**分块帧**（`wire/commands.ts:35-454`、`rpc-frame.ts:6,9`） | JSON-RPC 2.0 + Content-Length 帧，64 MiB 逻辑上限，超限即错（`crates/uva-app-server/src/rpc/framing.rs:32`） | **对齐（切 10）**：逻辑上限同 omp v2；分块/协商对 Content-Length 不适用（见切 10）；出站背压落盘 |
| 增量读 | — | `get_entries?since` + 分页游标（`docs/rpc.md`） | `sessions/events` **每次整本**返回（`crates/uva-app-server/src/routes/sessions/mod.rs:827-855`） | **缺口 G11（本轮切）** |
| 客户端的「正在动哪个文件」 | — | 工具调用的 `locations`（`acp-event-mapper.ts:225-228,482-485`，从参数里抽路径） | **已落（切 30）**：`Tool::locations(&input, &context)`（默认无）——工具在**调用开始前**答自己会动哪个文件；ACP 面把它发成协议的 `locations` 成员（客户端的 follow-along） | 对齐（omp 从参数里抽路径，本仓让**工具自己答**——工具才知道自己怎么解析参数，发布的位置不可能漂移） |
| 客户端侧工具 | — | `set_host_tools` + `host_tool_call`/`host_tool_result` 帧（`packages/coding-agent/src/modes/rpc/host-tools.ts:76`、`rpc-types.ts:907-953`） | **fs 委派已落地**（客户端声明 `clientCapabilities.fs` 后，会话的 `read`/`write`/`edit` 经 ACP `fs/*` 回到客户端，见「切 5」与「切 12」）；**terminal 委派已落地**（客户端声明 `clientCapabilities.terminal` 后，会话的 `shell` 命令经 ACP `terminal/*` 跑在客户端的窗格里，见「切 11」）；**客户端自有的工具已落地**（第三轮切 23：核心面 `tools/register owner=client` + `tools/execute` 问答回到客户端）；ACP 面没有 `set_host_tools` 原语，那一半如实缺席；每会话 MCP 仍如实缺席 | **对齐（切 5 + 11 + 12 + 23）**：核心面有了原语，ACP 面如实缺席 |
| 会话的名字 | 耐久标题状态（最新一次胜出，模型或 provider 生成，`packages/session/session-title`）+ 异步 provider（`session-title-llm`） | 用小模型生成标题（`packages/coding-agent/src/utils/title-generator.ts`） | **已落（切 29）**：标题**从日志派生**（第一条 user 消息的第一行，截到 80 字符），`sessions/list` 与 `sessions/get` 都带着它 | **有意不同**：不调模型、不另存一份可能漂移的真相（本轮范围明确排除提示词工程；dsh/omp 的标题是模型生成的摘要） |
| 帧内事件投影 | `--json` 只投影**已提交**事件，单串 8 KiB / 单事件 32 KiB 上限（`packages/bundle/headless/src/json-stream.ts:20-27`） | v1 物理帧 1 MiB，v2 分块到 64 MiB（`rpc-frame.ts:6,9`） | live 事件 + durable 行两路（`crates/uva-app-server/src/routes/chat/stream.rs:1-40`） | 对齐（上限策略不同，见切 10） |
| 后台作业 | `job_output` / `job_list` / `job_kill`（`packages/jobs/tool-jobs/src/index.ts:1-60`）：完成通知送进主人的下一步 | 后台作业 + `wait` 工具（`packages/coding-agent/src/tools/wait.ts`） | **已落**（切 13 + 14）：`job` 工具的 `start` / `output`（可等）/ `list` / `kill`，作业住在一个部署级的注册表里；**完成通知主动送达**（作业结束 → 拥有它的会话收到通知，切 14） | 对齐 |
| 工具族 | `tool-fs` / `tool-fs-search`（`glob` + `grep`，走打包的 ripgrep）/ `tool-str-replace-editor` / `tool-bash` / `tool-subagent(-control)` / `tool-session-{control,list,query}` / `tool-schedule` / `tool-skill` / `tool-ask-user` / `tool-jobs` / `tool-goal` / `tool-terminal` / `tool-worktree` / `tool-lsp` / **`tool-web`（`web_search` + `web_fetch`）**（`packages/*/tool-*`） | 31 个内建：`read` / `bash` / `edit` / `ast_grep` / `ast_edit` / `ask` / `glob` / `grep` / `find` / `lsp` / `checkpoint` / `rewind` / `task` / `wait` / `todo` / `web_search` / `write` / `memory_edit` / `manage_skill` …（`packages/coding-agent/src/tools/builtin-names.ts:3-33`） | read / write / **edit**（切 12）/ shell / **job**（切 13）/ **glob** + **grep**（切 15）/ **lsp**（切 17）/ todo / web / search（语义）/ memo / task / 技能 | **大半对齐**：两个参考都有的文件工具齐了（切 12 的 `edit`、切 15 的 `glob`/`grep`），**目标模式齐了**（切 16 的 `goal` 工具 + 宿主的续跑），**语言服务器齐了**（切 17 的诊断面 + 切 18 的查询面）；**提问齐了**（切 24 的 `ask`：结构化问题、客户端作答、有界等待）；**检索齐了**（切 26 的 `web_search`：JSON 搜索契约 + 有界答案）；**如实缺席**：`checkpoint`/`rewind`（撤销——两个参考各自的「会话回卷」与「工作区 worktree」是两件不同的事）、`ast_grep`/`ast_edit`（结构化编辑）、`lsp` 的**编辑面**（omp 的 `applyWorkspaceEdit`——本仓的 `edit` 工具是它的有界替身） |
| 文件编辑 | `str_replace_editor`（`view` / `create` / `str_replace` / `insert`）：旧文本必须**逐字**命中；多处命中被拒并点名行号（`packages/fs/tool-str-replace-editor/src/index.ts:295-320`） | `edit`（`old_string` / `new_string` / `replace_all`）：同一逐字规则，外加两条让失败可解释的拒绝（相同替换、空白替换_all）与「为什么没命中」的诊断（`packages/coding-agent/src/edit/magic-tools.ts:191-210`） | **已落**（切 12）：`edit` 工具逐字命中、多处命中点名行号、`replace_all` 显式开启；失败分两种可能（已应用 / 文本不符）各给一条下一步 | 对齐 |
| 文件改动的呈现 | 工作区改动摘要（git 工作树快照 + 文件工具的整文件捕获，`packages/deliverables/workspace-changes`） | 编辑器的 diff 面板（工具结果带 `oldText`/`newText`，`packages/coding-agent/src/modes/acp/acp-event-mapper.ts:708-745`） | **已落（切 25 + 切 28）**：文件工具把改动报进结果 metadata（`path` / `old_text` / `new_text`）；ACP 面发 `ToolCallContent::Diff`，核心面 live 帧带 `diff`；**durable 行也带 `changed`**（事后读日志仍知道这次调用改了哪些文件） | 对齐（durable 行带**路径**，不带 diff 本身——diff 太大，日志只记事实） |
| 退出契约 | 终态 `completed` → 0，其余 → 1（`packages/bundle/headless/src/index.ts:430-440`） | `prompt_result.status`（`docs/rpc.md`） | `chat/turn/event` 的 `end.reason`（`routes/chat/stream.rs:270-280`） | 对齐 |

---

## 4. 缺口清单（按严重度）

| # | 缺口 | 严重度 | 证据 |
|---|---|---|---|
| G1 | ~~被持有的审批永不超时~~ **已落（切 1）**：策略带 `timeout_secs`，控制器在 `AwaitingApproval` 时起截止时间，到期走**同一条** resume 路径（派生 token → 构造拒绝决定 → 提交 → 起跑）；日志状态与模型文案区分「没人回答」与「有人说不」 | — | `crates/uva-approval/src/approval.rs:90`；`crates/uva-app-server/src/session/controller/dispatch.rs` |
| G1b | ~~`approval` 行的 config 是摆设~~ **已落（切 1）**：`[approval]` 配置段（`mode` / `ask_unknown` / `timeout_secs` / 逐工具 `overrides`）经 `BootExtras.approval` 发布——部署的策略第一次真的生效 | — | `crates/uva-app-server/src/config/server/mod.rs:185-219` |
| G2 | ~~`replay_state` 是死字段~~ **已结**（切 8）：字段背后的机制——推理连续性——已由「签名 + 压缩边界规则」结构化交付；两个在仓适配器都没有可铸造的原生状态（OpenAI 走 Chat Completions，Anthropic Messages 无可回放的 response id）。字段保留为 seam，文档如实 | — | 同左 |
| G5 | ~~压缩后原样回送 Anthropic 签名思维块~~ **已落（切 4）**：作废判定放在知道前缀被改写过的那一层（harness）——压缩点之前的消息失去签名（`Transcript::void_signatures_before`），provider 自己的规则丢掉无签名的思维块 | — | `crates/uva-harness/src/agent/re_act/transcript.rs:235` |
| G8 | ~~委派没有无头控制面~~ **已落（切 2）**：核心面有 `subagents/list` / `subagents/cancel` / `subagents/steer`（`rpc/methods/names.rs:71-75`），宿主能列举、硬杀、转向 | — | 同左 |
| G11 | ~~durable 读没有游标~~ **已落（切 3）**：`sessions/events` 接受 `since` 序号，只回之后的行并回 `last_seq` | — | 同左 |
| G4 | ~~压缩摘要是旁路请求~~ **已落**（切 6）：`SummaryInput` 信封让摘要复用被路由请求的暖前缀 | — | `crates/uva-session/src/compaction/mod.rs`（`SummaryInput`） |
| G7 | ~~并行工具桶不观察取消~~ **已驳回**：取消检查在工具流的 drain 里（`crates/uva-harness/src/agent/re_act/loop/step/tools/route.rs:168`），两个桶都经过它——未开始的调用不跑。补了一个回归钉子（`agent/re_act/tests/parallel_unsafe.rs`） | — | 同左 |
| G9 | ~~无跨进程会话租约~~ **已落**（切 9）：`owner.lock` + flock，append 拒绝失败关闭 | — | `crates/uva-session/src/ownership.rs` |
| G10 | ~~无分块帧 / 协议协商 / 背压落盘~~ **已落（切 10）**：背压落盘 + 64 MiB 逻辑上限（omp v2 同值）；分块帧与协议协商对 Content-Length 传输**不适用**（逐条理由见切 10） | — | 同左 |
| G12 | ~~无客户端侧 fs/terminal 委派~~ **已落**（切 5 的 fs 半边 + 切 11 的 terminal 半边）：`fs/*` 与 `terminal/*` 两套委派都走同一条「客户端拥有」的路 | — | 同左 |
| G14 | ~~没有后台作业~~ **已落（切 13 + 切 14）**：`job` 工具把命令放到后台（`start` / `output` / `list` / `kill`），**完成通知主动送达**（切 14：作业结束 → 会话收到通知） | — | 同左 |
| G16 | ~~工具族少 `goal`（目标模式）~~ **已落（切 16）**：目标是会话日志里的行（每变一次一行），生命周期 + token 预算 + 宿主续跑（预算撞线或续跑 25 回合 → 暂停目标） | — | `crates/uva-tool-plugins/src/goal/` |
| G17 | ~~工具族少 `lsp`~~ **已落（切 17 + 切 18）**：诊断面（编译器的话）与查询面（dsh 的四个导航动作）都齐了；服务器由部署声明，能力缺一项就按能力名回绝 | — | `crates/uva-tool-plugins/src/lsp/` |
| G15 | ~~工具族少 `glob` 与 `grep`~~ **已落（切 15）**：两个工具都走本仓自己的遍历（不依赖外部 ripgrep），答案有界且自报截断 | — | 同左 |
| G13 | ~~工具族少一个 `edit`~~ **已落（切 12）**：逐字替换，旧文本必须原样出现，多处命中被拒并点名行号；编辑走 `read`/`write` 同一条缝 | — | `crates/uva-tool-plugins/src/edit/` |
| G18 | **工具批不守模型序**：按模式分桶（全部 Parallel 先跑，再全部 Sequential），模型说「先写后读」本仓先读。**已落（第二轮切 19）**：按模型序的波，排他调用是屏障 | 高 | 旧 `…/step/tools/mod.rs` 的 `bucket_by_execution_mode` + 两个桶 |
| G19 | **并发安全只按工具名判**：dsh 的 `isConcurrencySafe(args)` 与 omp 的 `resolveToolConcurrency(tool, args)` 都按**调用**判，本仓 `shell` 永远串行、`ls` 也排队。**已落（第二轮切 20）**：`Tool::mode_for(input)`，shell 只读命令可并行 | 中 | 旧 `Tool::mode()` + `registry.execution_mode(name)` |
| G20 | **无工具级超时**：stdio MCP 服务器不出声就把回合永远挂住（dsh 的 `timeout-policy` 给每个工具武装 `timeoutMs`，到期是结构化 `TOOL_TIMEOUT`）。**已落（第二轮切 21）**：结构化词汇 + 行内 code + stdio 传输的每请求预算 | 高 | `crates/uva-mcp/src/stdio.rs`（此前无截止时间） |
| G28 | **探索失败后回不去**：长任务走错路，最便宜的恢复不是撤销文件，而是**把探索从对话里剪掉、留下教训**（omp 的 `checkpoint`/`rewind`）。**已落（第四轮切 31）**：`checkpoint` 标边界（记在自己那一行上），`rewind` 用一次 `Replace` 把边界之后的表面换成报告——**会话树不变**（不 fork），剪的是**对话**不是文件 | 中 | `crates/uva-tool-plugins/src/checkpoint/` |
| G27 | **客户端不知道 agent 正在动哪个文件**：ACP 的 `ToolCall.locations`（「follow-along」）从不发送——编辑器看不见「正在编辑 foo.rs」。**已落（第四轮切 30）**：`Tool::locations` 在调用开始前答「会动哪个文件」，ACP 面发协议的 `locations` | 低 | `crates/uva-tool/src/traits.rs` |
| G26 | **会话列表只是一串 id**：两个参考都给会话起名字（dsh 的耐久标题状态、omp 的小模型标题），本仓的 `sessions/list` 只有 id / 状态 / 时间——客户端分不清两个会话。**已落（第四轮切 29）**：标题**从日志派生**（第一条 user 消息的第一行），不调模型、不另存一份可能漂移的真相 | 中 | `crates/uva-app-server/src/routes/sessions/mod.rs` |
| G25 | **durable 行不知道这次调用改了哪些文件**：改动只活在 live 帧与 ACP 的 diff 块里，客户端断开后再读日志，只剩工具的一句话摘要。**已落（第四轮切 28）**：`tool_result` 行增 `changed` 成员（harness 的提交路径从工具的报告里提出来），wire 信封与前端类型同步 | 中 | `crates/uva-session/src/events/mod.rs` |
| G24 | **半行让日志不可读**：日志末尾那半行（写到一半的行——读者撞上正在写的行，或崩溃撕开的行）让**整个读取失败**：客户端在 agent 正在写的时候读会话，会拿到一个错误；崩溃撕开的尾巴则让日志**永远读不了**（仓库此前只合成 closer，从不截断断尾）。**已落（第四轮切 27）**：读者只读**完整的行**（末尾那半行不是行），写者在第一次写之前**切掉**它（否则下一行会被粘上去） | 高 | `crates/uva-session/src/jsonl/mod.rs` |
| G23 | **模型看不见网**：只有 `web_fetch`（读一个模型已经知道的 URL），没有 `web_search`——两个参考都有，账本此前漏记。**已落（第四轮切 26）**：`web_search` 工具 + JSON 搜索契约 + 有界答案 | 高 | `crates/uva-tool-plugins/src/web/search.rs` |
| G22 | **文件改动到不了客户端**：工具结果只有一句话（「改了第 12 行」），ACP 的 `Diff` 内容块与 omp 的 diff 面板都没有数据可画。**已落（第四轮切 25）**：工具报改动 → ACP 发 diff 块 → 核心面 live 帧带 `diff` | 中 | `crates/uva-app-server/src/adapters/acp/projection.rs` |
| G21 | **工具族少一个 `ask`**：模型要问人，只能走审批闸门（一个有界替身，且只会问「准不准」）。**已落（第四轮切 24）**：结构化问题 + 客户端作答 + 有界等待；ACP 面如实缺席（ACP v1 没有提问原语） | 中 | `crates/uva-tool-plugins/src/ask/` |

---

## 5. 本轮切刀

### 切 1 — 审批有截止时间（G1 + G1b）

**要什么**：一个被持有的调用在 `approval.timeout_secs` 内没有答案，就按「拒绝」结算，
决定**落进日志**（`approval_decided`），回合**继续**（模型看到一条明确的超时拒绝），
而不是永远挂着。

**怎么落**：

1. `ApprovalPolicy` 加 `timeout_secs: Option<u64>`（策略是部署配置，秒是它能说的粒度）。
2. `approval` 行的 `config` 成为部署策略的真源（mode / ask_unknown / overrides /
   grants / timeout_secs）——行的配置第一次真的生效（G1b）。
3. 控制器在「运行以 `AwaitingApproval` 收尾」时**起一个定时器**；到期后走**同一条**
   resume 路径（派生 token → 构造 `rejected` 决定 → 提交）。定时器到期时先读日志：
   没有开口的审批 ⇒ 有人已经回答过了，什么都不做（幂等）。

**为什么定时器在宿主而不是 harness**：harness 在挂起时就把回合**结束**了
（`crates/uva-app-server/src/adapters/acp/turn.rs:16-22` 记录了这个顺序约束），
等待操作者是宿主的事；宿主也正是持有策略与日志的那一层。

**落点**：
- `crates/uva-approval/src/approval.rs`：`ApprovalPolicy::timeout_secs` +
  `timeout()`；`ResumeStatus::TimedOut`（日志词汇仍是 `denied`，模型可见的
  文案区分「没人回答」与「有人说不」）。
- `crates/uva-app-server/src/config/server/mod.rs`：`[approval]` 配置段
  （`mode` / `ask_unknown` / `timeout_secs` / 逐工具 `overrides`）→
  `ApprovalConfig::policy()`。
- `crates/uva-app-server/src/composition/kernel/mod.rs`：`approval` 行发布
  **部署的**策略（此前发布的是写死的默认值）；`BootContext::with_approval`
  承载它。
- `crates/uva-app-server/src/session/controller/dispatch.rs`：控制器在会话
  处于 `AwaitingApproval` 时**起**截止时间；到期走与人工作答同一条 resume
  路径（派生 token → 一次一个决定 → 提交 → 起跑）。

**验收**：
- 单元：`ApprovalConfig` → 策略（`config/tests.rs`）；`TimedOut` 的日志状态
  与模型文案（`uva-approval`）；策略带期限（`uva-approval`）。
- 行为（`crates/uva-app-server/src/session/controller/tests/approval_deadline.rs`）：
  挂起 → 超时 → 回合以 `TimedOut` 决定继续；同一时刻人工回答 → 决定仍是
  `Resolved`，截止时间不再决定任何事。

### 切 2 — 委派的头控制面（G8）

**要什么**：宿主能列举一个会话的子代理、硬杀一个、给它发一条消息——
omp 的 `get_subagents` / `cancel_subagent` / `steer_subagent`
（`packages/coding-agent/src/modes/rpc/wire/commands.ts:196-232`）。

**怎么落**：机器已经在（`crates/uva-tool-task/src/control/`：`ControlSurface::kill`、
`registry.list`、steer 通道），缺的是**门**：`tool-task` 行把控制面挂到
`BootServices` 的槽位（与 `subagent_writers` / `session_index` 同一手法），
核心 JSON-RPC 面加三条方法：`subagents/list`、`subagents/cancel`、`subagents/steer`。

**落点**：
- `crates/uva-tool-task/src/plugin.rs`：`tool-task` 行把控制面发布为
  `control` 服务（manifest 的 `provides`）。
- `crates/uva-tool-task/src/control/tools.rs`：steer 的投递
  （先落 durable 行，再进 inbox）提成 `deliver_steer`，模型与宿主走**同一
  扇门**。
- `crates/uva-app-server/src/routes/subagents.rs` + `rpc/methods/subagents.rs`：
  三条方法（`subagents/list` / `subagents/cancel` / `subagents/steer`）。

**验收**：`crates/uva-app-server/src/routes/tests/subagents.rs`——名册列出
子代理；取消是终态且**幂等**（第二次答 `cancelled: false`）；steer 送达
运行中的子代理；未知子代理被拒并说明理由。

### 切 3 — durable 读的游标（G11）

**要什么**：`sessions/events` 接受 `since`（一个序号），只回它之后的行，并回一个
`last_seq` 让客户端接着读——omp 的 `get_entries?since`
（`docs/rpc.md`「Read the append-history; with `since`, only entries strictly after
that durable entry id」）。

**怎么落**：行序号是本仓的日志坐标（`LogWriter` 分配，见
`crates/uva-session/src/writer/mod.rs:150-160`），所以游标就是一个序号；
过滤发生在读之后、分类之前。

**落点**：`crates/uva-app-server/src/routes/sessions/mod.rs`
（`list_session_events(state, id, since)` + `TypedEventResponse::last_seq`）
与 `rpc/methods/sessions.rs`（`EventsParams`）。

**验收**：`crates/uva-app-server/src/routes/sessions/tests.rs`——整本读、
带游标的读（只回之后的行）、游标在尾部时读为空且游标不动。

### 切 4 — 压缩作废的签名思维块（G5）

**要什么**：Anthropic 把思维块的签名绑定在**它被生成时的请求前缀**上。
压缩改写了前缀，于是幸存下来的签名**作废**；provider 会因此拒绝**整个请求**
（不是只拒绝那个块）。本仓此前原样回送这些签名——压缩后的第一轮请求就是
一次 400。

**怎么落**：作废的判定放在**知道前缀被改写过的那一层**（harness），而不是
provider：

- 日志是**只追加**的，能改写消息前缀的只有压缩（`SurfaceOp::Replace`）；
- 因此「签名前缀被改写过」等价于「这条消息写在某次压缩之前」；
- harness 在两处清理：开跑前（从日志里的压缩行算出边界）与本轮压缩落盘后
  （边界就是压缩行的序号）。

清理的是**签名**而不是整块思维：签名清空后该 part 变成「无签名」，而
provider 自己的规则（本仓的 Anthropic transform）就会丢掉无签名的思维块——
**模型失去那段推理，回合不失去**。

规则是**粗**的：压缩点**之前**那些其实前缀未变的签名也会被作废。作废一个
有效签名只是让模型少一点上下文；保留一个作废签名会赔上一整个回合。精确
到每条消息的判定需要前缀摘要（= G2 的 `replay_state`），那是下一刀。

**落点**：
- `crates/uva-harness/src/agent/re_act/transcript.rs`：
  `Transcript::void_signatures_before(boundary)`。
- `crates/uva-harness/src/agent/re_act/loop/step/prepare.rs`：开跑前从日志的
  压缩行算出边界并清理。
- `crates/uva-harness/src/agent/re_act/loop/compaction/mod.rs`：本轮压缩落盘
  后按压缩行的序号清理。

**验收**：
- 单元（`transcript.rs`）：边界之前的消息失去签名，边界之后的保留。
- 行为（`agent/re_act/tests/compaction.rs`）：日志里带一次压缩、助手轮带签名
  思维时，发给 provider 的请求里那个 part **无签名**，而助手轮的正文仍在
  请求里（证明作废的是签名，不是整条消息）。

### 切 5 — 客户端侧的文件系统委派（G12 的 fs 半边）

**要什么**：ACP 允许 agent 反向调用客户端的 `fs/*`——一个声明了
`clientCapabilities.fs` 的客户端拥有它开的会话的**文件**：`read` / `write`
工具以客户端的 `cwd` 为根，读写经 `fs/read_text_file` /
`fs/write_text_file` 回到客户端。此前本仓的 ACP 面显式声明「不委派」，编辑器
（Zed、JetBrains）驱动时工具跑在部署的盘上，读写的是部署的文件副本。

**怎么落**：一个 seam，一个注册表，一个 owner：

- **seam**（`uva-tool-plugins::files`）：`FileSystemOwner` ——「这个会话的
  文件在哪里、谁持有字节」。`session_root`（默认 `None` = 部署的工作区）、
  `read_file`、`write_file`。`FileError::Missing` 与 `FileError::Failed`
  分开——「文件不存在」与「读取失败」把工具引向不同的下一步。缺目录的创建
  归 owner：部署的盘能建，客户端（ACP 没有目录操作）不能，写入就按客户端
  自己的回答失败。
- **注册表**（app-server 的 `session::files::SessionFileSystem`）：会话 id →
  客户端 owner。没人认领的会话落到部署自己的盘——与委派存在之前完全一致。
- **owner**（`adapters/acp/files.rs`）：`ClientFileSystem` 持有连接 + 客户端
  的 `cwd` + 它在 `initialize` 里声明的能力；`read_file` /
  `write_file` 把 ACP 请求发给客户端（未声明的能力是**明确的错误**，不是
  静默回退到本地盘）。

**顺序**：`read` / `write` 工具先把路径按 `session_root` 解析并做安全检查
（客户端的会话以客户端的 `cwd` 为根），再交给 owner；工具的渲染
（行号、Created/Updated 文案）不变——**模型看到的形状不因文件在哪台机器
上而变**。

**落点**：
- `crates/uva-tool-plugins/src/files/mod.rs` — the seam + `LocalFileSystem`。
- `crates/uva-tool-plugins/src/{read,write}/mod.rs` — the tools hold the owner。
- `crates/uva/src/tool_files.rs` — the facade module（`tool-files` feature）。
- `crates/uva-app-server/src/session/files.rs` — the registry。
- `crates/uva-app-server/src/composition/kernel/mod.rs` — the `tool-read` /
  `tool-write` rows get the registry as their owner（移到 services-bound
  工厂列表）。
- `crates/uva-app-server/src/adapters/acp/{files,agent}.rs` — the client
  owner + the registration（`initialize` 记能力，`session_new` 认领会话）。

**验收**：
- 单元：本地 owner 读写盘、报告 missing（`files`）；注册表认领 / 释放 /
  未认领回退（`session::files`）。
- 行为（`tests/acp_stdio.rs`）：真二进制 + 真管道——客户端声明
  `fs.readTextFile`，开会话（`cwd = /client`），prompt 触发 `read`；agent 发
  `fs/read_text_file`（path = `/client/notes.txt`——**客户端自己的路径**），
  客户端答 `"from the client"`，模型的工具结果里就是那些字节。

**留下的半边**：`terminal/*` 委派（ACP 允许，本部署没有终端操作可委派）与
每会话 MCP 服务器。这两项在能力里继续如实缺席。

### 切 6 — 缓存友好的压缩（G4）

**要什么**：压缩的摘要请求是一个**旁路**请求——自己的 system prompt、把
span 渲染成一段文字。provider 的前缀缓存对它毫无用处，每轮压缩都是一次
全新计费。dsh 的形状（`compaction-basic/src/summarizer.ts:74-79`）：摘要请求
重放**会话自己的** system prompt 与工具 schema、被遮蔽的消息按 surface 顺序、
指令作为**最后一条 user 消息**——它是上一个被路由请求的**真前缀**，暖缓存
覆盖除指令外的一切。

**怎么落**：`SummaryInput { system: Option<String>, tools }` 走完整条路：

- **harness 的步内压缩**（热路径）：`RunState` 出 system prompt，
  `RunState::projection` 出工具 schema——与下一个请求**逐字节相同**的投影
  （被调用过的 Deferred 工具的晋升也一致）。
- **手动压缩**（`sessions/compact`）：路由只有日志、没有 run，拿不到请求
  信封——`SummaryInput::fresh()` 的独立形状是诚实的回退（span 渲染成文字，
  指令仍是最后一条消息）。
- **工具 schema 上了请求但答复必须是文字**：`ToolChoice::None`。空 schema
  列表保持请求自己的默认（`Auto`）——Anthropic 拒绝带 `tool_choice` 却没有
  tools 的请求，手动路径会因此 400。

**顺手修掉一个潜伏的适配器缺陷**：Anthropic 适配器此前**完全忽略**
canonical `CompletionRequest.tool_choice`——任何调用方设 `None` 都被静默
当作 `auto`。现在映射到线上字段（`none`/`any`/`tool{name}`；`auto` 省略——
Anthropic 默认就是它）。OpenAI 适配器本就处理 `none`，不动。

**落点**：
- `crates/uva-session/src/compaction/mod.rs`：`SummaryInput` +
  `Compactor::summarize(plan, input, cancel)` + 新指令（dsh 的分段骨架）。
- `crates/uva-harness/src/agent/re_act/loop/compaction/mod.rs`：步内压缩
  重放 run 自己的信封。
- `crates/uva-provider/src/anthropic/{types.rs,provider/transform/mod.rs}`：
  `AnthropicToolChoice` 线上类型 + 映射。
- `crates/uva-app-server/src/routes/follow.rs`：手动压缩的回退信封。

**验收**：
- 单元（session）：带信封的摘要请求 = `[system, …shadowed…, user(指令)]`，
  schema 重放且 `tool_choice = None`；无信封 = `[user(render), user(指令)]`
  且 `tool_choice = Auto`（Anthropic 空工具陷阱）。
- 行为（harness）：摘要请求的第一条消息与**主循环被路由的请求**的 system
  prompt 逐字节相同，指令是最后一条 user 消息，`tool_choice = None`。
- 适配器（provider）：四个 `ToolChoice` 变体的线上形状各一条（`Auto` 省略）。

### 切 7 — 插件树的审计报告（G6）

**要什么**：dsh 的 `auditRows`（`mount.ts:115-152`）回答一个问题——「这棵树
有没有不能用的行」。本仓此前只有逐 id 的 `PluginState` 查询，没有报告面：
一个部署跑起来，操作者看不到哪一行在等什么。

**怎么落**：`profile/audit`，dsh 的三态语义——不在报告里的行就是 **usable**，
报告里只有两类：

- `failed`：从未启动，或激活被拒（缺的 key 为空——失败是激活的，不是依赖的）；
- `pending`：在等一个组合不提供的服务，`missing` 逐一列出。

缺口计算复用 mount 自己的不动点（`audit_pending` 的同一套：live 行的
provides 汇聚 + 父作用域已可解析的 key），所以报告说的与 mount 会说的是同一
件事——修好一个依赖后两边同时转绿，不会漂移。

**落点**：
- `crates/uva-app-server/src/composition/kernel/mod.rs`：`RowAudit`（wire
  类型）+ `plugin_tree_audit`（与 `audit_pending` 共享 live_provides /
  parent_service_available 的同一台计算）。
- `Assembly::audit` / `AppState::profile_audit` → RPC 方法 `profile/audit`
  （名字表 + 注册 + 两侧一致性测试随方法表自动覆盖）。

**验收**：健康树报告为空；一行注入不存在的 key → 内核状态 `Pending`，审计
报告 `pending` 且 `missing = [key]`。

### 切 8 — `replay_state` 的诚实结算（G2）

**这个缺口是什么**：`CompletionRequest/Response.replay_state` 全仓只写
`None`，而历史 CHANGELOG 声称「已按 dsh §7 对账」。字段死了，声明没死。

**核实三件事之后的结论**：

1. **两个在仓适配器都没有可铸造的原生状态。** OpenAI 适配器说
   Chat Completions（`openai/provider/request.rs:25`）——`previous_response_id`
   属于 Responses API；Anthropic Messages 没有可回放的 response id
   （响应的 `id` 不被请求接受）。
2. **dsh §7 的 `replayState` 机制，本仓已结构化交付。** dsh 用它做的事是
   推理连续性：Anthropic 的思维块签名绑定请求前缀。本仓的签名随
   `ContentPart::Reasoning` 本身持久化，而切 4 的边界规则把它作废判定
   做对了——**且是精确的，不是近似**：压缩的 span 永远从 surface 头开始
   （`compaction/mod.rs:425`），所以每条 kept-tail 消息的前缀都包含被
   遮蔽的 span，不存在「前缀幸存却仍被作废」的消息。本仓唯一的另一个
   前缀改写者是 steer 的重排，而 `ModelOrder` 把 held-back 的 steer 放
   在工具结果之后、下一条 assistant 之前——生成序与 surface 序对签名
   而言一致。新增回归钉子：`a_signature_in_the_kept_tail_is_voided_too`。
3. **dsh 的逐消息前缀摘要在此不划算。** 它需要把摘要持久化到每条
   assistant 行（行 schema、fold、api-frozen 基线、web types 全动），
   换来的只是在「任意前缀编辑」场景下省一点上下文——而本仓今天没有那种
   改写者。在边界规则已精确的前提下，那是投机性基建。

**落点**：
- `crates/uva-provider/src/types/completion/mod.rs`：两个 `replay_state`
  字段的文档改为如实——在仓适配器不铸造；连续性由「签名 + 压缩边界规则」
  结构化承载；字段是**给会铸造的适配器留的 seam**（Responses-API 适配器的
  `previous_response_id`、逐消息前缀摘要），铸造在响应、回传在请求、校验
  在适配器——seam 两端已存在。
- `crates/uva-harness/src/agent/re_act/tests/compaction.rs`：kept-tail 的
  精确性钉子。

**判定**：G2 以「机制已交付、声明已对齐、seam 留给真实载荷」结案，而不是
以「给一个无人消费的字段再接一层管线」结案。

### 切 9 — 跨进程的会话所有权（G9）

**要什么**：两个进程共享一个 workspace 时，都能打开同一会话并各自追加——两个
写者盖出**碰撞的序号**，而一个序号无法复现的日志是不可读的。dsh 用
`flock(2)` 于 `session.lock`（`lease.ts:1-24,66-88`）：读句柄自由共存，
**无过期**——僵死的持有者锁到进程死为止（过期能撕开日志）。

**怎么落**：`owner.lock` 认领在**首次追加**时（`State::ensure_ownership`），
不是在 sink 构造时——列表会话时每个 durable 会话开一个 sink 只读，读不认领
（dsh 的读句柄规则）。持有到 sink 落地/进程退出：崩溃进程的内核关闭描述符
即释放。**拒绝失败关闭**：第二个写者的 append 得到
`SessionError::OwnedByAnotherProcess { holder: Some(pid) }`——写路径的既有
语义本就是致命错（「洞让后续压缩不可解」），拒绝沿着同一条路走。与 omp 的
差异如实记录：omp 换新兄弟文件（它的会话 id 允许），本仓的会话 id 就是
身份，换文件等于丢身份——拒绝是唯一安全答案。

**落点**：
- `crates/uva-session/src/ownership.rs`：`SessionOwnerLock`（flock
  `LOCK_EX|LOCK_NB`，pid 写进文件供拒绝点名；非 unix 用 O_EXCL + pid，
  僵死文件同样失败关闭——更重，但不出错）。
- `crates/uva-session/src/jsonl/mod.rs`：`append` / `append_many` 的
  closed / 版本门之后、任何字节移动之前认领。
- `crates/uva-session/src/sink.rs`：`SessionError::OwnedByAnotherProcess`。

**验收**：
- 单元（ownership）：同目录第二次认领被拒且点名 pid；drop 后可再认领。
- 行为（jsonl）：同一目录第二个 sink——开与读不认领（读共存）；append 被拒
  （点名 pid）；owner drop 后第二个 sink 写入成功、两行都在日志里。

---

### 切 10 — 出站背压落盘 + 逻辑上限（G10 的可迁移部分）

**要什么**：omp 的 `rpc-frame.ts`：v1 物理帧 1 MiB，v2 分块重组到 64 MiB；
背压时把待发字节**溢写到私有临时文件**，按 64 KiB 块排空
（`rpc.md`：「the server spills pending bytes to a private temporary file
and drains it in 64 KiB blocks」）。本仓的传输是 JSON-RPC 2.0 +
Content-Length 帧——**没有「一行」就没有分块的理由**，但**「读者比作者慢」的
理由完全一样**：出站通道是无界的，慢读者让进程内存无限增长。

**怎么落**：
1. 出站拆成两个任务：**编码器**（把信封通道排空成帧字节）与**写入器**（拥有
   stdout）。编码器急切地编码，慢读者不再在通道里堆 `Value` 图——帧字节比
   信封小一个数量级。
2. 队列 = **内存段**（水位 4 MiB）+ **磁盘段**（spool 临时文件）。溢出之后
   新帧一律进磁盘段，因此**到达顺序**由构造保证：内存段永远排在磁盘段
   之前。排空按 64 KiB 块，内存里只留一块。
3. 入站逻辑上限 8 MiB → **64 MiB**（omp v2 的重组上限）。Content-Length
   没有物理帧上限要分块绕过，唯一可能被撞到的旋钮就是逻辑上限，而 64 MiB
   是行业客户端预期能发的天花板。

**为什么不抄「分块帧」与「协议协商」**：Content-Length 按长度定界，一个帧
就是一个 JSON 值——把它拆开要在 JSON-RPC 层加重组协议（omp 的
`chunk_begin/chunk/chunk_end` 是它自己 JSONL 帧协议的形状），那是一次
**线格式破坏**：所有客户端都要跟着改，而本仓的帧里装的是 JSON-RPC 2.0 的
单一 JSON 值。**协议协商**在只有一个版本的协议里是投机——协商出来的第二
个版本还不存在。本仓的等价旋钮就是帧上限，已对齐 omp 的逻辑天花板。

**落点**：
- `crates/uva-app-server/src/rpc/spool.rs`：`Spool`（私有临时文件，pid + nonce
  命名，64 KiB 块排空，排空完即删；写失败**大声失败**而不是无声丢帧）。
- `crates/uva-app-server/src/rpc/stdio.rs`：`OutboundQueue`（内存段 + 磁盘段，
  到达顺序，内存水位可测）+ 编码器/写入器两个任务。
- `crates/uva-app-server/src/rpc/framing.rs`：`encode_frame`（帧的字节形态，
  编码器用它拼帧）+ `DEFAULT_MAX_FRAME_SIZE` 8 MiB → 64 MiB。

**验收**：
- 单元（spool）：两帧跨块排空，字节与顺序不变；排空后文件随 drop 消失。
- 单元（queue）：10 MiB 压进队列 → 内存段峰值不超过水位（4 MiB），其余在
  磁盘；排空后字节与顺序不变。
- 行为（stdio）：慢读者（64 字节 duplex）下两帧应答都不丢、顺序稳定。


### 切 11 — 客户端侧终端委派（G12 的 terminal 半边）

**要什么**：ACP 的 `terminal/*`（SDK 5.6：`terminal/create` /
`terminal/output` / `terminal/wait_for_exit` / `terminal/kill` /
`terminal/release`）让**客户端拥有终端**：agent 不在自己机器上 spawn
进程，而是请客户端在它自己的窗格里跑——用户看得见、能打断。omp 的
`set_host_tools` 是同一件事的另一种说法（宿主把自己的工具交给 agent）。

**怎么落**：
1. **一条缝**：`uva_tool_plugins::shell::terminal::TerminalOwner`——与
   `FileSystemOwner` 同构的「谁执行」缝：默认的 `LocalTerminals` 谁也不拥有，
   每个会话都在本机跑；客户端拥有终端的会话走客户端。
2. **工具按会话路由**：`ShellTool` 持有 `Arc<dyn TerminalOwner>`；会话被客户
   端拥有时，命令交给客户端，否则走原来的本机路径（策略、沙箱、进程组、超时
   全部不变）。**描述必须诚实**：工具的描述是静态的（一段文本对所有会话），
   所以它同时写明两条路——本机路径讲策略，客户端路径讲「客户端的规则代替策略」。
   一段谎称沙箱在生效、而命令其实跑在用户窗格里的描述，比没有描述更糟。
3. **注册表**：app-server 的 `SessionTerminals` 与 `SessionFileSystem` 同构
   （会话 id → 客户端 owner），挂在 `AppState` 上，`tool-shell` 行拿到它。
4. **ACP 侧**：`ClientTerminal` 按 ACP 的五个方法实现 `TerminalOwner`：
   `create`（`sh -c <command>`，cwd）→ `wait_for_exit`（带我们自己的截止时间，
   超时则 `kill`）→ `output`（窗格的文本）→ `release`。客户端在
   `initialize` 里声明 `terminal: true`，其会话才登记这个 owner；没声明的
   客户端照旧跑在本机。

**为什么恢复会话也要认领**：`session/load` 与 `session/new` 走同一条
`claim_session`——恢复的会话同样是客户端的会话（文件与终端都是）。此前 fs
半边只在 `session/new` 认领，恢复的会话会落回本机磁盘；这一刀把两边一起对齐。

**落点**：
- `crates/uva-tool-plugins/src/shell/terminal.rs`：`TerminalOwner` /
  `TerminalRun` / `TerminalError` / `LocalTerminals`。
- `crates/uva-tool-plugins/src/shell/mod.rs`：`ShellTool::with_terminals` 与
  按会话的路由；`TERMINAL_OWNER_PROMPT`（诚实的描述）。
- `crates/uva-app-server/src/session/terminals.rs`：`SessionTerminals`。
- `crates/uva-app-server/src/adapters/acp/terminal.rs`：`ClientTerminal`。
- `crates/uva-app-server/src/adapters/acp/agent.rs`：`claim_session`（新旧会话
  同一条认领路径）。

**验收**：
- 单元（shell）：被客户端拥有的会话，命令到客户端、输出与退出码按 shell 的
  形状渲染；没被拥有的会话照旧在本机跑，客户端一次也没被问。
- 单元（registry）：认领的会话走客户端；释放后回到本机。
- 行为（ACP，真实协议）：客户端声明 `terminal` 能力 → 开会话 → 部署把该会话
  的终端登记给客户端 → 部署请客户端跑命令 → 客户端的输出与退出码回到部署
  （`terminal/create` → `wait_for_exit` → `output` → `release` 全部走过）。

---


### 切 12 — `edit` 工具：精确改一处（G13）

**要什么**：工具族里有 `read` 与 `write`，**没有 `edit`**——改一行就要把整个
文件重写一遍，评审者看到的 diff 也因此变大。两个参考都有精确编辑：dsh 的
`str_replace_editor`（`packages/fs/tool-str-replace-editor/src/index.ts:295-320`）
与 omp 的 `edit`（`packages/coding-agent/src/edit/magic-tools.ts:191-210`）。

**怎么落**：
1. **一个模块 + 一个 feature**（插件族的老规矩）：`uva_tool_plugins::edit`，
   feature `edit`，facade 的 `tool-edit`，app-server 的 `tool-edit` 行
   （`profile.toml` 与能力矩阵同步）。
2. **逐字命中**：旧文本必须原样出现；多处命中被拒，**并点名行号**（dsh 的
   做法），除非显式 `replace_all`。
3. **失败可解释**：两种「没命中」给两种下一步——新文本已经在文件里（这次改动
   已经应用过），否则就是「逐字不符，去重读并复制原文」（omp 的诊断思路）。
   另外两条在读文件之前就拒：旧新相同（不改任何东西）、`replace_all` 配空白
   旧文本（会命中 everywhere）。
4. **走同一条缝**：文件归谁所有，编辑就落在谁的字节上——客户端拥有工作区时，
   `edit` 改的是客户端的文件（`FileSystemOwner` 这条缝在切 5 已经铺好）。

**落点**：
- `crates/uva-tool-plugins/src/edit/mod.rs`：`EditTool`（逐字匹配、行号、
  两种失败解释）。
- `crates/uva-tool-plugins/src/edit/plugin.rs`：`EditToolPlugin`（注册进
  `tools` 注册表，卸载即撤销）。
- `crates/uva-app-server/src/composition/kernel/mod.rs`：`tool-edit` 行。

**验收**：
- 单元（edit）：逐字命中改对了并点名行号；多处命中被拒且点名行号；
  `replace_all` 改全部；空新串即删除；两种「没命中」各给各的下一步；相同替换与
  空白 `replace_all` 在读文件前被拒；缺文件与越界路径被拒；编辑落在 owner 的
  字节上（本机磁盘没被碰）。
- 行为（app-server）：部署挂载了 `edit`（工具注册表里有它）。

---


### 切 13 — 后台作业（G14）

**要什么**：`shell` 工具在前台跑命令，到截止时间（600 s 的硬上限）就放弃。一次
构建、一套测试、一次迁移都装不进这个窗口——所以模型把它**放到后台**，之后再回来
取。

**怎么落**：
1. **一个作业注册表 + 一个 `job` 工具**：dsh 的 `job_output` / `job_list` /
   `job_kill` 三件套（`packages/jobs/tool-jobs/src/index.ts:1-60`）与 omp 的
   后台作业 + `wait`（`packages/coding-agent/src/tools/wait.ts`）的共同形状：
   一个部署的作业住在一个注册表里（工具是部署级单例，所以作业活得和部署一样
   久），`start` 给你 id，`output`（可等）给你输出与状态，`list` 列出，`kill`
   停掉。
2. **同一条执行策略**：后台作业跑在 shell 工具同一条策略下
   （`resolve_confined_command`）——**后台不是绕过沙箱的路**。受限策略没有后端
   就在这里拒绝。
3. **spawn 只有一处**：shell 与 job 共用 `crate::process`（argv、工作目录、
   环境、进程组、管道读取）——「命令在工作区里跑」这句话对两个工具是同一句。
   kill 走同一条进程组升级路径（SIGTERM → 宽限 → SIGKILL）。
4. **还没做的另一半**：dsh 把作业完成的通知送进主人的下一步（或在空闲时开一
   回合）；本工具是**问**出来的——模型自己用 `action = "output"` 来问。这是回
   合机器层的事（回合循环要能收旁路事件），不是工具层的事，账本里如实留着。

**落点**：
- `crates/uva-tool-plugins/src/process.rs`：唯一的 spawn 出口（unix）。
- `crates/uva-tool-plugins/src/jobs/mod.rs`：`JobRegistry` + `JobTool`。
- `crates/uva-tool-plugins/src/jobs/plugin.rs`：`JobToolPlugin`。
- `crates/uva-tool-plugins/src/shell/`：`shell/exec.rs` 的管道读取搬进
  `crate::process`（两个工具共用一份）。

**验收**：
- 单元（jobs）：作业跑完 → 输出与退出码回到模型；长作业在模型来看时仍是
  「还在跑」；`list` 列出它；`kill` 停掉它；未知 id 与缺参数被拒；受限策略
  无后端被拒。
- 行为（app-server）：部署挂载了 `job`（工具注册表里有它）。

---


### 切 14 — 运行中的回合能收到消息；作业完成主动送达

**要什么**：两件事，都是「运行中的会话怎么收到外面来的消息」。

1. **live steer 兑现承诺**：`delivery: "steer"` 的路由文档写着「注入**当下**正在跑
   的回合，下一个 step 边界把它带进模型的下一个请求」，控制器的注释也写着「steer
   追加行时回合还在 Running，下一个 step 边界就会注入它」——但**注入的那一步从
   来没有接线**：`AgentInput::inbox` 这条通道只有 `task` 工具的委派控制在用，主
   会话没有。于是行是耐久的，模型这一轮**看不到**它，而响应却回答
   `"delivery": "steer"`——**说了没做到**。
2. **作业完成主动送达**：切 13 的作业完成后，模型得自己去问。dsh 的做法是把完成通
   知送进主人的下一步（或在空闲时开一回合）。

**怎么落**：
1. **主会话有自己的 inbox**：控制器在 `mark_running`（与 `Running` 状态翻转同一个
   闸门）建通道，把 `AgentInput::with_inbox` 交给这一轮；`append_live_steer` 在行
   落盘之后把消息交给通道——顺序就是 `RunInbox` 文档写的「先写行，再交消息，中
   间崩了丢的是内存副本、耐久的事实还在」。回合结束时通道随控制器一起消失（弱引
   用 + 停止信号，不留后台任务）。
2. **作业完成走同一条路**：作业注册表在作业结束时通知订阅者；会话控制器订阅与自己
   会话 id 相同的那条，按 steer 的方式送达（耐久行 + 活回合的 inbox；没有活回合
   时退化成排队的下一轮——dsh 的 `wakeup` 语义）。

**落点**：
- `crates/uva-app-server/src/session/controller/inner.rs`：inbox 的安装、投递与清空。
- `crates/uva-app-server/src/session/controller/run_task.rs`：输入带 inbox；回合
  收尾时清空。
- `crates/uva-app-server/src/session/controller/mod.rs`：作业完成的投递任务（弱引用
  + 停止信号，控制器没了它就没了）。
- `crates/uva-tool-plugins/src/jobs/mod.rs`：注册表的完成通知（`JobCompletion`）。

**验收**：
- 行为（steer）：live steer 的消息到达活回合的 inbox（控制器测试 + 路由测试各
  一条）。
- 行为（job）：`job` 工具起一个作业 → 作业结束 → 拥有它的会话收到通知（端到端：
  真命令、真注册表、真控制器）。

---


### 切 15 — `glob` 与 `grep`：文件发现与内容搜索（G15）

**要什么**：账本的§3 一直缺一行——**工具族**这一轴从没对过账。两个参考都有文
件发现与内容搜索：dsh 的 `tool-fs-search`（一个插件注册 `glob` 与 `grep`，走打包的
ripgrep，`packages/fs/tool-fs-search/src/glob.ts:93`）与 omp 内建的 `glob` /
`grep`（`packages/coding-agent/src/tools/builtin-names.ts:3-33`）。本仓此前只有语
义 `search`（跨目录检索）与 shell 里的 `rg`——**受策略限制或没有 shell 时，模型找
不到文件**。

**怎么落**：
1. **自己遍历，不带 ripgrep**：dsh 的实现是 spawn 一个打包的二进制；本仓不引二进制
   依赖，改为一次有界的遍历（`crate::walk`）：`.git` 不进、符号链接目录不进（防环）、
   最多看 20000 个文件。
2. **glob 方言**：`*`（段内任意字符）、`?`（一个字符）、`**`（任意段）、其余按字
   面；**不含 `/` 的模式按文件名匹配任意深度**（ripgrep 的 `--glob` 就是这条规则），
   含 `/` 的模式匹配整条相对路径。
3. **答案有界且自报截断**：`glob` 最多 100 条（dsh 的 `GLOB_MAX_RESULTS`）、`grep`
   最多 250 条（`GREP_MAX_MATCHES`）、单行最多 2000 字节（`GREP_MAX_LINE_BYTES`）；
   遍历撞到上限或答案撞到上限时，**答案里写明**——截断过的搜索不能被当成完整的搜
   索。
4. **客户端拥有的工作区如实拒绝**：ACP 的 `fs/*` 只有按名读写，没有目录列举，
   所以遍历看不见客户端的文件——这两个工具在这种会话里**明确拒绝**并说明原因，
   而不是给出一个只覆盖本机磁盘的、看起来完整的答案。

**落点**：
- `crates/uva-tool-plugins/src/walk.rs`：有界遍历 + glob 方言（两者共用 ✓）。
- `crates/uva-tool-plugins/src/glob/`、`crates/uva-tool-plugins/src/grep/`：两个
  工具与它们的内核插件。
- app-server：`tool-glob` / `tool-grep` 两行（profile 文档与能力矩阵同步 ✓）。

**验收**：
- 单元（walk）：glob 方言的五条规则 + 遍历跳过 `.git` + 不跟符号链接 + 相对路径 ✓。
- 单元（glob）：相对路径的答案、子目录检索、无命中、空调法、越界路径、客户端拥有
  的工作区被拒 ✓。
- 单元（grep）：`path:line:text` 的形状、`include` 过滤、正则、非法正则、二进制跳
  过、客户端拥有的工作区被拒 ✓。
- 行为（app-server）：部署挂载了 `glob` 与 `grep` ✓。

---


### 切 16 — 目标模式：耐久目标 + 宿主续跑（G16）

**要什么**：两个参考都有目标模式——dsh 的 `tool-goal` 与 omp 的 goal mode
（`packages/coding-agent/src/modes/rpc/rpc-goal.ts`）：一个**模型创建的目标**、
一个生命周期（create / pause / resume / complete / drop）、一个 token 预算，以及
**宿主在回合结束后继续跑**。本仓此前只有 `todo`（一份清单 ✗——不是跨回合的目
标）。

**怎么落**：
1. **目标是会话日志里的行**（切 16 之一）：新的 `goal` 行，每变一次写一行，
   最后一行即目标状态；log-only，不进模型可见表面（与审批行同一套「日志即真源」
   的规则）。词汇的每个穷尽点都表了态：`surface_role=LogOnly`、`row_kind=None`、
   `ignorable=false`、`empty_event`、构造器、前端 handler（null）与重新生成的
   wire 类型。
2. **工具**（切 16 之二）：`goal` 的六个动作；有活目标时再 create 被拒；`pause`
   只对 active、`resume` 只对 paused、`complete`/`drop` 只对未结束的目标。工具
   **不拼任何行**：`GoalStore` 缝（宿主的会话注册表实现）读最后一行、写一行并
   flush。
3. **宿主续跑**（切 16 之三）：一个回合结束、没有排队的回合、而目标是 active
   时，宿主自己起下一回合（把「继续朝着目标工作」park 成一行耐久的 prompt）。
   两道边界：**预算**（会话 `usage` 行的 token 总和撞上目标预算 → 暂停目标）与
   **续跑上限**（25 回合 → 暂停目标）；任何用户回合把计数清零。不完成的目
   标于是变成一次可恢复的暂停，而不是无限自转。

**落点**：
- `crates/uva-session/src/events/`：`goal` 行（词汇、构造器、fixture、wire 类型）。
- `crates/uva-tool-plugins/src/goal/`：`GoalStore` 缝 + `goal` 工具 + 内核插件。
- `crates/uva-app-server/src/session/goals.rs`：`SessionGoals`（读最后一行、
  写一行并 flush、`spent_tokens`、`budget_allows`）。
- `crates/uva-app-server/src/session/controller/`：`continue_goal`（回合结束后的
  续跑、预算与上限、暂停）。

**验收**：
- 词汇：34 个标签的穷尽点全部表态（计数、角色、kind、ignorable、fixture、
  构造器回读、前端 handler 与契约）。
- 工具：10 条单测（生命周期、每种拒绝、store 的写入次数）。
- store：4 条（最后一行胜出、非 goal 行忽略、预算、只有 active 才续跑）。
- 续跑（真控制器 + 真日志）：active 目标起下一回合且 prompt 携带目标；完成的
  目标不再续跑；撞上上限时目标被暂停（一条单测各验证一条）。

---

---

### 切 17 — `lsp` 工具：编译器对这个文件说了什么（工具族的最后一格）

**要什么**：两个参考都有语言服务器面。dsh 的 `tool-lsp`
（`packages/lsp/tool-lsp/src/index.ts`）是四个**查询**动作
（`goToDefinition` / `findReferences` / `goToImplementation` / `hover`），
走 `ctx.lsp` 的会话级客户端，60 s 的工具超时。omp 的 `lsp`
（`packages/coding-agent/src/lsp/`）大得多：诊断 + 定义 + 引用 + 工作区符号 +
编辑 + 客户端管理 + 配置。

**怎么落**：
1. **诊断面先行**（切 17 之一）：模型最需要的那一格是「编译器说了什么」——
   不然模型只能去 grep 错误文本。新 `lsp` 工具启动部署为该语言配置的服务器，
   `initialize` → `initialized` → `textDocument/didOpen`，然后等服务器发布的
   `textDocument/publishDiagnostics`，把诊断按 `path:line:col severity message`
   的形状回给模型。
2. **协议手写，不引依赖**（切 17 之二）：仓库里没有 LSP crate，而 LSP 的
   `Content-Length` 定界与本仓自己的 RPC 定界是同一形状的不同协议——抄一份
   40 行的定界器比拉一个新依赖便宜（本仓的依赖纪律见 `AGENTS.md` §1.4）。
3. **服务器是部署的声明**（切 17 之三）：`[lsp]` 配置段（`[[lsp.servers]]`：
   `language` / `command` / `args`），`tool-lsp` 行发布给工具；一种语言一个
   服务器（两个服务器会让工具无从选择），没有对应服务器时工具按语言名回绝。
4. **一次调用一个服务器**：工具拥有服务器的长度是一次调用——启动、问、停。
   部署付的是「每次调用一个语言服务器」的价，不是「每个会话一个」。

**如实缺席**：切 17 只落了诊断面——查询面（definition / references /
implementation / hover）是切 18。

**落点**：
- `crates/uva-tool-plugins/src/lsp/`：`client.rs`（协议：握手、didOpen、诊断的
  收集、停机）、`mod.rs`（`LspServer` / `LspServers` 缝 + `LspTool`）、
  `plugin.rs`（内核插件）。
- `crates/uva-app-server/src/config/server/mod.rs`：`[lsp]` 配置段。
- `crates/uva-app-server/src/session/lsp.rs`：`DeploymentLspServers`（配置 → 值）。
- `crates/uva-app-server/src/composition/kernel/mod.rs`：`tool-lsp` 行。

**验收**：
- 工具：8 条单测——服务器发布的诊断真的到达模型（`path:line:col` 的形状）、
  没有服务器的语言按名字回绝、工具名、串行模式、URI 的百分号编码、severity
  的名字。
- 端到端：假语言服务器（脚本）→ 握手 → 诊断 → 模型的答案。

---

### 切 18 — `lsp` 的查询面：符号在哪里（G15 的第二格）

**要什么**：dsh 的 `tool-lsp` 是四个**查询**动作
（`goToDefinition` / `findReferences` / `goToImplementation` / `hover`，
`packages/lsp/tool-lsp/src/index.ts`），走 `ctx.lsp` 的会话级客户端；
omp 的 `lsp` 也有同一组（`packages/coding-agent/src/lsp/`）。模型的
导航问题——「这个符号在哪定义」「谁用了它」「这个 trait 谁实现了」——
文本搜索答不准，切 17 的诊断面也答不了。

**怎么落**：
1. **同一支客户端，多一种问法**（切 18 之一）：客户端在握手时记下服务器
   宣告的 capabilities，切 17 的 `didOpen` 变成公共的一步（诊断与查询
   都先打开文件——没见过文件的服务器指不出地方）。
2. **能力先问，位置后传**（切 18 之二）：服务器没宣告
   `definitionProvider` / `referencesProvider` /
   `implementationProvider` / `hoverProvider` 时，工具**按能力名回绝**
   （`the server does not advertise \`hoverProvider\``）——不说「没有结果」。
   位置在工具的边界上是 1 行的行号与列号（模型读得懂的形状），上线时
   变成协议的 0 基 UTF-16；服务器宣告的 positionEncoding 不是 `utf-16`
   时握手失败（每个位置都会是错的）。
3. **引用含声明**（切 18 之三）：`findReferences` 的请求带
   `context.includeDeclaration = true`（dsh 的工具描述就是这么写的）✓。
4. **三种地点形状都要读**（切 18 之四）：协议给的是 `Location`、
   `Location[]` 或 `LocationLink[]`（后者带 `targetUri` 与
   `targetSelectionRange`）——三种都要读成同一个「路径:行:列」的形状；
   `hover` 的内容可能是字符串、`MarkupContent`、`MarkedString` 或它们的
   数组。

**落点**：
- `crates/uva-tool-plugins/src/lsp/client.rs`：`Operation`（四个动作 ↔ 四个
  方法 ↔ 四个能力）、`Location`、`QueryResult`、能力检查、位置编码协商、
  三种地点形状与 hover 内容的归一化。
- `crates/uva-tool-plugins/src/lsp/mod.rs`：工具的 `operation` / `line` /
  `character` 参数、导航的调度与渲染（工作区相对路径）。

**验收**：
- 工具：12 条单测——诊断面仍然通、导航面答出「路径:行:列」、服务器没宣告
  能力时按能力名回绝、缺光标时先拒绝（不读文件）、动作名与参考一致。

---

### 切 19 — 工具批的模型序：排他调用是屏障（第二轮切 1）

**要什么**：两个参考都按**模型写的顺序**执行工具批。dsh 把计划分成
组：`mode === 'parallel' ? planned.slice(next) : [first]`，池在遇到
第一个排他调用时停止填充（`packages/core/tools/src/batch.ts:78-83`），
结果按模型序提交。omp 用 `lastExclusive` 链出同一个屏障
（`packages/agent/src/agent-loop.ts:4044-4060`）：排他调用等前面的
共享调用排干，后面的共享调用等它。本仓此前把批**按模式分桶**——全部
Parallel 先跑（并发），再全部 Sequential 串行——模型序被破坏：模型说
「先写后读」，本仓先读。

**怎么落**：调度器重写为**按模型序的波**（`schedule.rs`）：连续的
Parallel 调用共享一波（受 tracker 推荐的并发上限约束），Sequential
调用独占一波——它等前面的波排干，后面的波等它。既有的保全语义按新序
重新表述：排他调用失败 ⇒ 其后的排他调用跳过（文案不变），共享波照跑
（dsh/omp 都不停兄弟）；取消 ⇒ 未派发的调用答「cancelled before
dispatch; safe to retry」。

**落点**：
- `crates/uva-harness/src/agent/re_act/loop/step/tools/schedule.rs`（新）：
  `run_batch` / `take_shared_wave` / `run_shared_wave` /
  `run_exclusive_wave` / `recommended_concurrency`。
- `crates/uva-harness/src/agent/re_act/loop/step/tools/mod.rs`：桶阶段
  换成 `schedule::run_batch`；删掉 `bucket_by_execution_mode` 与两个桶。

**验收**：`tests/model_order.rs` 三条——批按模型序跑（共享、独占、共享），
独占是屏障（后面的共享等它），一波内的共享调用仍然重叠。门夹具用通道
而不是计时器：harness 在无运行时的线程上跑调用，`tokio::time::sleep`
在那儿会 panic（旧睡眠夹具恰好因此是空转的）。

---

### 切 20 — 并发安全按调用判定：`Tool::mode_for`（第二轮切 2）

**要什么**：两个参考都**按调用**判并发安全——dsh 的
`isConcurrencySafe(args)`（`packages/core/tools/src/pipeline.ts:108`，
SSH 工具：读共享、写独占），omp 的 `resolveToolConcurrency(tool, args)`
（`packages/agent/src/agent-loop.ts:120-135`，未声明 ⇒ 共享、抛错 ⇒
独占）。本仓只按**工具名**判（`Tool::mode()` ⇒ 注册表
`execution_mode(name)`）：`shell` 永远串行，`ls` 也排队。

**怎么落**：`Tool` 增加带默认实现的 `mode_for(&input)`（默认回落
`mode()`）；注册表增加 `execution_mode_for(name, input)`（未知名仍是
Sequential，失败关闭）；harness 的调度改问**这一次调用**。`shell` 第一个
 override：只读命令（`classify_command_risk == Low`，失败关闭的白名单）
⇒ Parallel，其余 ⇒ Sequential。

**落点**：
- `crates/uva-tool/src/traits.rs`：`Tool::mode_for`。
- `crates/uva-tool/src/registry/mod.rs`：`execution_mode_for`。
- `crates/uva-harness/src/agent/re_act/loop/mod.rs` +
  `step/tools/schedule.rs`：调度问调用。
- `crates/uva-tool-plugins/src/shell/mod.rs`：只读 ⇒ 可并行。

**验收**：shell 单测——`ls`/`cat`/`rg`/`pwd` 可并行，`rm`/重定向/
`cargo build`（未知）/`git push` 独占；无 `command` 参数不判并行。

---

### 切 21 — 工具级超时：结构化 `TOOL_TIMEOUT`（第二轮切 3）

**要什么**：dsh 的 `timeout-policy` 是包住 `tools/execute` 的守卫
（`packages/guard/timeout-policy/src/index.ts:26`）：工具声明
`timeoutMs`，包装器在 exec 信号上武装截止时间，到期把结果换成结构化
错误——`content` 是 `Error: tool call timed out after Nms`，
`error.info` 是 `{name: 'ToolTimeoutError', code: 'TOOL_TIMEOUT'}`，
重试/沙箱插件可以按 code 路由。本仓一处都没有：stdio MCP 服务器不出声
就把回合**永远**挂住（无头端没有操作员会注意到），超时了也只是散文。

**怎么落**：
1. **词汇**（uva-tool）：`ToolOutput::timeout(after)` 造出 dsh 形状的
   结构化失败（文本 + `is_error` + `metadata.error.{name, code}`），
   `ToolOutput::error_code()` 读回 code。
2. **行**（uva-harness）：`commit_tool_result` 把 `metadata.error.code`
   提升进 durable 行的 `error` 成员——中断行已经在用的那个位置，TUI 的
   outcome 与修复过程不用解析模型可见文本就能路由。
3. **强制**（uva-mcp）：stdio 传输的每个请求跑在一个预算下
   （默认 30 s = HTTP 兄弟的 `DEFAULT_HTTP_TIMEOUT`；
   `StdioConfig::call_timeout` / `with_call_timeout` 可调），
   到期是 `Error::Timeout`；`McpTool` 把它映射成
   `ToolOutput::timeout(transport.call_budget())`——传输知道自己
   武装了多少（trait 的 `call_budget()`）。

**落点**：
- `crates/uva-tool/src/types.rs`：`timeout` / `TIMEOUT_CODE` /
  `error_code`。
- `crates/uva-harness/src/agent/re_act/loop/step/tools/commit.rs`：
   code 提升进行。
- `crates/uva-mcp/src/{lib.rs,stdio.rs,client.rs,tool.rs}`：预算、
   `Error::Timeout`、`call_budget`、结构化映射。

**验收**：
- uva-tool：构造形状（文本 / 标志 / code / metadata.name）与
  无 code 时不造一个。
- uva-harness：结构化失败码到达 durable 行（`data.error`）。
- uva-mcp：不出声的 stdio 服务器（`sh` 脚本握手后 `sleep 300`）在
  一个预算内返回 `Error::Timeout`，包出的工具答案是结构化超时；
  预算 400 ms 时端到端 < 5 s。
---
### 切 22 — 核心面的出站请求通道（第三轮切 1）

**要什么**：核心面此前只有一条方向上的请求——客户端问，核心答。
两个参考都有另一条：omp 的 RPC 服务器会向宿主发 `host_tool_call` 并等
`host_tool_result`（`packages/coding-agent/src/modes/rpc/host-tools.ts:76`），
dsh 的插件树经 `fs/*`、`terminal/*` 问客户端（切 5 / 切 11 已对齐）。本仓的
核心面（自有 JSON-RPC 2.0 + Content-Length）**发不出自己的问题**：出站只有
响应和通知，没有「core→client 请求 + 应答关联」。

**怎么落**：
1. **通道**（`rpc/client_calls.rs`）：核心铸自己的请求 id
   （`core:internal:<n>`——保留前缀，与网关的 `gateway:internal:` 同一纪律），
   挂一张等应答的表；`RpcRuntime::call_client(method, params, timeout)` 发出
   请求帧并在预算内等应答，过期从表里摘除、答 `Timeout`。
2. **路由**（`rpc/stdio.rs`）：应答帧先查这张表，再落 ACP——ACP 的
   `answers()` 此前认领**每一张**响应形状的帧（「核心不问自己的问题」），
   这条不变量被本刀打破，注释同步改口：只有表命中的才归核心自己。
3. **网关**：零改动。网关的 `route_frame` 早就按**会话所有权**路由核心发的
   请求帧（ACP 的 `session/request_permission` 走的就是它），所以核心的
   请求 params 带 `session_id` 即可：直连 stdio 时对端是唯一客户端，网关
   模式下浏览器收到问题、应答回到核心。

**落点**：
- `crates/uva-app-server/src/rpc/client_calls.rs`：表 + id 铸造 +
  `ClientCallError {Timeout, Refused, Closed}` + `ClientToolResult`。
- `crates/uva-app-server/src/rpc/mod.rs`：`RpcRuntime` 持表，
  `call_client` / `resolve_client_answer`。
- `crates/uva-app-server/src/rpc/stdio.rs`：应答帧先查表。
- `crates/uva-app-server/src/adapters/acp/channel.rs`：`answers()` 注释改口。

**验收**：
- 表：铸造的 id 带保留前缀且互异；result 帧到达等它的 waiter；error 帧
  映射成 `Refused`；外来的帧（客户端自铸 id 的应答 / 请求 / 无 result 也无
  error）不碰表；超时摘除后晚到的应答找不到 waiter。
- 端到端在切 23 一并证明（本刀的通道在那里被完整走一遍）。

---

### 切 23 — 客户端拥有的工具（第三轮切 2）

**要什么**：omp 的 `set_host_tools`（`packages/coding-agent/src/modes/rpc/
rpc-types.ts:85,920`）：宿主把工具定义交给 agent，模型看到它们，调用时
RPC 服务器向宿主发 `host_tool_call`，宿主答 `host_tool_result`。dsh 没有
对应物（它的客户端拥有的是**能力**——文件、终端——不是工具）。§6 此前
记它「ACP v1 没有原语、本仓不能单方面补」——那半句对 **ACP 面**成立；
**核心面是本仓自己的协议**，可以单方面补，这一刀就在核心面补。

**怎么落**：
1. **注册**（`routes/tool/mod.rs`）：`tools/register` 增 `owner`（默认
   `core` = 今天的部署级 passthrough；`client` = 客户端实现）与
   `session_id`（`owner=client` 必填——问题按会话路由，网关用它找拥有
   该会话的连接）。注册装一个 `ClientOwnedTool` 入注册表；集群广播**不
   发**——对端节点没有客户端可问。
2. **执行**（`session/client_tools.rs`）：`ClientOwnedTool` 是切 22 通道的
   第一个用户——`tools/execute` 请求（`params.{session_id, name,
   arguments}`）发给拥有该会话的客户端，应答
   `result.{content, is_error}` 映射成 `ToolOutput`；超时是切 21 的结构化
   `TOOL_TIMEOUT`（60 s 预算——omp 无界、dsh 有 `timeoutMs`，无头端取有界）。
   调度模式 `Sequential`（客户端的执行对核心不透明，猜并行不 fail closed）。
3. **所有权**：注册表是**节点一个**，名字全局（两个客户端不能同名）；
   执行归注册会话的客户端，**别的会话调用被明确拒绝**（fail closed）；
   模型面全节点可见（harness 投影的注册表就是节点的那个）——这是注册表
   形状带来的已知 wart，如实记着。

**落点**：
- `crates/uva-app-server/src/session/client_tools.rs`：`ClientOwnedTool` +
  `CLIENT_TOOL_EXECUTE` + 预算。
- `crates/uva-app-server/src/routes/tool/mod.rs`：`owner` / `session_id` /
  `ToolOwner` + 注册分支。
- `crates/uva-app-server/tests/stdio_roundtrip.rs`：端到端。

**验收**：
- 单元：外会话调用被拒（拒绝点名拥有者）；无应答超时是结构化
  `TOOL_TIMEOUT`；注册表面（名字/描述/schema/Sequential）按客户端的声明。
- 路由：`owner=client` 无 `session_id` → `validation`；带会话注册后
  `tools/list`/`tools/get` 如实展示声明；同名二次注册 → `already_exists`。
- 端到端（stdio）：客户端注册 `client_tool` → `!tool client_tool {}` 驱动
  一回合 → 核心发 `tools/execute`（id 带 `core:internal:` 前缀、params 带
  会话与工具名）→ 客户端答 `ran on the client` → 回合 `completed`，
  durable 行里 tool_result 的文本就是客户端的应答。

---

### 切 24 — `ask`：模型把问题交给人（第四轮切 1）

**要什么**：两个参考都有一个「模型问人」的工具：dsh 的 `tool-ask-user`
（`ask_user_question`，`packages/interaction/tool-ask-user/src/index.ts:36-108`）
与 omp 的 `ask`（`packages/coding-agent/src/tools/ask.ts:76-79`）。形状是同一
个：**模型发问 → 工具挂起 → 人回答 → 回答作为普通工具结果回到模型**。本仓此前
只有审批闸门——一个「准不准」的有界替身，问不了「用哪个数据库」。

**怎么落**：

1. **工具**（`crates/uva-tool-plugins/src/ask/`）：问题取 dsh 的结构化形状
   （`id` / `question` / `header` / `options` / `multi_select`），回答取同一个
   形状（`{id, selected, custom}`）——omp 的 `ask` 收一段 HTML `<form>`，那是
   **渲染**，核心面是 JSON-only 的，所以本仓取结构化的一边。
2. **一条缝**：`QuestionOwner`——与文件（`FileSystemOwner`）、终端
   （`TerminalOwner`）同构的「谁回答」缝。工具自己不碰传输；宿主装实现：
   核心自己的 JSON-RPC 面（`questions/ask`，切 22 的出站通道）或一个能提问的
   客户端的原生原语。没人装 → `NoOperator`：**失败关闭，而不是把回合挂在
   一个没人答得了的问题上**。
3. **有界的等待**：dsh 的定时变体默认 120 s，omp 的 `ask.timeout` 可配。头less
   的默认是**有界**：客户端走人不能让回合永远挂着——预算用尽是结构化的
   `TOOL_TIMEOUT`（切 21 的词汇），模型看得见、可以重试。
4. **答案是不可信的输入**：客户端回的批次必须**每个问题恰好一次**——漏答、
   重答、答了没问的问题，都不是答案；把这样的批次当答案喂给模型，等于让
   一个坏客户端替人说话。
5. **ACP 面如实缺席**：ACP v1 只有一个原语能问人——`session/request_permission`
   （一次调用的准 / 不准），没有提问、没有选项、没有自由文本。ACP 客户端开的
   会话因此**立刻回绝**（「这个客户端说 ACP，ACP 没有提问原语；自己拿主意」），
   而不是把 120 s 的预算耗在一个编辑器根本给不出的答案上。

**落点**：
- `crates/uva-tool-plugins/src/ask/{mod,plugin}.rs`：工具 + 内核插件。
- `crates/uva-app-server/src/session/questions.rs`：`SessionQuestions`（会话 →
  主人）、`CoreQuestionOwner`（核心面作答）、`RefusingQuestions`（ACP 面回绝）。
- `crates/uva-app-server/src/composition/kernel/mod.rs`：`tool-ask` 行。
- `crates/uva-app-server/src/adapters/acp/agent.rs`：ACP 会话认领时登记回绝。

**验收**：
- 单元（ask）：答案成为工具结果（dsh 的 JSON 形状）；跳过是一个答案而不是失败；
  漏答 / 重答 / 答了没问的问题都被拒；没人答时失败关闭；回绝带理由；超时是
  结构化的 `TOOL_TIMEOUT`；空问题与超量问题**不**抵达操作者。
- 单元（questions）：认领的会话问它的主人；未认领的问默认主人；释放后回到
  默认主人；没人应答的客户端是超时。
- 端到端（stdio）：模型发问 → 核心发 `questions/ask`（id 带 `core:internal:`
  前缀、params 带会话与问题）→ 客户端答 → 回合 `completed`，durable 行里
  tool_result 的文本就是人的答案。

---

### 切 25 — 文件改动以 diff 呈现（第四轮切 2）

**要什么**：ACP 的 `ToolCallContent` 有一个 `Diff` 变体
（`agent-client-protocol-schema-1.22.0/src/v1/tool_call.rs:543-561`：
`path` / `old_text` / `new_text`），omp 的 ACP 面就靠它把每次编辑画成
diff（`packages/coding-agent/src/modes/acp/acp-event-mapper.ts:708-745`：
「Emit a `diff` ToolCallContent for each per-file edit result that carries
oldText/newText」）。本仓此前**从不发这个变体**——编辑到了客户端，只剩一句
「Replaced the text on line 12 of '/srv/app.rs'.」：编辑器画不出改动，
无头客户端也算不出。

**怎么落**：

1. **词汇**（`crates/uva-tool/src/diff.rs`）：`FileDiff`（`path` /
   `old_text` / `new_text` / `truncated`）+ 保留的 metadata 键
   `diff`。**形状就是协议的形状**——画 diff 的客户端不需要翻译。
2. **报告在 metadata 里，不在内容里**：模型看到的还是那句话；改动是给
   **人**看的字节，放进内容就是拿上下文窗口换渲染。工具的输出多一个
   metadata 成员，模型的视图一字不变。
3. **有界**：一边的文本上限 128 KiB。装不下的改动标 `truncated` 且
   **不带文本**——半个 diff 比没有 diff 更糟（它会显示不存在的改动）。
   消费方遇到 `truncated` 就画摘要，不画 diff。
4. **两个消费方**：ACP 面的投影在**结算**的那次更新里发 `Diff` 内容块
   （改动只有跑完才知道，所以不是 `tool_call` 起始帧——omp 也一样）；核心面的
   live 帧多一个 `diff` 成员，无头客户端据此画 diff。

**如实缺席**：**durable 行不带 diff**。行的载荷归词汇所有（ADR 0021），
把 diff 塞进行里要么改行 schema、要么另立一行类型——那是下一次的事。
今天的形状是：改动是**活的事实**，客户端看着它画；事后只读得到工具的摘要。
（dsh 的 `workspace-changes` 走另一条路：回合首尾的 git 工作树快照——
它答的是「这一回合改了哪些文件」，与本刀答的「这一次调用改了什么」是两件事。）

**落点**：
- `crates/uva-tool/src/diff.rs`：`FileDiff` + 保留键 + 上限。
- `crates/uva-tool-plugins/src/{write,edit}/mod.rs`：两个工具报改动。
- `crates/uva-app-server/src/adapters/acp/projection.rs`：`Diff` 内容块。
- `crates/uva-app-server/src/routes/chat/stream.rs`：live 帧的 `diff` 成员。

**验收**：
- 单元（uva-tool）：两边都在；新文件没有旧文本；装不下就是 truncated 且不带
  文本；报告在 metadata 里来回读得出来。
- 单元（write / edit）：改动被报出来（路径 + 改前 + 改后）；新文件没有旧文本。
- 单元（ACP 投影）：结算帧带 `Diff` 块（`path` / `oldText` / `newText`）；
  truncated 的报告**不**变成 diff。
- 单元（live 帧）：`tool_result` 帧带 `diff`。

---

### 切 26 — `web_search`：模型能查网（第四轮切 3）

**要什么**：本仓此前只有 `web_fetch`——读一个**模型已经知道**的 URL。dsh 的
`tool-web` 同时注册 `web_search` 与 `web_fetch`
（`packages/web/tool-web/src/index.ts:52-58`），omp 也有 `web_search`。
**模型不能搜索就是瞎的**：训练之后发生的事它一无所知，而无头 harness 没有
人来递一个链接。账本的§3 此前把 `web` 一行算作覆盖，漏了这一格——本轮补上。

**怎么落**：

1. **一条缝**：`WebSearchProvider`——工具自己不碰传输，宿主装实现。与
   `FileSystemOwner` / `TerminalOwner` / `QuestionOwner` 同构。
2. **一个契约**：随仓发布的适配器问一个**回答 JSON 的搜索端点**
   （`{"results":[{"title","url","content"}]}`——SearXNG 及其兼容实例的形状）。
   一个形状、一个适配器；部署要别的供应商，就写一个 `WebSearchProvider`
   ——工具不变。
3. **没有端点就失败关闭**：没配端点的部署**照样挂载**工具（dsh 的规矩：
   「an enabled tool remains visible when its provider is unavailable and fails
   with a structured error at execution time」），调用时明确说「没配搜索供应商」
   ——而不是回一句「没有结果」，假装网上是空的。
4. **答案有界**：最多 10 条（dsh 的 `WEB_SEARCH_MAX_RESULTS`），每条摘要最多
   800 字符（`WEB_SEARCH_SUMMARY_MAX_CHARS`），整篇答案最多 16 KiB；被截断的
   答案**自己说被截断**。

**落点**：
- `crates/uva-tool-plugins/src/web/search.rs`：缝 + 适配器 + 工具。
- `crates/uva-tool-plugins/src/web/search_plugin.rs`：内核插件。
- `crates/uva-app-server/src/config/server/mod.rs`：`[web_search]` 配置段。
- `crates/uva-app-server/src/composition/kernel/mod.rs`：`tool-web-search` 行。

**验收**：
- 单元（search）：工具名与模式；查询抵达供应商；答案就是结果；没有供应商时
  失败关闭；空查询不抵达供应商；空结果如此说；条数有界；摘要被截；答案被截
  时自报。
- 单元（config）：配了端点 ⇒ 供应商真的问那个端点（**真的本地 HTTP 服务器**，
  端到端）；没配 ⇒ 回绝的供应商。
- 行为（app-server）：部署挂载了 `web_search`。

---

### 切 27 — 断尾截断：半行不是行（第四轮切 4）

**要什么**：日志是**追加**的，每一行都以换行结尾。日志末尾那**没有换行**的半截
不是一行：它是「读者撞上了正在写的行」或「崩溃撕开的行」。dsh 的崩溃修复第一
步就是**断尾截断**（`packages/core/session/src/repair.ts:59-105`）。

本仓此前**两头都缺**：

- **读者不容忍**：读到最后那半行会**解析失败**，整个读取失败 ⇒ 客户端在
  agent 正在写的时候读会话（`sessions/events` / `follow`）会拿到一个错误，
  而客户端分不清「agent 正在写」与「日志坏了」。
- **写者不截断**：崩溃撕开的尾巴**留在原地**，下一次追加会被**粘上去**——
  一行变成两行，写者盖的序号从此读不出来；而且那半行让日志**永远读不了**
  （本仓的修复只合成 closer，从不截断断尾）。

**怎么落**：

1. **读者只读完整的行**（`complete_lines`）：文件不以换行结尾时，末尾那截
   **不是行**——丢掉它。中间的坏行仍然**大声失败**（那是真损坏，不是半行）。
2. **写者在第一次写之前切掉它**（`cut_torn_tail`）：切到最后一个完整行的
   末尾，文件恢复可写，下一行不会被粘上去。检查只做一次（`tail_cut`），
   而且**只读尾巴**（`read_tail` 有预算）——开一个 sink 仍然是 O(1)（日志长
   度无关）。日志的长度先问、字节后读：**不是普通文件的东西（设备、管道）
   不会被读**。
3. **切不切由写者说了算**：读者只读不写（单写者纪律），截断是**写**。

**验收**：
- 单元（session）：半行日志读回**完整的行**（不失败）；写者切掉断尾后再写，
  新行**完整**（没有被粘上）；写与读同时进行时读**不失败**。

---

### 切 28 — 耐久的「这次调用改了哪些文件」（第四轮切 5）

**要什么**：切 25 让改动**当场**看得见（ACP 的 diff 块、核心面的 live 帧），
但 live 帧是**活的事实**：客户端断开、重连、或者事后读日志，改动就没了——
只剩工具的一句话摘要（「改了第 12 行」）。dsh 的 `workspace-changes` 答的是
「这一回合改了哪些文件」，omp 的编辑结果带着 `oldText`/`newText`。

**怎么落**：

1. **行上多一个成员**：`tool_result` 行增 `changed`（`Vec<String>`，绝对
   路径）。**路径**而不是 diff 本身：diff 是两个文件的大小，进日志太重；
   路径是事实，客户端拿它自己对版本库求 diff。
2. **harness 的提交路径提出来**：与它已经提 `metadata.error.code` 进行的
   `error` 成员**同一条漏斗**——工具在 metadata 里报了改动，harness 在提交
   那一行时把路径提进行里。
3. **词汇是唯一的主人**：行形状归词汇（ADR 0021），构造器多一个参数；
   wire 信封（`SessionTurnEnvelope`）与前端类型**重新生成**——契约测试
   （「行信封覆盖词汇能写的每一个成员」）自己抓到了这个新成员 ✓。

**落点**：
- `crates/uva-session/src/events/mod.rs`：`tool_result` 行的 `changed`。
- `crates/uva-session/src/typed_event_builders.rs`：构造器多一个参数。
- `crates/uva-session/src/events/vocab/restore.rs`：读回时把 `changed` 还原。
- `crates/uva-harness/src/agent/re_act/loop/step/tools/commit.rs`：提路径。
- `crates/uva-app-server/src/api/v1/session_view.rs`：wire 信封 + 重新生成的
  前端类型。

**验收**：
- 单元（harness）：改了文件的调用，行上带着 `changed`；没改东西的调用，
  行上**没有**这个成员（不是空数组）。
- 单元（session）：行上的 `changed` 读回时还在。

---

### 切 29 — 会话列表给会话起名字（第四轮切 6）

**要什么**：dsh 有**耐久的标题状态**（`packages/session/session-title`，最新一次
胜出）+ 一个异步 provider（`session-title-llm`）；omp 用小模型生成标题
（`packages/coding-agent/src/utils/title-generator.ts`）。本仓的
`sessions/list` 此前只有 id / 状态 / 时间——**一串 id 不是索引**：客户端分不清
两个会话。

**怎么落**：

1. **派生，不存**：标题是**日志里第一条 user 消息的第一行**（截到 80 字符，
   截在词边）。**不调模型**（本轮范围明确排除提示词工程与成本优化），也
   **不另存一份**可能与日志漂移的真相（改写过的 prompt、fork 出来的会话，
   存的标题都会过期）。
2. **读头就有**：列表本来就为「创建时间」读**头几行**（每会话一次有界
   读）⇒ 标题**不额外读盘**；磁盘上才有的会话（进程重启前留下的）也一样
   ——扫描本来就读头 ✓。
3. **没有消息就没有名字**：刚建的会话、没跑过的委派，名字缺席而不是被
   编造。

**落点**：
- `crates/uva-app-server/src/routes/sessions/mod.rs`：`title_from_head` /
  `cut_title`；`SessionSummary` 与 `SessionDetail` 增 `title`。

**验收**：
- 行为（app-server）：列表里的会话带着第一条消息的第一行；只有空白消息的
  会话**没有**名字。

---

### 切 30 — 客户端的「正在动哪个文件」（第四轮切 7）

**要什么**：ACP 的 `ToolCall` 有一个 `locations` 成员——「clients implement
'follow-along' features that track which files the agent is working with in
real-time」。omp 的 ACP 面就发它（`acp-event-mapper.ts:225-228`，从调用的
**参数**里抽路径：`path` / `oldPath` / `newPath`）。本仓从不发送——编辑器
看不见「正在编辑 foo.rs」。

**怎么落**：

1. **工具自己答**：`Tool::locations(&input, &context)`（默认：无）。omp 从
   参数里**抽**路径是猜——本仓的规则是「工具的边界随工具走」：工具才知道
   自己怎么解析参数（客户端的工作区、会话的根），**发布的位置**与**真正
   被碰的文件**出自同一个答案，不可能漂移。`read` / `write` / `edit` 三个
   工具把「解析路径」提成自己的一个方法，`call` 与 `locations` 共用它。
2. **在调用开始前答**：`locations` 在调用**前**答，ACP 面把它发在 `tool_call`
   （起始帧）上——客户端在调用**进行中**就能跟着走。重放（`session/load`）
   没有注册表可问，发的是不带 locations 的形状：重放的调用带身份，碰过的
   文件是日志的事（`changed` 成员）。
3. **注册表代问**：`ToolRegistry::locations_for(name, input, context)`——
   未知名字答「无」（失败关闭）。

**落点**：
- `crates/uva-tool/src/{types,traits}.rs`：`ToolLocation` + `Tool::locations`。
- `crates/uva-tool/src/registry/mod.rs`：`locations_for`。
- `crates/uva-tool-plugins/src/{read,write,edit}/mod.rs`：三个工具实现它
  （解析提成一个共享方法）。
- `crates/uva-app-server/src/adapters/acp/projection.rs`：起始帧带 `locations`。

**验收**：
- 单元（edit）：调用答出它要碰的文件（解析后的绝对路径）；工作区外的调用
  **不**答（工具会拒绝的调用不点任何文件）。
- 单元（ACP 投影）：声明了位置的工具，起始帧带 `locations`（协议的形状）。

---

### 切 31 — checkpoint/rewind：探索，然后剪回去（第四轮切 8）

**要什么**：omp 有一对工具（`packages/coding-agent/src/tools/checkpoint.ts`）：
`checkpoint(goal)` 标一个边界，`rewind(report)` 把对话剪回那个边界、用报告替换
探索——「这条路不对，把探索从上下文里剪掉，留下教训」。长任务（本仓的 goal 模式
正是这种）走错路时，这是最便宜的恢复：**剪对话，不动文件**（探索写下的东西还在，
报告里说要拿它怎么办）。dsh 没有对应物（它的 `tool-worktree` 是工作区隔离，
`tool-session-control` 是跨会话消息——三件不同的事）。

**怎么落**：

1. **标记在自己的行上**：`checkpoint` 工具的结果行多一个 `checkpoint` 成员
   （harness 的提交路径从工具的报告里提出来——与 `changed` 同一条漏斗）。
   日志自己就能答「当前活跃的检查点是什么」；崩溃后也答得出。
2. **剪是一次 Replace**：`rewind` 写一条 `user_message` 行，载荷是报告，
   `surface_op` 是 `Replace`——**词汇不变**（重跑的回合早已用同一形状），
   fold 在下一次读日志时把探索剪掉 ✓。`source: "rewind"` 是关掉检查点的标记。
3. **剪在当前回合的 prompt 前停**：调用 `rewind` 的那个回合是平衡的（它的调用
   结果正在写），剪掉它会留下没有调用的工具结果。omp 把兄弟调用重新挂回去；
   本仓让剪的终点停在当前回合的 prompt 上——探索被剪，正在进行的回合完好。
4. **没有 store 就失败关闭**：`RewindStore` 缝（与 `GoalStore` 同构）；随仓的
   `NoRewinds` 让两个工具都明确拒绝——一个没人兑现的检查点比没有更糟。
5. **效果落在下一次 run**：fold 在日志下次被读时应用 Replace——当前回合继续
   用它正在推理的上下文。这正是这对工具自己的形状：模型把 `rewind` 当最后
   一步调，下一个回合（用户的下一条消息、goal 的续跑）从
   `[…前缀…, 报告]` 开始。

**落点**：
- `crates/uva-tool-plugins/src/checkpoint/{mod,plugin}.rs`：两个工具 + 一块砖。
- `crates/uva-session/src/events/mod.rs`：`tool_result` 行的 `checkpoint` 成员。
- `crates/uva-harness/src/agent/re_act/loop/step/tools/commit.rs`：提标记。
- `crates/uva-app-server/src/session/rewinds.rs`：store（读标记、一次 fold 定
  剪的坐标、写 Replace 行）。
- `crates/uva-app-server/src/composition/kernel/mod.rs`：`tool-checkpoint` 行。

**验收**：
- 单元（checkpoint）：标记进 metadata（harness 会提进行）；已有活跃检查点时
  拒绝并点名；空白 goal/report 在读任何东西之前拒绝；没有 store 时双双失败关闭。
- 单元（store）：最新的标记决定检查点（checkpoint 行开、rewind 行关）；没有
  检查点的 rewind 被拒；**剪之后真 fold 一遍**——前缀在、探索没了、报告顶上、
  正在剪的回合完好。
- 端到端（stdio）：两个回合（demo provider 现在按**指令**计一次调用，不再按
  会话）——标记在 durable 行上，报告在日志上，rewind 行带 Replace 与
  `source: "rewind"`。

---

## 6. 下一轮候选（不在本轮）

- ~~**G12 之后**：与编辑器对接的下一层是客户端的**扩展工具**（omp 的
  `set_host_tools`：宿主把自己的工具交给 agent 调用）~~ **已落**（第三轮
  切 22 + 切 23）——**在核心面上**：出站请求通道 + `tools/register
  owner=client`。ACP v1 仍没有对应的原语，那一半继续如实缺席——ACP 面
  的协议缺口不是本仓能单方面补的。
- **G10 的 JSON-RPC 重组协议**（若真要抄 omp v2 的分块语义）——那是线格式
  破坏：帧里不再是一个 JSON 值，而是一串需要重组的片段，客户端全都要改。
  切 10 已把**可迁移的部分**落完（背压落盘 + 64 MiB 逻辑上限），剩下的只有
  这一条真破坏。
（G7 已驳回——取消检查在工具流的 drain 里。G2 已结——切 8：连续性已
  结构化交付，seam 留给会铸造原生状态的适配器。G4 已落——切 6。
  G6 已落——切 7。G9 已落——切 9。G10 已落——切 10。G12 已落——切 5（fs）+ 切 11
  （terminal）。G13 已落——切 12。G14 已落——切 13（工具）+ 切 14（通知送达）。
  G15 已落——切 15（`glob` + `grep`）。G16 已落——切 16（`goal` + 宿主续跑）。
  工具族一行新加，剩余缺席如实记着：`ast_grep`/`ast_edit`
  （`checkpoint`/`rewind` 已落——切 31）。G17 已落——切 17 + 切 18。客户端扩展工具已落
  ——第三轮切 22 + 23（核心面）。G18/G19/G20 已落——切 19/20/21。
  G21 已落——切 24（`ask`）。G22 已落——切 25（文件改动的 diff）。
  G23 已落——切 26（`web_search`）。G24 已落——切 27（断尾截断）。
  G25 已落——切 28（durable 行的 changed）。G26 已落——切 29（会话的名字）。
  G27 已落——切 30（ACP 的 locations）。G28 已落——切 31（checkpoint/rewind）。）

---

## 7. 一条与参考无关、但更要命的缺陷

这条缺陷不是对账的结果，是**验证**的结果：跑全量测试时，`acp_over_rpc`
这个集成测试（ACP over core 的 stdio 通道）**挂在握手那一步**——它发的
握手帧得不到回答。查下去是切 10（出站背压落盘）留下的伤：出站拆成
「编码器 + 写入器」两个任务后，写入器只 `write_all` 而**没有 flush**，
而 stdout 是**行缓冲**的——帧停在写入器的缓冲里，直到**下一帧**到来才
被带出去。对端看到的因此是「上一帧 + 这一帧」，而不是「这一帧」。

两个参考都没有这个形状可以抄：dsh 与 omp 的 stdio 通道各自有自己的
写法 ✓。但这条缺陷的性质是**协议层**的——Content-Length 定界的传输上，
「写完就要让对端看见」是契约的一部分 ✗ 而不是实现细节 ✓。

修法两处 ✓：**写入器每写一帧就 flush** ✓（帧是小的 ✓），以及一条
**钉子** ✓——测试用**带缓冲的**写入器和一根**不结束**的输入管道跑泵 ✓，
没有 flush 时对端在超时里看不到那一帧 ✓ ✓（先红后绿 ✓）。

教训也记在这里 ✓：原来的泵测试用的是 **duplex** ✓——一个**无缓冲**的
写入端 ✗，所以**恰好**看不见这个缺陷 ✓ ✓。测试替身与真实替身不同的时候 ✗，
缺陷就从缝里漏过去 ✓ —— 下一轮的测试替身若要代表 stdout ✗，必须**也
缓冲** ✓ ✓。

---

## 8. 诚实声明

- 本轮只做**无头面**的对账；前端三端（浏览器 / 终端 / 桌面壳）的界面能力不在账本里。
- dsh 与 omp 的实现是**参考**，不是依赖：本仓不引入它们的代码，也不引入它们的
  运行时依赖（仓库的依赖纪律见 `AGENTS.md` §1.4）。
- 账本里的「对齐」指**行为同形**，不指实现同构：本仓是 Rust、单一事件流、
  单写者日志，dsh 是 Cordis 插件树，omp 是 Rust 核心 + TS 产品层。
