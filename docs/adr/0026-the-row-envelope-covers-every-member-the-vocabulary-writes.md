# published row 的线上形状是一份完整的超集：契约必须覆盖词汇写的每一个成员

`GLOSSARY.md` 的「行词汇」写着「载荷带哪些字段」由它说了算。**线上形状没人说了算。**

会话日志的行是 `serde_json::Value`——路由写的是裸值，契约靠一份手写的
`SessionTurnEnvelope`（`crates/uva-app-server/src/api/v1/session_view.rs`）去**描述**
它。而那份 envelope 只列了 **9 个成员**：`type` / `data` / `ts` / `seq` / `source` /
`is_error` / `call_id` / `attachments` / `surface_op`。

词汇的 33 个变体一共能写 **51 个成员**（`crates/uva-session/src/events/mod.rs`）。
差的那 42 个不是可有可无的：`kind` / `turn` / `step` / `tool_call_id` /
`child_session_id` / `depth` / `status` / `risk` / `arguments_hash` / `token` /
`outcome` / `id` / `by` / `scope` / `workspace_hash` / `usage` / `park_id` …全都在
线上——**它们本来就以类型化字段写在行顶层**。

于是浏览器只能去读 payload 包里的**第二份拼写**：`data.kind`（`uva-web/src/lib/
session-to-messages.ts:1189`）、`data.step`（`lib/chat-window.ts:72`）、
`data.child_session_id` / `data.agent_name` / `data.depth` / `data.status`
（`session-to-messages.ts:967-991`）。这些键在 `data` 袋里存在，只因为**契约没写
顶层那份**——同一个事实的第二种拼法（ADR-0021 的原话：「类型化字段是读者的事实、
`data` 袋只是同一条事实的第二份拼写」）。

漂移已经发生了两处，而且方向相反：

- `tool_result` 行的顶层事实叫 `tool_call_id`（`publish.rs` 在旧方言路径上把它提到
  顶层，`events/mod.rs:319` 把它写成类型化字段）——envelope 里**没有**这个成员。
- `call_id` 在 envelope 里挂在**每个**分支上，可它只有 6 个变体带；而它的文档写着
  「Set on a `tool_output` row」——连作用域都说错了。

`check-web-contract` 拦不住：它 diff 的是「生成的 TS」与「提交的 TS」，两者都来自
**同一份手写镜像**。镜像与词汇的分歧，它结构上看不见。

## 决定

- **envelope 是 published row 的完整超集**：词汇能写的每一个成员都列上，全
  optional；行带哪些由它的 tag 决定。`ignorable` 也列上——它是 writer 盖的标记
  （`writer/mod.rs:699`），不是变体字段，却是真实的线上成员。
- **成员清单由词汇派生，不手写第二份**：给 `SessionEvent` / `AttachmentRefRow` /
  `UsageBuckets` 派生 `JsonSchema`（`uva-session` **早就**依赖 `schemars`——为
  `SurfaceOp`），再由一条契约测试断言 envelope 的成员集**覆盖**派生 schema 里
  全部变体的字段名。没有第二份手写清单要维持，只有一处比较。
- **浏览器读顶层类型化成员**：读行上的 `kind` / `step` / `child_session_id` /
  `agent_name` / `depth` / `status`，payload 里的同名键只作**旧行兜底**——这些字段
  是后加的，早于它们的行只在 payload 袋里带着一份。
- **`publish` 不归一旧行**：published row 仍是原始行（GLOSSARY 的「发布」：序号与
  时间戳连同缺席一起原样保留）。旧行的类型化成员缺失，由读侧的兜底链承担，不由
  发布边界补写。
- **`call_id` 的文档改成它真实的范围**（6 个变体），`tool_call_id` 单列。

## 参考

- [ADR-0006](0006-row-vocabulary-owns-the-row.md)：行词汇住在 `uva-session`；标签集
  是 schema `oneOf` 与前端 union 的唯一来源。
- [ADR-0009](0009-row-vocabulary-owns-the-payload.md)：`SessionTurn` 的分支从词汇
  的标签联合读出来；载荷的读法归词汇；`data` 只窄化两个消息行。
- [ADR-0021](0021-row-payload-shape-in-one-owner.md)：类型化字段与 payload 包是同一
  对，由构造器一次写下；`normalize` 只为旧日志补空缺。
- [ADR-0007](0007-row-vocabulary-restores-at-the-read-boundary.md)：归一在读边界，
  读者只见当前行。

## Considered Options

- **直接发布 `SessionEvent` 派生的 schema**：被否。派生的联合是 **per-variant** 的，
  而 published row 是 **per-tag** 的；更要命的是派生 schema 把 `seq` / `ts` 标成
  required（旧方言发布的行可能两者都没有），并且 `data` 在 33 个分支上全是
  `unknown`——丢掉契约特意做的 `data?: Message` 收窄（浏览器正靠它
  `messageText(entry.data)`）。
- **envelope 变成 per-tag**：成员清单要么手写 33 份（比现在更糟），要么从词汇的
  schema 逐变体生成。后者要求「旧方言发布的形状」是某个变体的子集——今天成立，但
  没人守着；而且它把浏览器的读法从「一个扁平成员集」变成「先按 tag 收窄」，投影
  模块要重写一遍。生成的 TS 也会小一个数量级（每分支只列自己的成员）。**值得在
  浏览器那侧的按 tag 收窄成为真需求时重开**——本刀不取。
- **由 `publish` 归一旧行**：published row 于是永远带顶层类型化成员，浏览器只读
  一处。被否（本刀）：它给「发布」加了一件新活，并改旧行的线上字节。GLOSSARY 允许
  「在词汇说它不对的地方改」，但那该是独立一刀——先把契约补齐，让浏览器看得见
  类型化的那份，再看兜底链还需不需要。
- **手写 per-tag 成员清单**：被否——那正是会漂移的第二份清单。

## Consequences

- **契约终于看得见词汇**：一个变体长出新字段，契约测试变红；envelope 补上成员，
  `make generate-web-types` 重新生成 TS，浏览器按类型化成员读。
- **生成的 TS 变大**（2570 → 8637 行）：envelope 的成员出现在**每个**分支上（33 个
  分支 × 52 个成员）。文件是生成的、有字节门禁，但人读不动——这是本刀付的代价，与
  「per-tag」那条路正好相反。
- **浏览器读法变了**：顶层类型化成员优先，payload 兜底给旧行。`data.reason` 与
  `data.phase` **没有**顶层对应（词汇没有这两个字段），它们继续只从 payload 读。
- **一个真缺陷被修掉**：`Plugin` 变体**没有 `data` 字段**（只有 `kind` / `payload` /
  `original_bytes`），而浏览器读的是 `entry.data`——于是**每一个插件事件都渲染成
  「插件事件 \*\*unnamed\*\*」且没有正文**。现在读行自己的字段。
- **冻结面不变**：`SessionEvent` / `AttachmentRefRow` / `UsageBuckets` 都已是
  `uva-session` 的 crate 根名字，`scripts/frozen-api/baseline` 与
  `docs/api-frozen.md` 都不用动；派生 `JsonSchema` 不新增根名字，
  `check-pub-surface-strict` 的 `pub use` 行计数（18）也不动。
- **依赖不变**：`uva-session` 早就依赖 `schemars`（为 `SurfaceOp`），本次只是多三处
  derive。
- **门禁**：`check-web-contract` 重新生成并 diff `uva-web/src/api/types.ts`；
  `check-row-vocabulary` 的三条探测器（不拼 tag 字面量、不在构造器模块外构造
  `SessionEvent::X {`、不改写 `data[...]`）不受影响——本次没有拼任何 tag 字面量。
