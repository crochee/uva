# 挂载管线归一个模块：装配变成运行时的路径只有一份

AGENTS.md §3.7 定下「profile HMR 只重挂 diff 的 id」这条不变量，但没有回答**重挂本身怎么
建**。答案是两份手拼的管线：

- boot 走 `composition/kernel` 的 `mount_profile`：`native_factories` → `BootContext`
  （带 `with_blocking` / `with_compaction` / `with_sessions_dir`，来自 `BootExtras`）→
  `build_plugins` → pair 收拢 → `mount_all_with` → `audit_pending`。
- settings 侧 `state/app_state/assembly.rs` 有**另外三个** `build_plugins` 调用点
  （`remount_row` 能力写、`apply` 重挂集、`apply` 内的 rollback 旧集重建），各自手拼
  `BootContext::new(workspace).with_host(..).with_clock(..)`——**没有**那三个 with。

这不是拼写重复，是已经落地的**行为分叉**：`BootContext` 的三个字段有真实消费者——
`search_plugin`（`sessions_dir` 缺省回退 `<workspace>/sessions`，`blocking` 缺省丢阻塞池）、
`session_store_plugin`（同两个字段）、`compaction_plugin`（无 extras 时以 **空 model** 重建）。
所以 HMR 重挂 `context-compactor` 会丢压缩模型，重挂 `search` / `session-store` 会丢阻塞池
与显式会话根。另外 `audit_pending`（停泊行定点计算、`REQUIRED_ROWS` fatal）只在 boot 跑：
settings 加行停泊既无报告也无审计。

我们把「装配 → 挂载」收成 `composition/kernel` 里的**一个 stanza**。

## 决定

- **stanza 是两段，不是一段。** `build_rows`（一个配方：factories + 从 `BootExtras` 铸的
  `BootContext` + `build_plugins` + pair 收拢）与 `mount_built`（一组挂载 + 审计）。两段是
  `apply` 的次序约束要的：**先 build、再 unload、后 mount**——坏行不能拉下在用插件。一个
  原子的 build+mount 函数要么先挂后卸（同 key 双 provide 冲突），要么放弃这条既有保证。
- **`BuiltRows` 不透明。** pair 形状（plugin 值 + `MountRow`）是 stanza 的实现，不是调用方
  知识；它自带审计输入（factory 目录与所建行的 manifest），`mount_built` 审计时调用方无需
  再拼。
- **`mount_profile` = `build_rows` + `mount_built`。** boot 的入口签名不变。
- **`Assembly` 存 boot 的 `BootExtras`**（经 `BootedAssembly` 携带）：每次重挂与 boot 同一
  配方。`AssemblyContext` 的 `workspace_root` 借用删除——配方里有了，不再按次借。
- **审计进 stanza，`REQUIRED_ROWS` 停泊在**任何**挂载点 fatal。**「没有模型循环的 agent
  host 绝不能看起来活着」是进程级不变量，不是 boot 的装饰。`apply` 的审计失败走它既有的
  best-effort rollback 分支（挂载错误与审计错误同一条路）；`remount_row` 返回错误（旧行为：
  挂载失败即 Err，本次一致）。纯**移除**不触发挂载，不审计——`removing_the_provider_row_…`
  钉住的「移除成功、消费侧大声失败」语义原样。

## 参考

- [ADR-0013](0013-controller-state-transition-table-in-one-module.md)、
  [ADR-0011](0011-delegation-discovery-in-one-module.md)：同一纪律（一份规则，不留多个
  写点各写一遍）在控制器状态与委派发现上的落点。
- AGENTS.md §3.7「profile HMR 只重挂 diff 的 id」——本 ADR 不碰 diff 语义，只收**重挂怎么建**。

## Considered Options

- **一个原子的 build+mount 函数**：被否，见上——次序约束是承重的。
- **把 diff / unload / rollback 整个搬进模块**：被否。`Assembly::apply` 是文档化模块
  （挂载树唯一移动者、拥有 raw/effective 两个读模型），diff 本来就只有一份；搬它不增加
  locality。
- **HMR 审计只告警不 fatal**：被否。不变量是进程级的；`apply` 已有 rollback 机器承接。
- **`BootExtras` 塞进 `BootServices`**：被否。containers（注入容器）与 recipe（构建输入）
  是两类事实；`BootServices` 是行注入的宿主，extras 是行构建的环境。

## Consequences

- **行为变更（切面①）**：HMR 重挂的行保住 boot 配方——重挂的 `context-compactor` 保留
  压缩模型，`search` / `session-store` 保留阻塞池与显式 `sessions_dir`。回归测试
  `a_remount_keeps_the_boots_session_corpus` 钉住 sessions corpus 通道（改造前必红）。
- **行为变更（切面②）**：把必需行挂到停泊的 settings 编辑现在**整笔失败并回滚**
  （旧行为：静默停泊 + dep_lost 写回）；`an_edit_that_strands_a_required_row_fails_and_rolls_back`
  钉住。stanza 级 `a_subset_mount_that_parks_a_required_row_is_fatal` 钉住 fatal 在 HMR 路径。
- **`AssemblyContext` 删 `workspace_root` 字段**；`BootedAssembly` 增 `extras`。
- **词汇表**：「挂载」词条补一句——路径只有一份。
- **切面**：⓪ ADR + 词汇表 + CHANGELOG → ① 收拢 + 配方统一 + 回归测试 → ② 审计进
  stanza、fatal 统一、apply 失败走 rollback。每片独立可评审、可回滚。
- **本 ADR 不裁定的**：settings 读模型（`RowCollector`）对 `effective_inject` 的展示——它
  读 manifest 的事实，不是第二份管线；`remount_row` 失败后不回滚旧行——重建用的是同一
  entry，审计结果确定性地相同。
