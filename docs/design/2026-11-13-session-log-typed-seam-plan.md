# 刀 A · 会话日志的类型化写入路径 —— Implementation Plan

> 决策记录见 [`docs/adr/0002-typed-session-log-seam.md`](../adr/0002-typed-session-log-seam.md)。
> 本文档是切面 A 的执行计划；切面 B / C / D 各有一份。

## Global Constraints

- **冻结面**：`uva-session` 在 `scripts/frozen-api/baseline` 的名单里（127 个名字）。这道门禁只比对 crate 根 `pub use` 的**名字**，形状按 `docs/api-frozen.md`:79-82 走 pre-1.0 常规演进并记进 changelog。本切面**不新增** crate 根公开名字，也不删除任何一个。
- **serde 形状**：`SessionEvent` 是 `#[serde(tag = "type", rename_all = "snake_case")]` 的**内部标签**枚举（`crates/uva-session/src/events/mod.rs`:215）。变体字段与 `"type"` 平铺在同一张 map 里，所以**任何新字段都必须带 `#[serde(default, skip_serializing_if = ...)]`**，否则老日志反序列化失败、行被静默丢弃。仓里已有 102 处同样的先例（如 `UserMessage.source` `:237`、`ParkedPrompt.park_id` `:759`）。
- **五处硬编码的 32 标签清单**必须同步移动：`KNOWN_SESSION_EVENT_TYPES`（`:1676`）、`from_value` 的 `matches!`（`:1494`）、`type_tag()`（`:1537`）、`empty_event_of`（`:1720`）、`repair::event_ts`（`repair/mod.rs`:529）。**本切面只加字段、不加变体**，所以这五份清单不动。五份如今都被钉住：const 与枚举各变体在 `#[serde(rename)]` 里声明的线上标签做**集合相等**交叉检查（`log_surface/tests.rs` 的 `the_known_event_types_are_exactly_the_enum_s_wire_tags`，缺一个、多一个都红）；`type_tag()` / `empty_event_of` 被 `events/tests.rs` 的往返测试钉住（const 里每个标签都必须造得出骨架且标签往返一致）；`from_value` 的 `matches!` 被交叉检查的夹具行钉住（每个变体的夹具行都必须能经 `from_value` 解析）；`event_ts` 是无兜底分支的穷尽 match，编译器钉住。原先 `events/tests.rs` 手维护的「31」计数断言已删除——钉住词汇表的不再是数字。
- **单写者**：一个会话的日志只有一个写入者（`crates/uva-session/src/writer/mod.rs`:163 的 `LogWriter` 拥有序号来源）。`scripts/check-single-writer.sh` 守这条。
- **兼容**：老日志里的 `Message` / `UserInput` 两种方言必须继续**可读**。可读性与可写性从此是两件事——可写性收口到类型化接缝，可读性留给兼容层。
- **不破坏行格式**：新写入的行必须仍能被 `log_surface::classify_row`（`crates/uva-session/src/log_surface.rs`:107）折叠。老方言走兼容分支，新行走类型化分支。

## Review Focus

1. **新字段是不是「载荷的一个视图」而不是第二份真相**——读者应当优先读类型化字段，读不到时回退到 `data` 里的同名字符串键。老日志因此仍可读，新日志则不必再靠字符串键。
2. **回退分支有没有被钉住**——每一处「读类型化字段 → 回退 `data["..."]`」都需要一条测试喂老行形状。
3. **裸写入是不是真的清零**——生产代码里不该再出现 `json!({ "type": ... })` 的行。收尾时用 grep 证明。

---

### Task 1: 给 `SessionEvent` 的变体加类型化字段

哪些变体需要——跟着 20 处「把载荷的键当字符串查」的生产读取点走：

|变体|读取点读的键|新字段|
|---|---|---|
|`Turn`|`data["kind"]` / `["phase"]` / `["turn"]` / `["reason"]` / `["status"]` / `["message"]`|`kind: Option<String>`、`turn: Option<u64>`|
|`Step`|`data["kind"]` / `["turn"]` / `["step"]`|`kind: Option<String>`、`turn: Option<u64>`、`step: Option<u64>`|
|`ToolCall`|`data["call_id"]` / `["name"]`|`call_id: Option<String>`、`name: Option<String>`|
|`ToolResult`|`data["tool_call_id"]` / `["call_id"]`|`tool_call_id: Option<String>`|
|`RequestHeader`|`data["provider"]` / `["model"]`|`provider: Option<String>`、`model: Option<String>`|
|`SubagentEnter`|`data["child_session_id"]` / `["parent_session_id"]` / `["agent_name"]` / `["depth"]`|`child_session_id: Option<String>`、`parent_session_id: Option<String>`、`agent_name: Option<String>`、`depth: Option<u64>`|
|`SubagentExit`|`data["child_session_id"]` / `["parent_session_id"]` / `["status"]`|`child_session_id: Option<String>`、`parent_session_id: Option<String>`、`status: Option<String>`|

`AssistantMessage` / `UserMessage` / `AssistantChunk` 的载荷是**provider 的 Message**（`{"role":…,"content":[…]}`），它的形状属于 `uva-provider`，不属于会话日志的词汇——这三者的 `data` 保持 `Value`。

实现要点：

- 每个新字段：`#[serde(default, skip_serializing_if = "Option::is_none")]`（`Option<u64>` 同形）。
- `empty_event_of`（`:1209`）为这些变体补上新字段的 `None` 初值。
- `typed_event_builders.rs` 的对应构造器填上新字段（值与 `data` 里的键一致，两处同源，不允许手抖）。
- `from_value` 不需要改——`serde_json::from_value`（`:1025`）会把新字段一并解出来。

**验证**：
- `cargo test -p uva-session`（含 `events/tests.rs` 的标签往返测试与 `log_surface/tests.rs` 的 32 标签集合相等交叉检查）
- 新增测试：喂一行**只有 `data`、没有新字段**的老行，`from_value` 必须仍返回该变体，且新字段为 `None`。

---

### Task 2: 4 处裸写入迁到类型化构造器

生产代码里的裸 `json!` 行写入（`grep -rn '"type": "UserInput"\|"type": "Message"' crates/*/src` 应归零）：

|位置|现状|改成|
|---|---|---|
|`crates/uva-app-server/src/routes/messages/mod.rs`:637|`json!({ "type": "Message", "data": message })`|`uva::session::user_message(...)`（该构造器已存在，`typed_event_builders.rs`:414）|
|`crates/uva-app-server/src/session/subagent_router.rs`:142|`append_nowait(json!({ "type": "UserInput", "data": { "text": prompt } }))`|`uva::session::user_message(...)`|
|`crates/uva-app-server/src/session/subagent_router.rs`:246|同上（异步路径）|同上|
|`crates/uva-app-server/src/session/controller/run_task.rs`:441|`json!({ "type": "UserInput", "data": { "text": … } })` + 手工塞 `data.attachments` 与 `surface_op`|对应的带附件构造器（`UserMessage` 变体已有 `source` 与 `surface_op` 字段，`:201,:204`）|

注意 `subagent_router.rs` 的两处写的是**子代理的提示词**——那是 `UserInput` 语义，与 `run_task.rs` 的停放提示词同型。

**验证**：
- `cargo test -p uva-app-server`
- `grep -rn '"type": *"UserInput"\|"type": *"Message"' crates/*/src` → 只剩 `#[cfg(test)]` 里的命中。

---

### Task 3: 20 处字符串键读取点改读类型化字段

读取点分布（`crates/uva-app-server` 8 处、`crates/uva-harness` 4 处、`crates/uva-session` 8 处）——逐处把 `data["kind"]` 换成 `kind`，读不到再回退 `data.get("kind")`。

**回退是必须的**：老日志没有新字段。回退分支就是兼容层，不是遗留代码。

`crates/uva-app-server/src/routes/tasks.rs`:216-220 现在的 `unwrap_or_else` 降级（指针解析不到就悄悄不显示委派行）要改成**类型化字段优先**，并让「两者都拿不到」走一条显式的失败路径，而不是静默。

**验证**：
- `cargo test -p uva-session` / `cargo test -p uva-harness` / `cargo test -p uva-app-server`
- 新增测试：喂老行形状（只有 `data`）与新行形状（带类型化字段），两条都得到相同结果。

---

### Task 4: `SessionLog` 收窄到 7 个方法

现接口 8 个方法，其中 7 个是纯转发（`crates/uva-session/src/writer/mod.rs`:751-803），只有 `repair`（`:789`）有实质行为。生产在用的 4 个调用点**全都要整份日志**（`prepare.rs`:337、`inner.rs`:414、`run_task.rs`:36、`follow.rs`:1414），没有一处要部分读。

目标接口：`id` / `last_seq` / `read` / `append_event` / `append_event_nowait` / `flush` / `repair`。

**验证**：`cargo test -p uva-session`；`cargo test -p uva-harness`（harness 握的就是 `Arc<dyn SessionLog>`）。

---

### Task 5: `SessionSink` 收窄读面

- `read_from`（`sink.rs`:176`）从**必实现**改为**默认实现**（委托 `read` 再过滤）。可行性核查已证明零生产行为改变：`JsonlSessionSink::read_from`（`jsonl/mod.rs`:718）本来就 `fs.read_file` 读整个文件，I/O 逐字节相同；`InMemorySessionSink::read_from`（`in_memory.rs`:139）只是多一次克隆。
- `read_from_index`（`sink.rs`:266`）**删除**——全仓零重写、零调用。
- 14 个手写 `impl SessionSink` 测试替身改写（少实现两个方法）。

**验证**：`cargo test -p uva-session` / `cargo test -p uva-harness` / `cargo test -p uva-app-server`。

---

### Task 6: 控制器搬到 `Arc<dyn SessionLog>`

`SessionController::spawn`（`crates/uva-app-server/src/session/controller/mod.rs`:125）现在收 7 个位置参数，其中 `session_store: Arc<dyn SessionSink>` 与 `writer: Arc<LogWriter>` 是**两个**——类型系统无从校验它们配的是同一个会话。

改成只收 `Arc<dyn SessionLog>`：`SessionSink` 不再出现在控制器里（它由 `LogWriter` 内部持有）。参数 7 → 6。控制器内部那两处「写用 writer、关用 session_store」（`inner.rs`:299` 与 `:368`）改走同一个句柄。

17 处测试调用点（`session/controller/tests/**` 与 `routes/tests/follow.rs`）跟着改。

**验证**：`cargo test -p uva-app-server`；配错 sink/writer 的组合从此不可表达——用「传不存在的类型」验证编译期拒绝。

---

### Task 7: 冻结面基线与文档收口

- `bash scripts/check-frozen-api.sh --print > scripts/frozen-api/baseline`（本切面**不新增也不删除**任何 crate 根名字，基线内容应当不变——变了就说明 Task 1-6 越界了）。
- `docs/api-frozen.md` 的名字表格按实际重写（现状：表格写 452 个名字，基线实际 423；`uva-context` 表格写 43、实际 19，文档里还列着 `CompactionDetails`、`DagConfig`、`SUMMARY_TAG`、`ToolCallArchive` 这些已经不存在的名字）。
- `CHANGELOG.md` 记一笔形状演进。

**验证**：`make check-frozen-api` 绿；`make check-public-docs` 绿。

---

### Task 8: 端到端验收

1. `cargo test -p uva-session` / `-p uva-harness` / `-p uva-app-server` 全绿。
2. `make fmt-check` / `make lint-rust` / `make doc-check` 全绿。
3. `make check-single-writer` 绿（单写者没被破坏）。
4. `make check-frozen-api` 绿，且基线**逐行未变**。
5. **冒烟**：起 `uva-app-server core`，发一轮消息，读回会话日志——新写的行是类型化的（`"type": "user_message"`），`fold_log_surface` 折叠出的消息与改前一致（老方言的行也要能折）。
6. `grep -rn '"type": *"UserInput"\|"type": *"Message"' crates/*/src` → 只剩测试里的命中。
