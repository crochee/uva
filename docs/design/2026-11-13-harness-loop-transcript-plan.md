# 刀 D · harness 的消息记录与循环阶段顺序 —— Implementation Plan

> 决策见第一至四轮盘问记录（无 ADR：这两处的形状读代码就能看出来）。
> 顺序：**先 #7 消息记录，再 #6 步骤顺序**。

## Global Constraints

- **形状预算**：`scripts/harness-shape/clippy.toml` —— `too-many-lines-threshold = 100`、`excessive-nesting-threshold = 4`。`scripts/check-harness-shape.sh` 只跑 `--lib`（`#[cfg(test)]` 模块与 `crates/uva-harness/tests/**` 不在其中），位置过滤 `crates/uva-harness/src/`——**新模块同样受限**。
- **harness 与插件分离**：AGENTS.md §3.7 —— harness 只留核心循环，委派类工具住在 `uva-tool-plugins::task`。本切面**不把插件实现搬进来**。
- **`ReasoningStrategy` 是唯一的外部接缝**（`agent/re_act/mod.rs`:35）。步骤模块是**内部**接缝：它的内部拆法不进公开面。
- **助手轮次只能构建一次**：`sink.rs`:322` 的 `write_assistant_turn` 用 `Message::new(Role::Assistant, Content::Multi(parts.to_vec()))`，`loop/sample.rs`:425` 的 `commit_assistant` 用结构体字面量 `Content::parts(parts)`——两条构造路径不同，**同一批 parts 序列化出的 wire 形状可能不一样**，而没有任何断言比对过它们。本切面把两处收成一次构建。
- **不变量要能被自己的那层测**：`state.rs`:222-268` 现在的两个单元测试只断言字段是空的；不变量是隔着日志间接验证的。

## Review Focus

1. 不变量是不是**进了类型**——「按下标并行」与「不交错」应当由类型保证，而不是由约定保证。
2. 阶段顺序是不是只有**一处**——七步脚本与持久化边界应当在同一个模块里。
3. 有没有顺手改变**行为**——本切面是重构，行为必须逐条不变。

---

### Task 1: `Transcript` 类型

新文件 `crates/uva-harness/src/agent/re_act/transcript.rs`。它接管 `RunState` 里其实是同一个对象的四个字段（`state.rs`:69`、`:86`、`:100`、`:103`）：

|字段|语义|
|---|---|
|`messages: Transcript`|本次运行的消息流|
|`surface_seqs: Vec<u64>`|每条消息对应的持久化序号（`0` = 自己没有行）——**与 `messages` 下标并行**|
|`pending_tool_calls: HashSet<String>`|未闭合的工具调用窗口|
|`deferred_user: Vec<Message>`|被窗口挡住的用户轮次|

接口（五个方法）：

|方法|语义|
|---|---|
|`append_message(msg, seq)`|追加一条消息**并带上它的来源**——两者不再可能不同步|
|`open_call(id)` / `answer_call(id)`|开/关一个工具调用|
|`release_deferred()`|窗口关闭后释放被挡住的用户轮次|
|`splice_compaction(range, replacement)`|压缩时替换一段|

`Transcript` 这个名字已经被 `state.rs`:13` 的**类型别名** `Arc<Vec<Message>>` 占用（它同时是 `sample_once_or_fail` 的请求类型，`loop/mod.rs`:568`）。新结构体**替换**那个别名，别名连同它的 4 处使用点一起改。

**验证**：
- `cargo test -p uva-harness`
- 新测试直接断言不变量：`messages.len() == surface_seqs.len()` 永远成立；窗口打开时 `release_deferred` 之前 `deferred_user` 不被释放。
- **删除旧的间接验证之前**，先确认新测试能独立证住同一件事。

---

### Task 2: 助手轮次只构建一次

`EventSink::write_assistant_turn`（`sink.rs`:322`）与 `commit_assistant`（`loop/sample.rs`:425`）合成一处：助手 `Message` 由 `Transcript` 构建一次，日志行与内存里的消息**用同一个值**。

**验证**：
- 新测试断言「写入日志的那一行」与「内存里那条消息」序列化后**逐字节相同**——这是今天缺的断言。
- `cargo test -p uva-harness`

---

### Task 3: 步骤模块

`crates/uva-harness/src/agent/re_act/loop/mod.rs` 现有 **16 个**自由函数（9 私有 + 6 `pub(super)` + 1 `pub(crate)`），每个**只有一个调用点**——`iteration_body`（`:440`）是七步编号脚本，而它们必须共同守住的那几个持久化边界写在三个文件里：`step_end` 在 `mod.rs`:429 与 `suspend.rs`:39 两处；`turn_open` 声明在 `mod.rs`:69、写在 `mod.rs`:239 / `resume.rs`:113 / `suspend.rs`:47、读在 `finalize.rs`:32。

收成一个内部模块，接口是「跑一次迭代，给出结果」。`prepare` / `sample` / `tools` / `suspend` 变成它的实现，不再是必须互相达成一致的邻居。

**验证**：
- `cargo test -p uva-harness`
- `make check-harness-shape` 绿（新模块同样受 100 行 / 4 层的预算约束）
- 现有的 `tests/lifecycle/step_pairing.rs` 与 `one_row_per_turn.rs` 仍然绿——它们从**日志**侧断言那两个不变量，与重构前一致。

---

### Task 4: 验收

1. `cargo test -p uva-harness` 全绿。
2. `make check-harness-shape` / `make check-harness-docs` 绿。
3. `make lint-rust` / `make fmt-check` 绿。
4. `cargo run --example runtime_agnostic -p uva-harness` 绿（§3.9 的运行时中立证明）。
5. 冒烟：一次真实运行（`make examples` 里带模型的那种）产出的日志与重构前**逐行相同**——本切面不改行格式。
