# 挂载点契约下沉到 harness 之下，判据是仓外依赖树

> 依据：设计稿 [`2026-11-15-plugin-crate-splits-design.md`](../design/2026-11-15-plugin-crate-splits-design.md)
> §6.2；调研 [`…plugin-crate-splits-dsh-omp.md`](../research/2026-11-15-plugin-crate-splits-dsh-omp.md)
> 与 [`…subagent-live-visibility.md`](../research/2026-11-15-subagent-live-visibility.md)。
> 证据 id（形如 `[E019]`）均出自前者。

uva 的拦截栈三层齐备且与 dsh 同形：词汇（`uva-steering` 的 `HookEvent`/`HookDecision`，
crate 文档自述语义采自 dsh `[E021]`）、注册表（`uva-harness::Host` `[E031]`）、机制
（`uva-kernel::Waterfall`，注册可撤销），七个挂载点键（`agent/step-pre|step-post`、
`tool/pre-execute|post-execute`、`llm/request|result`、`prompt/sections`）早已是 pub 常量
`[E019]`。卡点在契约：插件要实现的 `Hook`/`Gate`/`RequestTransformer`/`ResultTransformer`
与它们拿到的 `HookContext` 都住在 `uva-harness::agent::host`，`HookContext.events` 的类型
是 harness 私有模块的 `EventSink`；waterfall 载荷 `SeamPass` 也是 harness 类型，外部
middleware 拿到的是不透明 payload，只有 harness 自己 downcast `[E065][E070]`。于是挂载点
名义公开、实际不可用：想在 `tool/pre-execute` 读到工具名、调用 id 与入参的第三方 crate，
必须把整个 harness（ReAct 循环、委派、seam）拉进依赖树。

我们把契约下沉成 **`uva-hook`**：四个拦截 trait + 解耦后的 `HookContext`（**删 `events`
字段**，只留运行状态与取消令牌）+ 七个键 + `HookRegistry`（可撤销注册）与仲裁；harness
的 `Seams` 退化为「构造载荷 → 跑注册表 → 翻译裁决」的分发器。判据是机械的，不是名义的：
样板插件 crate 以 `uva-hook` 为唯一 normal 依赖（harness 只进 dev-dependencies），新门禁
在**样板内**跑 `cargo tree -e normal` 断言依赖树不含 `uva-harness`——不能拿
`cargo tree -p uva-hook` 当断言，对契约 crate 自己跑必然为真——且样板测试真的拦下一次
工具调用（首个 `Block` 短路、`Ask` 汇入审批挂起）。「键是 pub 的」从此不算「对外可用」。

## Considered Options

- **只搬键名、契约留在 harness**：被否。键早已公开，卡点从来不在键；契约与载荷类型留在
  harness 里，仓外 crate 依然读不懂事件（不透明 payload 只有 harness 自己会 downcast）。
- **另造一套面向仓外的载荷词汇**：被否。`HookEvent` 已在 harness 之下（`uva-steering`），
  本身就是完整载荷——`BeforeTool` 带工具名、调用 id 与入参 `[E031]`；第二套词汇就是第二套
  事实，内外两套仲裁正是设计稿的反目标 A4。
- **照两家参考的包形状（拦截留在循环包内，不独立成 crate）**：被否，这是与参考实现的
  分歧点。dsh 没有 hooks 包（工具拦截在 `core/tools`、轮次拦截在 `core/agent-loop` 的
  `agent/*` 事件 `[E088]`），omp 的扩展通道也只是 coding-agent 包内的一个目录 `[E084]`。
  两家的「不独立」建立在扩展与宿主同仓同生态的前提上；uva 的第三方通道是 MCP server
  （`AGENTS.md` §3.7），挂载点面向的是仓外 Rust crate，没有先例可抄 `[E075]`。正因无先例，
  判据必须是依赖树断言，不能是「参考也这么做」。
- **`HookContext` 保留 `events` 字段、把 `EventSink` 类型一并下沉**：被否。全仓只有 harness
  自己的 `record_hook_block` 读过这个字段；下沉它等于把 harness 私有的 sink 半边拖进契约。
  删字段、sink 改显式传参（`host.seam_context()` 本就握着），`hook_block` 行由 harness 落
  ——顺带收口「外部插件短路不写行」的已知缺陷 `[E032]`。

## Consequences

- 仲裁一张表：同一条 waterfall、同一张 `HookEvent::accepts` 合法性表服务内外两类注册；
  首个 `Block` 先到先决、`Ask` 收集全部理由、注册表末尾跑一道只有拒绝没有放行的终审
  guard（dsh 单调拒绝：监听顺序无法把拒绝翻回放行 `[E004][E048]`）。外部插件 `Block` 时
  由 harness 落 `hook_block` durable 行——重载会话后仍知道谁拦了什么；durable 行才是重放
  真源，live 事件不是（子代理可见性调研的同一条原则）。
- 冻结面：trait 名并入 harness 现有 use 树 re-export 保路径（`check-pub-surface-strict`
  语句数 5/5 不增 `[E041][E092]`）；键常量在 `seams` 模块内部 re-export、不上 crate root；
  `HookContext` 删 `events` 字段是破坏性形状变化，记 `docs/api-frozen.md`。
- `Seams`/`Host` 自身不下沉（方案 C 的 hook 半边，明确不做）：harness 仍是分发器，换掉的
  只是契约归属；循环可换性交给依赖方向表达，不靠搬走分发器。
- 注册表接线走 `AgentLoopServicePlugin::resolve` 的 **`ctx.get` 机会式解析**，与 `subagents`
  的判定同源，但**不是** `AGENT_LOOP_INJECTS` 的边：注入键是 kernel 静态校验的硬依赖，缺
  provider 会拒绝整组，那会把 `hooks` 行变成必选——写在本行之前的 profile 文档与把行设为
  `Off` 的部署都会在 boot 期失败。`hooks` 行因此可选，缺少它时循环只是没有外部注册。
  新门禁 `check-hook-external` 按 `AGENTS.md` §3.6 四处同步。
