# uva-tool-task

`task` 工具：把一次模型调用变成一次**子 run**。调用方在 `agent`
字段里点名一个已注册的同伴 agent，工具解析它、为它在自己的会话里开档、
内联驱动它跑完，再把子代理的最终文本作为工具结果返回。

与 `read` / `shell` 一样，它是一个普通 `Tool`：经
[`TaskToolPlugin`](src/plugin.rs) 注册进 `ToolRegistry`，定义/曝光/限制/
参数校验/并发模式全部走注册表既有机制。它不依赖 `uva-harness`——依赖集是
agent 词汇（[`uva-agent`](../uva-agent/README.md)）、事件词汇
（[`uva-events`](../uva-events/README.md)）与工具 trait
（[`uva-tool`](../uva-tool/README.md)）。

## 一次派发的边界

| 项 | 行为 |
|---|---|
| 子会话 | 派发前铸造 child id，经 `SubagentWriters` 开档：`subagent_enter(child,parent,depth,agent)` + 派发词；子 run 结束后追加 `subagent_exit` 并 flush |
| 父侧行 | 只有工具的 `tool_dispatch` / `tool_output` / `tool_result`；`tool_output` 是 L1 摘要的 durable 副本（重载由它重放），`tool_result` 是模型的副本并以 child id 收口；子会话是唯一全保真记录 |
| 深度 | 深度规则归 [`round::depth`](src/round/depth.rs) 模块：上限判定与深度记账只有一处，会话深度按会话 id 查表、子会话为 `+1`；`MAX_SUBAGENT_DEPTH = 3` 时**发起轮次的那一方**（`task` 的调用方、`agents_message` 的调用方）不能再起一轮，给出可读错误 |
| 并发 | 一个 `task` 调用起一个子代理。工具声明 `ExecutionMode::Sequential`，批次调度器因此把同一批里的 `task` 调用串行化——委派起的是整个 run，批次内并发会改动取消与批次语义，不只是时序 |
| 取消 | 子 run 拿自己的令牌（`ChildCancel`）：父 run 的令牌仍然透传，父取消即子终止；子自己的令牌由控制面的 `interrupt` 点火 |
| 校验 | `gate`（子 run 后跑验证命令）与 `isolation`（git worktree 隔离）沿用原 `task` 语义，由 `CommandRunner` seam 执行 |

## 参数声明归一个模块

`task` 的每个参数由一份声明拥有——名字、JSON 形状、边界、含义：一张表写在
[`args`](src/args.rs) 模块里，模型读到的 schema 与工具接受的参数都是这张表
的两个投影。

- **模型读到的**：[`args`](src/args.rs) 把表里「告知模型的那一侧」渲染成
  JSON Schema；有多种形状的字段渲染成 `oneOf`；`isolation` 仍由
  `worktree_isolation` feature 门控，feature 关掉时模型连这个词都看不到。
- **工具接受的**：[`spec`](src/spec.rs) 按表里「解析器收的那一侧」读参数，
  连键名也取自声明——解析器自己不写线名，改名只改一处；形状不合就按表里
  写的文案拒绝，边界也在同一处判定。

两侧并排放就是漂移的账本：模型被告知而工具不收的形状是契约上的洞，工具
收而模型没听过的形状是秘密。`worktree_isolation` 开着时，今天两边没有洞、
也没有秘密——工具收的每一种形状都写进了模型读到的 schema：验证门是**命令
对象或一条裸命令**，隔离是**布尔或一个对象**，两者都还收 `null`（＝「没有」）。
已发布的调用方不受影响：这些形状今天就被收着，schema 只是如实说出来。
feature 关掉时 `isolation` 仍可解析——工具收、模型没听过——这是**故意**留的
不对称：一个没有 worktree 能力的部署不该向模型广告这个词，而解析端不为
同一个词维护第二套规则（决策记录见
[`docs/adr/0023`](../../docs/adr/0023-one-owner-for-the-tool-arguments.md)）。

边界两边说的是同一件事：schema 写下的每条边界，解析器都兑现；解析器多守的
一条也不藏。今天没有例外。

- `agent` 非空：解析器早就拒空名，schema 现在写明（`minLength: 1`）。
- 命令数组至少一项：解析器早就拒空数组，schema 现在写明（`minItems: 1`）。
- `prompt` 是必填字符串：schema 一直这么说，解析器过去却把缺失与非字符串
  静默当成空串——现在改为拒绝，拒绝的话说得清是哪个字段不对。

`args` 模块自己的测试逐字段核对两侧的类型与边界一致（模型被告知的每个形状
解析器都收，解析器收的每个形状模型都被告知——对象成员也算字段，`gate.command`
在册）；[`tests/argument_shapes.rs`](tests/argument_shapes.rs) 从公开面钉住
今天的行为——schema 的形状、今天被接受的形状、拒绝的文案逐字不变（`prompt`
缺失这一条按新事实改成被拒），并断言解析器读的键集合就是声明表的 `name`
集合：解析器既不读表外的键，也不把表里的键写成第二份字面量。

## 没有读者的字段从声明里消失

两个字段解析之后**没有人读**：`trace_context`（模型在 schema 里看不见它，
以为带了上下文，实际没人读）与 `gate.timeout_secs`（模型被告知可以给一个
墙钟预算，实际那个预算被丢掉，门照跑不误）。两者都从声明里消失：模型读到
的 schema 里没有它们，解析器也不再读它们。声明里今天没有未兑现的边界了
——唯一一条没兑现的，跟着它自己那个字段一起走了。

「消失」不等于「静默丢弃」：其它没声明过的键今天仍然照旧被丢掉，但这两个
字段模型**被告知过**，所以传它们会得到一个说得出理由的拒绝：

- `trace_context` → 「`trace_context` is no longer an argument of the
  `task` tool: the tool never read it, so a trace context sent with a call
  was parsed and then dropped」
- `gate.timeout_secs` → 「`gate.timeout_secs` is no longer an argument of
  the `task` tool: the gate never applied it, so a wall-clock budget sent
  with a call was parsed and then dropped」

被删掉的字段连同它那句「为什么没有读者」的记录一起留在声明里（`REMOVED`），
所以这条拒绝文案与别的文案一样只有一处出处。

公开面随之收缩：`TaskTraceContext` 这个类型与 `TaskSpec::trace_context`
这个字段一起消失；`GateSpec::timeout` 字段与 `with_timeout` 构造器也删了——
生产代码从来填不上它，`run_gate` 也不执行它，一个公开字段不该是这种处境。
`GateVerdict::Timeout` 留着：`verdict_summary` 与 `apply_gate_to_output` 读
它，一个想给验证门记自己预算的宿主也用得上它——只是 `run_gate` 今天不产出
这个结局（门跑到退出或父侧取消为止）。「显式 `null`」按「没带」处理：传
`"trace_context": null` 或 `"gate": {"timeout_secs": null}` 的调用不会被拒，
与旧解析的行为一致。

## 工具描述归一处

工具自己的那句描述也只有一份，写在 [`args`](src/args.rs) 模块里
（`DESCRIPTION`）：模型读到的那句就是这一句，逐字不差。以前仓里导着一份
没人调用的工具定义，挂载给模型的却是另一个变体——两份描述，谁也不知道
哪份算数；现在发布出去的只有一份，而它就是仓里记着的那份。

- **发布的那一份**：挂在模型上的工具（[`TaskTool`](src/tool.rs)）带着这句
  描述进注册表，注册表交给模型的就是它。
- **没人调用的那份没了**：crate 不再导出任何工具定义，外部拿不到第二份
  描述来跟这一份分家。

[`tests/tool_description.rs`](tests/tool_description.rs) 从仓里自己的源码
读出记着的那句，与模型收到的那句逐字对比；同一份测试还钉住「这句话在
crate 里只写了一遍」与「crate 不发布自己的定义」。

## 一轮子会话归一个模块

一轮子会话——子 run 的一次——由 [`round`](src/round.rs) 模块拥有：装配子
run 的输入、挂取消、抽干同伴的输出、把结束原因映射成轮次结果、把尾巴
（名册结算 + `subagent_exit` 脚手 + 工件集）与用量记账落下来。委派路径喂它
一份输入，拿一个结果值（文本、结束原因、补丁、子会话 id）；模型看到的东西
基本没变——失败文案里句柄行前多了一个换行：委派路径原先自己拼那句，只用
一个 `\n`，现在两条路共用轮次模块拼的 `\n\n`。

隔离（worktree 生命周期）与验证门留在调用方：要不要隔离、要不要过门是那次
委派的决定，不是轮次的一部分——轮次只问它的隔离「在哪跑」「改了什么」。

深度规则也在这个模块里（[`round::depth`](src/round/depth.rs)）：**发起轮次
的那一方**（`task` 的调用方、`agents_message` 的调用方）坐在上限上时不能再
起一轮。委派路径问它「能不能派发、子会话跑在第几层」，唤醒路径问它「能不能
再起一轮」，深度表也只有这一处写：调用方不再自己数层数。

一轮的**结局**只有一套词：名册的活动行、L1 进度行的收尾摘要、子会话的
`subagent_exit` 脚手写的是同一个词（`completed` / `cancelled` / `error`），
由 [`RoundOutcome::as_str`](src/control/state.rs) 一处给出——三处各写一份
词表就是同一件事的三种说法。模型看到的失败文案说的是**原因**
（`max_iterations` / `suspended` / …），那是比结局细的一根轴，不换成结局词。

## L1 进度：父页工具块里的子代理摘要

子 run 运行时，`task` 覆写 `Tool::stream`，把子代理的当前状态写进父会话的
工具块——每行 `[<child_id>] <status>: <当前活动>`：

```text
[01J8…A1] running: LLM pass 1 (1/5)
[01J8…A1] running: calling read
[01J8…A1] idle: completed
```

- **合并窗口 ~150 ms**：子事件流（每次模型增量、每个步进通知）频率很高，
  直接转发会打爆父侧事件汇（软上限 1024）。`progress` 模块按 child 只保留
  **最新**一行，每个窗口至多产出一个分片；一分钟 6 万次子活动也只产出
  数百个分片。子 run 在一个窗口内结束时仍会 `flush` 一行收尾摘要。
- **载荷**：分片是普通的 `ToolOutput`——文本是上面的行，`metadata.children`
  带 `{child_id, status, activity}` 三个字段供结构化消费。
- **两条腿**：分片既走既有的 live 通道（`SystemEvent::ToolProgress` → 活性
  帧，追加不结算），也经 harness 落 durable `tool_output` 行；重载由该行
  重放同一份摘要。
- **L2 入口**：每行都带 child id，点开即订阅子会话；`tool_result` 尾部也仍
  有 `[subagent session: <id>]`。
- **延时 seam**：窗口等待走 `uva_core::Sleeper`（默认 runtime-free
  `ThreadSleeper`）；宿主在运行时上装配时用 `TaskTool::with_sleeper(...)`
  换成本运行时的实现。

## 控制面（三个模型工具）

`task` 派发之后，模型还要能管住自己派出去的子代理。控制面是同一块
brick 里的三个普通工具（同样经注册表定义/曝光/限制）：

| 工具 | 行为 |
|---|---|
| `agents_list` | 列本会话的子代理：id / 名 / 状态（running / idle / parked / aborted）/ 当前活动一行 |
| `agents_message` | 按目标态分流：running → steer（写子 run 的转向通道，子日志出现 `source: "steer"` 的用户行）；idle（在内存）→ 追问（同写入者再跑一轮）；本进程不持有 → 冷唤醒（读子日志头取 agent 名 → 解析 peer → 用该子会话写入者再跑一轮，进入 running） |
| `agents_interrupt` | 点火该子保留的取消令牌：当前轮终止 → idle（不是杀死，仍可续聊）；硬终止才是 aborted 终态 |

追问与冷唤醒跑的是**同一个轮次模块**（[`round.rs`](src/round.rs)）：它装配
子会话的输入、抽干同伴、映射结束原因、落尾巴，并报告模型看到的东西。因此
两条路的结果同形——都带 `[subagent session: <id>]` 句柄行，结束方式
（正常 / 取消 / 失败）的文案也逐字相同。唤醒路径不再自带一份轮次流程；
它保留自己的语义：不铸造子会话 id、不开子会话、不隔离、不过验证门，
自己写用户轮行，追问时也不重新入册。

生命周期状态机是纯逻辑（[`control/state.rs`](src/control/state.rs)），迁移表
以实现来源 [`docs/prototypes/plugin-splits-b-double-prime.prototype.html`](../../docs/prototypes/plugin-splits-b-double-prime.prototype.html)
验证过的 reducer 为准：非法迁移被**拒不**并给出模型可读的错误，既不静默生效
也不挂起。

每子的句柄（转向通道发送端 + 取消令牌 + 会话写入者 + peer）由控制面的
句柄表 [`ControlRegistry`](src/control/registry.rs) 持有；`task` 派发时入表、
子 run 结束时结算，冷唤醒的实例也归它所有。

### 名册的数据源

名册从句柄表读（形状 `{children:[{id,name,status,activity}]}`，与扫描版
一致）。读之前表先与会话**对账**：子会话以
`subagent_enter(child,parent,depth,agent)` 开档，所以读各会话头部就能
反查本会话的孩子；表里没有、会话里有的（上一个进程派发的）按 `parked`
**收养**——`parked` 的定义就是「本进程不持有句柄」——于是重启后名册不空，
给它的消息直接走冷唤醒。活行永远压过日志副本。

宿主通过 [`SessionIndex`](src/control/scan.rs) seam 提供会话访问：
`session_ids` / `read_head` / `read_tail` / `writer`，以及
`session_dir(id)`（工件集的落点，默认 `None` 即不写工件）。读日志的逻辑
在 crate 内。

### 生命周期与停放

四态 `running` / `idle` / `parked` / `aborted` 由句柄表执行：

- `settle` / `interrupt` 使子代理进入 `idle`，并记下「空闲起点」。
- `ControlRegistry::tick`（TTL = `PARK_TTL` = 5 分钟）把跨过 TTL 的
  `idle` 子代理停放：**释放**转向通道、取消令牌、会话写入者与 peer，
  行留着。运行中、已停放、已终止的子代理不老化；表的所有者（主会话）
  永不停放。
- `message` 到 `parked` 子代理 → 冷唤醒：读日志头 → 解析 peer → 新一轮，
  唤醒用新一轮的句柄**接管**那一行。`kill` 进入 `aborted` 时也释放句柄
  （终态不会再需要一轮）。

停放循环是可选的加速器：`ControlSurface::set_park_loop(spawner, sleeper)`
装宿主的运行时 seam，首次 tick 起一条只持弱引用的循环（`PARK_TICK` =
60 秒），brick 卸载即结束；不装 seam 的宿主照样在每次控制面调用前老化一次。
`uva-app-server` 的 `tool-task` 行装上 tokio 的 `Spawner` / `Sleeper`。

### 工件集

每轮结束（正常、取消、唤醒后的轮都算）在**子会话目录**写
`artifacts/<id>.{md,jsonl,patch}`：`.jsonl` 是本轮结束时子日志的冻结副本，
`.md` 是同一转录的可读渲染，`.patch` 只在隔离轮（worktree）确实改了东西时
出现。约定见 [`docs/log-format.md`](../../docs/log-format.md)；写入
best-effort——轮已经发生，摘要没落地只记一行日志。

## 注册

```rust
use std::sync::Arc;

use uva_agent::AgentRegistry;
use uva_tool_task::{TaskTool, TaskToolPlugin};

let tool = TaskTool::new(Arc::new(AgentRegistry::new()));
let brick = TaskToolPlugin::new(Arc::new(tool));
// 把 brick 挂进提供 `tools` 的内核上下文即可；卸载即撤走注册。
// brick 同时登记 `task` 与控制面的三个工具。
```

宿主若要让子代理落盘，用 `TaskTool::with_writers(...)` 装一个
`SubagentWriters` 来源；没有来源时子 run 照常流式产出，但不写会话。
来源实现

```rust
fn open_child(
    &self,
    child_session_id: &str,
    parent_session_id: &str,
    depth: usize,
    agent_name: &str,
    prompt: Option<&str>,
) -> Arc<dyn SessionLog>;
```

：工具在派发前铸造 child id，把父会话 id、嵌套深度、peer 名与派发词一并
交出，由来源按自己的归属规则开档（服务器的 `SessionSubagentWriters` 从父
会话 id 反查用户）。

宿主若要让名册能从日志重建、并让工件集落盘，用
`TaskTool::with_session_index(...)` 装一个 `SessionIndex` 来源；没有来源时
名册只列本进程握在手里的子代理。要让停放自己发生，再给 brick 装上运行时
seam：`TaskToolPlugin::with_park_loop(spawner, sleeper)`（服务器装的是
`server_spawner()` / `server_sleeper()`）。

## 示例

- `delegation_gate` — `gate`：子 run 结束后跑一条验证命令，失败即把子结果
  改成可读错误。
- `worktree_isolation` — `isolation`：子 run 在临时 git worktree
  （`uva/agent-<ulid>` 分支）里跑。

跑：`cargo run -p uva-tool-task --example <name>`。
