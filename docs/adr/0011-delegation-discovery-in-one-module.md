# 委派发现归一个模块：枚举窗口、头部解析与镜像方言只有一份

「读最近的会话、认出头部的 `subagent_enter`、报出这个部署里有哪次委派」这条规则，
在两个 crate 里写了三遍、外加第四个解析点，且**已经分歧**：

- `uva-tool-task/src/control/scan.rs`：`SCAN_LIMIT`（`:32`）、`discover`（`:125`）、
  `child_record`（`:151`）——头部取 **4 行窗口**，认「名字是本会话」的 enter 行；
  不认镜像方言、不去重。
- `uva-app-server/src/routes/tasks.rs`：`TASK_SCAN_LIMIT`（`:92`）、`scan_recent_sessions`
  （`:176`）、`delegation_child`（`:249`）、`recent_session_ids`（`:267`）——**只认第一条
  事件**，但认旧构建写进父日志开头的**镜像行**并按孩子去重（`:203`、`:235`）。
- `routes/sessions/mod.rs:227` 的 `subagent_attribution`（四处调用点）与
  `routes/follow.rs:599` 的 `agent_attribution`——又是两份「只认第一条事件」的解析。

分歧不是假设：头部前面垫了一行元数据的子会话，名册找得到（4 行窗口）、tasks 页看不见
（只认第一条）；镜像方言只有 tasks 页认，名册「不收养镜像孩子」是遗漏的结果，不是决定。
同一个「最近窗口」预算也写了两遍——而 `ServerSessionIndex::session_ids`
（`session_index.rs:66-107`）早就拥有了双源枚举（注册表 + 磁盘、排除临时会话），
`recent_session_ids` 是它的按用户版重写；`ServerSessionIndex::read_head`（`:109-117`）
则是**全文读再 `take(limit)`**——「窗口是预算」在读取层从未成立。app-server 经 facade
够得着 `uva::tool_task::SessionIndex`（`composition/kernel/mod.rs:861`），深化不新增依赖边。

我们把发现收成 `uva-tool-task::control::scan` 里的**一个模块**。

## 决定

- **一个 `discover(index, scope)`**：`Scope::{Parent(id), User(id)}` 两个作用域，一个
  `SCAN_LIMIT`。名册走 Parent（全局枚举后按父过滤，与今天相同——父的孩子不可能属于
  别的用户）；tasks 页走 User。另有一个**纯头部解析**原语，供归因调用方（会话列表 /
  详情 / follow）复用。
- **头部规则统一为「头 4 行窗口内第一个 `subagent_enter`」**：名字是本会话 →
  `RecordSource::OwnLog`；名字是另一个孩子 → `RecordSource::ParentMirror`（旧构建把
  镜像写在父日志的第一行，`tasks/tests.rs` 的 `seed_delegation` 夹具就是这形状）。
  去重一份：同一孩子的自有日志压过镜像。「普通会话的靠后行冒充头部」的顾虑由
  「enter 行必须自名」答掉——enter 行只来自子会话开档与旧镜像写入，普通会话的
  行里没有它。
- **`ChildRecord.finished` 变 `Option<bool>`**：Parent 作用域探尾窗（名册依赖它）；
  User 作用域不探（tasks 页随后的全文读自己给状态）；镜像永远 `None`。
- **名册只收养 `OwnLog`**：镜像孩子按构造就是「自有日志读不动」的孩子，冷唤醒要它的
  writer——无物可收养。tasks 页两者都渲染，镜像降级（无派发词、无结局）。
- **`SessionIndex` 增 `session_ids_of_user`**：枚举是宿主知识，归适配器——
  `ServerSessionIndex` 读单用户目录 + 注册表（排除临时会话），`FakeIndex` 过滤。
  路由侧的 `recent_session_ids` 删除。
- **发现 / 描述分界**：发现答「谁存在」（窗口证据：头 4 行 + 尾 4 行）；描述答「它说了
  什么」（孩子日志全文：派发词、`subagent_exit`、状态）——留在 tasks 路由与
  `delegation_of_child`（`get_task` 不动）。排序、分页、wire 投影是调用方的事。
- **预算成真**：`SessionSink` 增默认方法 `read_head_rows(window)`，`JsonlSessionSink`
  按字节预算实现（fs 层早有带预算的头部读取），`ServerSessionIndex::read_head` 委托它。

## 参考

- [ADR-0006](0006-row-vocabulary-owns-the-row.md) 与
  [ADR-0008](0008-session-status-owns-the-session-state.md) 定下同一条纪律：一份规则，
  不留多个读点各写一遍。本次是它在「委派」上的落点。
- `scan.rs` 的模块头早就写着「会话是委派的 durable 记录，读头部就能反查孩子」——本次
  只是让这句话成为唯一实现。
- `uva-tool-task/README.md`：「读各会话头部就能反查本会话的孩子；表里没有、会话里有的
  按 parked 收养」——收养面消费发现，所以发现的语义（含镜像取舍）必须在发现模块里
  一处说清。

## Considered Options

- **把发现挪进 `uva-session`**：被否。枚举是宿主访问，不是行词汇；会把 `SessionIndex`
  接缝拖过 crate 边——ADR-0006 以同样的理由否过「为叶子 crate 已有的东西再造结构」。
- **只共享头部解析、枚举各留一份**：被否。窗口策略仍两份，tasks 页的「最近 50」与名册
  的「最近 50」会继续漂移——这正是本次要消掉的分歧形状。
- **`discover` 保持 parent 过滤签名，tasks 页另写入口**：被否。两个入口就是两份策略，
  镜像方言会留在其中一个。
- **名册也收养镜像记录**：被否。冷唤醒要孩子的 writer；镜像孩子无日志可开，收养必然
  失败，只是把失败从「发现处过滤」挪到「唤醒处报错」。
- **User 作用域也探尾窗**：被否。tasks 页随后的全文读自己给状态，尾窗是白付的读。
- **不加 `read_head_rows`、接受全文读**：被否。名册今天就为每个会话付一次全文读；
  把 tasks 页迁上来会让它从「有界的首行读」退化为 50 次全文读，且窗口预算的文档
  成为虚构。

## Consequences

- **行为变化四条，都是有意为之**：①头部垫了元数据行的子会话进入 tasks 列表（此前
  不可见）；②旧镜像孩子以降级记录出现在 tasks 列表（无派发词、无结局），且永远不被
  收养；③会话列表 / 详情与 follow 的归因从「只认第一条事件」放宽为头 4 行窗口——
  垫了元数据行的子会话第一次归因正确；④tasks 页的头部读从一次首行读变为有界的
  窗口读（预算内）。
- **冻结面不动**：`SessionSink` 与 `SessionIndex` 的新成员是既有名字上的新方法，
  基线记的是 crate 根名字（ADR-0009 对 `SessionEvent::message()` 的同一裁定）。
  `Scope` / `RecordSource` 是 `uva-tool-task` 的普通新公开项（该 crate 不在冻结面）。
- **测试面**：发现语义（新序窗口、镜像方言与去重、两个作用域、头部规则、自名过滤）
  对着 `FakeIndex` 测在 `uva-tool-task`——接口就是测试面；app-server 留下 wire 形状、
  排序 / 游标、`get_task` 与降级记录在 wire 上的渲染。路由里的 `seed_delegation`
  镜像夹具迁到 tool-task。
- **词汇表新增「委派发现（delegation discovery）」**，发现 / 描述的分界写进定义。
- **切面**：⓪ ADR + 词汇表 → ① `read_head_rows` → ② `session_ids_of_user` →
  ③ 统一 `discover`（含 tool-task 内部调用点与 FakeIndex 测试）→ ④ tasks 页 /
  self_mcp 迁移并删路由副本 → ⑤ 归因折叠。每片独立可评审，CHANGELOG 随片记。
