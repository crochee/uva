# 刀 8 · UI 树与插槽契约（UI 装载器 + 浏览器 + 终端）Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `ui-bundle` 行从「一串无主组件」升级为**声明插槽的 UI 树**：UI 装载器在激活阶段装配「声明 + 注册」（Effect 语义：注册即返回句柄、drop 即回滚；未声明插槽注册、重复声明已占用插槽、`one` 槽超基数 → 激活直接失败），**两个宿主各渲染一次同一棵树**（浏览器 = DOM、终端 = ratatui）⇒ TUI 因此可扩展。v1 永不执行远端 JS/CSS。

**Architecture:** 装载器住在 app-server（新模块 `crates/uva-app-server/src/ui/tree.rs`）：装配期读 `[ui]` 行 → `UiTree`（插槽表 + 注册表，全部由可撤销句柄持有）。`AppState` 从持 `Vec<UiSchema>` 改为持 `UiTree` —— HMR 换树 = drop 旧树 = 全量回滚。`ui/schema` 的响应在既有 `schemas` 之外**新增**顶层 `slots`（wire 只增不改）。浏览器侧（`uva-web/src/components/ui-schema/`）：新增唯一取数 store 与 `SlotRegion`，只在 kind 注册表（`panel` / `list` / `key-value`）内渲染，未知 kind 走既有 `UnknownKind` 占位。终端侧（`crates/uva-tui`）：定义自己的 wire DTO + ratatui kind 渲染器（D18：共享止于 wire 契约，呈现各写各的）。**核心一行不动**：`ui.*` 服务键的拒绝点已存在（`profile/mod.rs::build_one` → `ProfileError::NamespaceViolation`），本轮只钉测试与文档交叉引用。

**Tech Stack:** Rust（stable 通道）、serde 1.0（workspace 已带 `derive`，`Cargo.toml:142`）、serde_json 1.0、thiserror 2.0（`UiError` 既有）、ratatui 0.29（workspace，`default-features = false, features = ["crossterm"]`）、React 19 + TypeScript + Vite、Playwright MCP（AGENTS.md §4.2 的验证通道，**不建测试套件**）

**Spec:** `docs/design/2026-11-09-desktop-frontend-tree-design.md` —— §6.1「UI 树（两个宿主共用，D25）」、§7 的 D25 / D26 / D32 / D38 / D18、§8 的刀 8 与刀序图（刀 7 → 刀 8）、§2 的 N2 / N5 / N9；Axiom 参考 A6（容器不是插件，内容是插件）/ A7（`docs/superpowers/specs/2026-11-08-axiom-harness-reference.md`，本地规划层）

## Global Constraints

- **核心一行不动**：`crates/uva-kernel/**`、`crates/uva-core/**` 零改动。`ui.` 边界的判定函数
  `namespace_allows` / `UI_PREFIX` / `Namespace` 已存在（`crates/uva-kernel/src/scope.rs:15-39`），
  本轮**不改**，只消费。
- **命名空间边界（既有强制点，本轮写清并钉测试）**——两条边界并列，不要合并：
  1. **核心装载器拒绝任何 `ui.*` 服务键**：`crates/uva-app-server/src/profile/mod.rs::build_one`
     （对 `manifest.provides.iter().chain(manifest.inject.iter())` 逐键检查 `starts_with(UI_PREFIX)`，
     约 `:306-313`）→ 返回 **`ProfileError::NamespaceViolation`**（`profile/model.rs:189`，
     文案 ``plugin `{id}` declares core-side service `{key}`; `ui.` keys are reserved for the
     frontend loader``）。既有钉子：`crates/uva-app-server/tests/profile.rs:465`。
  2. **UI 装载器拒绝 `ui.` 之外的行 id**：`ui/schema.rs::project`（`namespace_allows(Namespace::Ui,
     &entry.id)`）→ **`UiError::NamespaceViolation`**。既有钉子：`ui/schema.rs` 的
     `ui_loader_rejects_core_key` 测试（随 Task 1 迁往 `ui/tree/tests.rs`）。
  3. 配套的路由保证：`profile/model.rs:509` 的 `segment_of` 把 `RuntimeKind::UiBundle` 行**永远**分进
     `Segment::Ui`，所以 `ui-bundle` 行到不了 `build_plugins`（Task 2 补一条回归测试）。
- **插槽 id ≠ 服务键**：slot id（`chat.sidebar`）是行 `config` 里的声明，不进 kernel 的服务注册表，
  不带 `ui.` 前缀，也不受其约束；`ui.` 前缀只作用于**服务键**（`provides` / `inject`）与 **`[ui]` 行 id**。
  插槽 id 与宿主无关（D25）：浏览器渲染成 DOM、终端渲染成 ratatui 部件。
- **wire 只增不改**：`ui/schema` 的 `schemas` 形状与顺序保持
  （每行一个 `{version, components[{slot, kind, props}]}`，按装配序；无组件的行仍产出空 schema ——
  `an_empty_config_is_an_empty_schema` 既有语义），新增顶层 `slots`。服务器**继续不拒未知 `kind`**
  （占位属于宿主的注册表，`registry.ts` 既有立场）。
- **v1 永不执行远端 JS/CSS**：装载器拒绝 `script` / `scripts` / `javascript` / `stylesheet` / `css`
  字段（既有 `reject_executable` 的 `FORBIDDEN` 表，`ui/schema.rs:222-237`）；Task 1 把拒绝范围从
  「行 config 顶层」扩到「行 config 顶层 + 每个 slot 声明 + 每个 component 对象」。
- **错误处理**：`UiError` 沿用 `thiserror`（现状，`ui/schema.rs:46`），不引 `anyhow` 进这个模块。
- **前端路径事实**：前端在仓库根的 **`uva-web/`**（本刀任务描述里的 `crates/uva-web/` 不存在）。
- **前端不建测试套件**（AGENTS.md §4.2）：不写 `*.test.ts`、不建 `uva-web/tests/`、不写任何
  playwright spec 文件；验证 = Playwright MCP 驱动真实浏览器 + 页面内
  `await import('/src/...')` 断言。注意 §4.2 的两个陷阱（后台 tab 的 `visibilityState === 'hidden'`
  会让 app 标 Offline；隐藏 tab 里 `requestAnimationFrame` 永不触发）。
- **D18：不共享呈现**：浏览器与终端各自实现 kind 渲染器；两个宿主共享的只有 `ui/schema` 的
  响应形状。TUI 因此**自己定义** Deserialize DTO，不引 app-server 的 lib 类型（刀 2 计划摘掉这条
  lib 依赖，本轮不回焊）。
- **测试布局门禁**（`make check-test-layout`，`TEST_BLOCK_LIMIT=400`、`TEST_BLOCK_BASELINE=0`，
  即**不允许**新增 ≥300 行的内联测试块）：新模块的测试放同级 `tests.rs`
  （`ui/tree/tests.rs`、`crates/uva-tui/src/ui_schema/tests.rs`；ratatui 断言进既有
  `src/render/tests.rs`）。
- **依赖**：TUI 新增 `serde.workspace = true`（root `Cargo.toml:142` 已带 `features = ["derive"]`，
  `dep.workspace = true` 继承）；其余零新依赖、零 feature 变化。
- 每次提交前：`cargo +nightly fmt --all` +
  `cargo clippy --all-targets --all-features --tests --all -- -D warnings`。
- 测试按 crate 分批：`cargo test -p uva-app-server`、`cargo test -p uva-tui`
  （**禁止** `cargo test --workspace`，AGENTS.md §3.3）。
- 不 push 远程仓库。

## Review Focus

下面五条是本刀最容易咬人的地方，每条都在对应任务里钉上测试。

1. **激活失败必须发生在装载器，不能拖到渲染端**。boot 在
   `state/app_state/boot.rs:270-272` 对装载器错误直接 `?`（进程起不来）；HMR 在
   `AppState::reload_profile`（`state/app_state/mod.rs:467` 起）同样 `map_err(...)?` 后**拒绝换树**。
   两个入口都要有测试：坏行 ⇒ boot 失败 / reload 报错且旧树保留。
2. **drop 即回滚**。`AppState.ui_tree` 换树时旧 `UiTree` drop ⇒ 声明与注册**全部**撤销。句柄语义照
   kernel `EffectHandle`（`crates/uva-kernel/src/effect.rs`：clone 共享守卫，最后一个 drop 才
   回收）。做错的症状：HMR 后 `components_in` 读到两代注册的叠加。钉一条「换树后旧插槽内容不可见」
   的测试（Task 2）。
3. **wire 兼容**。`schemas` 的形状、每行一条、装配顺序都不能变；`routes/ui.rs` 既有测试先迁移到用
   `load_ui_tree` 构造状态，**序列化断言保持原样**再跑（Task 2）。
4. **每实例取数**。`UiSchemaPanel` 今天每次挂载各取一次 schema（设计文档 Q10 点名的债）。`SlotRegion`
   会有多个实例（侧栏 + 详情 + 未放置桶）⇒ store 必须是**唯一取数方**，`SlotRegion` 只读快照
   （Task 3）。
5. **终端布局回归**。空树时 `render::draw` 的约束与缓冲必须与今天**完全一致**（既有
   `render/tests.rs` 不改就该绿）；`chat.sidebar` / `session.detail.extensions` 只在非空时改变布局
   （Task 4）。

---

### Task 1: UI 装载器 —— 插槽表、可撤销句柄与激活失败

**Files:**
- Create: `crates/uva-app-server/src/ui/tree.rs`
- Create: `crates/uva-app-server/src/ui/tree/tests.rs`
- Modify: `crates/uva-app-server/src/ui/mod.rs`（声明 `pub mod tree;`，模块文档写清三个命名空间强制点）
- Modify: `crates/uva-app-server/src/ui/schema.rs`（`UiError` 加 3 个变体；行级装载逻辑迁出到 `tree.rs`）

**Interfaces:**
- Consumes: `crate::profile::{Entry, RuntimeKind}`、`uva::kernel::{Namespace, UI_PREFIX, namespace_allows}`、`schema.rs` 的 `UiSchema` / `UiComponent` / `UiError`
- Produces: `load_ui_tree`（boot 与 HMR 的激活入口）、`UiTree` / `SlotDecl` / `SlotInfo` / `Cardinality` / `SlotHandle` / `ComponentHandle`

```rust
// crates/uva-app-server/src/ui/tree.rs（公开面；私有实现细节注释说明）
pub use super::schema::{UiComponent, UiError, UiSchema};

/// How many components one slot accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cardinality {
    /// Exactly one; a second registration fails activation.
    One,
    /// Any number, in registration order.
    Many,
}

/// One slot a `ui-bundle` row declares.
///
/// Slot ids are host-agnostic (`chat.sidebar` is a DOM region in the
/// browser and a ratatui column in the terminal) and are **not**
/// kernel service keys.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SlotDecl {
    /// The slot id, e.g. `chat.sidebar`.
    pub id: String,
    /// How many components the slot accepts.
    pub cardinality: Cardinality,
}

/// The wire projection of one declared slot (`ui/schema`'s `slots`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SlotInfo {
    /// The host-agnostic slot id.
    pub id: String,
    /// How many components the slot accepts.
    pub cardinality: Cardinality,
    /// The declaring `[ui]` row's id (`ui.`-namespaced).
    pub owner: String,
}

/// The assembled UI tree: declarations and registrations, each owned
/// by a revocable handle (kernel `EffectHandle` semantics — a clone
/// shares the guard, the last drop rolls back).
#[derive(Clone, Default)]
pub struct UiTree { /* Arc<UiTable> + retained handles */ }

impl UiTree {
    pub fn new() -> Self;
    /// Declare one slot. Fails with [`UiError::SlotOccupied`] when the
    /// id is already declared.
    pub fn declare(&self, owner: &str, decl: SlotDecl) -> Result<SlotHandle, UiError>;
    /// Register one component into a declared slot. Fails with
    /// [`UiError::UnknownSlot`] (nothing declares the slot) or
    /// [`UiError::CardinalityExceeded`] (a `one` slot already holds a
    /// component).
    pub fn register(&self, owner: &str, version: u32, component: UiComponent)
        -> Result<ComponentHandle, UiError>;
    /// The declared slots, in declaration order.
    pub fn slots(&self) -> Vec<SlotInfo>;
    /// One `UiSchema` per `[ui]` row, in assembly order. A row without
    /// components keeps its empty schema (today's wire shape).
    pub fn schemas(&self) -> Vec<UiSchema>;
    /// The components delivered into `slot`, in registration order.
    pub fn components_in(&self, slot: &str) -> Vec<UiComponent>;
}

/// Dropping rolls the declaration **and every registration into it** back.
pub struct SlotHandle { /* Arc<UiTable> + slot id */ }
/// Dropping rolls this one registration back.
pub struct ComponentHandle { /* Arc<UiTable> + registration seq */ }

/// The activation stage: read the `[ui]` rows in assembly order,
/// declare each row's slots, then register its components. Any
/// violation fails the whole activation — boot refuses to start and
/// the HMR reload refuses to swap the tree.
///
/// # Errors
///
/// Returns [`UiError`] naming the first row (or component) that
/// cannot be activated.
pub fn load_ui_tree(entries: &[Entry]) -> Result<UiTree, UiError>;
```

`UiError` 新增三个变体（加在 `schema.rs` 既有变体之后，保持 `Debug, Clone, PartialEq, Eq,
thiserror::Error` 派生）：

```rust
/// A row declared a slot that is already declared.
#[error(
    "entry `{id}` declares slot `{slot}`, already declared by `{owner}`"
)]
SlotOccupied {
    /// The declaring row's id.
    id: String,
    /// The contested slot id.
    slot: String,
    /// The row that declared it first.
    owner: String,
},
/// A component addressed a slot no row declares.
#[error(
    "entry `{id}` registers into slot `{slot}`, which no row declares"
)]
UnknownSlot {
    /// The registering row's id.
    id: String,
    /// The undeclared slot id.
    slot: String,
},
/// A `one`-cardinality slot received a second registration.
#[error(
    "entry `{id}` registers into `one` slot `{slot}`, already holding \
     a component from `{owner}`"
)]
CardinalityExceeded {
    /// The registering row's id.
    id: String,
    /// The full slot.
    slot: String,
    /// The row whose component holds it.
    owner: String,
},
```

profile 侧的 wire 形状（`[ui]` 行的 `config`，与既有 `components` 并列）：

```toml
[[ui.entries]]
id = "ui.shell"
runtime = "ui-bundle"
config = { version = 1, slots = [
  { id = "chat.sidebar", cardinality = "many" },
  { id = "session.detail.extensions", cardinality = "many" },
] }

[[ui.entries]]
id = "ui.slot.notes"
runtime = "ui-bundle"
config = { version = 1, components = [
  { slot = "chat.sidebar", kind = "panel", props = { title = "Notes" } },
] }
```

`slots` 与 `components` 都是可选数组（缺省为空）；`cardinality` 缺省为 `"many"`（与今天的
多组件共存行为一致）。

- [ ] **Step 1: 写失败的测试**

`crates/uva-app-server/src/ui/tree/tests.rs`（`tree.rs` 尾部声明 `#[cfg(test)] mod tests;`）。
把 `schema.rs` 既有内联测试（`ui_loader_rejects_core_key` / `unknown_schema_component_falls_back`
/ `ui_loader_rejects_a_non_ui_runtime` / `ui_loader_rejects_an_executable_payload` /
`an_empty_config_is_an_empty_schema`，`ui/schema.rs:265-364`）**原样迁来**，再新增：

```rust
use uva::kernel::RuntimeKind;
use serde_json::json;

use super::{Cardinality, SlotDecl, UiTree, load_ui_tree};
use crate::profile::Entry;

fn row(id: &str, config: serde_json::Value) -> Entry {
    Entry { id: id.to_owned(), runtime: RuntimeKind::UiBundle, config, capabilities: Vec::new() }
}

#[test]
fn a_row_declares_slots_and_a_later_row_registers_into_them() { /* … */ }
#[test]
fn registering_into_an_undeclared_slot_fails_activation() { /* … */ }
#[test]
fn declaring_an_occupied_slot_fails_activation() { /* … */ }
#[test]
fn a_one_cardinality_slot_rejects_a_second_registration() { /* … */ }
#[test]
fn dropping_a_component_handle_rolls_the_registration_back() { /* … */ }
#[test]
fn dropping_a_slot_handle_rolls_the_declaration_and_its_registrations_back() { /* … */ }
#[test]
fn a_cloned_tree_shares_the_table() { /* clone 后另一端注册，本端可见；句柄 drop 遵循最后 drop */ }
#[test]
fn executable_fields_are_rejected_in_rows_slots_and_components() { /* … */ }
#[test]
fn every_row_keeps_its_schema_even_without_components() { /* … */ }
#[test]
fn schemas_are_grouped_by_row_in_assembly_order() { /* … */ }
```

关键断言示例（第一条）：`load_ui_tree(&[shell_row, notes_row])` 成功，
`tree.slots()` 的 id 依声明序为 `["chat.sidebar", "session.detail.extensions"]`，
`tree.components_in("chat.sidebar")` 恰含 notes 行的那条 panel；
`schemas()` 长度 == 行数，且每行的 `version` 落在各自的 `UiSchema` 上。
`cardinality` 缺省断言：不带 `cardinality` 的声明投影出 `Cardinality::Many`。

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p uva-app-server --lib ui::tree`
Expected: FAIL（``file not found for module `tree` ``，编译错误）

- [ ] **Step 3: 实现 `tree.rs` 并迁移 `schema.rs`**

要点：

- **内部表**：`UiTable { rows: Mutex<Vec<(String, u64)>>, slots: Mutex<Vec<SlotRecord>>,
  next_seq: AtomicU64 }`（`std::sync`，不用 tokio —— 装载只在 boot/HMR 发生，读路径取快照）。
  `SlotRecord { decl: SlotDecl, owner: String, registrations: Vec<Registration> }`，
  `Registration { seq: u64, owner: String, component: UiComponent }`。
- `rows` 记录每个被处理的行 `(owner id, version)`（装配序）：`schemas()` 沿它投影，保证
  「每行一个 schema、无组件的行产出空 schema、顺序 == 装配序」。
- `UiTree { table: Arc<UiTable>, retained: Mutex<Vec<Retained>> }`；`declare` / `register`
  拿 `&self`，成功后把句柄克隆进 `retained`（树活着 ⇒ 注册活着；树 drop ⇒ 全部回滚）。
  `Clone` 派生在 `UiTree` 上：`Arc` 共享表、句柄向量克隆（共享守卫，最后一个 drop 回收）。
- 句柄 `Drop`：锁表 → `SlotHandle` 删掉该 `SlotRecord`（其下注册随之消失）；`ComponentHandle`
  按 `seq` 删掉那条注册。回滚失败只可能来自锁中毒，`unwrap` 即可（与 kernel `EffectGuard`
  同立场）。
- **`load_ui_tree` 算法**（逐行，按 `entries` 装配序）：
  1. `namespace_allows(Namespace::Ui, &entry.id)` 否则 `UiError::NamespaceViolation`（迁自 `project`）；
  2. `entry.runtime == RuntimeKind::UiBundle` 否则 `UiError::WrongRuntime`（迁自 `project`）；
  3. `config` 为 `Null` ⇒ 记一行 `(id, 1)` 后继续（`empty_schema` 语义）；非对象 ⇒ `InvalidConfig`；
  4. `reject_executable` 作用域扩为：行 config 顶层 + 每个 slot 声明对象 + 每个 component 对象
     （`FORBIDDEN` 表不变）；
  5. 解析 `version`（缺省 1，非负整数，迁移既有校验）；记录 `(id, version)`；
  6. 解析 `slots`（缺省 `[]`）：每项必须是对象，`id` 非空字符串（复用 `required_string` 风格），
     `cardinality` 缺省 `Many`、取值必须 `"one"` / `"many"` 否则 `InvalidConfig`；
     `tree.declare(&entry.id, decl)?`；
  7. 解析 `components`（既有 `component()` 校验整体迁来）：每项
     `tree.register(&entry.id, version, component)?`。
- **`schema.rs` 瘦身**：保留 `UiSchema` / `UiComponent` / `UiError`（含新变体）/ `UI_NAMESPACE`
  的定义与文档；`load_ui_manifest` / `project` / `component` / `required_string` /
  `reject_executable` / `empty_schema` / `invalid` / `runtime_name` **迁入 `tree.rs`**（私有）。
  若 `load_ui_manifest` 的两个调用方（`boot.rs:270` / `mod.rs:467`）在本任务时尚未迁移，先保留一个
  `pub(crate) fn load_ui_manifest(entries: &[Entry]) -> Result<Vec<UiSchema>, UiError>` 薄包装
  （`= load_ui_tree(entries).map(|t| t.schemas())`），**Task 2 Step 3 删掉**。
- `ui/mod.rs` 加 `pub mod tree;`，并在模块文档里写清三个强制点（见 Global Constraints 的三条引用，
  链接到 `build_one` 与 `ProfileError::NamespaceViolation`）。
- 所有 `pub` 项带 rustdoc 文档注释（app-server 的 lib 面在 `make check-public-docs` 的纪律下）。

- [ ] **Step 4: 跑本模块测试，确认通过**

Run: `cargo test -p uva-app-server --lib ui::`
Expected: PASS（迁移的 5 条 + 新增 10 条全绿）

- [ ] **Step 5: 跑 crate lib 全量，确认没有迁移遗漏**

Run: `cargo test -p uva-app-server --lib`
Expected: PASS

- [ ] **Step 6: 格式与 lint**

Run: `cargo +nightly fmt --all && cargo clippy --all-targets --all-features --tests --all -- -D warnings`
Expected: 无输出（0 warning）

- [ ] **Step 7: 提交**

```bash
git add crates/uva-app-server/src/ui
git commit -m "feat(ui): slot declarations with revocable handles; activation fails on unknown or occupied slots"
```

---

### Task 2: `ui/schema` 扩 `slots`，`AppState` 换持 `UiTree`

**Files:**
- Modify: `crates/uva-app-server/src/routes/ui.rs`
- Modify: `crates/uva-app-server/src/state/app_state/mod.rs`
- Modify: `crates/uva-app-server/src/state/app_state/boot.rs`
- Modify: `crates/uva-app-server/src/state/app_state/for_test.rs`
- Modify: `crates/uva-app-server/src/state/tests/reload.rs`
- Modify: `crates/uva-app-server/tests/profile.rs`

**Interfaces:**
- Consumes: Task 1 的 `UiTree` / `SlotInfo` / `load_ui_tree`
- Produces: `ui/schema` 响应 `{ slots: [...], schemas: [...] }`；`AppState.ui_tree: RwLock<UiTree>`

```rust
// crates/uva-app-server/src/routes/ui.rs
pub struct UiSchemaResponse {
    /// The slots this deployment's `[ui]` rows declare, in declaration order.
    pub slots: Vec<crate::ui::tree::SlotInfo>,
    /// The schemas of every `[ui]` row, in assembly order (unchanged shape).
    pub schemas: Vec<UiSchema>,
}

pub fn list_ui_schemas(state: &AppState) -> UiSchemaResponse {
    let tree = state.ui_tree.read();
    UiSchemaResponse { slots: tree.slots(), schemas: tree.schemas() }
}
```

```rust
// state/app_state/mod.rs（字段替换，:250-251）
/// The `[ui]` segment assembled into the slot tree. Swapping the tree
/// (HMR) drops the old one, which rolls every declaration and
/// registration back.
pub ui_tree: parking_lot::RwLock<crate::ui::tree::UiTree>,
```

- [ ] **Step 1: 写失败的测试**

1. `routes/ui.rs` 的测试模块：既有三条（`an_empty_segment_reports_no_schemas` /
   `the_resolved_schemas_are_projected_in_order` / `the_method_is_served_under_its_wire_name`，
   `routes/ui.rs:28-101`）里 `*state.ui_schemas.write() = vec![...]` 的直接写法改成用
   `crate::ui::tree::load_ui_tree(&[entry(..)])` 构造再 `*state.ui_tree.write() = tree`；
   **既有 JSON 断言保持原样**（`schemas` 形状不变是 Review Focus #3）。新增一条：

```rust
#[tokio::test]
async fn the_response_carries_the_declared_slots() {
    // 两个声明 + 一条注册 ⇒ serde_json::to_value 后
    // body["slots"] == [{"id":"chat.sidebar","cardinality":"many","owner":"ui.shell"}, …]
    // body["schemas"] 形状与既有断言一致
}
```

2. `state/tests/reload.rs` 新增（fixture 用 `AppState::for_test(registry, dir)`，照
   `hmr_remounts_only_the_changed_plugin` 的写法）：

```rust
fn ui_row(id: &str, config: serde_json::Value) -> crate::profile::Entry { /* runtime = UiBundle */ }

#[tokio::test]
async fn hmr_swaps_the_ui_tree_and_the_old_slots_roll_back() {
    // first: shell 行声明 chat.sidebar + notes 行注册一条 panel(title="before")
    //   ⇒ reload 后 components_in("chat.sidebar") 恰一条，props.title == "before"
    // second: 同样的 shell 行 + notes 行注册 panel(title="after")
    //   ⇒ reload 后恰一条，props.title == "after"（旧树已 drop，无叠加）
}

#[tokio::test]
async fn a_bad_ui_segment_is_rejected_and_the_old_tree_stays() {
    // second 里塞一条向未声明插槽注册的行 ⇒ reload_profile 返回 Err
    //   （错误文案含 "which no row declares"），
    //   且 state.ui_tree.read().components_in("chat.sidebar") 仍是旧内容
}
```

3. `crates/uva-app-server/tests/profile.rs` 新增（`segment_of` 的回归钉子）：

```rust
#[test]
fn a_ui_bundle_row_never_reaches_the_core_loader() {
    // TOML：一条 native core 行 + 一条 ui-bundle 行 → resolve 后
    // profile.ui.len() == 1
    // && profile.core.iter().all(|e| e.runtime != RuntimeKind::UiBundle)
}
```

- [ ] **Step 2: 跑测试，确认失败**

Run: `cargo test -p uva-app-server --lib routes::ui state::tests::reload`
Expected: FAIL（`ui_tree` 字段不存在 / `slots` 成员不存在，编译错误）

- [ ] **Step 3: 实现**

- `routes/ui.rs`：`UiSchemaResponse` 加 `slots`，`list_ui_schemas` 读 `state.ui_tree`。
  `rpc/methods/misc.rs:40` 的 `ui/schema` 注册**不动**（它委托给 `list_ui_schemas`）。
- `state/app_state/mod.rs`：字段 `ui_schemas` → `ui_tree: parking_lot::RwLock<UiTree>`；
  访问器 `ui_schemas()`（`:405-408`）改为 `pub fn ui_tree(&self) -> UiTree`（clone 快照），
  旧名删除；`reload_profile`（`:467` 的 `load_ui_manifest(&next.ui)` → `load_ui_tree(&next.ui)`；
  `:535` 的 `*self.ui_schemas.write() = schemas` → `*self.ui_tree.write() = tree`，
  **先建新树成功再换** —— `load_ui_tree` 返回 Err 时函数提前返回，旧树原样保留）。
- `state/app_state/boot.rs:270-272`：`load_ui_manifest(&effective_profile.ui)` →
  `load_ui_tree(&effective_profile.ui)`，`ui_schemas` 变量名与 `:552` 的字段初始化同步改。
- `state/app_state/for_test.rs:256-257`：`ui_schemas: RwLock::new(Vec::new())` →
  `ui_tree: RwLock::new(crate::ui::tree::UiTree::new())`。
- 删除 Task 1 Step 3 保留的 `load_ui_manifest` 薄包装（若有）：
  `grep -rn "load_ui_manifest" crates/uva-app-server/src` 应零命中。

- [ ] **Step 4: 跑受影响测试，确认通过**

Run: `cargo test -p uva-app-server --lib routes::ui state::tests::reload`
Expected: PASS

- [ ] **Step 5: 跑本 crate 全量（含 tests/ 目录）**

Run: `cargo test -p uva-app-server`
Expected: PASS（`tests/profile.rs`、`tests/stdio_roundtrip.rs` 等全绿）

- [ ] **Step 6: 激活失败的负路径冒烟（不起浏览器）**

```bash
tmp=$(mktemp -d)
cat > "$tmp/bad.toml" <<'EOF'
[ui]
entries = [
  { id = "ui.slot.rogue", runtime = "ui-bundle", config = { components = [
      { slot = "no.such.slot", kind = "panel", props = {} },
  ] } },
]
EOF
cargo run -q -p uva-app-server -- --directory "$tmp" --profile "$tmp/bad.toml" core </dev/null
```

Expected: 进程**退出非 0**，stderr 含 `registers into slot` 与 `which no row declares`
（`UiError::UnknownSlot` 文案）。同法各验一次：两行声明同一 slot（期望 `already declared by`）、
`one` 槽注册两条（期望 `already holding a component from`）、config 带 `script` 字段
（期望 `never execute remote JavaScript or CSS`）。

- [ ] **Step 7: 格式与 lint，提交**

Run: `cargo +nightly fmt --all && cargo clippy --all-targets --all-features --tests --all -- -D warnings`
Expected: 无输出

```bash
git add crates/uva-app-server
git commit -m "feat(ui): serve the assembled slot tree; AppState holds UiTree and HMR swaps it atomically"
```

---

### Task 3: 浏览器宿主 —— 单一 store、`SlotRegion` 与两个放置点

**Files:**
- Modify: `uva-web/src/api/ui-schema.ts`
- Create: `uva-web/src/components/ui-schema/store.ts`
- Create: `uva-web/src/components/ui-schema/SlotRegion.tsx`
- Create: `uva-web/src/components/ui-schema/UnplacedSlots.tsx`
- Modify: `uva-web/src/components/chat/SessionSidePanel.tsx`
- Modify: `uva-web/src/pages/SessionDetailPage.tsx`
- Modify: `uva-web/src/components/ui-schema/ui-schema.css`
- Delete: `uva-web/src/components/ui-schema/UiSchemaPanel.tsx`

**Interfaces:**
- Consumes: `api/client.ts` 的 `api.get`（`'GET /api/v1/ui/schema' → ui/schema` 映射已存在，**不动**）、
  既有 `registry.ts`（`KINDS` / `FALLBACK_KIND`）与 `UiSchemaRenderer.tsx`（**不动**）
- Produces: `store.ts`（唯一取数方）、`SlotRegion`（按 slot id 渲染）、`UnplacedSlots`（未放置桶）

```ts
// uva-web/src/components/ui-schema/store.ts —— 纯 TS，不 import React
export type UiSchemaState =
  | { status: 'idle' | 'loading' | 'error'; slots: never[]; schemas: never[]; error: string | null }
  | { status: 'ready'; slots: UiSlot[]; schemas: UiSchema[]; error: null };

/** The slots this build places; everything else lands in UnplacedSlots. */
export const PLACED_SLOTS: readonly string[] = ['chat.sidebar', 'session.detail.extensions'];

export function getSnapshot(): UiSchemaState;
export function subscribe(listener: () => void): () => void;   // 快照不可变，变更才通知
export function refresh(signal?: AbortSignal): void;           // 幂等：loading 中不重复发
export function componentsInSlot(state: UiSchemaState, slot: string): UiComponent[];
export function unplacedComponents(state: UiSchemaState): UiComponent[];
```

```tsx
// SlotRegion.tsx —— 只读快照：useSyncExternalStore(subscribe, getSnapshot)；
// 空插槽渲染 null（区域不占位）；每个组件走既有 UiSchemaRenderer（注册表 + 未知 kind 占位）。
export function SlotRegion({ slot }: { slot: string }): JSX.Element | null;
// 根节点 data-testid={`slot-region-${slot}`}，子项沿用 UiSchemaRenderer 的
// data-testid={`ui-schema-component-${kind}`} 与 UnknownKind 的 ui-schema-unknown。

// UnplacedSlots.tsx —— 渲染 unplacedComponents()，按 slot 分组、组头是 slot id，
// 外壳沿用既有 Section（title="UI extensions"、testId="ui-schema"），fetch 失败时
// 在这里显示错误行（沿用 ui-schema-error 文案）。空则渲染 null。
export function UnplacedSlots(): JSX.Element | null;
```

`api/ui-schema.ts` 扩展（`UiComponent` / `UiSchema` 不动）：

```ts
/** How many components one slot accepts. */
export type Cardinality = 'one' | 'many';
/** One declared slot. */
export interface UiSlot {
  /** The host-agnostic slot id, e.g. `chat.sidebar`. */
  id: string;
  cardinality: Cardinality;
  /** The declaring `[ui]` row's id. */
  owner: string;
}
/** `ui/schema` response body. */
export interface UiSchemaResponse {
  slots: UiSlot[];
  schemas: UiSchema[];
}
```

放置点（两个宿主各一份放置表，这是浏览器这份）：

- `chat.sidebar` → `SessionSidePanel.tsx` 的 `<aside className="nt-chat-panel">` 内、
  三个既有 section（todos / artifacts / sub-agents）之后：`<SlotRegion slot="chat.sidebar" />`
  （`SessionSidePanel` 由 `ChatPage.tsx:2000` 渲染 —— **不改 ChatPage**）。
- `session.detail.extensions` → `SessionDetailPage.tsx:545` 的 `<UiSchemaPanel />` 处替换为
  `<SlotRegion slot="session.detail.extensions" />` + `<UnplacedSlots />`。

- [ ] **Step 1: 扩 API 类型**

按上面的块改 `uva-web/src/api/ui-schema.ts`（文件头注释补一句 `slots` 的来源）。

- [ ] **Step 2: 写 store**

按上面的块写 `store.ts`：模块级单例状态 + 监听器集合；`refresh` 调
`uiSchemaApi.get(signal)`，成功置 `ready`（新对象引用，通知监听器），失败置 `error`
（`AbortError` 静默）。`componentsInSlot` 按 slot 过滤 `schemas.flatMap(s => s.components)`
（与今天 `UiSchemaPanel` 的投影一致）；`unplacedComponents` 过滤 `slot` 不在
`PLACED_SLOTS` 的组件。

- [ ] **Step 3: 写 `SlotRegion` / `UnplacedSlots`，接线并删除 `UiSchemaPanel`**

- `SlotRegion`：首个挂载即 `refresh()`（store 幂等保证多实例只发一次）；渲染
  `componentsInSlot(...)` 的每个组件。
- `UnplacedSlots`：按上面的规格。
- `SessionSidePanel.tsx` / `SessionDetailPage.tsx` 按放置表接线。
- 删除 `UiSchemaPanel.tsx`；`ui-schema.css` 补 `slot-region` 与分组的少量样式
  （复用 `nt-uischema__*` 既有类，别起新视觉体系）。
- 确认无残留引用：`grep -rn "UiSchemaPanel" uva-web/src` 零命中。

- [ ] **Step 4: 前端门禁**

Run: `make check-web`
Expected: `tsc --noEmit` + `eslint` + `prettier --check` 全过（0 错误）

Run: `cd uva-web && npm run build`
Expected: 构建成功退出 0（无 TS / rollup 错误）

- [ ] **Step 5: 起真实后端（带插槽的 profile）**

```bash
tmp=$(mktemp -d /tmp/uva-knife8.XXXXXX)
cat > "$tmp/profile.toml" <<'EOF'
[ui]
entries = [
  { id = "ui.shell", runtime = "ui-bundle", config = { version = 1, slots = [
      { id = "chat.sidebar", cardinality = "many" },
      { id = "session.detail.extensions", cardinality = "many" },
      { id = "debug.dump", cardinality = "many" },
  ] } },
  { id = "ui.slot.notes", runtime = "ui-bundle", config = { version = 1, components = [
      { slot = "chat.sidebar", kind = "panel", props = { title = "Notes", body = "knife 8 fixture" } },
      { slot = "chat.sidebar", kind = "weather-card", props = { city = "Berlin" } },
      { slot = "session.detail.extensions", kind = "key-value", props = { title = "Facts", entries = { build = "knife8" } } },
      { slot = "debug.dump", kind = "list", props = { title = "Dump", items = ["one", "two"] } },
  ] } },
]
EOF
cargo run -q -p uva-app-server -- --directory "$tmp" --profile "$tmp/profile.toml" serve --port 8080
```

Expected: 网关起来，`curl -s localhost:8080/readyz` 最终 200（core 宣布 ready 后）；
浏览器 WS 上的 `ui/schema` 会同时带回 `slots`（3 个）与 `schemas`（2 行）。

- [ ] **Step 6: Playwright MCP 验证（AGENTS.md §4.2，不写 spec 文件）**

1. 另开终端 `cd uva-web && npm run dev`（Vite 5173，`vite.config.ts` 已代理 `/rpc` ws 与
   `/readyz` 到 8080）。
2. **先处理后台 tab 陷阱**：用 `browser_run_code_unsafe` 在导航前
   `page.addInitScript(...)` 把 `document.visibilityState` 固定为 `'visible'`、`hidden` 为
   `false`，并 `bringToFront`；或直接 mock `/readyz` 返回 200。验证里**不要**用
   `requestAnimationFrame` 等待（用 `setTimeout` / `browser_wait_for`）。
3. `browser_navigate` → `http://localhost:5173/`，进一个会话（ChatPage）后断言：
   - `[data-testid="slot-region-chat.sidebar"]` 存在，且位于
     `[data-testid="session-side-panel"]` 内；
   - 其中含 `[data-testid="ui-schema-component-panel"]`（title 为 Notes）与
     `[data-testid="ui-schema-component-weather-card"]`，后者内部是
     `[data-testid="ui-schema-unknown"]`（未知 kind 占位，可见而非致命）；
   - 进会话详情页：`[data-testid="slot-region-session.detail.extensions"]` 含
     `[data-testid="ui-schema-key-value"]`；
   - 未放置桶：`UnplacedSlots` 的分组容器（`testId="ui-schema"` 下的分组）列出
     `debug.dump` 组及其 `ui-schema-component-list`。
4. 纯函数按 §4.2 用页面内 import 断言（`browser_evaluate`）：

```js
const { getSnapshot, refresh, componentsInSlot, unplacedComponents, PLACED_SLOTS } =
  await import('/src/components/ui-schema/store.ts');
refresh();
await new Promise((r) => setTimeout(r, 600));
const s = getSnapshot();
// 断言：s.status === 'ready'
//       s.slots.map(x => x.id) 深等于 ['chat.sidebar','session.detail.extensions','debug.dump']
//       componentsInSlot(s, 'chat.sidebar').length === 2
//       unplacedComponents(s).map(c => c.slot) 深等于 ['debug.dump']
//       JSON.stringify(s) 不含 '"script"'（wire 上没有任何可执行字段）
```

Expected: 全部断言为真。

- [ ] **Step 7: 提交**

```bash
git add uva-web/src
git commit -m "feat(web): SlotRegion renders the declared slots; one store fetches ui/schema for every region"
```

---

### Task 4: 终端宿主 —— wire DTO、`RequestKind::UiSchema` 与 ratatui 渲染

**Files:**
- Create: `crates/uva-tui/src/ui_schema.rs`
- Create: `crates/uva-tui/src/ui_schema/tests.rs`
- Modify: `crates/uva-tui/src/lib.rs`（`pub mod ui_schema;`）
- Modify: `crates/uva-tui/src/transport.rs`（`RequestKind::UiSchema` + `Request::ui_schema()`）
- Modify: `crates/uva-tui/src/app.rs`（`Action::FetchUiSchema`、`App.ui` 字段、`apply_ready`、`apply_success`）
- Modify: `crates/uva-tui/src/app/tests.rs`
- Create: `crates/uva-tui/src/render/ui_slots.rs`
- Modify: `crates/uva-tui/src/render.rs`（布局 + `mod ui_slots;`）
- Modify: `crates/uva-tui/src/render/tests.rs`
- Modify: `crates/uva-tui/Cargo.toml`

**Interfaces:**
- Consumes: `ui/schema` 的响应形状（Task 2 定的 wire；D18 ⇒ TUI 自己定义 DTO，不引 app-server lib）
- Produces: 终端的 UI 树投影与渲染入口（`chat.sidebar` = 右列，`session.detail.extensions` = 底部块）

```rust
// crates/uva-tui/src/ui_schema.rs —— 纯数据，无 I/O
use serde::Deserialize;

/// The slots this build places. Slot ids are host-agnostic (D25): the
/// same id the browser renders as DOM renders here as ratatui widgets.
pub const SLOT_CHAT_SIDEBAR: &str = "chat.sidebar";
pub const SLOT_SESSION_EXTENSIONS: &str = "session.detail.extensions";
/// Everything declared but not in this list lands in the one-line
/// unplaced summary.
pub const PLACED_SLOTS: [&str; 2] = [SLOT_CHAT_SIDEBAR, SLOT_SESSION_EXTENSIONS];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Cardinality { One, Many }

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct UiSlot { pub id: String, pub cardinality: Cardinality, pub owner: String }

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UiComponent {
    pub slot: String,
    pub kind: String,
    #[serde(default = "serde_json::Value::null")]
    pub props: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct UiSchema { pub version: u64, #[serde(default)] pub components: Vec<UiComponent> }

/// The `ui/schema` response, as the terminal host reads it.
#[derive(Debug, Clone, PartialEq, Default, Deserialize)]
pub struct UiSchemaResponse {
    #[serde(default)] pub slots: Vec<UiSlot>,
    #[serde(default)] pub schemas: Vec<UiSchema>,
}

impl UiSchemaResponse {
    /// The components delivered into `slot`, in registration order.
    #[must_use] pub fn components_in(&self, slot: &str) -> Vec<&UiComponent>;
    /// The slot ids that carry components this host does not place.
    #[must_use] pub fn unplaced_slots(&self) -> Vec<String>;
}
```

transport / reducer：

```rust
// transport.rs —— RequestKind 枚举加一个成员（method() 映射 "ui/schema"），Request 加构造器
pub enum RequestKind { /* …既有 7 个… */ /// `ui/schema`. UiSchema }
impl RequestKind { /* … */ Self::UiSchema => "ui/schema", }
impl Request { /// `ui/schema`（无参数）。 #[must_use] pub fn ui_schema() -> Self { /* … */ } }

// app.rs
pub enum Action { /* …既有… */ /// `ui/schema`. FetchUiSchema }
impl Action { Self::FetchUiSchema => Request::ui_schema(), }
pub struct App { /* …既有字段… */ ui: crate::ui_schema::UiSchemaResponse, }
impl App {
    /// The terminal host's UI tree (empty until the first `ui/schema` answers).
    #[must_use] pub fn ui(&self) -> &crate::ui_schema::UiSchemaResponse;
}
// apply_ready：在它返回的 action 向量末尾追加 Action::FetchUiSchema（每代 core 就绪后取一次，
//   UI 树与具体会话无关）。
// apply_success：新增 RequestKind::UiSchema 分支 —— serde_json::from_value::<UiSchemaResponse>
//   成功则替换 self.ui；失败只写 status（"the ui schema was unreadable: …"），不让 TUI 崩。
```

ratatui 渲染（`render/ui_slots.rs`，`render.rs` 声明 `mod ui_slots;` 并保持 `pub fn draw` 签名不变）：

```rust
/// The kind registry, mirroring the browser's kinds.tsx (D18: each
/// host owns its presentation; only the wire is shared).
/// panel → 标题行 + 正文行 + "• item" 行；list → 标题 + "• item" 行；
/// key-value → 标题 + "key: value" 行；未知 kind → 一行 "? <kind>"（占位，同 UnknownKind 立场）。
pub(crate) fn component_lines(component: &UiComponent) -> Vec<Line<'static>>;
/// The `chat.sidebar` column: a bordered block titled by the slot id,
/// stacking every component's lines.
pub(crate) fn draw_sidebar(frame: &mut Frame, area: Rect, components: &[&UiComponent]);
/// The `session.detail.extensions` block plus the one-line unplaced summary.
pub(crate) fn draw_extensions(frame: &mut Frame, area: Rect, app: &App);
/// The extensions block's height (components stacked, capped at 8 lines).
pub(crate) fn extensions_height(components: &[&UiComponent]) -> u16;
```

`render::draw` 的布局变化（Review Focus #5）：

- 中区（今天的 `areas[1]`）在 `components_in(SLOT_CHAT_SIDEBAR)` 非空时按
  `Layout::horizontal([Constraint::Min(40), Constraint::Length(34)])` 拆出右列；空时**不拆**。
- 垂直约束从固定的 `[Length(1), Min(3), Length(3)]` 变为动态：extensions 非空时在
  transcript 与 composer 之间插入 `Length(extensions_height(...))`；空时与今天**逐项相同**。

- [ ] **Step 1: Cargo 依赖**

`crates/uva-tui/Cargo.toml` 的 `[dependencies]` 加（注释说明：wire DTO 的反序列化）：

```toml
# The `ui/schema` wire DTOs deserialize through serde's derive.
serde.workspace = true
```

（root `Cargo.toml:142` 已是 `serde = { version = "1.0", features = ["derive"] }`，
`workspace = true` 继承 feature，**不**在 crate 内重复声明版本。）

- [ ] **Step 2: 写失败的测试（DTO 与 reducer）**

`ui_schema/tests.rs`：

```rust
use super::*;
use serde_json::json;

#[test]
fn the_wire_response_deserializes() { /* json!({...}) → UiSchemaResponse：3 slots、2 schemas、组件按注册序 */ }
#[test]
fn components_in_returns_registration_order() { /* … */ }
#[test]
fn unplaced_slots_names_only_unplaced() { /* debug.dump 在、chat.sidebar 不在 */ }
#[test]
fn a_missing_slots_member_defaults_to_empty() { /* 只有 schemas 的响应也能解（向后兼容旧服务器） */ }
```

`app/tests.rs` 新增：

```rust
#[test]
fn ready_fetches_the_ui_schema() { /* apply(Ready{..}) 的返回值包含 Action::FetchUiSchema */ }
#[test]
fn a_ui_schema_response_replaces_the_tree() {
    /* apply(Response{request: Some(UiSchema), Ok(json)}) 后
       app.ui().components_in("chat.sidebar") 可见 */
}
#[test]
fn an_unreadable_ui_schema_only_notes_the_status() { /* Ok(json!("not-an-object")) 不 panic、status 有提示 */ }
```

`render/tests.rs` 新增（照既有 `TestBackend` 写法）：

```rust
#[test]
fn an_empty_ui_tree_leaves_the_layout_untouched() {
    // 空树 draw 后：缓冲不含 "Notes"，Conversation 边框宽度 == 终端宽度（没有右列）。
}
#[test]
fn a_chat_sidebar_slot_renders_a_right_column() {
    // 注入含 panel(title="Notes") 的树 → 缓冲含 "Notes"，Conversation 边框宽度 < 终端宽度。
}
#[test]
fn an_unknown_kind_renders_a_placeholder_line() {
    // kind = "weather-card" → 缓冲含 "? weather-card"。
}
#[test]
fn session_extensions_render_below_the_transcript() {
    // 注入 session.detail.extensions 的 key-value → 缓冲含其标题行与 "build: knife8"；
    //   再放一条 debug.dump → 缓冲含未放置摘要行（含 "debug.dump"）。
}
```

（`render/tests.rs` 既有测试**不改** —— 它们跑空树，必须原样保持绿，这就是 Review Focus #5 的钉子。）

- [ ] **Step 3: 跑测试，确认失败**

Run: `cargo test -p uva-tui`
Expected: FAIL（`ui_schema` 模块不存在 / `RequestKind::UiSchema` 不存在，编译错误）

- [ ] **Step 4: 实现**

按 Interfaces 的三个代码块实现：`ui_schema.rs`（DTO + 查询）、`transport.rs`（枚举成员 +
构造器，照 `Request::events()` 的写法）、`app.rs`（字段 + accessor + `apply_ready` 追加 +
`apply_success` 分支）、`render/ui_slots.rs`（kind 注册表 + 两个绘制函数）、`render.rs`
（动态布局 + `mod ui_slots;`）。`lib.rs` 加 `pub mod ui_schema;`（模块文档写明：终端的一棵
UI 树，D25；slot id 与浏览器同源）。

- [ ] **Step 5: 跑本 crate 测试，确认通过**

Run: `cargo test -p uva-tui`
Expected: PASS（既有 reducer / render / transport 测试 + 新增全绿）

- [ ] **Step 6: 终端手工冒烟（同一份 profile ⇒ 两个宿主拿到同一棵树）**

沿用 Task 3 Step 5 的 `$tmp/profile.toml`：

```bash
cargo build -p uva-app-server
UVA_APP_SERVER=target/debug/uva-app-server \
  cargo run -p uva-tui -- --directory "$tmp" --profile "$tmp/profile.toml"
```

Expected: 对话视图右侧出现 `Notes` 面板列（`chat.sidebar`）；未知 kind 显示 `? weather-card`；
发一轮对话确认 transcript / composer 布局不塌；`Ctrl-C` 退出后终端恢复正常。

- [ ] **Step 7: 格式与 lint，提交**

Run: `cargo +nightly fmt --all && cargo clippy --all-targets --all-features --tests --all -- -D warnings`
Expected: 无输出

```bash
git add crates/uva-tui
git commit -m "feat(tui): render the shared slot tree — chat.sidebar as a column, extensions under the transcript"
```

---

### Task 5: 收口 —— 文档、CHANGELOG 与全量门禁

**Files:**
- Modify: `crates/uva-app-server/README.md`（`[ui]` 段落，`:189-193`）
- Modify: `crates/uva-tui/README.md`
- Modify: `CHANGELOG.md`（`[Unreleased]`）

**Interfaces:**
- Consumes: Task 1–4 的全部产物
- Produces: 与代码一致的入库文档；`make ci` 全绿

- [ ] **Step 1: 改 `crates/uva-app-server/README.md` 的 `[ui]` 段**

把 `:189-193` 那段扩写为（保持中文、同样的短段落风格）：

```markdown
`[ui]` 条目必须是 `ui.` 命名空间、`ui-bundle` 运行时；行的 `config` 可以声明插槽
（`slots = [{ id = "chat.sidebar", cardinality = "many" }]`，`cardinality` 取 `one` / `many`，
缺省 `many`）并向已声明的插槽注册组件（`components = [{ slot, kind, props }]`）。插槽 id 与宿主
无关：浏览器渲染成 DOM、终端渲染成 ratatui 部件，两个宿主读同一份 `ui/schema`
（`{ slots: [{id, cardinality, owner}], schemas: [{version, components}] }`）。装配是 Effect 语义：
声明与注册各持一个句柄，drop 即回滚；向未声明的插槽注册、重复声明已占用的插槽、`one` 槽注册第二条，
都在激活阶段（boot / HMR reload）直接失败。核心装载器拒绝任何 `ui.*` 服务键
（`build_one` → `ProfileError::NamespaceViolation`）。前端只渲染注册表内的 kind
（`panel` / `list` / `key-value`），未知 kind 显示占位；v1 永不执行远端 JavaScript 或 CSS，
装载器拒绝 `script` / `scripts` / `javascript` / `stylesheet` / `css` 字段。
```

- [ ] **Step 2: 改 `crates/uva-tui/README.md`**

新增一小节（放在传输 / 渲染说明之后，风格照邻居）：终端也有一棵 UI 树（D25）—— 每代 core
`ready` 后取一次 `ui/schema`，`chat.sidebar` 渲染为对话右侧列、`session.detail.extensions`
渲染为 transcript 下方的块，未知 kind 显示 `? <kind>` 占位行；新增一个 UI 插件 = 一行 profile
+ 一份组件数据，终端代码零改动。

- [ ] **Step 3: 改 `CHANGELOG.md` 的 `[Unreleased]`**

一条即可：

```markdown
- **UI 树与插槽契约（刀 8）**：`ui-bundle` 行声明插槽（`chat.sidebar`、
  `session.detail.extensions`…，id 与宿主无关），UI 装载器以 Effect 语义装配（注册即句柄、
  drop 即回滚，未声明插槽 / 重复声明 / `one` 超基数在激活阶段失败）；`ui/schema` 新增
  `slots`；浏览器（`SlotRegion`）与终端（ratatui 右列 / 底部块）各渲染一次同一棵树，TUI 因此
  可扩展。v1 仍永不执行远端 JS/CSS。
```

- [ ] **Step 4: 分批跑两个 crate 的测试**

Run: `cargo test -p uva-app-server && cargo test -p uva-tui`
Expected: 两个 crate 全部 passed

- [ ] **Step 5: 全量门禁**

Run: `make ci`
Expected: 全绿（`check-test-layout` 不新增 ≥300 行内联块；`check-claim-language` 不受影响 ——
本刀只改 README 的事实陈述，不写绝对化声明；`check-public-api-runtime` 不涉及 —— 两个 crate
本就在豁免/合规名单内且未新增 tokio 路径）。

- [ ] **Step 6: 提交**

```bash
git add crates/uva-app-server/README.md crates/uva-tui/README.md CHANGELOG.md
git commit -m "docs: the slot tree contract — one ui/schema, two hosts, effect-scoped registrations"
```
