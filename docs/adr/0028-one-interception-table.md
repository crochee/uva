# 拦截只有一张表：十二个挂载点，一个主人

ADR-0004 把拦截契约下沉到 `uva-hook`，ADR-0020 把裁决收进一个 owner——但「一条拦截
注册在哪里、由谁派发」仍有两套：`HookRegistry` 的七个键，与 `Host` 的六条 kernel
链加一张残差表。后果是四条 trait（`Hook` / `Gate` / `RequestTransformer` /
`ResultTransformer`）有两套注册面且覆盖不重叠：五个观察点（`before_run` /
`after_run` / `before_provider_call` / `on_error` / `on_compaction`）只有进程内
`Host` 能注册，仓外插件在契约上够不着。循环的 `Seams::dispatch_on` 为此合并
registry → chain → 残差三条路；同一个 `Hook` 走 `Host::hook` 又会同时进六条链与
残差表。`Host` 自己的文档已经写明「新代码应走 kernel effects，而不是这张表」，但
kernel effects 仍不是拦截的对外注册面。

我们把拦截注册与派发收成一张表。

## 决定

- **`uva-hook::HookRegistry` 是唯一注册表**，承载四条拦截 trait 与 prompt
  sections。`MountPoint` 枚举声明十二个挂载点的线上拼法：六个 pass 键
  （`agent/step-pre`、`agent/step-post`、`tool/pre-execute`、`tool/post-execute`、
  `llm/request`、`llm/result`）、五个观察点、一个提示集合。观察点由此第一次对
  仓外注册可见。
- **部署里只有一个实例。** boot 先建 `HookRegistry`；`Host::default_policy(workspace,
  registry)` 把进程内策略写进同一张表；`hooks` 行发布同一个实例；`Seams` 从
  `Host` 读它。没有 `hooks` 行的部署仍然有默认策略；没有部署表的嵌入方拿到一张
  空表。
- **每条注册带 `RegistrationOrigin`。** `Plugin` 的注册 panic 后保活（跳过本次），
  `InProcess` 的注册 panic 后撤除并诊断——ADR-0020 的两条策略原样保留，但现在是
  表里每条记录的事实，不再由「哪条派发路径」决定。
- **同一点的次序是来源车道 + 注册序。** 仓外注册先于进程内注册，车道内按注册
  先后。这是把今天「registry 先、host 后」的观察结果写成规则，不是新语义。
- **`Host` 只留 registry 表达不了的表**：guards、hints、trackers、output
  transformers、tool 绑定。`Host::{hook, gate, request_transformer,
  result_transformer}` 删除；`host::Plugin` / `Host::install` / `PluginHandle`
  这套没有生产使用的装包接口一并删除。`Seams` 的读转发与 `host()` 不在本次范围。
- **prompt sections 是集合，不是事件。** `MountPoint::PromptSections` 存在，但由
  集合方法按注册序返回，不造合成的 `HookEvent`。给一个只收集、不裁决的点造事件，
  只会让 `HookEvent::accepts` 回答一个它没有的问题。

## Considered Options

- **`Host` 拥有这张表、`HookRegistry` 做 adapter**：被否。`uva-hook` 在
  `uva-harness` 之下，adapter 会让契约 crate 反向依赖 harness。
- **保留两套注册面，只统一派发**：被否。这正是要修的裂缝：仓外插件仍然看不见五个
  观察点，循环仍然要合并三条路。
- **从调用方推断注册来源**：被否。跨 crate 不可表达；两个构造家族
  （`register_hook` / `register_internal_hook`）又把一条安全相关的区别藏进命名约定。
- **所有注册一条全局顺序**：被否。它会改变默认策略与仓外插件谁先跑；来源车道 +
  注册序保留了今天的可观察行为，而车道只是表里一个字段，不是第二条派发路径。
- **给 prompt sections 造一个合成 `HookEvent`**：被否。它没有裁决语义，见上。
- **把 guards / hints / trackers 一起收进 registry**：被否。它们是进程内类型化策略，
  没有仓外注册面；`Host::default_policy` 仍是它们的组合点。

## Consequences

- **公开面破坏性变更**：删除 `Host` 的四个注册方法与 `host::Plugin` 装包接口。冻结
  基线按名字比较；`Host` 与四条 trait 的名字都不移动，只记一条 shape change。
  `docs/api-frozen.md` 记这次形状变化，`SEAMS.md` 把「policy layer」的注册入口改指
  `HookRegistry`。
- **行为变更一条**：仓外插件现在能在五个观察点注册并收到事件。既有的派发次序、
  panic 策略、fail-closed 与 `hook_block` durable 行都保持不变。
- **测试面**：`Host` 两路合流的测试删除；`uva-hook` 的表测试覆盖十二个挂载点、
  来源车道与两条 panic 策略；`check-hook-external` 样板增加一条观察点断言——仓外
  注册 `before_run` 并看到它触发，这是本 ADR 的验收面。
- **切面**：① `uva-hook` 落地 `MountPoint`、来源、五个观察点与表派发，不改
  harness；② harness 收口：boot 接线、`Seams` 一条派发、`Host` 删除；③ 样板、门禁、
  文档。ADR-0004 的「`Seams`/`Host` 自身不下沉」仍然成立；本 ADR 取代的是
  ADR-0020 的「三个 adapter」——裁决 owner 不变，adapter 只剩注册表一个。

## 参考

- [ADR-0004](0004-hook-contract-sinks-below-harness.md)：契约下沉与「仲裁一张表」；
  本次把「注册与派发」也收进同一个契约 crate。
- [ADR-0020](0020-interception-arbitration-in-one-owner.md)：裁决 owner 与两条
  panic 策略；本次保留规则，删掉其中两个 adapter。
- [ADR-0014](0014-mount-pipeline-in-one-module.md)：boot 与重挂共用一条构建配方；
  registry 的出生点跟随同一约束。
