# profile 文档成为插件树的唯一真源（dsh entry 模型移植）

> 状态：已实现（2026-11-16 起）。用户裁决：除 id 寻址外，全部采用 dsh 的方案。
> 上游依据：DeepSeek Harness（`~/workspace/deepseek-harness`）的 loader 与
> cordis 实现，源码行号随文标注。

## 1. 意图

一份部署描述在一处读完：装哪些插件、每个插件依赖谁、每个插件开不开。结构与
dsh 的 entry 模型同构，装载与覆盖逻辑照抄它的形状。

## 2. 决定

### 2.1 行形状 = dsh `EntryOptions` 的直译

dsh（`vendor/loader/src/config/entry.ts:9-23`）：

```text
EntryOptions { id, name, config?, group?, disabled?, inject? }
```

我们的 `Manifest`（`crates/uva-app-server/src/profile/model.rs`）：

```text
Manifest {
    id: String,                    // 行身份，文档内全局唯一
    name: String,                  // 装哪个插件：工厂目录的键
    config: Value,                 // Null = 无意见；组行的 config = 子行数组
    capabilities: Vec<Capability>, // uva 扩展：宿主能力授权
    group: bool,                   // 嵌套组（子行在 children / config 数组）
    children: Vec<Manifest>,       // 组行的子行（线上是 config 数组）
    disabled: bool,                // 本行（组行则含后代）不进装配
    inject: Option<Vec<String>>,   // None = 无意见；Some = 并入插件默认
    intercept: Option<Map<String, Value>>,  // 服务键 → 拦截配置
    isolate: Option<Map<String, IsolateLabel>>, // true = 行内独占；字符串 = 共享标签
}
```

与 dsh 的差异只有两处：**id 全文档唯一**（dsh 为树内局部 id + `:` 路径，
用户裁决保留唯一 id）与 **capabilities**（uva 的宿主能力门禁扩展）。

### 2.2 有效边 = 插件 manifest ∪ 文档

dsh 的 `Inject.resolve(entry.options.inject, fiber.inject)`
（`vendor/loader/src/index.ts:133`，`vendor/cordis/src/registry.ts:71`）只写进、
从不清空，同名键条目赢。我们的等价物在装载边界（`profile/mod.rs` 的
`effective_inject`）：工厂 manifest 的 `inject` 在前，行声明的键在后，同名键
以行为准。文档只能**加**依赖，挖不掉插件的固有最小依赖。

### 2.3 缺依赖挂起 + 开机审计

dsh 的 fiber 缺服务时挂 PENDING（`vendor/cordis/src/fiber.ts`），不等服务就
不激活。我们照抄：内核撤掉静态「缺依赖即整组拒绝」（`uva-kernel/src/mount.rs`
不再有 `check_dependencies`），`mount_all_with` 接受 Pending（`PluginState::Pending`），
拓扑排序保留（缺 provider 的键在排序中被跳过）。开机后 audit_pending
（`composition/kernel/mod.rs`）逐行列出挂起的行与它缺的服务名；只有
`agent-loop` 与 `model-adapter` 挂起才致命。

### 2.4 组 = 运行时插件

dsh 的组由 `Group` 插件承载（`vendor/loader/src/config/group.ts:76`），子行是
那个插件的配置。我们的等价物：`GroupShell`（`profile/mod.rs`）在自己的 fiber
下用内核的子挂载原语（`uva::kernel::mount_all_with`）挂子行；组的 fiber 拥有
子树的生命周期，卸载组即卸载子树。组行自身恒视为启用（dsh `entry.ts:76`），
`disabled` 只作用于后代。

### 2.5 isolate / intercept

dsh 的 `isolate?: Dict<true | string>`（`vendor/loader/src/config/isolate.ts`）
用 Realm 把服务键重映射到独占/共享槽；`intercept?: Dict` 把拦截配置挂进上下文
链。cordis-rs 0.6.2 已有对应原语：`isolate_with` / `isolate_many` /
`new_isolation` / `intercept_value` / `intercepts`（`uva-kernel/src/mount_ctx.rs`
的 `Realms` + `derive_context` 在挂载时按行派生上下文）。共享标签在进程内稳定
（`Realms` 由 boot 与 HMR 共用）。

### 2.6 运行时写回

dsh 的 loader 在一行因丢依赖被卸载时把它写回 `disabled = true`
（`vendor/loader/src/index.ts:156-173`）。我们照抄：装配层算出「因 provider
被移除而失去依赖」的行（`assembly.rs` 的 `dep_lost_rows`），写事务与热重载两条
路径都把它们写回文档（`disabled = true`）。

### 2.7 其余对齐项

- **disabled**：布尔位，祖先链继承；旧的三态 `enabled`（判定表）与判定器、
  两波挂载机制整块删除。
- **HMR**：diff 的单位是顶层行（组 = 一棵子树）；`inject` / `disabled` /
  `intercept` / `isolate` 参与行的身份；级联重挂按**有效边**算依赖方。
- **序列化**：`disabled` 恒显式写出；`inject` 声明即写；组行的子行写进
  `config` 数组；旧形状文档 `deny_unknown_fields` 点名报错（clean cutover）。
- **wire**：`ProfileRow` 带 `name` / `group` / `disabled` / 有效 `inject` /
  `intercept` / `isolate`；`profile/set` 参数 `enabled` → `disabled`。
- **model-adapter 行**：provider 目录摊平为行配置顶层键（`WorkspaceConfig`
  本身不动；uva-runtime 不在冻结清单内）。

## 3. 门禁

- `check-profile-document`（新）：入库 `.agents/profile.toml` 必须复述每个
  插件 manifest 的依赖边——有效边与文档声明不一致即红。
- `check-web-contract`：前端类型随服务端 wire 再生成。

## 4. 已知分歧

- **id 寻址**：全文档唯一（dsh 为树内局部 id + 路径）。用户裁决。
- **volatile-only 热应用**：沿用uva 既有机制（工厂声明 volatile 字段、watch
  消费），行为与 dsh 一致，实现不同。
- **volatile 字段的组内语义**：组内配置变更一律重挂整组（组拥有子行 fiber）。
