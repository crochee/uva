# 行载荷形状归一个主人：载荷包与类型化字段由构造保证一致

[ADR-0015](0015-row-construction-in-one-module.md) 把「行构造归一个模块」立成裁定：
`uva-session` 之外无人拼 `SessionEvent` 变体字面量。它收的是行的**外壳**——
哪个变体、哪些字段、`seq: 0` / `ts: ""` 的未盖章约定。**载荷那一半从未被裁定**：

一行有两个视图。**类型化字段**是枚举变体里的具名字段（`turn` / `step` / `is_error` /
`tool_call_id` …），**载荷包**是变体里那个自由 JSON 包（`data`）。读侧靠两视图一致才能
还原一行——`normalize`（`crates/uva-session/src/events/mod.rs`）在读边界把两者**接上**：
类型化字段为空时，从载荷包里取。

改造前的树上有四处**载荷的第二主人**：

- **修理器**（`crates/uva-session/src/repair/mod.rs`）为补齐崩溃留下的尾巴，手拼了
  `ToolResult` / `Step{end}` / `Turn{end}` 三个变体字面量，把载荷包**又抄了一遍**——
  抄漏了一个键：正常写出的步结束行带 `action`，补齐出来的不带。
- **压缩器**（`crates/uva-session/src/compaction/mod.rs`）手拼四个压缩相关行，其中压缩行
  的**六键载荷包**（`summary` / `source_indices` / `shadowed_token_count` / `provider` /
  `model` / `usage`）只存在于那一个调用点。
- **harness**（`crates/uva-harness/src/agent/re_act/loop/step/mod.rs`）在行归属 crate
  **之外**直接改一行载荷：取出行，往 `data["message"]` 里塞失败原因。
- **表面投影**（`crates/uva-session/src/log_surface.rs`）把日志行投影成折叠可读的行——
  它是行的**生产者**，不是读模型——却在 match 臂体里手拼 `UserMessage` /
  `AssistantMessage` / `ToolResult` / `Compaction` 四个变体，载荷包与类型化字段都在臂体里
  现写。这一处长期藏在构造探测器的盲区里：旧的行内判别「开行带 `=>` 就放行」把 match
  臂体里的构造也当读侧放过了（见「决定」里收尾行判别那一条）。

更值得警惕的是：**不一致会被静默修好**。`normalize` 只补**空缺**的类型化字段，所以一个
少写了类型化字段的构造器不会失败——读侧替它补上，测试照样绿。既有那条
「构造器写出的行两视图一致」的断言只覆盖 `turn` 一个标签，且断言读的是**补齐之后**的值，
因此它发现不了「构造器少写了一个键」。承诺与现实的落差就在这里。

## 决定

- **主人是已有的构造器模块**（`uva-session::typed_event_builders`）。ADR-0015 已经把它立成
  「行构造归一个模块」；本次补齐它的另一半：每个构造器拥有该标签的**载荷包 + 类型化字段**
  这一对，两者**由构造保证一致**。一个标签一个构造器，不做通用入口。
- **修理器与压缩器改问构造器**：它们不再手拼变体字面量。修理器今天抄漏的 `action` 键
  随之消失——构造器写什么，修理器就写什么。压缩四行各得一个按标签的构造器
  （`compaction_start` / `compaction_summary` / `compaction_row` /
  `compaction_end_committed` / `compaction_end_failed`），六键载荷包从「只存在于一个调用点」
  变成构造器的一部分。插件行的构造器也从枚举上搬进词汇（`plugin_row`）。
- **harness 不再改载荷**：它要写的是「带失败原因的助手轮结束行」，于是
  `turn_end` 接受一个可选的原因（`Option<&str>`），构造器一次写出载荷包与类型化字段。
- **`normalize` 的职责收窄为读侧**：它继续为**旧日志**补齐缺失的类型化字段，且**只补空缺、
  从不覆盖已有值**；写侧不再制造需要被补齐的行。构造器的一致性由构造保证，不靠读侧修复。
- **不变量进门禁与性质测试**：
  - `check-row-vocabulary` 的**构造探测器**作用域从「`uva-session` 之外」扩到**整个工作区**
    （豁免两个所有者文件：构造器模块与标签↔事件对应表 `events/vocab/mod.rs`）；臂的判别
    以**收尾行**为准——字面量闭合那一行带 `=>` 才是 match 臂（读侧），开行上的 `=>` 属于
    臂体，遮不住一次构造。
    并新增第三张探测器：**除构造器模块之外**，生产代码不得改写一行的载荷
    （`data[...] =` 形状）——`uva-session` 内部也在扫描范围内，漂移恰恰长在 crate 内部。
  - 逐标签性质测试钉住「构造器写出的那一行」的键集与两视图一致——断言的对象是**写出的行**，
    不是读回来的行，所以写歪当场失败。
- **表面投影也改问构造器**：`log_surface` 投影出来的四行由构造器模块的四个 crate 私有
  构造器（`projected_user_message` / `projected_assistant_message` / `projected_tool_result` /
  `projected_compaction`）产出——投影是行的生产者，形状不该在它的 match 臂体里现写。
  它们不进公开面：投影行不是 writer 追加的行，冻结面没有它们的位置。
- **自由格式的行不动**：警告行、插件行、hook 行按 ADR-0009 保留自由形状，没有类型化字段
  可言。它们在性质测试里被**明确排除并写明理由**，不是被漏掉。
- **不改变线上字节**：构造器写出的载荷包与今天一致；唯一一处差异是**崩溃补齐出来的步结束行
  多一个 `action` 键**（见 Consequences）。

## 参考

- [ADR-0015](0015-row-construction-in-one-module.md)：行构造归一个模块。本 ADR 是它的
  对偶补齐——那条收了外壳，本条收载荷。
- [ADR-0006](0006-row-vocabulary-owns-the-row.md)：行标签的唯一住处。
- [ADR-0009](0009-row-vocabulary-owns-the-payload.md)：自由格式载荷的裁定（警告行、插件行、hook 行）。
- [ADR-0018](0018-folded-payload-reads-live-in-one-place.md)：折叠载荷的读法归一处（读侧）。

## Considered Options

- **一个通用 `row(tag, payload)` 入口**：被否——它把载荷形状推回调用方，与 ADR-0006/0015
  的方向相反。
- **把 `normalize` 改成构造期校验**（写行时校验两视图一致，不一致就报错）：被否——写侧要为
  旧日志的宽容付出代价，而读侧对旧日志的宽容正是老会话仍能读的前提；两侧的职责不能互换。
- **门禁只查 `uva-session` 之外**：被否——漂移恰恰长在 crate 内部（修理器、压缩器），
  只查外部等于把第二主人留在门里。
- **用 schema 生成载荷包**：被否——多数构造器的载荷是「类型化字段的原样镜像」，不是独立的
  schema；生成器只是把同一件事写两遍。

## Consequences

- **行为变更两条**：
  1. 崩溃补齐出来的**步结束行**多一个 `action` 键（值 `interrupted`），与正常写出的
     步结束行同载荷。既有日志不受影响（新写的才有），旧日志照旧可读。
  2. `interrupted_turn_closers`（公开 API）返回的 closer 不再带最后一条事件的
     时间戳——`ts` 为空串、`seq` 为 0；落盘时 writer 本来就重盖 `ts` 与 `seq`，
     落盘字节不受影响，但直接读这个函数返回值的调用方看到的 `ts` 变了。
  轮结束行、工具结果行与其余构造器写出的行：线上字节不变。
- **公开面增长**：`uva-session` 的构造器新增 `compaction_start` / `compaction_summary` /
  `compaction_row` / `compaction_end_committed` / `compaction_end_failed` / `plugin_row`
  六个名字（`tool_result_interrupted` 是 crate 私有的），另有 `STEP_INTERRUPTED`——
  补齐步结束行写进载荷的 `action` 值——随后补进 crate 根，仓外读者按根级名字匹配它；
  `scripts/frozen-api/baseline` 与 `docs/api-frozen.md` 已同步。`turn_end` 的签名变了
  （多一个可选参数），`SessionEvent::plugin` 已移除（插件行改由 `plugin_row` 构造，
  同签名同语义），项目未到 1.0，破坏性变更直接切干净，不留兼容层。
- **门禁变严**：构造探测器现在覆盖整个工作区，载荷探测器扫描除构造器模块外的全部生产文件
  （`uva-session` 内部也在内），审计例外表从 27 条缩到 19 条（只许变短）。
- **AGENTS.md §3.6** 的 `check-row-vocabulary` 行同步了新作用域、三张探测器与两个所有者文件。
- **本 ADR 不裁定的**：行标签集本身的一次派生（九个穷尽 match 与字段清单克隆）——编译器已经
  让那九处穷尽，风险低而改动大，留作独立一步。
