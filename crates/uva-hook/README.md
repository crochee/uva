# uva-hook

拦截契约 crate：住在 agent 循环（`uva-harness`）之下的挂载点契约。
第三方插件只依赖本 crate 就能实现 hook / gate / transformer，依赖树里
不必再拉进整个 harness。

## 内容

- **四个拦截 trait**：`Hook`（观察并可否决）、`Gate`（安全裁决，
  失败即阻断）、`RequestTransformer`（请求参数改写）、
  `ResultTransformer`（工具出参改写）。
- **十二个挂载点**（`MountPoint`）：六个 pass 点
  （`agent/step-pre|step-post`、`tool/pre-execute|post-execute`、
  `llm/request|result`）、五个观察点（`before_run`、`after_run`、
  `before_provider_call`、`on_error`、`on_compaction`）与
  `prompt/sections` 集合；每个变体只有一个线上拼法，拼写与解析读
  同一份声明。旧的七个字符串键（`seam` 模块）已删除，不留别名。
- **注册来源**（`RegistrationOrigin`）：每条注册显式声明来自仓外
  插件还是进程内策略。同一点上插件车道先于进程内车道，车道内按
  注册序；插件 panic 后跳过本次并保留，进程内注册 panic 后撤除并
  留下 `Fact`。
- **解耦 `HookContext`**：只有运行状态（`state`）与取消令牌
  （`cancel`）——不带任何 sink 句柄；策略否决的 `hook_block` 行由
  harness 以显式传参写入。
- **`HookRegistry`**：承载四条 trait 与 prompt sections 的唯一
  表，注册返回可撤销句柄（`HookRegistration`）；prompt sections 由
  `prompt_sections()` 按表序返回，不造合成 `HookEvent`。同一条
  仲裁：首个 `Block` 短路、`Ask` 收集全部理由、`Terminate` 只走
  `HookEvent::accepts` 的五点表、末尾折叠只有拒绝没有放行（注册
  顺序无法把拒绝翻回放行）。
- **`HookPlugin`**：内核插件，把部署的那一张表发布成 `hooks` 服务。
  表的主人仍是 `Host`：`Host::default_policy(workspace, registry)` 写
  默认策略，`Seams` 从 host 读它；`hooks` 行只是把同一实例发布给仓外
  代码，因此是不是有这一行都不影响默认策略。
- **`async_trait` / `HookDecision` / `HookEvent` / `RequestParams`
  re-export**：外部插件实现 trait 只需本 crate 一个 normal 依赖。

裁决词汇不另造：注册方返回 `uva-steering` 的 `HookDecision`（五档，
含 `ReplaceParams`），载荷事件即 `uva-steering` 的 `HookEvent`。

## 对外可用的机械判据

样板 [`docs/examples/hook-only-consumer`](../../docs/examples/hook-only-consumer)
把 `uva-hook` 列为唯一 normal 依赖、把 harness 只放进 dev-dependencies：
`make check-hook-external` 在该 crate 内跑 `cargo tree -e normal`
断言树里没有 `uva-harness`，并跑其 dev 测试驱动一次真实工具调用
（handler 读到工具名 / 调用 id / 入参、`Block` 短路、注册可撤销，
以及仓外注册的 `before_run` 观察者收到事件）。
不能拿 `cargo tree -p uva-hook` 当判据——契约 crate 本来就是 harness
的依赖，那个查询必然为真。

## 路径保持

`uva-harness` 以合并 use 树 re-export 本 crate 的拦截名字：crate 根
（`uva_harness::Hook` 等）与既有深路径
（`uva_harness::agent::host::Hook` 等）不变。七个挂载点键随 ADR 0028
从 `uva_harness::seams` 删除，冻结基线记录了这次移除；`MountPoint`
与 `HookRegistry` 由本 crate 提供。

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
