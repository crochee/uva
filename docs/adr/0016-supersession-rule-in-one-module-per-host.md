# 退役规则归一个模块：每个客户端各答一次

ADR-0012 把活动投影的身份换成持久位置，并把**取代判据**定成协议：一条内容行（助手
消息 / 工具结果）的序号大于投影的 `start_seq` 即取代投影；另有一条边界规则保留——
**轮**的边界行（`turn` 的 start / end）在运行已结束时取代投影。

但这条规则在客户端被重新推导了三次，三份拷贝的分叉各不相同：

- **网页端 `live-projection.ts`**：只拥有「内容行序号 > start_seq」这一半。
- **网页端 `ChatPage.tsx` reducer**：在模块之外又拼了轮边界条件（`(starting ||
  reason !== null) && !current.running`）、复活守卫（`supersededStart !== null &&
  live.start_seq <= supersededStart`）与打标。
- **网页端 baseline（重连）路径**：**只**用内容行这一半。基线里只有轮结束行、没有
  内容行（失败轮）时，投影不被退役——而增量路径会退役：**同一个客户端内部两条路径
  对同一件事给出不同答案**。
- **TUI `app.rs`**：内容行 ✅、轮**结束**行 ✅（`apply_turn_row` → `retire_live`），
  轮**开始**行只清文本**不打标** → 迟到的旧投影帧能在 TUI 里复活（网页端不会）。

所以「新增一种会退役投影的行」要落三个地方（两个 TS、一个 Rust），而其中两个地方
的规则还互不相同。

我们把退役规则收成**每个客户端一个模块**：规则是协议（ADR-0012 已定），客户端只负责
回答「这一条行 / 这一帧是否退役投影、是否准入」。

## 决定

- **网页端扩 `live-projection.ts`**：模块从「半条规则」变成「整条规则」——
  `retiresProjection(row, seq, position)`（内容行 + 轮边界）与
  `liveFrameAdmitted(startSeq, supersededStart)`（复活守卫）。reducer 与 baseline
  路径都只问模块；模块内不再有第二份判定。
- **TUI 新增 `supersession.rs`**：`retires_projection(tag, seq, start_seq, run_over)`
  与 `live_frame_admitted(start_seq, superseded_start)`；`app.rs` 只调用。
- **「运行是否已结束」由各 host 回答，模块不猜**：网页端用投影自己的 `running`
  标志（既有语义不变）；TUI 用轮边界行的含义——轮结束行即运行结束（`suspended`
  除外，那一轮还要恢复），轮开始行意味着上一轮已结束。
- **不重开 ADR-0012**：序号判据、边界规则、协议地位都不动；本 ADR 只把规则收到
  一个模块，并把两个客户端对齐到同一条规则。

## 参考

- [ADR-0012](0012-live-projection-is-identified-by-its-durable-position.md)：判据的
  协议地位与「两个客户端同一条规则」的要求。本 ADR 是它的收口，不重裁。
- [ADR-0015](0015-row-construction-in-one-module.md)、[ADR-0014](0014-mount-pipeline-in-one-module.md)：
  同一纪律（一份规则、一个 owner）在写行侧与挂载侧的落点。

## Considered Options

- **服务端发布「这条行取代了投影」的事实**：被否。ADR-0012 当初选序号规则恰恰是为了
  避开打标；再引入事实字段等于重开已决事项，且 wire 变更要连带契约重生成。
- **只收网页端**：被否。ADR-0012 写的就是「两个客户端同一条规则」，TUI 的缺口是实现
  缺口，不是决定——把它写进文档等于把协议分叉合法化。
- **两端合到一个共享库**：被否。两端是 TS 与 Rust，共享的是**规则**，不是代码；各一
  个同名职责的模块（`live-projection.ts` / `supersession.rs`）足够对齐。

## Consequences

- **行为变更两条**（均为向 ADR-0012 的规则对齐）：
  1. **网页端重连**：基线里只有轮结束行（失败轮、无内容行）时，投影被退役 → 页面
     显示持久转录，不再显示陈旧的流式回答（此前基线路径与增量路径不一致）。
  2. **TUI**：轮开始行也退役投影并打标 → 迟到的旧投影帧不再能在 TUI 里复活（网页端
     早已如此）。`suspended` 的轮结束行仍不退役（那一轮还要恢复），此例外不变。
- **测试面**：网页端 `live-projection.test.ts` 覆盖整条规则（内容行 / 轮边界 / 复活
  守卫）；TUI `supersession/tests.rs` 逐条件覆盖。
- **词汇表**：「取代」词条补上轮边界与「每客户端一模块」。
