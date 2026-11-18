# 行词汇归 `uva-session` 所有，读路径经它读

ADR-0002 把会话日志的**写**路径收成一条类型化的接缝（`Arc<dyn SessionLog>` + 类型化 `SessionEvent` 构造器），但**读**路径与词汇本身仍是调用方的知识。33 个标签在 `crates/uva-session/src/events/mod.rs` 一个文件里重述 8 遍（`:1446` `seq()`、`:1490` `surface_role()`、`:1550` `surface_parts()`、`:1663-1697` `from_value` 的 `matches!`、`:1707-1743` `type_tag()`、`:1847` `KNOWN_SESSION_EVENT_TYPES`、`:1892` `empty_event_of()`、`:1172-1289` `normalize()`），另加 `surface/fold.rs:44` 与 `repair/mod.rs:529` 两份穷尽匹配；其中只有 5 处受编译器保护，5 处标签字符串表不受任何编译器保护，角色还额外写在散文里、由 `log_surface/tests.rs:283` 的 `include_str!` 与 `:609-640` 的散文解析守住。

镜像散在四个 crate 与两种语言里：app-server 手写了一份 33 变体枚举（`api/v1/session_view.rs:54`），靠 `api/contract.rs:741` 的集合相等测试钉住；TUI 两张手写表（`app.rs:1948`/`:1964`）；tool-task 的 `control/artifacts.rs:137` 手写 9 个 tag 的 `match` 且**没有任何门禁**——新增一个行种在工件集里静默消失；Web 的 `session-to-messages.ts:1152` 已是生成的 union 上的 `Record`（编译器强制），但旧方言识别仍是手写的。`KNOWN_SESSION_EVENT_TYPES` 与 `empty_event_of` **零生产调用方**，且前者的文档声称「app-server 的 union 派生自这份表」与「持久化层会拒绝未知类型的行」——两句都不成立。更糟的是**方言集已经跨接缝分歧**：Rust 的 `log_surface.rs:107-121` 认 `Message`/`UserInput`/`Model`，Web 的 `session-to-messages.ts:126/131/140` 认 `UserInput`/`Model`/`Agent`——两边各缺一个对方才认的方言（记为 `.scratch/architecture-deepening/issues/32`）。

我们把「一行是什么」收成 `uva-session` 内的**行词汇**模块。接口形状经四路并行设计（design-it-twice：最小接口 / 最大弹性 / 最常见调用方 / 跨语言接缝）比较后定案为混合：

- **`RowTag`**：封闭枚举，**变体名即线上标签**——`LifecycleShutdown` 改名 `SessionShutdown` 之后，33 个显式 `#[serde(rename)]` 全部删除，`rename_all = "snake_case"` 派生一切。`RowTag` 派生 `JsonSchema`，是 schema 的 `oneOf` 与前端 TS union 的唯一来源；app-server 手写的 33 变体 `SessionEventType` 随之删除。
- **`RowDialect`**：第二个封闭枚举（`UserInput` / `Model` / `Message` / `Agent`），同样发布进 schema；`SessionTurn.type` 的生成类型是一个具名别名 `SessionRowTag`（Rust 侧是 schema 视图 `RowTagOrDialect`：`Tag(RowTag) | Dialect(RowDialect)`，TS 侧展开成 `SessionEventType | SessionRowDialect`）——方言集从此只有一份答案，Web 被迫对 legacy `Message` 行表态（今天它静默丢弃一个 Rust 会折叠的行）。
- **`RowClass`**：`Typed(RowTag) | Legacy(RowDialect) | Unknown(&str) | Untyped` 四分；「当前词汇之外的新标签」不再被 app-server 归进 `legacy`（`routes/sessions/mod.rs:872`）。**解码失败另立 `broken()`**——已知标签但载荷解析不了是缺陷，不是方言；今天 `from_value` 把两者混在一个 `None` 里。
- **`carries(tag)`**：「同一种逻辑事件、新旧两种方言」的对应关系（`user_message ↔ UserInput`）由模块拥有，不再在每个读点各写一遍双标签匹配。
- **`Rows<'a>`**：借用视图，`of(tag)` **先比标签再解码**——今天 `from_value` 为读一个标签要 `clone` 整行再反序列化。
- **`surface_role` 与行种挪到 `RowTag` 上一处声明**，散文角色解析随之删除。
- **对应关系靠两个穷尽 `match`**（`SessionEvent::tag()` 穷尽于 `SessionEvent`，`RowTag` 侧穷尽于 `RowTag`）加一条平局测试钉住；每变体的代码（`seq()` / `surface_parts()` / `normalize()`）保持穷尽 `match` 不动——编译器本来就守着它们。
- **消费者策略 = 各消费者自己的穷尽记账**（TUI 与 tool-task 各一张穷尽 `match`；Web 是两张 `Record`——`TYPED_ROW_TREATMENT` 覆盖 `SessionEventType`、`DIALECT_ROW_TREATMENT` 覆盖 `SessionRowDialect`——加一个按方言表键做的运行时分区判定 `isDialectTag`），不是数据表——`rustc` / `tsc` 的全同性是比自觉的会计测试更强的门禁，且 GLOSSARY 明文要求「新增一种行而不声明它的角色，代码不编译」。
- 无宏、无 codegen、无新依赖。

## Considered Options

- **「每变体一行」的声明经 `macro_rules!` 展开出枚举、封闭标签类型与角色/行种匹配**（design-it-twice 之前的初案）：`macro_rules!` 生成不了 `SessionEvent` 的枚举本体——33 个变体各带自己的字段、serde 属性与文档散文；生成不了枚举就消灭不了名字重复，宏只买到角色/行种两个匹配，而那两个匹配编译器本来就守着。四路并行设计独立走到同一结论。**被否**。
- **消费者策略做成数据表**（`Treatment<T>`：词汇增长不破消费者构建，`unaccounted()` 当会计钩子）：会计测试是自觉的，不是强制的；穷尽 `match` 的全同性由编译器保证。**被否**。
- **运行时单一 `match` 返回一份事实结构，其余访问器问它**：收得比两个方向各一个穷尽 `match` 少，且无法表达 `empty_event_of()` 的每变体构造。**被否**。
- **加 build.rs / codegen，从一份声明生成所有表**：多一条构建期管线，且生成的枚举会绕开 `SessionEvent` 本身，等于出现第二个「行是什么」的定义。仓库现在零宏、零 build.rs、零 codegen。**被否**。
- **把行词汇挪进新 crate**：为一个叶子 crate 已经拥有的东西再造一个 crate，会让 app-server、tool-task、tui、web 全部多一条依赖边，违反 `AGENTS.md` §3.7 的依赖单向与公开面最小化。**被否**。

## Consequences

- **本 ADR 的「不再在每个读点各写一遍双标签匹配」没有兑现**：`carries` 只被两个读点用上，其余读点仍在自己写两分支 match，`routes/follow.rs:953` 还漏掉了方言分支——旧方言日志里的助手行拿不到回合标签。根因是 `RowDialect` 是**运行期类别**：每个读点都要自己判断这一行是当前词汇写的还是旧方言写的。**[ADR-0007](0007-row-vocabulary-restores-at-the-read-boundary.md)** 把归一移到读边界，读者不再见到方言，该承诺在那里兑现；本 ADR 的决策（行词汇住在 `uva-session`、「一行是什么」由它说了算）不变。
- **冻结面变动**：`KNOWN_SESSION_EVENT_TYPES`（`scripts/frozen-api/baseline:242`）与 `empty_event_of`（`:302`）删除（两者零生产调用方）；`SessionEvent::type_tag()` 由 `SessionEvent::tag().as_wire()` 取代；`SessionEvent::LifecycleShutdown` 改名 `SessionShutdown`；新增 `RowTag` / `RowDialect` / `RowClass` / `classify_row` / `Rows` / `carries` / `broken`。按 `AGENTS.md` §3.10 同步 `docs/api-frozen.md` 与基线。
- **删除而非留兼容层**：`contract.rs:741` 的漂移测试、`log_surface/tests.rs:283` 的源码文本解析与 `:609-640` 的散文角色解析、手写的 `session_view.rs:54` 枚举、app-server 的 7 处生产裸 tag 读点（`follow.rs:870/952/1618`、`tasks.rs:345`、`run_task.rs:57/102`、`sessions.rs:810`）、`subagent_router.rs:359` 的裸写、`artifacts.rs:137` 的手写 `match`、`LogWriter` 的仓外裸 `Value` 写入口，一并删掉。切面之间只有依赖顺序，没有共享的半成品状态。
- **切面一（Rust 侧）先做，切面二（跨语言）依赖它**：切面二已随 ADR-0007 落地，且比这里的初估**更小也更彻底**——服务端经 `publish_row` 只发布当前行后，Web 侧的手写方言识别不是「改为读生成的 union」，而是**整表退场**（`DIALECT_ROW_TREATMENT`、四个 `applyLegacy*`、`isDialectTag`、`SessionRowDialect` / `SessionRowTag` 联合全删）；TUI 的两张手写表是**转录取舍**（每个读模型自己决定收哪些行），换成一张穷尽 `match` 而非生成物。
- **新门禁**：`check-row-vocabulary` 断言生产代码不得自己认 tag 字符串，按 `AGENTS.md` §3.6 同步四处清单。
- **本 ADR 不裁定的**：前端转录的折叠语义（Web 按 `min..max` 区间影子、Rust 按被引用的行逐位对账）——那是另一件事，不在本次收拢范围内。
