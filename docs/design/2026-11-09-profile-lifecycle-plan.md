# 刀 7 · Profile 启用位 + 事务性动态变更 + 内置 MCP Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 让 profile 的每一行自带启用位（`true` / `false` / 插件判定），禁用的行不进 Fiber 树（能力与界面同时消失）；让一次 profile 变更成为**事务**（整组提交或整组不生效），结果回报四态 `applied` / `restart-required` / `overridden` / `failed`；让创建 / 删除能力走**内置 MCP 通道**（界面与 agent 都不直接写文件）。

**Architecture:** 全部落在既有 seam 上，**`uva-kernel` 一行不改**。启用位是 profile 数据模型的新字段（`Entry.enabled`），在**装配边界**（build / mount 之前）被求值成"有效行集"（`effective`）：`enabled = false` 或判定不可用的行被过滤掉，于是 enable 翻转天然复用现有 `diff_profiles` 的 `added` / `removed` 语义与 `AppState::reload_profile` 的行级 remount。boot 用**两波挂载**（先挂静态行，再用挂好的 Context 求值判定行并挂第二波）——这是对既有 `mount_all` 的两次普通调用（HMR reload 已经在同一个 Context 上做"boot 组 + reload 组"的多组挂载，语义已被验证）。事务性写入器 `ProfileStore` 是 app-server 内的新模块：编辑文档层 → 内存重解析整层栈 → `overridden` 检测 → **试装**（复用 reload 的 build-before-unload + 失败回滚）→ 成功才**原子替换**正式文件 → 广播 `profile/changed`。内置 MCP = 既有 self-management loopback server（`self_mcp.rs`）加第 10 个工具 `profile`，与新的 JSON-RPC 方法 `profile/*` 共享同一条写入路径；agent 的写动作靠 MCP 协议原生的 `annotations.destructiveHint` → `Risk::High` 走既有审批 stance。

**Tech Stack:** Rust 1.98（stable 通道）、tokio 1.x（既有，按需 feature）、serde / serde_json 1.0、toml（既有 profile 解析依赖）、thiserror 2.0（lib）、anyhow 1.0（仅二进制边界，本刀不新增）

**Spec:** `docs/design/2026-11-09-desktop-frontend-tree-design.md` —— §7 的 D19 / D20 / D21 / D26 / D28 / D32 / D33 / D37 / D38 / D39 / D40，§8 的刀 7，§10 的 R-14 / R-15

## Global Constraints

- **核心一行不动**：本刀不改 `crates/uva-kernel` 的任何文件，不改 Fiber / Effect / Service / Event 语义。全部启用位求值、四态编排、写入事务都落在 `uva-app-server`（宿主层）与两个非 kernel 的既有 crate（`uva-core` 加一个纯新增叶子模块、`uva-skill` 实现第一个判定服务、`uva-mcp` 贯通协议原生的 annotations 字段）。
- **`uva-core` 只加 `pub mod enable;`，不加 crate-root `pub use`**：`scripts/check-frozen-api.sh` 钉的是 lib.rs 的 crate-root `pub use` 名单，`scripts/check-pub-surface-strict.sh` 钉的是 crate-root `pub use` 行数——新契约以模块路径消费（`uva::core::enable::EnableChecks`，照 `uva_core::layout` 的先例），两个基线都**不必**动。新模块的每个 `pub` 项必须有 doc comment（`make check-public-docs` 的 `uva-core` 未文档化计数只降不升，现为 93）。
- 错误处理：lib 用 `thiserror`，app 用 `anyhow`（AGENTS.md §3.1）。`uva-app-server` 是 app crate ⇒ 新模块的对外错误用 `String` / `RpcError`（照 `reload_profile` 的 `Result<ProfileDiff, String>` 现状），不引 anyhow 到 lib 面。
- 依赖版本一律 `major.minor`、复用 `[workspace.dependencies]`；本刀**不新增任何依赖**（toml / serde_json / thiserror 都已在用）。
- tokio feature 按需声明；`uva-app-server` 的 lib 面不出现 `tokio::` 路径（该 crate 本就在 `check-public-api-runtime` 豁免名单里，但新模块照旧不把运行时类型漏到公开面）。
- 禁止 `pub use <mod>::*;` 与通配符导入；禁止 `map_or` / `for_each`（AGENTS.md §3.5）。
- **时间戳一律取 `state.clock`**（`make check-clock` 禁止直接 `Utc::now` / `Local::now`）。
- 单个生产文件的内联 `#[cfg(test)] mod` 不超过 300 行带（`make check-test-layout`）：新模块的测试放**兄弟测试文件**（照 `composition/kernel/tests.rs` 的先例，`#[cfg(test)] mod enabled_tests;`）。
- `uva-mcp` / `uva-skill` 的文件行数不超过 `scripts/plugin-shape/baseline`（现为 1086；两 crate 现最大文件分别是 `supervisor.rs` 620 行与 `provider.rs` 675 行，余量充足）。
- 每次提交前：`cargo +nightly fmt --all` + `cargo clippy --all-targets --all-features --tests --all -- -D warnings`。
- 测试按 crate 分批：`cargo test -p <crate>`（**禁止** `cargo test --workspace`，AGENTS.md §3.3）。
- 不 push 远程仓库；`openspec/` 本地层不入库。
- wire 兼容：`enabled` 缺省 = `true`；旧 profile 文档行为不变（`WireEntry` 无 `deny_unknown_fields`，但本刀显式解析该字段）。

## Review Focus

规格暗示、但现有测试没覆盖、最可能咬人的七处。每处在对应任务里钉上测试或明写取舍。

1. **判定不可用的默认 = fail-closed**（Task 2）：`enabled = { service, check, arg }` 求值时服务键不存在 / 方法未知 / 判定出错 ⇒ 该行**不进装配**，原因进 `profile/list`。理由：启用位是**被委托的开/关决定**（D39），委托人缺席时保守读法是"关"（审批流同理：审批者不可达 ⇒ 不批）；fail-open 会静默复活操作员 / 管理插件关掉的能力，正是 D37 要消灭的"改了但不知道有没有生效"。它的代价由 Task 3 的两波 boot 兜住（见下一条）。
2. **boot 时判定行拿不到判定服务**（Task 3）：求值发生在装配边界，而判定服务由**本代**插件提供——单波挂载时鸡蛋问题永远解不开（boot 必 fail-closed ⇒ 判定行首 boot 即死，且 `ProfileWatcher` 只在文件变化时重评估，永远救不回来）。解法是两波：wave 1 = 静态启用行；wave 2 = 判定行 + （经 manifest `inject`/`provides` 图算出的）静态依赖行；wave 1 挂好后用**同一个 Context** 求值并挂 wave 2。
3. **`hmr = false` 的部署上显式写入必须仍然生效**（Task 3）：今天 `reload_profile` 在 `diff.restart_required`（新 profile 关了 HMR）时早退不动树。零配置部署的 `native_profile()` 就是 `hmr = false`——若写路径复用 `reload_profile`，设置页在默认部署上每次写都变成"要重启"。解法：把挂载编排抽成内部 `reconcile`，`reload_profile`（watcher 入口，保留 HMR 语义）与新 `apply_resolved`（显式变更入口，**不看** hmr 标志）共用它。
4. **试装成功但文件替换失败的窗口**（Task 4）：顺序是"试装 → 原子替换文件"。rename 失败时树已新、文件仍旧——报 `failed` 并明写：`hmr = true` 的部署由下一次 watcher 轮询自愈（重新 resolve 回旧树），`hmr = false` 的部署保持新树直到重启。这是选"先装后写"（D21 原文顺序）的已知代价。
5. **UI 行的过滤必须发生在 `load_ui_manifest` 之前**（Task 3）：`enabled = false` 的 `ui-bundle` 行要"界面同时消失"（D32），过滤要对着 effective 段做，否则 schema 还在渲染。
6. **`mcp` 行的 remount 残留**（Task 4）：`reload_profile` 的文档已声明：重挂 `mcp` 行后 boot 拥有的 tick loop 不会重启，需要进程重启才能重新监督远端服务器。凡 diff 触到 `mcp` 行的变更一律回报 `restart-required`，不谎报 `applied`。
7. **agent 写路径的审批**（Task 5）：用户拍板 #14"agent 改设置走审批 + 审计"。落法是 MCP 协议原生的 `annotations.destructiveHint = true` → `McpTool` 的 `Tool::annotations` → `Risk::from_annotations` 得 `Risk::High` → 既有 approval stance 持有该调用。不新造审批机制；UI 走 JSON-RPC（人的动作，只审计不审批）。

---

### Task 1: `Enabled` 落进 profile 数据模型

**Files:**
- Modify: `crates/uva-app-server/src/profile/model.rs`（`Entry.enabled` / `EntryPatch.enabled` / wire 解析）
- Modify: `crates/uva-app-server/src/profile/merge.rs`（`apply_replacement` 应用 `enabled`）
- Modify: `crates/uva-app-server/src/profile/mod.rs`（`dump` 渲染 `enabled`；导出新类型）
- Modify: `crates/uva-app-server/src/composition/kernel/mod.rs:181`（`native_profile` 的 `Entry` 字面量）
- Modify: `crates/uva-app-server/src/ui/schema.rs:273`（测试助手 `fn entry`）
- Modify: 其余 `Entry` 字面量点（用 `grep -rn 'Entry {' crates/uva-app-server/` 枚举；已知：`src/composition/tests.rs`、`src/composition/kernel/tests.rs`、`src/state/tests/support.rs`、`tests/profile.rs`）
- Create: `crates/uva-app-server/src/profile/enabled_tests.rs`（兄弟测试文件，`profile/mod.rs` 里 `#[cfg(test)] mod enabled_tests;`——Task 2 起共用）

**Interfaces:**
- Consumes: 既有 `WireEntry`（`id` / `runtime` / `config` / `capabilities`，无 `deny_unknown_fields`）、`Profile::from_toml_str`、`Patch::from_toml_str`
- Produces:

```rust
// crates/uva-app-server/src/profile/model.rs
/// One row's enablement: on (default), off, or delegated to a
/// plugin-provided check (D39).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Enabled {
    /// The row assembles. Wire: `true` or absent.
    On,
    /// The row stays out of the fiber tree. Wire: `false`.
    Off,
    /// A plugin answers whether the row assembles.
    /// Wire: `{ service = "...", check = "...", arg = ... }`.
    Check(EnableCheck),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnableCheck {
    /// The kernel service key a judge provides `EnableChecks` under.
    pub service: String,
    /// The method name handed to the judge.
    pub check: String,
    /// The judge's argument (a skill id, a server name, …).
    pub arg: serde_json::Value,
}
```

wire 形状（TOML，`[[core.entries]]` / `[[ui.entries]]` / `[[insert]]` 行内）：

```toml
[core]
entries = [
  { id = "skills", runtime = "native", enabled = false },
  { id = "agent-extra", runtime = "native",
    enabled = { service = "skill-checks", check = "is_enabled", arg = "code-review" } },
]
```

`EntryPatch` 增 `pub enabled: Option<Enabled>`，`[[replace]]` 行可写 `enabled = false` 翻转启用位（`apply_replacement` 按 field 应用）。**既有 `Patch.disabled`（解析期整行移除）语义不变**：patch 的 `disabled` 是"这层声明该行不存在"，行上的 `enabled = false` 是"行在、开关关"——设置页（D26）要能看到后者。

- [ ] **Step 1: 写失败的解析测试**

`crates/uva-app-server/src/profile/enabled_tests.rs`（新）：

```rust
use crate::profile::{EnableCheck, Enabled, Profile};

#[test]
fn enabled_defaults_to_on_and_parses_both_other_forms() {
    let text = r#"
[core]
entries = [
  { id = "a", runtime = "native" },
  { id = "b", runtime = "native", enabled = false },
  { id = "c", runtime = "native",
    enabled = { service = "skill-checks", check = "is_enabled", arg = "x" } },
]
"#;
    let profile = Profile::from_toml_str(text).expect("parse");
    assert_eq!(profile.core[0].enabled, Enabled::On);
    assert_eq!(profile.core[1].enabled, Enabled::Off);
    assert_eq!(
        profile.core[2].enabled,
        Enabled::Check(EnableCheck {
            service: "skill-checks".into(),
            check: "is_enabled".into(),
            arg: serde_json::json!("x"),
        })
    );
}

#[test]
fn a_bad_enabled_shape_is_a_parse_error_not_a_silent_ignore() {
    let text = r#"
[core]
entries = [{ id = "a", runtime = "native", enabled = "maybe" }]
"#;
    assert!(Profile::from_toml_str(text).is_err());
}
```

并在 `profile/mod.rs` 声明 `#[cfg(test)] mod enabled_tests;`。

- [ ] **Step 2: 跑测试确认失败**

Run: `cargo test -p uva-app-server --lib profile`
Expected: FAIL（`no field 'enabled'` / `cannot find type Enabled` 编译错）

- [ ] **Step 3: 实现模型与 wire**

`model.rs`：定义 `Enabled` / `EnableCheck`（含 `Serialize` / `Deserialize`；`Deserialize` 手写——bool 或 table，其它形状报 `ProfileError::Parse`）；`Entry` 增 `pub enabled: Enabled`；`WireEntry` 增 `#[serde(default)] enabled: Option<Enabled>`，`Entry::from_wire` 取 `unwrap_or(Enabled::On)`；`WireEntryPatch` / `EntryPatch` 同理。`merge.rs` 的 `apply_replacement` 应用 `replacement.enabled`。

- [ ] **Step 4: 修全部 `Entry` 字面量并让 dump 渲染启用位**

`Entry` 字面量补 `enabled: Enabled::default()`（`impl Default for Enabled { fn default() -> Self { Self::On } }`）。`dump_entries`（`profile/mod.rs`）在 `enabled != On` 时多打一行 `"enabled": <json>`（`On` 省略 ⇒ 既有 golden 逐字节不变）。

- [ ] **Step 5: 跑本 crate 测试确认通过**

Run: `cargo test -p uva-app-server --lib && cargo test -p uva-app-server --test profile`
Expected: PASS（含既有 `dump_is_deterministic` 等 golden 不变）

---

### Task 2: 判定契约（`EnableChecks`）与求值器（effective）

**Files:**
- Create: `crates/uva-core/src/enable.rs`（新叶子模块，纯新增）
- Modify: `crates/uva-core/src/lib.rs`（仅加一行 `pub mod enable;`，**不加** crate-root `pub use`）
- Create: `crates/uva-app-server/src/profile/enabled.rs`（求值器）
- Modify: `crates/uva-app-server/src/profile/mod.rs`（`mod enabled;` + 导出）
- Modify: `crates/uva-skill/src/plugin.rs`（第一个 judge：提供 `skill-checks` 服务）
- Modify: `crates/uva-app-server/src/composition/kernel/mod.rs`（skill 工厂的 manifest 不动——manifest 由 `SkillPlugin::new` 自带，见下）

**Interfaces:**
- Consumes: `uva::kernel::Context::get::<T>(&str) -> Result<Option<Arc<T>>, PluginError>`（trait-object 服务先例：`Arc<dyn ModelProvider>` @ `KEY_LLM_DEFAULT`、`Arc<dyn Memory>` @ `KEY_MEMORY`）、`PluginFactories::manifest()`（`provides` / `inject`，无需 build 即可读）
- Produces:

```rust
// crates/uva-core/src/enable.rs —— 判定契约，宿主与插件共见
/// A plugin-provided enablement judge (D39).
///
/// A profile row may delegate its enablement to a plugin: the row
/// writes `enabled = { service, check, arg }` and the host resolves
/// `Arc<dyn EnableChecks>` under `service` at the assembly boundary.
pub trait EnableChecks: Send + Sync {
    /// Answer one check. `None` means "not mine / unknown" — the
    /// host treats that as unavailable (fail-closed).
    fn check(&self, method: &str, arg: &serde_json::Value) -> Option<bool>;
}
```

```rust
// crates/uva-app-server/src/profile/enabled.rs —— 装配边界求值器
/// Where the host asks judges; implemented over the mounted kernel
/// context in the app-server, over fixtures in tests.
pub trait CheckSource {
    fn check(&self, service: &str, method: &str, arg: &Value)
        -> CheckAnswer;
}

pub enum CheckAnswer {
    Enabled,
    Disabled,
    Unavailable(String),
}

pub struct SkippedRow {
    pub id: String,
    /// "core" | "ui" — which segment the row belongs to.
    pub segment: &'static str,
    /// Why the row stayed out (off, or the check was unavailable).
    pub reason: String,
}

pub struct EffectiveAssembly {
    /// The profile that will mount / render: On rows plus checks
    /// that answered enabled.
    pub profile: Profile,
    /// Off and fail-closed rows, with reasons, for `profile/list`.
    pub skipped: Vec<SkippedRow>,
}

/// Filter one resolved profile to its effective row set.
#[must_use]
pub fn effective(profile: &Profile, checks: &dyn CheckSource)
    -> EffectiveAssembly;
```

求值语义（写进 `effective` 的 doc comment）：

- `On` ⇒ 行保留；`Off` ⇒ 行进 `skipped`（reason `"disabled"`）。
- `Check { service, check, arg }` ⇒ `checks.check(service, check, arg)`：
  `Enabled` 保留；`Disabled` 进 `skipped`（reason 带判定服务的回答）；
  `Unavailable(reason)` 进 `skipped`（**fail-closed**，reason 记不可用原因，见 Review Focus 1）。
- **core 与 ui 段都过滤**（`ui-bundle` 行的判定同样问 core Context 的服务）。

app-server 侧的 `CheckSource` 实现（放 `state/app_state/mod.rs` 或 `composition`）：

```rust
/// Answers enable checks from the currently mounted kernel context.
struct KernelChecks<'a>(&'a uva::kernel::Context);

impl CheckSource for KernelChecks<'_> {
    fn check(&self, service: &str, method: &str, arg: &Value)
        -> CheckAnswer {
        match self.0.get::<Arc<dyn EnableChecks>>(service) {
            Ok(Some(judge)) => match judge.check(method, arg) {
                Some(true) => CheckAnswer::Enabled,
                Some(false) => CheckAnswer::Disabled,
                None => CheckAnswer::Unavailable(format!(
                    "judge `{service}` does not answer `{method}`"
                )),
            },
            _ => CheckAnswer::Unavailable(format!(
                "no enable-check service `{service}` is mounted"
            )),
        }
    }
}
```

skill judge（`uva-skill/src/plugin.rs`）：`SkillPlugin::apply` 在发布 `KEY_SKILLS`（`"skills"`）之外，再发布 `pub const KEY_SKILL_CHECKS: &str = "skill-checks"` 的 `Arc<dyn EnableChecks>`（实现读 `SkillRegistry` 里该 skill 的 frontmatter `metadata.enabled`，缺省 `true`；未知 skill 返回 `None` ⇒ fail-closed），manifest 的 `provides` 加入该键。设计文档 D39 示例里的 `"skill-registry"` 是示意名；本刀落成 `"skill-checks"`（`"skills"` 已被 `Arc<SkillRegistry>` 占用，一键一型）。

- [ ] **Step 1: 先证明 trait-object 服务查找可用（本刀最关键的前置事实）**

`crates/uva-app-server/src/profile/enabled_tests.rs` 加一个内核级测试：`Context::new()` 上 `mount_all` 一个仅 `provide_arc::<dyn EnableChecks>("t", …)` 的最小插件（用 `uva::kernel::{Context, Plugin, PluginManifest, mount_all}`，照 `src/state/tests/build_prompt_context.rs:25` 的写法），然后断言 `ctx.get::<Arc<dyn EnableChecks>>("t")` 命中。

Run: `cargo test -p uva-app-server --lib profile`
Expected: FAIL（`enable` 模块尚不存在）→ 实现后 PASS。**若此步失败**（cordis 对新 trait 的 TypeId 注册与 `Arc<dyn ModelProvider>` 行为不同），停下来报告——那是需要新核心能力的信号，不得绕过。

- [ ] **Step 2: 落 `uva-core/src/enable.rs`**

只含 `EnableChecks` trait（doc comment 全覆盖）。`lib.rs` 加 `pub mod enable;`（放在 `pub mod layout;` 旁）。不新增依赖（`serde_json` 已在 `uva-core` 的 `[dependencies]`）。

Run: `cargo test -p uva-core --lib`
Expected: PASS

Run: `make check-public-docs`
Expected: `OK`（`uva-core` 未文档化计数不升，仍为 93）

Run: `make check-frozen-api && make check-pub-surface-strict`
Expected: 两个 `OK`（没有 crate-root `pub use` 变更，基线不动）

- [ ] **Step 3: 实现 `effective` 求值器（含 fail-closed 测试）**

`enabled.rs` 按上述接口实现；`enabled_tests.rs` 用一个假 `CheckSource` 覆盖四种回答 × core/ui 两段：On 保留、Off 进 skipped、Check-Enabled 保留、Check-Disabled 进 skipped、**Unavailable 进 skipped（fail-closed）**。

Run: `cargo test -p uva-app-server --lib profile`
Expected: PASS

- [ ] **Step 4: skill judge**

`uva-skill/src/plugin.rs`：`KEY_SKILL_CHECKS` 常量 + manifest `provides` 加入 + `apply` 里 `provide_arc` 一个包着 `Arc<SkillRegistry>` 的判定实现（registry 在 `apply` 里 discover 后即可克隆）。测试：临时目录放一个 `metadata.enabled = false` 的 SKILL.md，断言 `check("is_enabled", "x")` 分别对已知启用 / 已知禁用 / 未知名字返回 `Some(true)` / `Some(false)` / `None`。

Run: `cargo test -p uva-skill --lib`
Expected: PASS

Run: `make check-plugin-shape`
Expected: `OK`（`plugin.rs` 增量远低于 1086 行基线）

---

### Task 3: 装配边界接 effective（两波 boot + reload 拆分）

**Files:**
- Modify: `crates/uva-app-server/src/composition/kernel/mod.rs`（`mount_profile` 两波化）
- Modify: `crates/uva-app-server/src/state/app_state/mod.rs`（`assembly` 字段；`reload_profile` 拆 `reconcile` / `apply_resolved`）
- Modify: `crates/uva-app-server/src/state/app_state/boot.rs`（构造器填 `assembly`；存 `profile_store` 占位 None）
- Modify: `crates/uva-app-server/src/state/app_state/for_test.rs`（fixture 默认值）
- Modify: `crates/uva-app-server/src/state/tests/reload.rs`（effective 感知的用例）

**Interfaces:**
- Consumes: `mount_all(&Context, &[Arc<dyn Plugin>])`（可对同一 Context 多次调用——HMR 已在做 boot 组 + reload 组）、`PluginFactory::manifest()`（不 build 即得 `provides` / `inject`）、`diff_profiles` / `build_plugins` / `rows_for` / `unload_plugin`
- Produces:

```rust
// composition/kernel/mod.rs —— boot 两波
pub(crate) fn mount_profile(
    services: Arc<BootServices>,
    profile: &Profile,        // 已经是 effective 的行集（见下）
    clock: SharedClock,
    extras: BootExtras,
) -> Result<(Context, MountHandle), KernelBootError>;
```

`mount_profile` 内部改为两波（对调用方签名不变，但**传入的 profile 必须已是 effective 行集**；两波在函数内完成）：

1. `partition_waves(rows, factories) -> (wave1, wave2)`：wave 2 = `enabled == Check` 的行 ∪ {静态行：其 `inject` 命中 wave 2 任一行的 `provides`}（沿 manifest 图做不动点闭包；判定行自环 / 环由 `mount_all` 的既有环检测兜底）。`partition_waves` 放 `profile/enabled.rs`（它需要 `PluginFactories`）。
2. `mount_all(ctx, wave1)`（build 用现有 `build_plugins`，输入按波切片）。
3. 用挂好的 `ctx` 做 `KernelChecks(&ctx)`，对 wave 2 的判定行求值（fail-closed 行出局，连同其独占的静态依赖行——依赖行仍挂：它 inject 的服务若由出局行提供则 `mount_all` 会按缺依赖报错，这正是我们想要的失败语义）。
4. `mount_all(ctx, wave2_enabled)`。两波的 `MountHandle` 都返回（boot 失败即进程失败，无需跨波回滚）。

```rust
// state/app_state/mod.rs
pub struct AppState {
    ...
    /// The raw resolved profile (rows carry `enabled`); the settings
    /// page's read model (D26).
    pub profile: parking_lot::RwLock<crate::profile::Profile>,
    /// The effective assembly the mounted tree corresponds to; the
    /// HMR diff's previous side. Boot and every apply update both.
    pub assembly: parking_lot::RwLock<crate::profile::Profile>,
}
```

`reload_profile` 拆分（行为契约照抄现 doc comment，加 effective）：

```rust
/// Internal shared reconcile: diff(prev_effective, next_effective)
/// → build changed → unload removed/remounted → mount as one group
/// → best-effort rollback on mount failure → re-sync provider
/// catalogue → update ui_schemas + assembly. Returns the diff and
/// whether the `mcp` row was touched (restart residual).
async fn reconcile(
    &self,
    next_raw: crate::profile::Profile,
) -> Result<Reconciled, String>;

/// The watcher entry (unchanged semantics): honours `hmr = false`
/// by returning `ProfileDiff::restart_required` untouched.
pub async fn reload_profile(&self, resolved: Profile)
    -> Result<crate::profile::ProfileDiff, String>;

/// The explicit-change entry: same reconcile, ignores the hmr flag
/// (an explicit write is not a passive file watch). `main.rs`'s HMR
/// loop keeps calling `reload_profile`.
pub async fn apply_resolved(&self, resolved: Profile)
    -> Result<Reconciled, String>;

pub struct Reconciled {
    pub diff: crate::profile::ProfileDiff,
    /// The mcp row was remounted/removed: supervision needs a
    /// process restart (see reload_profile's Residuals note).
    pub mcp_touched: bool,
}
```

`reconcile` 的求值：`next_effective = effective(&next_raw, &KernelChecks(&self.kernel))`；`prev_effective = self.assembly.read().clone()`；`diff_profiles(prev_effective, next_effective, factories)`；`rows_for(&next_effective, &changed)`；`load_ui_manifest(&next_effective.ui)`（Review Focus 5）。成功后 `*self.profile.write() = next_raw`、`*self.assembly.write() = next_effective`。

- [ ] **Step 1: 写失败的 reload 测试**

`src/state/tests/reload.rs` 加两组：

(a) `enabled_false_unmounts_the_row`：`state.current_profile()` 克隆，把 `skills` 行置 `enabled = Enabled::Off`，`apply_resolved` 后断言 `diff.removed == ["skills"]` 且 `state.plugin_state("skills").is_none()`、`state.assembly.read().find("skills").is_none()`，而 `state.profile.read().find("skills").is_some()`（行还在，设置页可见）。

(b) `judge_flip_reads_as_presence_change`：fixture 挂一个假 judge（Task 2 Step 1 的最小插件，服务键 `"test-judge"`，回答由 `AtomicBool` 控制），profile 里加一行 `enabled = Check { service: "test-judge", check: "is_enabled", arg: "x" }` 的行（用已有工厂 id，如 `skills` 的克隆行不行——id 必须唯一且工厂已注册；用 `eval` 行替换法或直接改现有行），翻转布尔后断言 diff 的 `removed` / `added`。

Run: `cargo test -p uva-app-server --lib state::tests::reload`
Expected: FAIL（`assembly` / `apply_resolved` 尚不存在）

- [ ] **Step 2: 实现 `partition_waves` 与两波 `mount_profile`**

`profile/enabled.rs` 加 `partition_waves`；`composition/kernel/mod.rs` 的 `mount_profile` 改两波（`composition/kernel/tests.rs` 既有用例补 `enabled` 字段后应全绿——无判定行的 profile 退化为单波，行为不变）。

Run: `cargo test -p uva-app-server --lib composition`
Expected: PASS

- [ ] **Step 3: AppState 加 `assembly`、boot 填充、`reconcile` 拆分**

`state/app_state/boot.rs`：boot 的 `effective_profile` 先过 `effective(raw, &KernelChecks(空))`？——**不**：boot 的两波求值在 `mount_profile` 内部完成，boot 传入 raw（含判定行），mount 完成后 `assembly = effective_profile 过滤后的行集`（`mount_profile` 需要把实际挂上的行集返回，或在 boot 里重算一次 `effective`——取后者，简单且两次求值幂等）。`for_test.rs` / 构造器补默认。`reload_profile` / `apply_resolved` / `reconcile` 按上述契约重构（挂载编排的 90 行逻辑只搬不复制）。

Run: `cargo test -p uva-app-server --lib`
Expected: PASS（含 Step 1 两组新用例与既有 reload 用例）

- [ ] **Step 4: 回归 stdio 集成测试**

Run: `cargo test -p uva-app-server --test stdio_roundtrip`
Expected: PASS（boot 路径改动不破坏分帧协议与 ready 通知）

---

### Task 4: 事务性写入器 `ProfileStore`（四态）

**Files:**
- Create: `crates/uva-app-server/src/profile/store.rs`（文档层编辑 / 序列化 / 层栈重解析——纯函数，无 AppState 依赖）
- Modify: `crates/uva-app-server/src/profile/mod.rs`（`mod store;` + 导出）
- Modify: `crates/uva-app-server/src/state/app_state/mod.rs`（`profile_store` / `profile_write` 字段 + 编排方法）
- Modify: `crates/uva-app-server/src/state/app_state/boot.rs`、`crates/uva-app-server/src/boot.rs`、`crates/uva-app-server/src/main.rs`（把 `ProfileInputs` 的四个路径传进 state）
- Create: `crates/uva-app-server/src/profile/store_tests.rs`

**Interfaces:**
- Consumes: `resolve(&[ProfileSource], Option<Patch>)`（公开，内存层栈）、`Profile::from_path` / `Patch::from_toml_str` / `Profile::from_toml_str`、`apply_resolved`、`Outbound::notify`、`state.clock`
- Produces:

```rust
// profile/store.rs —— 纯文档层操作（可单测）
/// The four layer paths a write must respect (from ProfileInputs).
pub struct ProfileStore {
    pub bundle: Option<PathBuf>,
    /// The profile document layer — the only layer a write touches.
    pub doc: PathBuf,
    pub home: Option<PathBuf>,
    pub overlay: Option<PathBuf>,
}

/// One requested row edit.
pub enum RowEdit {
    SetEnabled { id: String, enabled: Enabled },
    SetConfig  { id: String, config: Value },
}

pub enum CapabilityAction {
    /// Write the layout data file (agent definition / SKILL.md) and
    /// append the matching profile row, then run the transaction.
    Create { domain: CapabilityDomain, definition: Value },
    /// Remove the profile row and the layout data file.
    Delete { domain: CapabilityDomain, id: String },
}

pub enum CapabilityDomain { Agent, Skill }

/// The DSH-style four outcomes (D37), serialized onto both the
/// JSON-RPC result and the self-MCP tool output.
#[derive(Serialize)]
pub struct ChangeOutcome {
    pub state: ChangeState,
    pub reasons: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<crate::profile::ProfileDiff>,
}

#[derive(Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum ChangeState {
    /// Committed and mounted; the next turn uses the new tree.
    Applied,
    /// Committed but supervision needs a restart (the `mcp` row).
    RestartRequired,
    /// A higher layer (home patch / CLI overlay) owns the requested
    /// field; nothing was written or mounted.
    Overridden,
    /// The transaction failed: file unchanged, tree unchanged.
    Failed,
}
```

app-server 编排（`AppState`）：

```rust
pub struct AppState {
    ...
    /// Layer paths for the write path; None in fixtures (writes then
    /// fail with a clear error).
    pub(crate) profile_store: Option<ProfileStore>,
    /// Serializes profile writes (R-14 single writer).
    pub(crate) profile_write: tokio::sync::Mutex<()>,
}

impl AppState {
    /// The one transactional write path both the JSON-RPC methods
    /// and the self-MCP `profile` tool call.
    pub async fn profile_apply(
        &self,
        actor: &str,           // "ui" | "agent" | "mcp"
        action: ProfileAction, // RowEdit or CapabilityAction
    ) -> ChangeOutcome;
}
```

`profile_apply` 的事务顺序（每步的失败都映射四态）：

1. 取 `profile_write` 锁；`profile_store` 为 None ⇒ `failed("no profile document is configured")`。
2. 读文档层（`Profile::from_path(doc)`；文件不存在 ⇒ **物化默认**：把 `state.profile` 的 raw 行集序列化成新文档——零配置部署第一次写开关时生成 `profile.toml`）。
3. 应用编辑 ⇒ 新文档文本（行级 TOML 重排：`toml::Table` + `toml::to_string_pretty`，JSON→TOML 换算照 `translate.rs:337` 的 `json_to_toml` 模式 lift 成 store.rs 私有助手；**已知代价写进模块 doc**：UI/MCP 写入会重排 `profile.toml` 并丢失操作员注释）。`Profile::from_toml_str` 验证新文本，解析失败 ⇒ `failed`。
4. 内存重解析整层栈：bundle patch → 新文档 → home patch → CLI overlay（`resolve`，全内存，不落盘）⇒ `next_raw`。
5. **overridden 检测**：对每个请求字段，`next_raw.find(id)` 的该字段 != 请求值 ⇒ `overridden`（reason 指名 home patch / overlay 的高层覆盖），**不写文件、不动树**。
6. **试装**：`apply_resolved(next_raw.clone()).await`——build 在 unload 之前、mount 失败自动回滚旧行（既有保证）；Err ⇒ `failed`（文件未动、树未动——刀 7 验收第二条）。
7. `reconciled.mcp_touched` ⇒ `restart-required`（reason 引用 `reload_profile` 的 Residuals 说明），否则 `applied`。
8. **提交文件**：写同目录临时文件（`<doc>.tmp-<pid>`）→ `std::fs::rename` 原子替换；io 失败 ⇒ `failed`（Review Focus 4 的窗口，reason 明写自愈路径）。
9. **审计 + 通知**：`tracing::info!(target: "uva::profile", actor, action, ids, state, "profile change")`；`self.rpc.outbound().notify("profile/changed", json!({ "actor": …, "state": …, "ids": …, "at": <state.clock 时间戳> }))`——通知**不带** `session_id`，网关 `gateway.rs:232-234` 的规则会把它广播给全部连接（D33）。

`CapabilityAction` 的落点：`Create { domain: Agent, .. }` 复用 `uva::agents::write(&definition)`（`routes/agents.rs:363` 已用同一写入口）写排布数据文件 + 文档层 `[[insert]]` 对应行，再走同一事务；`Skill` 域写 SKILL.md（复用 `routes/skills.rs` 的 create 逻辑思路）+ 插行。**MCP server 行不做 create/delete**：`mcp.json` 的 `enabled` 位已由 `McpServerEntry::is_enabled`（`uva-mcp/src/config_file.rs`）读取，且 `mcp` 行的 remount 本就 restart-required（Review Focus 6）——写它没有热生效价值，留给后续刀。

- [ ] **Step 1: 写失败的 store 单测（纯函数部分）**

`store_tests.rs` 覆盖：(a) 无文档时物化默认并插入编辑；(b) `enabled` 编辑后 `from_toml_str` round-trip 相等；(c) home patch 覆盖该行 ⇒ 步骤 5 的 overridden 判定函数返回真；(d) 序列化拒绝 TOML 无法表达的值（null / 异构数组）⇒ 报错不落盘。

Run: `cargo test -p uva-app-server --lib profile`
Expected: FAIL（`store` 模块不存在）

- [ ] **Step 2: 实现 `store.rs` 并过单测**

Run: `cargo test -p uva-app-server --lib profile`
Expected: PASS

- [ ] **Step 3: `AppState::profile_apply` 编排 + 四态集成测试**

在 `src/state/tests/` 新增（或并入 reload 测试文件）：tempdir + 真实 `profile.toml` 文件 + `ProfileStore` 构造的状态，覆盖四态各一条：

- `applied`：改一行 config ⇒ 文件内容变化、`assembly` 更新、outbound 收到 `profile/changed`（用 `RpcRuntime::channel()` 观察）。
- `failed`：写入装不起来的行（`config` 违反 manifest schema，如给 `skills` 行塞非法字段）⇒ `Err` 回报、**文件字节不变**、`plugin_state` 不变。
- `overridden`：`UVA_HOME_PROFILE` 指向的 home patch 替换该行 config ⇒ 请求 config 无效。
- `restart-required`：diff 触到 `mcp` 行。

Run: `cargo test -p uva-app-server --lib state`
Expected: PASS

- [ ] **Step 4: 打通 `ProfileInputs` → `AppState`**

`main.rs` 的 `run_core` 把 `ProfileInputs` 四路径装成 `ProfileStore` 传给 `boot_core`（`boot.rs` 的 `boot_core` 加一个 `store: Option<ProfileStore>` 参数，唯一调用点是 `main.rs:288`）；`serve` 模式经 spawn 的 core 同样受益（网关透传 `--profile` 等参数已存在）。

Run: `cargo test -p uva-app-server --lib && cargo build -p uva-app-server`
Expected: PASS / 编译通过

---

### Task 5: 内置 MCP 工具 + JSON-RPC 方法 + 审批贯通

**Files:**
- Create: `crates/uva-app-server/src/rpc/methods/profile.rs`
- Modify: `crates/uva-app-server/src/rpc/methods/mod.rs`（`register_all` 加一行）
- Modify: `crates/uva-app-server/src/rpc/notify.rs`（`pub const PROFILE_CHANGED: &str = "profile/changed";`）
- Modify: `crates/uva-app-server/src/state/app_state/self_mcp.rs`（第 10 个工具）
- Modify: `crates/uva-mcp/src/server.rs`（`ServerTool` 增可选 annotations）
- Modify: `crates/uva-mcp/src/client.rs`（`McpToolSpec` 增可选 annotations，`tools/list` 透传）
- Modify: `crates/uva-mcp/src/tool.rs`（`McpTool` 实现 `Tool::annotations`）

**Interfaces:**
- Consumes: `ServerTool::new(name, description, input_schema, handler)`（`server.rs:110`）、`register_mcp_tools` / `loopback_pair` / `serve`（`self_mcp.rs` 的既有 wiring）、`Risk::from_annotations`（`destructive_hint == Some(true)` ⇒ `Risk::High`，`uva-tool/src/traits.rs:75`）、`Tool::annotations() -> Option<ToolAnnotations>`（默认 `None`）
- Produces（调用链，验收要求的核心）:

```
SPA / 宿主客户端                agent（对话中的 turn）
   │ JSON-RPC                     │ mcp__self__profile 工具调用
   ▼                              ▼
rpc/methods/profile.rs        self_mcp.rs profile 工具 handler
   profile/list|get|set|         action=list|get|set|create|delete
   create|delete                 （写动作带 annotations.destructiveHint
   ▼                              = true → Risk::High → 既有 approval
AppState::profile_apply          stance 持有，审批后才进 handler）
   ▼                              │
   └──────────► 同一条事务写路径（Task 4）◄──────┘
                  │
                  ▼
   文档层编辑 → resolve → overridden 检测 → apply_resolved（试装）
   → 原子 rename → tracing 审计 + profile/changed 广播
```

方法与动作形状（与既有 self-MCP 九工具同款 action 判别式，`self_mcp.rs:129` 的 `action_of`）：

- `profile/list`：raw 行（`state.profile`）+ 每行 effective 状态（`mounted` / `off:<reason>`，来自最近一次 `effective` 的 `skipped`）+ 当前挂载状态（`plugin_state`）。设置页的数据源（D26：界面是 profile 的反推）。
- `profile/get`：单行详情（`{ id }`）。
- `profile/set`：`{ id, enabled?, config? }` → `ChangeOutcome`。
- `profile/create` / `profile/delete`：`CapabilityAction` → `ChangeOutcome`。

uva-mcp 的 annotations 贯通（MCP 协议原生字段，旧 server 不带 ⇒ `None` ⇒ 现状不变）：

```rust
// ServerTool 增 builder；McpToolSpec 增字段并在 tools_list_result /
// tools/list 解析两侧透传；McpTool 增：
impl Tool for McpTool {
    fn annotations(&self) -> Option<ToolAnnotations> {
        self.spec.annotations.clone().map(Into::into)
    }
    ...
}
```

- [ ] **Step 1: uva-mcp annotations 贯通（测试先行）**

`uva-mcp` 既有内联测试（`server.rs` / `client.rs` 的 `mod tests`）加：`ServerTool` 带 `destructive_hint` 时 `tools_list_result` 含 `annotations`；`McpTool::annotations()` 回出该值 ⇒ `Tool::risk(input)` 为 `Risk::High`。

Run: `cargo test -p uva-mcp --lib`
Expected: FAIL → 实现 → PASS

- [ ] **Step 2: RPC 方法**

`rpc/methods/profile.rs` 按 `misc.rs` 的注册式样写五个 handler（`parse::<T>` 入参、`encode` 出参、全部委托 `AppState`）；`register_all` 加 `profile::register(router);`。测试：`RpcRouter` 直连 handler，mock state 断言四态响应形状。

Run: `cargo test -p uva-app-server --lib rpc`
Expected: PASS

- [ ] **Step 3: self-MCP 第 10 个工具**

`self_mcp.rs`：`self_mcp_tools` 加 `profile_tool(state)`（action 判别式照邻居；`list` / `get` 只读，`set` / `create` / `delete` 声明 `destructiveHint = true`）；`src/state/tests/self_mcp.rs` 加一条：工具描述出现在 `tools/list`、写动作带 annotations。

Run: `cargo test -p uva-app-server --lib state::tests::self_mcp`
Expected: PASS

---

### Task 6: 文档与仓库收口

**Files:**
- Modify: `CHANGELOG.md`（`[Unreleased]`）
- Modify: `AGENTS.md` §1（app-server 条目补一句启用位与事务写路径）
- Modify: `crates/uva-app-server/README.md`（profile 段：`enabled` wire 形状、四态、`profile/changed`、内置 MCP 工具）
- Modify: `crates/uva-core/README.md`（若其模块清单逐项列名，加 `enable`；否则跳过——改前先读）

**Interfaces:** 纯文档。

- [ ] **Step 1: CHANGELOG 一条**

```markdown
- **profile 生命周期**：profile 行自带 `enabled`（`true` / `false` / 插件判定
  `{ service, check, arg }`，判定不可用按 fail-closed 不装配）；动态变更成为
  事务（试装 → 原子替换文件），结果回报 `applied` / `restart-required` /
  `overridden` / `failed` 四态；创建 / 删除能力经内置 MCP 通道与
  `profile/*` JSON-RPC 方法走同一条写路径，agent 写动作按 `Risk::High` 走
  既有审批。
```

- [ ] **Step 2: AGENTS.md / README 更新并核对门禁**

Run: `make fmt-check && make check-public-docs && make check-claim-language`
Expected: 全 `OK`（注意 claim-language 门禁：文档不得出现未验证的"已支持/已通过"式断言）

---

### Task 7: 端到端验收（集成测试 + 手工冒烟）

**Files:**
- Create: `crates/uva-app-server/tests/profile_rpc.rs`（照 `tests/stdio_roundtrip.rs` 的 `Core` 驱动器 + `tests/support/mod.rs` 的 `install_test_provider`）

**Interfaces:**
- Consumes: `tests/support` 的 hermetic provider 布置、`stdio_roundtrip.rs` 的 Content-Length 分帧驱动
- Produces: 设计文档刀 7 验收四条的可执行证据

- [ ] **Step 1: stdio 集成测试**

tempdir 里放 `profile.toml`（含一行 `enabled = false` 的 `skills` 行 + 一行判定行），spawn `target/debug/uva-app-server core --directory <temp>`，分帧驱动：

1. `ready` 后发 `profile/list` ⇒ 断言两行都在（含 off 行与 reason）。
2. 发 `profile/set { "id": "skills", "enabled": true }` ⇒ `state == "applied"`；随后 `plugins` 域可见证该能力（如 `tools/list` 出现 `skill` 工具）∧ 收到 `profile/changed` 通知帧。
3. 再发一个装不起来的 `profile/set`（非法 config）⇒ `state == "failed"`；读回 `profile.toml` 字节与写前一致。
4. 再把该行 `enabled` 置 false ⇒ `applied` 后能力消失（`tools/list` 无 `skill`）∧ 第二条 `profile/changed`。

Run: `cargo test -p uva-app-server --test profile_rpc`
Expected: PASS（四条验收对应四组断言）

- [ ] **Step 2: 手工冒烟（网关 + 广播验证）**

```bash
cargo build -p uva-app-server
mkdir -p /tmp/k7/.agents && cp <测试用 profile.toml> /tmp/k7/profile.toml
UVA_BOOT_TEST_KEY=test-key ./target/debug/uva-app-server serve \
  --port 18099 --directory /tmp/k7
```

另开终端用任意 WS 客户端连 `ws://127.0.0.1:18099/rpc`，发 `profile/list` / `profile/set` 文本帧，断言：(a) 响应按 id 回帧；(b) `profile/set` 成功后**同连接**收到 `profile/changed` 通知（网关对无 `session_id` 通知广播，`gateway.rs:232-234`）。Expected: 两条都成立；若 (b) 不成立，报告网关路由事实并修正通知方法名（不得改网关转发语义）。

- [ ] **Step 3: 最终全量门禁（按 crate 分批）**

```bash
cargo +nightly fmt --all
cargo clippy --all-targets --all-features --tests --all -- -D warnings
cargo test -p uva-core --lib
cargo test -p uva-skill --lib
cargo test -p uva-mcp --lib
cargo test -p uva-app-server --lib
cargo test -p uva-app-server --test profile
cargo test -p uva-app-server --test profile_rpc
cargo test -p uva-app-server --test stdio_roundtrip
make ci
```

Expected: 全部 PASS / `OK`（`make ci` 含 frozen-api / public-docs / plugin-shape / clock / test-layout / single-writer 等全部棘轮）。

---

## 附：不改 kernel 的依据（评审用）

- 启用位求值全部发生在 **app-server 的装配边界**（`effective` / `partition_waves` / `reconcile`），kernel 的 `mount_all` / `MountHandle::dispose_plugin` / `Context::{provide_arc, get}` 均按现契约使用。
- 两波挂载 = 对同一 `Context` 的两次普通 `mount_all`；第二次的 `inject` 校验走 kernel 既有的 parent-context 探测（`mount.rs:456` `parent_service_available`），与 HMR reload 组挂到 boot 组旁边是同一操作。
- enable-flip 复用 `diff_profiles` 的 presence 语义（过滤后行消失 ⇒ `removed` / `added`），`watch.rs` 无需感知 `enabled` 字段。
- 判定契约 `EnableChecks` 是**宿主与插件共见的词汇**，落 `uva-core` 新叶子模块（纯新增、零语义改动、不加 crate-root re-export，frozen-api / pub-surface 基线不动）——这是本刀唯一落在 `uva-app-server` 之外的库改动，与"核心（kernel 及其 Fiber/Effect/Service/Event 语义）一行不动"不冲突。
