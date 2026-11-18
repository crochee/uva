# 行构造归一个模块：uva-session 之外无人拼 SessionEvent 变体

「行词汇」（ADR-0006）定下行标签的唯一住处，但**写行**这条路径没有跟上：一个行的
`type` 要问词汇，可这条行的**整个形状**——哪个变体、哪些字段、`seq: 0` / `ts: ""`
的未盖章约定——却可以被任何消费者当场拼出来。改造前的树上有 14 处在 `uva-session`
之外拼 `SessionEvent::变体 { ... }` 字面量：

- app-server 9 处（`routes/messages` 的种子路径、controller 的
  `Warning` / `ParkConsumed`×2 / `UserMessage` / `ParkedPrompt` / `Feedback` /
  `SessionShutdown`）——其中 6 处发生在 `typed_event_builders` 已有同名 builder 的
  情况下（builder 不收带角色的 `Message`，也不收非默认 `surface_op`）。
- harness 5 处（`HookBlock` / `AssistantMessage` / `Warning` / `ToolResult` /
  `ToolResultTransformed`）。

拼字面量不是风格问题：`inner.rs` 已经出现真实病灶——手动 `clock.now()` 盖
`ts`，而 `LogWriter::stamp` 在 append 时**总是重盖**（两次钟读、一次作废）。行形状
知识住在行家之外，就会长出这种漂移。

我们把「载荷 → 未盖章行」收成 `uva-session::typed_event_builders` 一个 seam。

## 决定

- **按标签补全 builder，不做通用入口。** 新增 `warning` / `tool_result` /
  `hook_block` / `tool_result_transformed` / `parked_prompt` / `park_consumed` /
  `feedback` / `session_shutdown`，加上两个语义入口：`message_row(&Message)`
  （带角色的 provider 消息 → 对应行，种子路径的 match 整体搬进 uva-session）与
  `turn_prompt_row`（本轮 prompt 行：attachments + fold 算好的 `surface_op`，
  `None` 追加、rerun 的 `Replace` 遮蔽）。一个 `row(tag, payload)` 通用入口会把
  wire 形状推回调用方，与 ADR-0006 的方向相反，被否。
- **读侧不动。** pattern match（fold、render、scan）是读模型，不是第二份构造知识；
  消费者解构行、词汇消费者解构标签，两类都合法。判别器因此只认**构造形状**。
- **不变量进门禁。** `check-row-vocabulary` 的 Rust 扫描器加一道构造判别：
  `uva-session` 之外，一行若拼出 `SessionEvent::X {` 且无 pattern 标记
  （`..` / `=>` / `matches!`）、前缀（剥掉路径段）不以 `let `/`Some(`/`match`/`|`/`@`
  结尾，即红。裸多行 match 臂与判别器同形——`uva-approval` 三处——进审计表（同一
  ratchet 机制：命中消失条目即红）。`--self-test` 为构造判别器加了夹具。

## 参考

- [ADR-0006](0006-row-vocabulary-in-one-module.md)：行标签的唯一住处。本 ADR 是它的
  写侧对偶——标签问词汇，形状走 builder。
- [ADR-0014](0014-mount-pipeline-in-one-module.md)、
  [ADR-0013](0013-controller-state-transition-table-in-one-module.md)：同一纪律
  （一份规则、一个 owner）在挂载管线与控制器状态上的落点。

## Considered Options

- **一个通用 `row(tag, payload)` 入口**：被否——接口最窄但形状知识回涌调用方。
- **把读侧 pattern 也收进词汇**：被否——读模型是合法消费者，收它不增加 locality
  只增加摩擦。
- **不改门禁、只靠评审**：被否——下一次「就拼一次」没有阻力，seam 会重新漏水。

## Consequences

- **行为变更一条**：`SessionShutdown` 行不再带调用侧的时钟读（writer 反正重盖），
  消除双钟读病灶；其余迁移是等价替换（builder 单测钉住 wire 形状，两个 crate 全量
  测试绿）。
- **`uva::session` 导出面**新增 10 个 builder 名（lib.rs 既有命名列表内追加，
  `check-pub-surface-strict` 按 `pub use` 行数计，不增）。
- **AGENTS.md §3.6** 的 `check-row-vocabulary` 行同步记录构造规则。
- **本 ADR 不裁定的**：`RowTag::empty_event` 的 tag↔event 对应性机制（测试专用的
  判别骨架，生产不调用，与 builder 不构成双表）；测试代码里的变体拼法（门禁按
  生产/测试分界豁免，与既有约定一致）。
