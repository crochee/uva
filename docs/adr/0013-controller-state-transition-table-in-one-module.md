# 控制器状态的转移表归一个模块：怎么走只有一份

ADR-0008 把「这个会话是什么状态」的**读**路径收成 `session/status.rs` 的纯折叠，并裁定
`SessionState`（控制器状态）与 `OperationState`（运行状态）是两套不合并的词汇。它没有碰
**写**路径——写路径至今没有 owner：

- `SessionState` 的写点有 **6 个**，散在 4 个文件：`inner.rs:425`（`mark_awaiting_approval`，
  只从 Idle/Cancelled）、`inner.rs:507`（`mark_running`，无条件）、`dispatch.rs:846`
  （resume parked，只从 AwaitingApproval）、`dispatch.rs:899`（cancel，无条件）、
  `run_task.rs:803-810`（settle，只从 Running）、`mod.rs:629`（`adopt_state`，拒绝覆盖
  Running）。每个守卫都是承重的——比如取消后排干的 settle 必须不覆盖 `Cancelled`——但
  「哪些转移合法」这条规则没有被任何一处拥有。
- 快照总线的发布点有 **3 个**（`inner.rs:525` Running、`dispatch.rs:903` Cancelled、
  `run_task.rs:837` Completing），而 `inner.rs:112-115` 的文档承诺发布 Running /
  **Idle** / Cancelled——`Idle` 从未被发布，`latest` 在一次运行结束后永远停在
  `Completing`。
- 这不是 cosmetic：`delete_session`（`routes/sessions/mod.rs:1020-1023`）用
  `turn_in_flight` 判断忙碌，而 `turn_in_flight` 把 `Completing` 算 in-flight（有测试钉住，
  `sessions/tests.rs:811`）——**一次运行完成后，该会话的 `delete_session` 永远返回
  `session_busy`**。
- `suspended`（`AtomicBool`，`inner.rs:161`）是 Suspended 事件到 settle 决策之间的单比特
  运输：`persist.rs:36` 记账、`run_task.rs:804` 消费、`reset_live`（`inner.rs:461`）清零。
  延迟翻转是有意的——立即翻转会让 `is_startable` 在旧任务排干时放行新 run。

我们把写路径收成 `session/controller/` 内的**一个模块**（`state.rs`）：它拥有转移表、
全部写点与快照发布。

## 决定

- **模块住 `crates/uva-app-server/src/session/controller/state.rs`。** `status.rs` 一字不动：
  它是读半边（纯折叠，ADR-0008 的裁定原样），写模块 import 它的 `SessionState`——枚举是
  读写两半共享的**词汇**，词汇住在读半边；写模块拥有的是**转移**，不是类型。
- **转移表是一个封闭枚举 + 一个穷尽 `apply`（全函数）。** 每个（当前状态 × 转移）组合都有
  定义的结果——迁移或**有意的停留**；无错误通道、无 panic。no-op 不是异常，是表里的一格
  （cancel-then-settle 靠它活着）。这与「状态形状的调用方没有错误通道」的既有规矩
  （`inner.rs:410-415`，ADR-0008 引为同向）一致，也与仓里「穷尽 match + 测试钉住」的
  既有形状（`RowTag::surface_role`、`status.rs` 的真值表）同构。

  | 转移 | Idle | Running | AwaitingApproval | Cancelled | 发布 |
  |---|---|---|---|---|---|
  | `BeginRun` | →Running | →Running | →Running | →Running | Running 帧 |
  | `Settle` | 停留 | →Awaiting/Idle | 停留 | 停留 | flush 后 Completing 帧（见下） |
  | `Cancelled` | →Cancelled | →Cancelled | →Cancelled | →Cancelled | Cancelled 帧 |
  | `ApprovalAwaited` | →Awaiting | 停留 | 停留 | →Awaiting | 不发 |
  | `ResumedParked` | 停留 | 停留 | →Idle | 停留 | 不发 |
  | `Adopt` | →给定值 | 停留 | →给定值 | →给定值 | 不发 |

- **发布归转移表，映射是部分的。** 只有 `OperationState` 有对应物的转移才发帧：
  `BeginRun` → Running、`Settle` → Completing（切面②起 Completing 之后紧跟 Idle）、
  `Cancelled` → Cancelled；`ApprovalAwaited` / `ResumedParked` / `Adopt` 不发——
  `OperationState` 是一次运行的生命周期，没有「等待审批」这一格（ADR-0008 的两套词汇
  裁定在这里落地）。`Settle` 的帧在 **flush 之后**发：订阅者必须先看到行落盘、再看到
  运行收尾，翻转与发帧因此是两次调用，由 run task 按既有顺序摆放。
- **`Settle` 发两帧：`Completing` 之后紧跟 `Idle`。** 结局与当下是两个事实：流上的消费者
  仍能读到最后一轮怎么结束（Completing），`latest` 从此报「现在」（Idle）。这是行为变更
  两条的来源：`delete_session` 解锁（今天一次运行完成后永远 `session_busy`）；
  `sessions/status` 的 `latest` 从永远 `completing` 变为 settle 后的 `idle`。
  `turn_in_flight` 不动——Running/Completing 在窄窗口里确实 in-flight，语义没变，
  变的是 `latest` 不再是最后一轮的遗迹。
- **`suspended` 收成模块私有记录。** `persist` 在 Suspended 事件上记一笔（领域命名的
  调用），`settle` 消费，`BeginRun` 清零；`AtomicBool` 不再是 `pub(super)` 字段。延迟翻转
  语义原样保留。
- **`adopt_state` 走表。** 接管是真实场景（`state/app_state/sessions.rs:487-488`：读
  `session_state()` 再注入），日志是权威——「从事实重建」是表里的 `Adopt` 一格（拒绝
  覆盖 Running 的守卫原样），不再是绕过表的裸 setter。
- **伴生动作留在调用方。** 模块只吃表与投影：状态翻转、快照发布、`is_running` /
  `is_startable` 两个查询（`BeginRun` 的前置条件是表的知识）。cancel token 铸造、
  `reset_live`、tracing 日志留在 `mark_running` / run task——那是 run 的机器，不是状态的
  知识。
- **`state()` / `session_state()` 两个读不变。** 路由读裸 `state()` 是给纯折叠喂
  `StatusFacts.controller`（`routes/sessions/mod.rs:206`、`routes/follow.rs:436`）——
  ADR-0008 安排的读法，窗口与错误策略归调用方的裁定一并原样。

## 参考

- [ADR-0008](0008-session-status-owns-the-session-state.md) 定下读路径的归属与两套词汇
  不合并；本次是同一形状在**写**路径上的落点，不与它任何一条裁定冲突。
- [ADR-0011](0011-delegation-discovery-in-one-module.md)、
  [ADR-0012](0012-live-projection-is-its-durable-position.md)：同一纪律（一份规则，不留
  多个写点各写一遍）在委派发现与活动投影上的落点。
- `run_task.rs:797-812` 的 steer 门注释：翻转与 steer 决策的排序约束——本次保持不变，
  settle 仍在 steer 门下翻转。

## Considered Options

- **`status.rs` 长出有状态的一半**：被否。ADR-0008 把它钉成纯函数；读写混住会让「窗口与
  错误策略留在调用方」的裁定无处安放。
- **只加一个纯转移函数、写点仍分散调用**：被否。表有一份，但发布仍三处、`suspended` 仍是
  公有字段——locality 没拿到。
- **`apply` 返回 `Result<_, IllegalTransition>`**：被否。错误通道引出「谁在什么情况下该
  忽略」的新知识，恰好是现在要消掉的；现行六个守卫已经是全函数，no-op 是定义不是异常。
- **Suspended 事件立即翻转 Running → AwaitingApproval**：被否。`is_startable` 会在旧任务
  排干时放行新 run（双跑）；延迟翻转是有意的，`suspended` 是结算参数的运输，不是第二份
  状态。
- **消费侧修 `turn_in_flight`（改用 `is_terminal`）**：被否。`latest` 报「最后一轮的遗迹」
  的问题仍在；发布侧修复让 `latest` 永远反映当下。
- **`Settle` 只发 `Idle` 一帧**：被否。结局信号从流上消失；两个事实总线都放得下。
- **把 `OperationState` 并进表里**：被否（ADR-0008 已否）。它是运行的生命周期，冻结面
  （`scripts/frozen-api/baseline:249-250`），枚举零改动。

## Consequences

- **冻结面不动**：`uva-app-server` 不在基线里；`OperationState` / `OperationSnapshot`
  零改动；契约零再生成（`types.ts` 不重出）。
- **行为变更两条（切面②）**：settle 后 `latest` 为 `Idle`——`delete_session` 在一次运行
  完成后不再永远 `session_busy`；`sessions/status` 的 `latest` 报 `idle`。`CHANGELOG`
  随片记。
- **测试面（切面③）**：truth-table 全表测试打在 `apply` 与模块公开面上（学
  `status/tests.rs:77-215` 的 20 行真值表形状）；`lifecycle` 的 `Running → Completing`
  断言扩成 `Running → Completing → Idle`；钉住「settle 后 latest 停在 Completing」这一
  旧形状的断言删除；被迁移测试的观察面改走快照总线。
- **词汇表**：「控制器状态」词条补一句——它的转移表归一个模块。
- **切面**：⓪ ADR + 词汇表 → ① 纯搬家零行为（六写点迁经 `apply`、`suspended` 收私有、
  `adopt` 走表、删 `dispatch.rs:104-109` 逐字重复的 lease 块）→ ② 发布收拢 + settle
  双帧（行为变更两条）→ ③ 测试面迁移。每片独立可评审、可回滚。
- **本 ADR 不裁定的**：`maybe_start_run`（552 行）与 `open_session_under_gate`（311 行）
  的拆分——转移表只会让它们各瘦几行，拆分是独立的形状工作；路由侧 `StatusFacts` 的
  四条采集路径——窗口是证据范围问题，归「窗口证明」那个候选。
