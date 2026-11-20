# 工具参数词汇归一个主人

`task` 工具的参数词汇在这条线之前被写了不止一遍：schema 端一份手写的 JSON
Schema、解析端一份手写的键名与形状检查、拒绝文案散在解析代码里。三处各自
为政（下表是**改之前**的样子）：

| 事 | schema 端 | 解析端 |
|---|---|---|
| 键名 | 手写 `"agent"` / `"prompt"` / `"gate"` / `"isolation"` / `"command"` | 又手写一遍 |
| 形状 | 手写 `type` / `oneOf` | 又一份逐类型的分支 |
| 边界 | 没写（`minLength` / `minItems` 不在 schema 里） | 解析器在守 |
| `prompt` 缺失 | `required` 说它必填 | 静默变空串，照跑 |
| 没有读者的字段 | `trace_context` / `gate.timeout_secs` 被广告 | 解析进来然后丢掉 |

这不是 cosmetic。**模型读到的东西与工具执行的东西是两套事实**：schema 说
`prompt` 必填、解析器却收下没有它的调用；schema 广告一个墙钟预算、门却永远
不执行它。

本 ADR 裁定：`task` 的每个参数由**一份声明**拥有——名字、JSON 形状、边界、
含义、缺失时的处置、拒绝的文案——写在 [`args`](../../crates/uva-tool-task/src/args.rs)
模块的一张表里（`ARGUMENTS` 与它引用的 `Field`）。模型读到的 schema 与工具
的解析器是这张表的**两个投影**：谁都不再自己写一份，谁也不能独自漂移。

## 决定

### 1. 名字也归表管

线名（模型发送的键名）从声明取：`Field::name` 是唯一出处，schema 端渲染
`properties` 用它，解析端取键也用它（`read_named` 一族做
`payload.get(field.name)`）。解析器模块（`spec.rs`）不再写任何线名字面量。
`tests/argument_shapes.rs` 有两条断言钉住这一点：解析器读的键集合就是
schema 发布的键集合（对象成员 `gate.command` 也在册——带值的调用必须被
解析器读到），以及解析器源码里没有一张等于线名的字符串字面量。改名自此只
改表里一处，两个投影一起动；表改名而解析器不改的脱钩会当场红。

### 2. 模型可见的四个变更（随本线落地）

| 变更 | 之前 | 之后 |
|---|---|---|
| `prompt` 缺失 / 非字符串 | 静默变空串 | 被拒：`task tool requires a string \`prompt\` field` |
| `agent` | schema 未说长度 | schema 写明 `minLength: 1`（解析器早就拒空名） |
| `gate.command` 数组 | schema 未说项数 | schema 写明 `minItems: 1`（解析器早就拒空数组） |
| `gate` / `isolation` 的多形状 | schema 只说一种 | schema 写明 `oneOf`：命令对象或裸命令、布尔或对象，两者都含 `null` |

这些不是收紧解析器，而是把解析器既有的行为**如实说出来**（裸字符串与对象
形态本来就被收着）；`prompt` 那一条是唯一的解析端行为变更，见第 7 节。

### 3. 被拒即硬失败

`trace_context` 与 `gate.timeout_secs` 从声明里消失：它们被解析了整个存在
期，却没有任何读者。模型被告知过它们，所以照旧 schema 缓存的模型还会发送
它们——现在带它们的调用**整次委派失败**，拒绝文案说明理由（「`gate.timeout_secs`
is no longer an argument of the `task` tool: the gate never applied it, so a
wall-clock budget sent with a call was parsed and then dropped」），不再是
「解析进来然后静默无效」。其它从未声明过的键照旧被丢掉：一个模型可以发任何
东西，只有被告知过的字段消失才值得一次拒绝。

### 4. 显式 `null` 按「没带」处理

一个被删字段带上显式 `null`（`"trace_context": null`、
`"gate": {"command": [...], "timeout_secs": null}`）**不触发第 3 节的拒绝**：
`null` 没有信息量，旧解析遇到它也是返回「没有」。一次没有信息量的发送不该
把一次本来成功的调用变成硬失败——拒绝留给真正携带值的调用。实现上
`carries` 遇到 `null` 值不算「带了」。

### 5. `GateSpec::timeout` 删除；`GateVerdict::Timeout` 留下并如实标注

`GateSpec::timeout` 字段与 `with_timeout` 构造器一并删除：工具不再喂它之后，
生产代码**永远填不上**这个公开字段，`run_gate` 也不执行它——一个公开字段
不该是这种处境。这是又一次公开面收缩（见第 9 节）。

同族的 `GateVerdict::Timeout` **留下**：它有读者（`verdict_summary` 与
`apply_gate_to_output` 都读它），一个想给验证门记自己预算的宿主也用得上
它。处境在变体的文档里如实写明：`run_gate` 今天**不产出**这个结局——门没有
预算，跑到命令退出或父侧取消为止。

### 6. 故意的不对称：feature 关掉时 `isolation` 仍可解析

`isolation` 由 `worktree_isolation` feature 门控的是**schema 一侧**，不是解析
一侧：feature 关掉时模型连这个词都看不到，但带上 `isolation` 的调用仍被解析。
这是故意的——一个没有 worktree 能力的部署不该向模型广告这个词，而解析端
不为同一个词维护第二套规则。「没有洞、也没有秘密」这句话因此**有条件**：
`worktree_isolation` 开着时成立；关掉时 `isolation` 是「工具收、模型没听过」。

### 7. 裁定：`prompt` 缺失由「静默变空串」改成「被拒」是变严，且在允许范围内

解析端唯一的行为变更是 `prompt` 缺失与非字符串从「静默变空串」改成「被拒」。
这是**变严**——一条过去成功的调用形状现在失败。它落在 spec 允许的公开承诺
范围内：schema 从来就说 `prompt` 是必填字符串，解析器过去是不兑现那条承诺，
不是在承诺宽松。空字符串仍然被收下（`minLength` 没有加在 `prompt` 上）：
「必填」不蕴含「非空」，后者是另一条边界、另一句模型可见的话。

### 8. 公开面

- `TaskTraceContext` 类型与 `TaskSpec::trace_context` 字段删除；
- `task_tool_definition` / `task_tool_definition_with_features` 两个导出删除
  （crate 不再发布任何工具定义，挂载的工具是唯一载体）；
- `GateSpec::timeout` 字段与 `with_timeout` 构造器删除（第 5 节）；
- `pub use` 计数 7 → 6，基线（`scripts/paradigm-surface/baselines/crates/uva-tool-task`）
  已同步。该 crate 不在冻结面（`docs/api-frozen.md` 不含它），故冻结基线
  无需改动。

## 参考

- [`crates/uva-tool-task/README.md`](../../crates/uva-tool-task/README.md)：
  面向宿主与使用者的说明（参数声明归一个模块、没有读者的字段）。
- [ADR-0022](0022-one-module-owns-a-child-round.md)：同一条 `task` 线上的
  相邻决定——一轮子会话归一个模块。本 ADR 管的是**模型与工具之间的词汇**，
  0022 管的是**一轮子会话的流程**。

## Considered Options

- **`prompt` 两侧加 `minLength: 1`（连空串一起拒）**：被否。那是再一次
  「变严」，会再多一条模型可见的边界；「必填」与「非空」是两句话，前者是
  spec 早已许下的，后者不是。
- **被删字段连 `null` 一起拒**：被否。见第 4 节——`null` 没有信息量，拒绝
  它只是把一次成功的调用变成硬失败，没有换来任何事实。
- **`GateVerdict::Timeout` 一并删除**：被否。它有读者（两个公开函数读它，
  宿主可构造它），删它要连读者一起删；留下的代价只是文档里一句「今天
  不可达」的实话。
- **feature 关掉时 `isolation` 连解析一起关**：被否。那要求解析端为同一个词
  维护第二套规则（feature 开一套、关一套），而 schema 一侧已经把词藏掉了；
  见第 6 节。
- **解析端继续手写键名、靠测试对拍**：被否。测试只能发现已经发生的脱钩，
  声明取键让脱钩**无法发生**——这是本 ADR 的主线。

## Consequences

- **模型可见**：CHANGELOG「工具参数词汇归一个主人」一条逐项记录（四条形状
  事实、被拒即硬失败、`null` 的处置）。
- **公开面**：按第 8 节收缩；`uva-tool-task` 的 `pub use` 计数 6。
- **测试面**：`args` 模块的不变量测试逐字段（含对象成员）核对两侧；对齐
  判定（`aligned`）同时核对形状、边界与「required ⟺ 缺失即拒」；
  `tests/argument_shapes.rs` 从公开面钉住 schema、被接受的形状与拒绝文案，
  并钉住「解析器读的键集合 == 声明表的键集合」「解析器不写线名字面量」。
  这几条断言都做过「故意改坏 → 红 → 改回」的牙齿验证。
