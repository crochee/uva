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
| 深度 | 父 run 深度按会话 id 查表，子 run 为 `+1`；`MAX_SUBAGENT_DEPTH = 3` 时工具拒绝派发并给出可读错误 |
| 并发 | 一个 `task` 调用起一个子代理。工具声明 `ExecutionMode::Sequential`，批次调度器因此把同一批里的 `task` 调用串行化——委派起的是整个 run，批次内并发会改动取消与批次语义，不只是时序 |
| 取消 | 子 run 拿自己的令牌（`ChildCancel`）：父 run 的令牌仍然透传，父取消即子终止；子自己的令牌由控制面的 `interrupt` 点火 |
| 校验 | `gate`（子 run 后跑验证命令）与 `isolation`（git worktree 隔离）沿用原 `task` 语义，由 `CommandRunner` seam 执行 |

## L1 进度：父页工具块里的子代理摘要

子 run 运行时，`task` 覆写 `Tool::stream`，把子代理的当前状态写进父会话的
工具块——每行 `[<child_id>] <status>: <当前活动>`：

```text
[01J8…A1] running: LLM pass 1 (1/5)
[01J8…A1] running: calling read
[01J8…A1] idle: finished
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
