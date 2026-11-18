# 聊天窗口读模型归一个模块：界面只做胶水

前端有两套读行词汇，中间没有缝：

- **转录**（`lib/session-to-messages.ts`，1565 行）是纯模块，有 vitest 覆盖；
- **窗口**（`pages/ChatPage.tsx` 内部，2001 行里的 570 行）是页面私有的：12 个 action
  的 reducer（`:465`）、窗口/轮/steer 语义（`turnEndReason`、`statusFromReason`、
  `stepStartIndex`、`advanceSteers`、`settleQueued`、`projectedDurable`、
  `steersFromRows`、`reconcileSteers`、`liveSegments`）、状态与 action 类型。

后果不是「文件太大」，而是**窗口这套读模型没有测试面**：它是页面文件里的私有
函数，vitest 进不去（ADR-0001 的前端测试政策只测纯函数，不测组件），于是页面与
已测的 lib 以不同节奏漂移——窗口 reducer 的每个分支都只能靠人肉点页面验证。

窗口 reducer 本身是纯的：它不碰 React、不碰 DOM、不读时钟（时间戳由 action 带
进来）。它只是没有出口。

## 决定

- **整块读模型搬进 `lib/chat-window.ts`**：reducer、状态与 action 类型、窗口/轮/
  steer 语义一起搬，单文件（~610 行，与仓内 `session-to-messages.ts` 同一形状）。
  模块的接口是窄的：reducer + `emptyWindow` + 三个页面真用得到的符号
  （`turnEndReason`、`liveSegments`、`projectedDurable`），其余封在模块里。
- **页面只做胶水**：`ChatPage.tsx` 从 2001 行降到 ~1430 行，剩下的全是 React
  渲染与 effects；`useReducer(windowReducer, …)` 的接线不变。
- **纯搬家，零语义变更**：只搬代码与改导入。搬迁途中若发现真实缺陷，单独记下、
  单独处理，不混在重构里。
- **测试面补齐**：新 `tests/chat-window.test.ts` 对 12 个 action 每个至少一条用例，
  外加 baseline 重建、live 帧守卫（退役后不得复活、序号单调）、steer 生命周期与
  轮边界分支。

## 参考

- [ADR-0001](0001-frontend-test-policy.md)：前端不保留 e2e 套件，纯函数单测用
  vitest——这条政策正是「读模型必须是纯模块」的理由：模块化之前，窗口 reducer
  连一个能进的测试面都没有。
- [ADR-0016](0016-supersession-rule-in-one-module-per-host.md)：退役规则归位后，
  窗口 reducer 只是**调用**该规则，不再自己判；本次把承载它的读模型整体模块化。
- [ADR-0006](0006-row-vocabulary-owns-the-row.md)：转录读法与词汇的对应关系。

## Considered Options

- **只搬 reducer，辅助函数留在页面**：被否——那会让 `lib/` 反向依赖 `pages/`，依赖
  方向反了。
- **拆成 `chat-window/` 目录（reducer / steers / live 三块）**：被否——当前规模
  三个文件互相引用多于各自独立；先单文件，超过 ~600 行再拆。
- **顺手修搬迁途中发现的缺陷**：被否——重构与行为变更混在一个 diff 里，评审时分
  不清哪行是搬家、哪行是语义。

## Consequences

- **行为变更零**：抽出的代码逐行未改（仅两处文档归位：`turnEndReason` 的 doc
  原本错放在 `rowData` 头上）。
- **测试面从零到 13 条**：窗口 reducer 的 12 个 action 全部有 vitest 用例（此前
  只能人肉点页面）。
- **层次上的已知小瑕疵**：模块为了 `SteerRecord` / `SteerState` 两个类型从
  `components/chat/SteerChip` 做 type-only 导入——运行时零依赖，但 lib 仍认一个
  组件路径；正解是把这两个类型搬到 lib 再由组件反向导入，本次不做（超出纯搬家
  的边界，单独处理更干净）。
- **页面行数**：`ChatPage.tsx` 2001 → ~1430 行；页面今后新增的是渲染与副作用，
  窗口语义加在模块里。
