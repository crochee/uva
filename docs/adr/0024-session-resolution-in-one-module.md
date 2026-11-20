# 会话解析归一个模块

「这个会话存在吗、它的日志在哪里、解析它会不会顺手把它建出来」这条规则在这条线
之前有三套答案：**必须存在**的证明（登记表里有，或磁盘上有转录目录）写在 follow
路由的私有逻辑里；转录目录的**位置**住在会话路由里，follow 跨模块调它；**按需
创建**住在状态层——拿到 id 就把会话开出来。于是同一个未知 id，follow 面答
404，聊天面把会话建出来（打错一个字就多一个会话目录），而聊天面为了不让「查不
到」这条唯一不该有副作用的路径创建会话，在自己身上打了一个「先读日志再开控制
器」的补丁。

这不是 cosmetic。会话是**持久**的东西，「它存在吗」的判据是持久事实；这条判据
住在路由里，而路由不该知道磁盘布局。

本 ADR 裁定：会话解析归一个模块——
[`state/app_state/session_resolution.rs`](../../crates/uva-app-server/src/state/app_state/session_resolution.rs)，
放在状态层（它拥有会话登记表与磁盘布局的视角）。模块拥有「会话在哪、日志怎么
读」的知识，给出两条**有名**的策略；路由按名字要策略，不再各自判断，也不再跨
模块问路径或要读法。

## 决定

### 1. 两个有名策略

- **`must_exist`**（读面按名字要它）：登记表里有这个会话，**或**磁盘上有它的
  转录目录，两者任一即为存在——所以一个重启后登记表被清空的进程仍能解析它自己
  磁盘上的会话。两者皆无，策略拒绝该 id，答复是 session 形 404（`not found:
  session '<id>'`）；**拒绝路径绝不 mint**——不建目录、不写登记条目、不开控制
  器。follow 面（`follow` / `resume` / `compact`）按名字要这条策略。
- **`create_on_demand`**（聊天面按名字要它）：会话**无论存在与否都被开出来使
  用**，无任何证人认识的 id 被完整 mint——磁盘上的目录、登记条目、控制器。创
  建是调用方要这条策略时就签下的**文档化副作用**，不是解析路径的意外；不该创建
  的调用方要 `must_exist`。

  **集群边界**：在**非 owner 节点**上，`create_on_demand` 交回的是**远端句柄**
  （转发），不是 mint——不建目录、不写登记条目。会话活在它的 owner 上，只有
  owner 节点创建；这条事实写进了策略的模块文档与方法文档（评审建议 S1）。

### 2. 副作用契约变化——本线唯一的语义变化

regenerate 与 feedback 的 **miss 路径**（查不到可重放的 user turn / 查不到可
评分的目标行）从「意外目录」变为「显式完整创建」：

- **之前**：chat 的 read-before-create 补丁把读日志挪到开控制器之前，挡住了登
  记条目与控制器；但读本身经由 `SessionRegistry::sink`，而 sink 对**任意** id
  `create_dir_all`——所以 miss 路径仍然 mint 了目录，只是没人认领。
- **之后**：路由第一步就按名字要 `create_on_demand`，miss 路径的副作用是策略
  的全部契约（目录 + 登记条目 + 控制器），显式、完整、有测试钉住。

**直接答复不变**：regenerate 对 miss 仍答 404 `no user turn to regenerate in
this session`，feedback 仍答 `feedback_target_unknown`——两条路由测试逐字钉住。
但「对外行为不变」有一条**必须写明的边界**：miss 之后该 id 已被 mint，所以
`sessions/status`（读控制器缓存）与 `sessions/events`（查登记表）对这个 id
**从 404 变为可见**——status 可能仍答 404，但理由从「会话不存在」变成「尚无
快照」；events 答一份空的事件分桶。承诺的范围是「A 族的错误码与文案」，不是
「该 id 在所有面上的可观测状态」。

### 3. 两种失败共存时，赢家变了（评审建议 S2，接受）

「日志 schema 不支持 + 无 user turn」的 regenerate：旧序（先读后开）里读失败
被 `read_last_user_turn` 折叠成 `None`，答 404 no-user-turn；新序（**先开后
读**）里控制器先开，schema 拒绝先到，答 schema Session 错误（`session error:
session '<id>' is stored in schema version N, …`，code `session`）。本 ADR 明示
接受这个赢家更替：新的优先级**更诚实**——「这个 build 打不开这份日志」是关于
会话的事实，「没有 user turn」则是对一份根本读不了的日志的越权断言。
`routes/chat/tests.rs` 的
`an_unopenable_log_outranks_the_missing_user_turn_on_regenerate` 钉住这条优
先级（并做过牙齿验证：把顺序改回先读后开，测试当场红）。

### 4. 防创建补丁删除

chat 的 read-before-create 补丁及其注释（「`get_or_create_session_controller`
会顺手把会话建出来，所以先解析会让『查不到』变成创建会话的那条路」）随策略显
式化一起消失：策略是显式的，路由不再需要解释自己为什么绕路。一条源码门测试
（`the_chat_surface_asks_for_its_policy_by_name`）钉住补丁注释不回来。

### 5. 路径知识与行解析归一（评审建议 S3 一并落地）

- `transcript_path` / `user_sessions_dir` / `is_path_component` 的**唯一住所**
  是会话解析模块；路由不再拼路径，`session_index` 改问模块要用户目录。
- **行解析原语** `transcript_events` / `transcript_lines`（读一份持久转录的容
  错行解析：跳过空行与非 JSON 行、去头行）也从会话路由**搬进会话解析模块**：
  「日志在哪」与「日志的行怎么读」由同一处回答。follow 的 `session_rows` 改问
  模块——票 02 的验收字面「会话路由不再被别的路由跨模块调用」就此完全满足。
- **范围声明**：`uva-session` 仍是磁盘布局的**真正主人**（布局怎么组织、怎么
  命名、头行什么形状都在那里）；本线搬的是 app-server 内「位置在哪、行怎么读」
  的知识，布局本身不动。

### 6. 豁免：Anthropic 兼容面（评审建议 S5，记档）

「按名字要策略」目前只覆盖 chat 面。Anthropic 兼容面（`routes/messages`）仍直
接调 `get_or_create_session_controller_with_parent`——它需要带父级参数的创建
变体，而这条线没有给策略做带父级的有名变体。豁免记在此处：后续若要收口，给它
一个 `create_on_demand_session_controller_with_parent` 之类的有名变体即可，路
由的改法与 chat 面同形。

### 7. 对外答复不变的形状

A 族错误码/文案**逐字保持**：未知会话在 follow / resume / compact 三个面上都
答 `not found: session '<id>'`（三条路由测试逐字钉住，且钉住「拒绝不 mint 目
录」）；regenerate 的 miss 与 feedback 的 `feedback_target_unknown` 亦逐字钉
住。本线唯一的对外变化是第 2、3 节写明的两条。

### 8. 源码门的措辞（评审建议 S4，收窄）

`state/tests/session_resolution.rs` 的源码门宣称过「transcript-location
knowledge has exactly one owner」，但门只认几个**函数名**：`session_index` 对
递到它手上的目录做裸 `join`，是基线就有的布局知识，不被门覆盖。措辞已收窄为
门真正钉住的事实——「生产代码可问的 transcript 答案只有一个出口（会话解析模
块），不得从会话路由借路径或借读法」——不再宣称门外的布局知识也归一。

## 参考

- [`crates/uva-app-server/src/state/app_state/session_resolution.rs`](../../crates/uva-app-server/src/state/app_state/session_resolution.rs)：
  模块本体——两条策略、路径知识、行解析原语。
- [ADR-0023](0023-one-owner-for-the-tool-arguments.md)：同期的相邻决定——工具
  参数词汇归一个主人。本 ADR 管的是**服务端解析会话的规则**，0023 管的是**模型
  与工具之间的词汇**。

## Considered Options

- **策略做成一个泛型参数 / trait，而不是两个有名方法**：被否。调用点要的正是
  「一眼看出要哪条策略」；两个名字（`must_exist_session_controller` /
  `create_on_demand_session_controller`）比一个泛型参数更直白，也更容易在评审
  里指着名字问「你确定要这条吗」。
- **regenerate / feedback 继续走「先读后开」补丁**：被否。补丁的顺序本身就是
  第二套策略（「读面语义、聊天面壳」），而且读经由 sink，miss 路径已经在 mint
  目录——补丁挡住的比它声称的少（见第 2 节）。
- **Anthropic 兼容面一并改问策略**：被否（本次）。它需要带父级参数的变体，为
  它现造一个有名策略变体是另一条线的体量；豁免先记档（第 6 节），形状已想好。
- **`transcript_events` / `transcript_lines` 留在会话路由**：被否。票 02 验收
  的字面是「会话路由不再被别的路由跨模块调用」，follow 的跨模块调用留着，字面
  就不满足；行解析原语与路径知识同住一处，「在哪」与「怎么读」不会漂移。
- **收紧源码门，把 `session_index` 的裸 join 也覆盖进去**：被否。那是基线就有
  的布局知识，收门禁是另一条线的事；本次只把门的措辞改诚实（第 8 节），不扩大
  门的扫描范围。

## Consequences

- **调用方可见**：CHANGELOG「会话解析归一个模块」一条记录第 2 节的副作用变化
  与第 5 节的归一。
- **模块面**：`MustExistError`（`Absent` / `Open`）是 follow 面把拒绝翻成 wire
  答案的依据；`create_on_demand` 的非 owner 语义（远端句柄）写进模块与方法文
  档（第 1 节）。
- **测试面**：策略的四象限（只有登记表 / 只有目录 / 皆无 / 未知 id）在
  `state/tests/session_resolution.rs`；mint 的两层副作用（目录 + 登记条目）与
  miss 答复在 `routes/chat/tests.rs`；错误优先级（第 3 节）与 A 族逐字答复
  （第 7 节）各有钉子；源码门（第 8 节）盯住「不从会话路由借答案」。
