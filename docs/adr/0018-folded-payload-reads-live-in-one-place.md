# 折叠载荷的读法归一处：词汇是主人，消费者改问

载荷的读法按 ADR-0009 归行词汇管：**消费者改问词汇**，「这一行带了什么」只有一处
说了算。但那条纪律只落实了一半：

- **两套解码实现**。词汇有 `vocab::restore::decode_message`（宽松，`Option`；认
  provider 完整形状、手写 `{"role":…,"content":[…]}` 形状，兜底认文本与调用 id），
  history 又长了一套 `decode_message`（严格，`Result`；认 provider 形状，另认压缩
  的 `{"summary": …}`）。ADR-0009 写的「一份解码实现」对词汇内部成立
  （`SessionEvent::message()` 与 `CurrentRow::message()` 共用），history 后来长出
  第二套。
- **`data.summary` 两个读者**。词汇的行投影（`CurrentRow::message()` 的压缩分支）
  与 history 各自读这个键，各写一遍「压缩摘要 → 助手轮」。
- **provider 内容块的键两个读者**。`surface/fold.rs` 读 `type: "tool_result"` +
  `tool_use_id`，`tool_pairing.rs` 读 `type: "tool_use"` + `id`——同一套块形状，
  两处各写一遍键名。

于是「一行/一块带的是什么」在这三处各有说法。

## 决定

- **折叠载荷的解码进词汇**。词汇新增 `decode_folded_payload`：provider 形状 → 否则
  压缩摘要 → 否则 `Err`。history 改问它，自己那套解码删掉。
  **严格度不变**：历史的投影仍然对无法证明归属的载荷报错，不被宽松兜底救成一条
  用户轮；新测试 `a_payload_that_is_neither_shape_fails_the_projection` 钉住这件事。
- **`data.summary` 只有词汇读**。词汇内部抽 `compaction_summary_message`（唯一读者），
  行投影与折叠解码共用。
- **provider 内容块形状进一个读者模块**。新增 `uva-session/src/content_blocks.rs`：
  `collect_tool_use_ids` / `collect_tool_result_ids` 一次性命名块的键，fold 与
  tool_pairing 改问它。**手读保持手读**：旧方言载荷解不成 provider 的类型，而折叠
  必须仍能在旧载荷里找到调用；类型化路径（`tool_pairing::first_tool_use_id`）读的是
  解码后的消息，两条路径读同一形状、同一处定义。
- **不重开 ADR-0009**：它定的是「载荷的读法归词汇」，本次是把它补齐到折叠载荷
  与内容块这两处未覆盖的地方；「自由格式的载荷保留手读」那条仍然成立。

## 参考

- [ADR-0009](0009-row-vocabulary-owns-the-payload.md)：本 ADR 是它的补齐——那条
  ADR 说的「一份解码实现」在 history 长出第二套之后不再成立，本 ADR 把它收回。
- [ADR-0007](0007-row-vocabulary-restores-at-the-read-boundary.md)、[ADR-0006](0006-row-vocabulary-owns-the-row.md)：
  同一纪律在读边界与行标签上的落点。

## Considered Options

- **history 直接用词汇的宽松解码**：被否。history 会变宽容——今天报
  `HistoryError::Decode` 的载荷将不再报错，模型历史的严格性被悄悄放宽。
- **把块阅读器放进 `uva-provider`**：被否（作为本次的形状）。provider 才是线上形状
  的真正主人，但它的类型解不了旧方言载荷，容错逻辑仍要手写；provider 是冻结 crate，
  公开面增长要同步基线。本次把知识收到 session 内一处，provider 那条路留给真正
  需要跨 crate 复用时再走。
- **只做折叠载荷、不动内容块**：被否——两处读者说的是同一套块形状，留着就是留着
  下一个漂移点；改动只有搬家，风险低。

## Consequences

- **行为变更零**：解码语义逐字保持（provider 形状 → 压缩摘要 → 报错），块的键名逐
  字保持。新测试钉住严格度与块的读法。
- **已知细节**：助手行若载荷完全不可解码，折叠在更早一步就把它丢了，投影根本
  不会报错——严格错误实际出现在压缩形状的载荷上（新增测试用的正是这条路径）。
- **公开面零增长**：`decode_folded_payload` 与 `content_blocks` 都是 crate 内的
  `pub(crate)`；`uva-session` 的冻结面不变。
- **词汇与 history 的依赖方向**：history 问词汇（`crate::events::decode_folded_payload`），
  词汇不认 history——词汇是主人，不是被问的一方。
