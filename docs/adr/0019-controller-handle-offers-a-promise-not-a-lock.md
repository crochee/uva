# 控制器的句柄不暴露锁：门是承诺，不是机制

会话控制器对外是一个句柄（`SessionController`），里面装着内层状态（`ControllerInner`）。
有两处，句柄越过了自己的边界，把内层的**表示**摆到了调用方面前：

- `compaction_gate()`（`session/controller/mod.rs`）返回
  `Option<tokio::sync::MutexGuard<'_, ()>>`——这是句柄上**唯一**一个在签名里
  点名锁的类型的方法（仓库里因这两个 API 而在签名里出现 `MutexGuard` 的地方只有
  这一处；调用方的签名一处也没有——泄漏止于被调方的返回类型，而它已经进了发布的
  rustdoc）。
  更要命的是**正确性靠约定**：调用方要把 guard 绑在一个活到函数结束的变量上，
  门才覆盖整段工作。生产调用点（`routes/follow.rs` 的 `/compact`）正是这么写的
  ——`let _compaction_gate = controller.compaction_gate().await;`，靠下划线前缀保住
  生命周期，靠"别提前 drop"守住语义。
- `clear_cached_history()` 直接写 `*inner.cached_history.lock() = None`——**清空
  策略在句柄里定**：内层知道缓存是什么（`CachedHistory`），句柄知道什么时候该丢
  （接管），两边各握一半。

## 决定

- **门变成"工作"形状：`with_compaction_gate(work)`。** 调用方把「这段工作期间不许
  开新 run」交给控制器，控制器保证工作期间门被持有、工作结束门被释放；guard 永不
  越出控制器模块。**"门覆盖整段工作"从此是结构保证，不是约定**——生产调用点不再
  依赖"这个变量别被提前 drop"。
- **两个具名方法落在 `ControllerInner`**（与 ADR-0013 同一纪律：内层拥有状态，方法
  拥有策略，方法返回**领域值**而不是 guard）：
  - `ControllerInner::with_compaction_gate(work)`——门的形状（`tokio::sync::Mutex<()>`）
    与它参与的锁序（外层门，`steer_gate` 在它里面）留在内层；
  - `ControllerInner::clear_cached_history()`——清空策略落在内层，句柄只转发。
- **句柄只叫名字**：`SessionController::with_compaction_gate` 是转发，`clear_cached_history`
  是转发；句柄上不再有返回锁的方法（`AppState::hold_spawn_gate` 是另一个锁——冷开
  门的 spawn gate——且只存在于 `#[cfg(test)]`，不在本次范围）。
- **远端句柄没有门，照跑工作**（行为不变）。远端句柄 `inner` 为 `None`：它不拥有
  run，也没有可串行化的工作，所以工作照跑。`/compact` 对远端会话本来就提前转发给
  所有者，生产路径不走到这个分支；新测试钉住这条语义。
- **不重开 ADR-0013**：它已经定了"转移是调用，不是重新决策"。本次补的是它旁边两个
  钉子：句柄不再重新决定缓存的清空策略，句柄不再把门的机制交给调用方。
- **不加机械门禁**：app-server 是应用 crate（`check-public-api-runtime` 明确豁免它
  的公开签名里的运行时类型），为一个"句柄不返回锁"再开一个脚本门禁，收益不配成本。
  规则写在本 ADR 里，靠评审守。

## 参考

- [ADR-0013](0013-controller-state-transition-table-in-one-module.md)：控制器状态转移表
  归一个模块——同一纪律（内层拥有、方法拥有策略）在控制器上的另一处落点。
- `session/controller/inner.rs` 的两把门的锁序注释（`steer_gate` 是内层门、
  `compaction_pass_gate` 是外层门）：本次没有改动锁序，只是不再让句柄碰到它们。

## Considered Options

- **返回命名包装类型（`CompactionGate<'_>`）**：被否。表示不再越界，但调用方仍握着一
  把"锁"，结构上仍可能提前放门；且要新增一个公开类型（公开面增长）。闭包形状把
  "覆盖整段工作"直接变成类型检查的事实。
- **闭包 + 意图命名（`without_new_runs(work)`）**：被否（作为本次的形状）。意图更清楚，
  但"门"是这套控制器自己的词汇（`compaction_pass_gate`、`steer_gate`），文档已把
  它挡什么、不挡什么写清楚；另起一个意图名会让锁序的两把门在名字上分家。
- **把压缩逻辑搬进控制器，让控制器自己持门**：被否。压缩规划、durable 行与 provider
  的组装是路由层的事（`run_manual_compaction`），搬动它是一次真实的职责搬迁，与本次
  的接口收口无关。
- **把 `cached_history` 字段也收成 inner 私有**：被否（作为本次的范围）。字段是
  `pub(super)`，run task 的两处读写与一个测试伸手都在控制器模块内，表示并没有越出
  模块；把字段收私有要连带改 run task 与测试的读写形状，收益小于本次要的"句柄不碰
  表示"。留作独立的一步。
- **远端句柄改为拒绝执行工作**：被否。这是行为变更（远端句柄今天照跑），且远端根本
  没有门可给——谎称有门不如如实照跑。

## Consequences

- **行为变更零**：门挡的仍然是 run start（不挡 steer、不挡 cancel），三条既有测试
  （`routes/tests/follow.rs` 的 run 不得开始、`controller/tests/cancel.rs` 的 cancel
  不被挡、`controller/tests/parked_prompt.rs` 的 steer 不被挡）改用新形状后逐字保持。
- **新增一条测试**：`session/controller/tests/compaction_gate.rs` 钉住远端句柄
  "无门照跑"。
- **公开面变化**：`SessionController::compaction_gate`（`pub`）被
  `with_compaction_gate`（`pub`）取代——`uva-app-server` 不在冻结面里
  （`docs/api-frozen.md` 只列库 crate），仓内也没有任何 crate 依赖它（应用 crate，
  `publish = false`），故无基线要更新。
- **一处已知事实仍在**：`tokio::sync::MutexGuard` 仍然跨 `await` 被持有——现在在内层
  方法里，而不是在路由里。`AGENTS.md` 那条"不跨 `await` 持有 `MutexGuard`"的理由
  （guard 不是 `Send`）对 tokio 的 guard 不成立（它是 `Send`），本次不改变这个事实，
  只把它挪进它该在的模块。
- **词汇表不动**：门的语义没变（仍只挡 run start），变的只是它怎么被请求到。
