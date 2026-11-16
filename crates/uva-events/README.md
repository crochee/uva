# uva-events

Run-event vocabulary for the agent loop: `AgentEvent`、`SystemEvent`、
`WarningKind`、`SessionEndReason`——run 循环流出的每一个事件、告警种类
与终止原因都住在这里。

> 详细 seam 与公开 API 见 [`uva-events/src/lib.rs`](src/lib.rs)；
> 下沉设计见 [`docs/design/2026-11-15-plugin-crate-splits-design.md`](../../docs/design/2026-11-15-plugin-crate-splits-design.md) §6.4 Step 1。

## 这个 crate 拥有什么

- 事件枚举本身；载荷类型不在这里——内容块与采样结果来自
  `uva-provider`，工具结果来自 `uva-tool`，压缩生命周期结果来自
  `uva-session`，挂起（`Suspension`）来自 `uva-approval`。只想读事件
  流的消费方因此依赖词汇 + 载荷类型，而不是循环。
- `uva-harness` 在 crate 根 re-export 全部事件名，并把 `events` 模块
  保留为 re-export，所以冻结面与所有消费方 import 路径零改动；
  `uva_harness::events::…` 深路径照常解析。

## 非目标

- 事件不写日志、不做投影：durable 事实由 harness 自己的 writer 写入，
  这里只有 live 流上的词汇。
- 子代理的事件包装变体与元数据已随委派通道退役删除：子 run 的事件留在
  子会话自己的 live 流里，父 run 不再收到包装帧。

## CI 契约

- `cargo test -p uva-events --lib` 绿；
- runtime-free（`make check-no-runtime`）；
- 可打包（`make check-publishable`）。
