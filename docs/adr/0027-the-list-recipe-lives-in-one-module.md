# 列表配方归一个模块：一条路由只声明它的排序键

六条列表路由（agents、tools、skills、schedules、tasks、sessions）各自手写同一套六步配方：
校验排序 → 解析页面 → 排序 → 应用方向 → 切片 → 造信封。抄本之间已经互不一致，而且每一处
分歧都是**静默**的——客户端拿到 200，拿到一个格式正确的页：

- **`sessions` 的默认顺序与它自己的文档相反。** 路由注释与 `SessionSummary` 都写「默认
  `-created_at`，最新在前」，但默认值只代入 `validate_sort`、没有代入页面解析，于是省略 `sort`
  返回**最旧在前**。浏览器两个面因此打架：工作区树显式传 `-created_at`（最新在前），命令面板
  不传（最旧在前）。
- **`schedules` 完全不理方向。** 三条键都排了，`descending` 从不读——`?sort=-name` 通过校验、
  返回升序。
- **`agents` 的白名单声明了 `kind`，排序代码从不读它。** `?sort=kind` 通过校验、返回按 name 排的页。
- **四条路由里两条没有 tiebreaker。** `skills`（创建时间）与 `schedules`（下次触发）键相等时
  页边界无定义——同一个 cursor 在两次请求之间可能重复或漏行。
- **`sessions` 的嵌套页面参数没有校验。** 带空 cursor 的请求在其余六条 list 方法上返回
  invalid-params，在 `sessions` 上返回第一页。
- **`evals/list` 解析页面之后把它扔掉。** 它接受 `limit`/`cursor`/`sort` 并全部无效；未知
  `sort` 在这里被接受、在别处被拒。

根因是一条：**白名单与排序实现是两处声明**，而切片算术在 `uva-core` 的 registry 分页里
已经有一份、AppServer 又写了一份逐行相同的拷贝——仓规 §3.7 已经明令「具名目录 + cursor
分页」的抽象复用 `uva-core`，「不要在自己的 `mod` 里再写一份拷贝」。

我们把整条配方收进 `routes/list/` 一个 crate 私有模块。

## 决定

- **一条路由只声明它的排序键。** 键的线上拼法、比较器、默认方向出自同一处声明，由穷尽
  `match` 保证一致；校验即解析，所以「声明了却不排」**编译不过**。
- **配方吃排序。** 路由交出未排序的行 + 自己的键声明，排序、方向、tiebreaker、切片、信封
  由配方完成。原「调用方 MUST 先排好」的散文前置条件消失——今天的 `paginate` 只读
  `effective_limit` 与 `last_seen_id`，从不读 `sort_field`/`descending`，所以忘了排序不会报错。
- **游标键即 tiebreaker。** 六条路由的取键与 tiebreaker 恰好总是同一个，配方只收一个取键
  闭包，tiebreaker 由构造带上。键相等时按它升序；降序是对 `(键, 游标键)` 整体翻转——保持
  今天两条已被兑现的路由的观感。
- **`ResolvedPage` 原样保留。** 键的解析发生在键类型自己身上，不在页面解析里；`PageQuery` /
  `SessionPageQuery` / `ResolvedPage` / `List` 四个公开类型形状不变，契约不重出。
- **`validate_sort` 删除，`parse_sort` 收私有。** 白名单概念被键类型取代；留着就是第二个入口。
- **切片算术委托给 `uva-core` 的分页原语。** 该原语去掉 `RegistryItem` 约束、改收取键闭包
  （两个生产 adapter 传 `|item| item.name()`）——同一个公开名字的**形状**变更，不是新增名字，
  同步 `docs/api-frozen.md`。
- **`evals/list` 摘掉分页参数。** handler 与 RPC 转发都不再解析页面；它诚实地返回全量，
  `next_cursor` 恒空。客户端本来一个参数都不传。
- **行类型可以不同于线上类型。** 技能的创建时间键是定义文件的 mtime，不在它的线上类型上；
  它排完「(线上行, mtime)」再投影，信封因此需要一个把一行映射成另一行的 `map`（保留游标与总数）。
- **门禁把信封字面量收在配方里。** 新 `check-list-recipe`：列表信封的结构体字面量只允许出现在
  配方模块与信封模块，例外表只许变短。按仓规同步四处（`Makefile` 目标与 `make ci` 聚合、
  `lint-sh` 清单、shellcheck 工作流清单、AGENTS.md §3.6 的 `lint-sh` 行）。

## Considered Options

- **保留 `&[&str]` 白名单 + 路由里另一个 `match`（今天的样子）**：被否。这正是 `agents` 的
  `kind` 坏掉的方式——两处声明可以不一致，而没有任何东西会红。
- **一张 `(&str, comparator)` 表代替类型化键**：被否。表能让校验与比较同行，但键不是类型，
  路由拿不到 `ALL` 的穷尽性，也无法把默认方向挂在键上。
- **`Sorted<T>` 类型见证，调用方仍然自己排序**：被否。它能守住「忘了排序」编译不过，但每条路由
  多写一行、方向与 tiebreaker 仍然是调用方的知识；配方吃排序后这两件事都只剩一份。
- **保留 AppServer 那份切片拷贝，只统一排序**：被否。算术的重复是仓规明确禁止的，而且 core 的
  那份在生产里对这一面是死代码——委托等于把它接回来，而不是新增第三条路。
- **把 `List` 的字段收私有、只经 builder 构造**：被否。`List` 是公开类型且被 `schemars` 导出，
  收字段会牵动 wire 契约；一道 grep 门禁代价小得多。
- **给 `evals` 真做分页**：被否。评测套件是有限小集合，没有分页需求；「解析了再扔掉」才是要修的
  东西。

## Consequences

- **行为变更四条**，全部要进 CHANGELOG 的 Unreleased：`sessions` 省略 `sort` 时从最旧在前变为
  最新在前；`schedules` 的降序开始生效；`agents` 的 `sort=kind` 从 200 变 400；`evals` 不再接受
  分页参数。仓规允许破坏性变更（未到 v1，不留兼容层）。
- **冻结面**：`uva-core::paginate_registry_list` 的形状变更必须同步 `docs/api-frozen.md`；
  没有新增公开名字。`uva-app-server` 不在基线里。
- **测试面**：AppServer 那份游标算术的七个单测删除（算术已委托，`uva-core` 的 registry 测试
  覆盖同一组边界）；新增一条配方接口的测试，与五条路由级顺序断言。`skills` 的比较器单测保留、
  改指到键类型的比较器。
- **词汇表**：「列表配方」「排序键」「游标键」三条进 `GLOSSARY.md`。
- **切面**：⓪ ADR + 词汇表 → ① core 原语去约束 → ② 配方模块 → ③ 六条路由逐条迁移（每条独立
  可评审）→ ④ `evals` 摘参数 → ⑤ 收掉旧入口与算术测试 → ⑥ 门禁。每片独立可评审、可回滚。
- **本 ADR 不裁定的**：`search` / `session_search` 的另一套分页（limit 截断、无游标）不是 cursor
  列表；`rpc/methods` 里五个三行转发 wrapper 的收拢是独立的形状工作。

## 参考

- [ADR-0006](0006-row-vocabulary-owns-the-row.md) 与 [ADR-0015](0015-row-construction-in-one-module.md)：
  同一纪律（一份词汇、穷尽 `match`、不留第二处声明）在行上的落点。
- [ADR-0023](0023-one-owner-for-the-tool-arguments.md)：参数声明归一处，与本次「排序键声明归一处」
  同构。
- [ADR-0025](0025-the-tui-uses-the-server-wire-types.md)：服务端拥有线上形状；本次不新增形状。
- AGENTS.md §3.7「Registry seam 收口在 `uva-core`」——本次是这条约束在列表路由上的落实。
