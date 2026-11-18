# 会话状态归 app-server 的一个模块所有，控制器状态与对外状态是两个枚举

同一个「状态」在仓里有四套词汇：`SessionState`（控制器的生命周期，4 值）、
`SessionSummary.status` 的 9 个标签（只有 5 个被产出）、`FollowHeader.state` 的 4 个标签
（与前一个标签集拼法不一致——同一个会话的取消在两个响应里分别作 `canceled` 与
`cancelled`）、以及 `OperationState`（一次运行的状态机，5 值，在冻结面里）。
「这个会话在等审批吗」这条规则被**四份实现**回答，其中 `derived_live_state`
（`routes/sessions/mod.rs:239`）不降级陈旧的内存标记，而 `reported_state`
（`routes/follow.rs:510`）降级——于是同一个会话在 `sessions/list` 与 `follow`
里可以答得不一样。线上那两个字段是**裸 `string`**，契约里没有对应类型，
所以没有任何东西校验它们的取值；浏览器照着服务端不产出的标签写了 CSS
（`uva-web/src/pages/ChatPage.css:89-110` 的 `.status-submitted` /
`.status-unspecified` / `.status-rejected` / `.status-auth-required`）。

我们把「这个会话现在是什么状态」收成 app-server 里的**一个模块**：
`crates/uva-app-server/src/session/status.rs`。

## 决定

- **模块住在 app-server**（`crates/uva-app-server/src/session/status.rs`）。app-server 只依赖
  `uva` facade，不新增依赖边。`uva-session` 是叶子 crate，**不能**调 `open_requests`
  （它在 `uva-approval`，而 `uva-approval` 依赖 `uva-session`），也不认识控制器的内存状态；
  `uva-approval` 是「暂停是日志的属性」这条规则的现有主人，但「会话在不在跑」不是审批概念。
  为一个叶子 crate 已有的东西再造一个 crate 被否（ADR-0006 同样的理由）。
- **两个枚举，不是一个。** `SessionState`（控制器状态：空闲 / 运行中 / 等待审批 / 已取消）
  从 `session/controller/ops.rs` 搬进模块，是控制器自己记的生命周期；
  `SessionStatus`（会话状态：从未开始 / 进行中 / 等待审批 / 已取消 / 已完成）是对外报告的，
  是控制器状态的**细化**外加「日志空不空」这条控制器不管的事实。合成一个枚举被否：
  控制器写「空闲」时不知道该写 `unspecified` 还是 `completed`——它不关心日志空不空。
- **模块是纯函数，收「已折叠的事实」**：控制器状态 + 日志里有无未决审批 + 日志空不空
  → 会话状态 → 线上标签。窗口规则留在调用方（列表只敢看尾部窗口，follow 要能证明自己
  看的是整个挂起回合），错误策略留在调用方（读不动 → 警告并退回报内存状态，
  `session/controller/inner.rs:410-415` 已把「状态形状的调用方没有错误通道」写成仓里的规矩）。
- **降级规则以日志为准。** 内存记着「等待审批」而日志里没有未决审批时，报**空闲**
  ——即 `reported_state` 的规则；`derived_live_state` 与磁盘扫描路径统一到它。
  这是一处行为变更：审批在别的节点被答掉时，`sessions/list` 由 `input_required`
  改为 `completed`。
- **`OperationState` 不动。** 它答的是「这一次 agent 循环跑到哪了」，与
  `SessionState` 是两套不相交的词汇（中间没有转换函数），且在冻结面里
  （`scripts/frozen-api/baseline:249-250`）；浏览器不消费它（`uva-web/src/api/sessions.ts:9-13`）。
  把两套合成一套被否——那是两个问题、两个生命周期。`OperationState::kind()` 与
  `is_terminal()` 零生产调用、`Failed` 无产出者，是**另一个**候选（冻结面瘦身），
  不在本次范围内。
- **9 个标签是筛选词汇，不是状态词汇。** 状态产出者只覆盖 5 个；`submitted` /
  `failed` / `rejected` / `auth_required` 今天没有产出者，但作为筛选词保留
  （线上 breaking，且砍掉的是别人在用的筛选词）。
- **线上只有一套会话状态词汇。** `SessionSummary.status`、`SessionDetail.status` 与
  `FollowHeader.state` 都从裸 `string` 变成 `SessionStatus` 的封闭 union，让「这个字段
  只能是这 5 个值」成为类型事实。`follow` 头原来自带一套 4 标签词汇（`idle` / `running` /
  `awaiting_approval` / `cancelled`）——它退场，控制器状态不再上线。**这是线上 breaking
  变更**，且字段的**含义**变了：本节点没有活着的运行，过去报 `idle`，现在报 `completed`
  （日志有行）或 `unspecified`（日志空）。
- **变体名即线上标签。** `SessionStatus` 的变体名派生线上拼写（`rename_all =
  "snake_case"`），照行词汇的规矩（ADR-0006「变体名即线上标签」）：`InputRequired`
  作 `input_required`、`Canceled` 作 `canceled`。`as_wire()` 是那份派生的手写镜像
  ——与 `RowTag::as_wire` 同一个形状——并由一个测试把两者钉住。会话状态因此说
  「需要输入」，控制器状态说「等待审批」——同一个事实，模块内两种说法；线上只有前者。
- **列表的 status 排序按线上拼写排**，不按枚举的声明顺序——客户端读的顺序是拼写的
  字典序，改成枚举序会让 `?sort=status` 的结果换一套。

## 参考

- [ADR-0002](0002-typed-session-log-seam.md) 把会话日志的**写**路径收成一条类型化的接缝，
  但读路径与词汇本身仍是调用方的知识——本次是同一形状在**状态**上的收拢。
- [ADR-0006](0006-row-vocabulary-owns-the-row.md) 把「一行是什么」收成行词汇模块，
  并留下「读点仍在写两分支 match」的未兑现承诺；[ADR-0007](0007-row-vocabulary-restores-at-the-read-boundary.md)
  把归一移到读边界。本次沿用同一条纪律：**一份规则，不留多个读点各写一遍**。
- `crates/uva-approval/src/approval.rs:467` 的 `open_requests` 是「哪些审批仍未决」的现有主人，
  它的文档写明「暂停是日志的属性，不是任何内存标记的属性」——本次的降级规则与它同向。
- `crates/uva-app-server/src/session/controller/inner.rs:410-415` 已经把「状态形状的调用方
  没有错误通道」写成仓里的规矩，本次不改这条。

## Considered Options

- **合成一个枚举**（控制器也写 `SessionStatus`）：被否。控制器写「空闲」时不知道该写
  `unspecified` 还是 `completed`——它不关心日志空不空，于是要么它存两个值，
  要么模块替它决定，多一层。
- **模块收原始行、自己调 `open_requests`**：被否。窗口规则是各调用点的**证据范围**问题
  （列表只敢看尾部窗口 512 行，follow 要能证明自己看的是整个挂起回合），
  塞进模块就得让模块同时知道这些调用点细节；且错误策略会跟着掉进模块。
- **模块住 `uva-session`**：被否。依赖方向上不可行（见上），且它不认识控制器。
- **模块住 `uva-approval`**：被否。「会话在不在跑」不是审批概念。
- **模块收 9 个标签的产出者**（补 `failed` / `submitted` 的产出路径）：被否。那会把一次
  深化变成加功能——`submitted` 的语义要先定义「park 了但没跑起来」到底算什么。
- **把 `OperationState` 并进来**：被否。它是运行态、冻结面、浏览器不消费；合成会造出
  一个既不是运行也不是会话的第三种状态。
- **线上一次收紧全部（`FollowHeader.state` 也变 union）**：在切面 ② 被否，在切面 ③ 落地。
  那套 4 标签词汇下一刀就要退役，这一刀拖它进来会让两个切面互相等待——但它终究要退役，
  所以第三刀把它收掉了（连同它带来的**语义**变化：字段从「本节点有没有活着的运行」改成
  「这个会话是什么状态」）。
- **纯移动、行为逐字节不变**（四处规则原样保留在模块内）：被否。那会把「同一模块里
  两套互相矛盾的规则」当成完成态发出去——那正是本次要消掉的东西。

## Consequences

- **冻结面不变**：`SessionState` 不在冻结面里（`scripts/frozen-api/baseline` 只含库 crate），
  `OperationState` / `OperationSnapshot` 不动，因此不需要同步 `docs/api-frozen.md`。
- **线上类型变**：契约目录新增 `SessionStatus`，`uva-web/src/api/types.ts` 由
  `make check-web-contract` 重新生成；`SessionSummary.status` / `SessionDetail.status` /
  `FollowHeader.state` 从 `string` 变成 5 值联合。列表与详情两处的拼写不变，前端的渲染点
  （`Sidebar.tsx:220`、`CommandPalette.tsx:104`）不用改；`follow` 头的拼写变了（见下）。
- **`follow` 头的语义变了**：它过去报的是**本节点**有没有活着的运行（`idle` /
  `running` / `awaiting_approval` / `cancelled`），现在报的是**会话状态**——本节点没有
  活着的运行、但日志有行的会话，过去报 `idle`，现在报 `completed`（日志空则
  `unspecified`）。这是本 ADR 明确接受的代价：两个响应说同一句话，比各说各的准。
  仓内唯一的读者是 TUI 的 `turn_from_state`（`working` → 运行中、`input_required` →
  等待审批、其余 → 空闲），浏览器不读这个字段。
- **行为变两处**：①审批在别的节点被答掉、内存标记陈旧时，`sessions/list` 与
  `sessions/get` 由 `input_required` 改为 `completed`（罕见场景，且是修正）；
  ②运行中的会话在 `approval_asked` 行落盘与挂起标记翻转之间的那个窄窗口里，
  `sessions/list` 由 `input_required` 改为 `working`——`follow` 的头今天就报
  `running`，这一刀让两个端点在同一时刻说同一句话。
- **测试面搬走**：`parse_status_filter` 的 3 个单测、`infer_status_from_parts` 的 4 个单测
  搬进 `src/session/status/tests.rs`，按新接口重写；`derived_live_state`、`reported_state`、
  `state_label` 今天**一个单测都没有**——两份答得不一样的那份正好是没被测的那份，
  新表补上「陈旧内存标记」这一格。路由级与控制器级断言不动。
- **文档与样式拉齐**：`api/v1/page_query.rs:139-146` 说的「规范集合」（5 个、连字符）
  与代码（9 个、下划线）对不上，改正；`SessionSummary.status` 的文档注释
  「the closed set of 9 status labels」说的是筛选集合而字段值集是 5 个，改正；
  `ChatPage.css:89-110` 那四个服务端不产出的类名清掉。
- **词汇表新增三词**：会话状态、控制器状态、运行状态（`GLOSSARY.md` 的「会话状态」一节）。
  三者同名不同义，是未来最容易搞混的地方。
- **切面**（见 `GLOSSARY.md` 的「切面」）：三刀。①模块立起来 + 降级规则统一 + 调用点全迁
  + 文档改正（`SessionSummary.status` 的注释进了生成的线上类型，所以 ① 也要重新生成一次
  `types.ts`）；②线上收紧成封闭 union + 前端迁移 + CSS 清理（依赖①）；③`follow` 头的
  4 标签词汇退役，头改报会话状态（依赖②）。
