# 刀 B · AppState 的装配生命周期 —— Implementation Plan

> 决策见第一至四轮盘问记录（本仓库无 ADR：装配子状态的形状读代码就能看出来，不满足 ADR 的三条判据）。

## Global Constraints

- **`AppState` 是应用 crate 的内部结构**，不在冻结面上（`scripts/frozen-api/baseline` 只覆盖七个库 crate）。
- **HMR 规则**：AGENTS.md §3.7 —— *profile HMR 只重挂 diff 的 id*。`hmr = false` 的变更只记录「需要重启」，绝不半重载。本切面**不得改变**这条行为。
- **注册表接缝**：`docs/architecture.md`:58 —— *Registry seam 收口 `uva-core`*。装配子状态不得引入第二份游标/分页实现。
- **`uva-kernel` 是框架边界**：`make check-kernel-deps` 禁止它依赖 agent / UI / 传输层。装配子状态含 `ui_tree`，因此**不能**放进 `uva-kernel`。
- **两份读模型语义不同**：`profile` 是原始解析结果（被关掉的行仍在，给设置页读），`assembly` 是生效的那份（给 HMR diff 当「上一次」读）。两者是同一个类型的两个值，但不可合并。
- **禁止 `cargo test --workspace`**：按 crate 跑。

## Review Focus

1. 子状态是不是真的**收窄**了接口，还是只把字段搬了个地方——`AppState` 的公开字段数与公开方法数要真的下降。
2. 两份读模型是不是仍然**各有一个读法**——不能出现「从 `Assembly` 读装配、从 `AppState` 读 profile」的分裂。
3. `for_test` 是不是真的与 `boot` 合流了，而不是变成第三份装配。

---

### Task 1: `Assembly` 子状态

新模块 `crates/uva-app-server/src/state/app_state/assembly.rs`，持有 8 个字段：

`profile`（`mod.rs`:245）、`assembly`（`:249`）、`profile_store`（`:253`）、`profile_write`（`:256`）、`mount`（`:242`）、`kernel`（`:233`）、`boot_services`（`:237`）、`ui_tree`（`:260`）。

接口（四个方法，够用即止）：

|方法|语义|
|---|---|
|`apply(next) -> Result<…>`|一次装配变更：重挂 diff 的 id、更新两份读模型、更新 ui_tree|
|`current() -> &Profile`|生效的那份（`assembly`）|
|`rows() -> Vec<Value>`|原始 profile 的读模型（设置页用）|
|`service<T>(key) -> Option<T>`|从 `kernel` 取挂载好的服务|

`AppState` 上的 8 个字段换成**一个** `pub assembly: Assembly`。

**验证**：`cargo test -p uva-app-server`；`make check-kernel-deps` 绿（子状态在 app-server 内，不触 kernel）。

---

### Task 2: 两份读模型的读法收口

现状：`profile` 经 `current_profile()`（`:404`）读——**生产零调用点**，20 处全在 `src/state/tests/`；`assembly` **连访问器都没有**，在 `mod.rs`:510 与 `profile_apply.rs`:378 直接读字段。

改成：

- `Assembly::current()` 读 `assembly`。
- `Assembly::rows()` 读 `profile`。
- `current_profile()` 变成 `Assembly::current()` 的别名（保留 20 处测试用法），公开面上少一个假装是产品接口的入口。
- `assembly` 字段不再被直接读——两处直读改走子状态。

**验证**：`cargo test -p uva-app-server`；`grep -rn "state\.assembly" crates/uva-app-server/src` → 只剩子状态内部。

---

### Task 3: `for_test` 与 `boot` 合流

两个构造器设置的**字段名差为零**（`boot.rs`:481-528 与 `for_test.rs`:221-273`，同样那 40 个字段），差别只在值。真正共享的是：40 字段字面量本身、`mount_profile` → `effective` → `load_ui_tree` 这一串、`BootServices`、以及若干 `kernel.require` 读取。

`for_test` 还多出一段**被丢弃**的 `agent_registry`（`for_test.rs`:105-113` 与 `:115-122` 完全重复，前一份建完即弃）。

改成：`for_test` 调用 `boot.rs` 的同一个构建函数，只换 profile 文档与 provider。

**验证**：`cargo test -p uva-app-server`；`grep -c "AgentRegistry::new()" crates/uva-app-server/src/state/app_state/for_test.rs` → 1。

---

### Task 4: 验收

1. `cargo test -p uva-app-server` 全绿。
2. `make lint-rust` / `make fmt-check` / `make doc-check` 绿。
3. `AppState` 的公开字段数从 33 降到 25、公开方法数从 27 降到 20（数量写进 changelog）。
4. HMR 行为不变：`state/tests/reload.rs` 的 616-646 行（「enabled = false 把行从装配里滤掉、原始 profile 仍留着」）必须仍然绿。
