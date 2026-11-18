# 活动投影的身份是它的持久位置：一条行用序号取代它

活动投影（`LiveTurn`）今天**按运行铸造**（`controller/inner.rs:460` 的 `reset_live`），
文本在整轮里累加（`live.rs` 的 `push_str`）；而**助手行是每次模型调用一条**
（`harness/.../step/sample.rs:342`，事件词汇也这么写：“one `assistant_message` row
per model turn”）。于是投影的身份比它的寿命**粗**：一条助手行就足以退休整轮投影。

后果有两处，都不是猜错，而是结构错配：

- **网页端在多步运行里从第二步起不再流式**：服务端给内容行打上 live id
  （`routes/follow.rs:916` 的 `turn_labels`），网页端据此退休投影并记下
  `supersededTurn`（`ChatPage.tsx:559`），同一轮后续的 live 帧因此被丢弃
  （`ChatPage.tsx:600`）；它自己的注释写着 “the projection is cumulative and would
  duplicate it”。
- **TUI 重复第一步的文本**：它根本没有身份 —— 帧里的 `turn_id` 在
  `app.rs:1591` 被丢弃，只能比较文本形状（`glued_message_text`，`app.rs:2117`）；
  投影跨步累加，于是第一步的文字出现两次。

服务端那一侧也留着另一半：“投影不再运行 ⇒ 标签为 `None`（`turn_labels` 的注释：
“`None` rather than a stale id”）”，于是网页端还得留着内容兜底
（`textHeld` / `callsHeld`）来替那些没被打标的行做判断。

我们把活动投影改成**按步铸造**，并把它的身份换成**持久位置**。

## 决定

- **投影按步铸造**：一次模型调用是一步；步边界处投影的文本、推理与工具调用清空，
  新的一步是新投影。投影与 durable 行同粒度 —— 一条助手行就是它那一步的投影。
- **身份是持久位置**：投影带 `start_seq` = 该步开始时日志**已落盘**的序号。控制器
  在步边界读写入器的 `last_seq()`；harness 先落盘该步的行、再发该步的事件
  （`session/controller/persist.rs:44`），所以那个序号正好是这一步内容的起点。
- **取代判据只有一条**：一条内容行（助手消息 / 工具结果）的序号**大于**投影的
  `start_seq`，即取代该投影。行与投影之间的关联因此是日志序号本身，不需要任何
  打标、id 匹配或文本比较。早于该步的行（序号 ≤ `start_seq`）不取代；结算后落地的
  行照样取代；baseline 里的行按同一条规则处理 —— 三条路径都不再需要特判。
  另有一条边界规则保留：**轮**的边界行（`turn` 的 start / end）在运行已结束时取代
  投影 —— 那一轮不会再有内容行来取代它。
- **服务端不再给行打标**：`row` 帧上的 `turn_id`、`turn_labels` 与
  `carries_live_content` 整套删除。项目未到 v1（`AGENTS.md` §3.1），不留兼容字段。
- **线上字段**：`live` 帧的 `turn_id`（字符串）换成 `start_seq`（数值），契约随之
  重新生成。
- **两个客户端同一条规则**：网页端的内容兜底与 TUI 的 `glued_message_text` 一并退场；
  TUI 补齐网页端已有的两个直播守卫（已被取代的投影、`live_seq` 单调）。

## 参考

- [ADR-0007](0007-row-vocabulary-restores-at-the-read-boundary.md) 的纪律：一份规则，
  不留多个读点各写一遍 —— 本次把它落到活动投影上。
- [ADR-0008](0008-session-status-owns-the-session-state.md)：把“是什么状态”收成
  一个模块；本次是同一形状在“这条行取代了哪份投影”上。
- `routes/follow.rs` 的既有约定：durable 行与 transient 帧分开，帧带 `live_seq`
  做单调护栏 —— 本次保留这条，只换掉“取代”的判据。

## Considered Options

- **保留每次运行的 id**：被否。身份比投影的寿命粗，第一条助手行就退休整轮投影，
  多步运行的第二步起不再流式 —— 那正是本次要修的缺陷。
- **按轮铸造但轮末才退休**：被否。投影显示整轮累加文本，与 durable 行逐条重复；
  网页端现在的注释就是在描述这个困境。
- **按步铸造一个 UUID**：被否。行与投影之间没有持久关联（步号在恢复后会重数，
  行里也不带投影 id），归属只能近似：poll 滞后时会把旧行的标签挂到新投影上，
  那一步的流式又会消失。序号是本来就存在的持久关联。
- **保留服务端打标、客户端按 id 比对**：被否。序号规则已经精确，打标是第二份身份，
  而两份身份会漂移 —— 今天的 `turn_labels` 与客户端的兜底正是这样漂移出来的。
- **保留客户端的内容兜底**：被否。它存在的唯一前提（服务端无法打标）已经消失；
  留着它就是留着“前缀相同就误退休”的那类缺陷。

## Consequences

- **行为变化**：多步（用工具的）运行里，**每一步都流式**；网页端不再在第一步后
  停流；TUI 不再重复第一步的文本。行帧少一个字段；live 帧的字段改名并改类型。
- **契约**：`LiveTurn` 的 `turn_id` → `start_seq`，`types.ts` 重新生成；网页端手写的
  帧解析器（`api/follow-frames.ts`）同步。
- **测试面**：取代规则住在每个客户端的 reducer 里 —— 网页端抽成可单测的纯模块
  （`uva-web/src/lib/live-projection.ts`，vitest 覆盖），TUI 在 `app/tests.rs`；
  服务端的测试从“标签规则”换成“投影按步铸造、`start_seq` 落在步边界那一行之后”。
- **词汇表新增**：活动投影、取代、步 / 轮。
- **切面**：⓪ ADR + 词汇表 → ① 控制器（按步投影 + `start_seq`）→ ② 服务端（线上
  字段 + 删打标）→ ③ 网页端 → ④ TUI → ⑤ 测试与门禁。每片独立可评审。
