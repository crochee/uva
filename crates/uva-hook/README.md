# uva-hook

拦截契约 crate：住在 agent 循环（`uva-harness`）之下的挂载点契约。
第三方插件只依赖本 crate 就能实现 hook / gate / transformer，依赖树里
不必再拉进整个 harness。

## 内容

- **四个拦截 trait**：`Hook`（观察并可否决）、`Gate`（安全裁决，
  失败即阻断）、`RequestTransformer`（请求参数改写）、
  `ResultTransformer`（工具出参改写）。
- **七个挂载点键**（`seam` 模块）：`agent/step-pre|step-post`、
  `tool/pre-execute|post-execute`、`llm/request|result`、
  `prompt/sections`。
- **解耦 `HookContext`**：只有运行状态（`state`）与取消令牌
  （`cancel`）——不带任何 sink 句柄；策略否决的 `hook_block` 行由
  harness 以显式传参写入。
- **`HookPayload`**：waterfall 链每次 pass 携带的载荷
  （`HookEvent` + 上下文），替代原 harness 私有的 `SeamPass`。
- **`HookRegistry`**：按七个挂载点键注册 / 撤销的可撤销句柄
  （`HookRegistration`）；同一条仲裁：首个 `Block` 短路、`Ask`
  收集全部理由、`Terminate` 只走 `HookEvent::accepts` 的五点表、
  末尾折叠只有拒绝没有放行（注册顺序无法把拒绝翻回放行）。
- **`HookPlugin`**：内核插件，把注册表发布成 `hooks` 服务；
  `agent-loop` 注入该边，把表穿进每次 run 的 `Seams`。
- **`async_trait` / `HookDecision` / `HookEvent` / `RequestParams`
  re-export**：外部插件实现 trait 只需本 crate 一个 normal 依赖。

裁决词汇不另造：注册方返回 `uva-steering` 的 `HookDecision`（五档，
含 `ReplaceParams`），载荷事件即 `uva-steering` 的 `HookEvent`。

## 对外可用的机械判据

样板 [`docs/examples/hook-only-consumer`](../../docs/examples/hook-only-consumer)
把 `uva-hook` 列为唯一 normal 依赖、把 harness 只放进 dev-dependencies：
`make check-hook-external` 在该 crate 内跑 `cargo tree -e normal`
断言树里没有 `uva-harness`，并跑其 dev 测试驱动一次真实工具调用
（handler 读到工具名 / 调用 id / 入参、`Block` 短路、注册可撤销）。
不能拿 `cargo tree -p uva-hook` 当判据——契约 crate 本来就是 harness
的依赖，那个查询必然为真。

## 路径保持

`uva-harness` 以合并 use 树 re-export 本 crate 的全部名字：crate 根
（`uva_harness::Hook` 等）与既有深路径
（`uva_harness::agent::host::Hook`、`uva_harness::seams::TOOL_PRE`
等）全部不变，冻结基线不动。

> 下沉判据（为什么是依赖树断言而不是键的可见性）见
> [`docs/adr/0004-hook-contract-sinks-below-harness.md`](../../docs/adr/0004-hook-contract-sinks-below-harness.md)；
> 设计见
> [`docs/design/2026-11-15-plugin-crate-splits-design.md`](../../docs/design/2026-11-15-plugin-crate-splits-design.md)
> §6.2。

## CI 契约

- `cargo test -p uva-hook --lib` 绿；
- runtime-free（`make check-no-runtime`）；
- 可打包（`make check-publishable`）；
- 依赖树不含 `uva-harness`；
- 对外样板门禁（`make check-hook-external`）绿。
