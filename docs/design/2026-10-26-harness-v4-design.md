# Harness V4 — 零运行时依赖的基座、双通道插件、无状态 server

日期：2026-10-26
状态：待批准
关系：接续 [`2026-10-25-harness-v3-design.md`](2026-10-25-harness-v3-design.md)（日志真源 / 可撤销接缝 /
按调用高风险审批，已落地）与
[`docs/research/harness-comparison-pi-dsh-2026-10-25.md`](../research/harness-comparison-pi-dsh-2026-10-25.md)
（pi × dsh 对照研究，本文是它的落地决议）。
本文取代 v3 §12 的"实施顺序"表，并新增 W2 插件通道与 W3 无状态化两段。

---

## 1. 意图与为什么

`uva` 要成为**社区基座库**：第三方 crate 装进来加 tool / 插件，不改 harness 一行；外围库用这个
harness + 外部插件集成为一个完整的 ai agent 应用。

今天离这个目标最近的差距有三条，每条都是**可验证的**，不是观感问题：

1. **"无运行时依赖"只做到公开 API 层。** `uva-harness` 自己的 `[dependencies]` 没有 tokio，
   但 `cargo tree -p uva-harness -e normal` 仍列出 7 个 tokio 相关条目。第三方
   `cargo add uva-harness` 会把 tokio 编进产物 —— 这与 pi / dsh 的"核心包不带运行时"不是同一
   个量级的事。
2. **插件只有编译期一条通道。** `Host` / `Plugin` / `Registration` 已经做得很完整（撤销语义、
   安装顺序、fail-closed 仲裁），但 `grep PluginRegistry` 零命中、`PluginEvent` 零命中。第三方插件
   只能作为 Rust 依赖参与编译，无法"交付一个插件包、部署方按名装上"。
3. **server 不是无状态服务。** `SessionController` 的 `live` / `pending_resume` /
   `pending_multimodal` / `pending_provider` / `steer_gate` 全在进程内。持久层无状态，计算层有状态。

**为什么现在做**：v3 已经把"日志是真源"和"接缝可撤销"这两块地基夯完（`derive_history` →
`HistoryProjection` 已进主路径，`Host` 仲裁规则已定），审批与压缩的 seam 也已就位。此刻做上面三件事，
不需要第二次推翻 harness 的循环结构；而拖到 v3 之后再做，破坏性改动会从"harness 公开面"扩大到
"facade feature 表 + server 路由"。

---

## 2. 已由对话确定

| 决定 | 内容 |
|---|---|
| 插件双通道 | 都要 —— 一条静态（编译期 Rust crate）、一条动态（运行期进程） |
| 动态通道形态 | **A：独立进程 + wire 协议，任何语言**（不是 WASM，不是 native dylib） |
| 动态插件权力 | **B：默认降权** —— 可观察 + 供工具 + 改写工具结果；**否决权（block / ask）留在进程内**；config 可对单个插件显式提权到完全平权 |
| 本轮范围 | **W1 + W2 + W3 全部**（harness 零运行时依赖 + 插件双通道 + server 无状态化） |
| 破坏性 | 允许。facade feature 表收缩是生态可见的破坏性改动，本轮一次切干净，不留 shim / 别名 |
| 依赖方向 | `uva` → `uva-*` → `uva-core`，单向，不回指 |
| harness 自身 | 零 cargo feature、零运行时依赖、公开 API 不出现具体运行时类型 |
| `prompt_section` | **本轮链接上** —— prompt 组装器读 host 的 sections，让插件的 prompt 片段真的到达 system prompt（今天注册了但只有测试读） |
| facade feature | **按方向收窄**（详见 §5.10）—— 每条 feature 只表达一个 crate 依赖；组合意图全部搬到运行时组合层 |
| `InputQueue` | **挪进 `uva-app-server`** —— session 不保留多余的语义与作用 |

---

## 3. 目标 / 非目标 / 反目标

### 目标

1. `cargo tree -p uva-harness -e normal` 不含任何 tokio 条目；第三方只依赖 harness 时，
   **产物里没有 tokio**。
2. 静态插件通道保持现有语义（`Plugin::install` → `PluginHandle`，drop 即撤销），并新增
   `PluginEvent` 逃生舱，让插件能发自己的领域事件（durable / live 两路）。
3. 动态插件通道：第三方用任何语言写一个进程，通过 wire 协议向宿主注册工具、可选地观察事件、
   可选地改写工具结果。宿主把它装进同一个 `Host`，与静态插件同权同序。
4. 运行时组合层：部署方在 config / profile 里按名挑选并排列插件与工具，**取代 facade 45 条
   cargo feature 承担的组合职责**。cargo feature 退化为纯"依赖轴"（这个 crate 在不在编译产物里）。
5. server 无状态化：计算节点不持有会话真源；多节点共享 workspace 文件系统 + cluster bus。
6. harness 生产代码的公开面收窄到"外部真的在用"的规模；热路径 clone 消除；基准表可复现。

### 非目标

1. 运行时热重载（插件进程内换代码）—— 与 ABI 稳定性目标冲突。
2. 让 `uva-harness` 自己变成"什么都有"的大 crate —— 保持薄。
3. provider 层的新模型 / 新协议。
4. 前端视觉重设计 —— 本轮前端只跟着接口走。
5. 为兼容旧签名保留 shim。

### 反目标（做到也算失败）

- 为了"零 tokio"把 tokio 换成 `async-std` / `smol` / `futures::executor` —— 换一个运行时不算解。
- 让 harness 的公开 API 出现 `tokio::` / `async_std::` / `smol::` 路径。
- 动态插件默认拿到否决权（那是 A，不是 B）。
- 远端插件自报的 `annotations` 参与审批判定。
- 远端 transformer 改写工具结果却不落审计行。
- `cargo test --workspace` 一次跑完。
- 用 `dead_code` / `allow(unused)` 抑制警告来"精简"。

---

## 4. 约束

继承 `AGENTS.md`：

- 库 crate 只用 `thiserror`；`uva-app-server` 是 app crate，用 `anyhow`。
- 依赖版本一律 `major.minor`；新依赖只能来自根 `Cargo.toml` 的 `[workspace.dependencies]`。
- `max_width = 80`；禁通配符导入；`uva-harness` 生产代码无函数 >100 行、无嵌套 >4 层。
- 墙钟走 `uva_core::Clock`，ID 走 `uva_core::IdGen`；新代码禁直接 `chrono::Utc::now()`。
- 库 crate 的公开 API 不得出现具体运行时类型。
- `uva-harness` 不得有 `[features]` 表，不得依赖 tokio。
- 测试按 crate 分批执行，禁止 `cargo test --workspace`。
- 破坏性 API 一次切干净。
- 注释预算只许降不许升（`scripts/harness-docs/baseline`）。

---

## 5. 架构

### 5.1 全景

```text
┌───────────────────────────────────────────────────────────────────┐
│ 消费方 / 部署方                                                     │
│   cargo add uva-harness     （产物零 tokio）                    │
│   cargo add uva-plugin-host （要跑远端插件时才加）              │
└────────────────┬──────────────────────────────────┬───────────────┘
                 │                                  │
      ┌──────────▼───────────┐          ┌───────────▼────────────┐
      │  静态通道             │          │  动态通道              │
      │  impl Plugin         │          │  任意语言进程           │
      │  （Rust crate）        │          │  JSON-RPC over stdio/http│
      └──────────┬───────────┘          └───────────┬────────────┘
                 │  Host::install                     │ wire
                 │  → PluginHandle                    ▼
                 │                        ┌────────────────────────┐
                 │                        │ uva-plugin-protocol │
                 │                        │  serde-only · 零运行时   │
                 │                        │  · 不依赖 harness       │
                 │                        └───────────┬────────────┘
                 │                                    ▼
                 │                        ┌────────────────────────┐
                 │                        │ uva-plugin-host     │
                 │                        │  远程能力 → Host 注册    │
                 │                        └───────────┬────────────┘
                 └────────────┬───────────────────────┘
                              ▼
              ┌───────────────────────────────────┐
              │  uva-harness                  │
              │  Host · Plugin · Registration     │
              │  Agent · ReasoningStrategy        │
              │  DelegatingTool · ApprovalPolicy  │
              │  零 tokio · 零 feature             │
              └────────────┬──────────────────────┘
                           ▼
              ┌───────────────────────────────────┐
              │  uva-app-server（app crate）       │
              │  运行时组合层：config → profile    │
              │  → 静态插件 + 动态插件 → Host      │
              │  无状态计算节点                    │
              └───────────────────────────────────┘
```

**关键结构决策：动态通道对 harness 完全不可见。** harness 只认 `Host` / `Plugin` /
`Registration`；远端能力由 `uva-plugin-host` 在宿主侧翻译成同一种注册。harness 一行不改，
也不需要知道"远端"这个概念存在。这同时解开了 v3 §5.2 里"插件 crate 依赖 harness 类型、harness 又
依赖插件 crate"的环 —— 环只存在于旧解法里。

### 5.2 crate 图（新增两个）

| crate | 依赖 | 职责 | 运行时 |
|---|---|---|---|
| `uva-plugin-protocol` | `serde` + `serde_json`（**仅此两个**） | wire 类型：方法名常量、请求/响应结构、能力协商、错误码 | 零 |
| `uva-plugin-host` | `uva-plugin-protocol` + `uva-harness` + `uva-core` | 进程管理、JSON-RPC 客户端、把远端能力装进 `Host` | 需要 spawn（tokio 可选） |

`uva-plugin-protocol` **不依赖 harness**，因此不构成环。第三方作者只需要它（或者是它生成的
JSON schema）就知道协议长什么样。

facade 上按 `AGENTS.md`「新增 crate 必须同步加 feature」加两条：`plugin-protocol` /
`plugin-host`。

### 5.3 W1-a：依赖闭包 tokio 清零

现状（`cargo tree -p uva-harness -e normal` 的 7 个 tokio 条目：`tokio-util` ×3、`tokio` ×3、
`tokio-macros` ×1）与修法。**关键区分**：把 *代码* gate 掉不等于把 *依赖边* 去掉 —— 依赖边由
`Cargo.toml` 决定，必须逐条动 manifest。

| # | 来源 | 现状证据 | 修法 |
|---|---|---|---|
| 1 | `uva-steering` → `tokio-util` | `tokio-util` 在 `[dependencies]`（`Cargo.toml:32`），但本 crate 内唯一用处是 `effect_gate.rs:190` 的 `#[cfg(test)]` | 移到 `[dev-dependencies]` |
| 2 | `uva-steering` → `uva-core/tokio-util` | `Cargo.toml:20` 在 **normal** 侧开了 core 的 `tokio-util` feature —— 这是把 `tokio-util` 拉进 harness 闭包的两条边之一 | normal 侧不开；只在 dev 侧开 |
| 3 | `uva-provider` → `uva-core/tokio-util` | `Cargo.toml:15` 同样在 normal 侧开；本 crate 内唯一用处是 `idle_watchdog.rs:221` 的 `#[cfg(all(test, any(anthropic, openai)))]`（另有 `tests/provider_test.rs` 9 处，那已经是 dev 侧） | normal 侧不开；只在 dev 侧开 |
| 4 | `uva-provider` → **`tokio` 本身** | `Cargo.toml:30` `tokio = { features = ["macros","time"] }` 是 **normal 无条件依赖**，与 `anthropic` / `openai` 无关。而 harness 的树里这两个 feature 是关的（workspace dep `default-features = false`，根 `Cargo.toml:58`）—— **代码被 gate 掉，依赖边还在** | `tokio` 改 `optional = true`；`anthropic = ["dep:tokio", …]`、`openai = ["dep:tokio", …]` |
| 5 | `uva-provider/retry.rs` | `tokio::time::sleep` × 6 处。其中 `retry_with_classification` 被两个 adapter 调用（4 处）；**`retry_with_backoff`（`retry.rs:97`）与 `retry_with_retry_after`（`:145`）全仓零调用点**，只有 `lib.rs:101,103` 的无条件 re-export | 前者 gate 到 `any(anthropic, openai)`；后两者是**死公开 API**，按 §5.7 直接删（注意 `uva-resilience/src/provider.rs:374` 有一个同名但无关的 `retry_with_backoff`，别误伤） |
| 6 | `uva-provider/streaming/idle_watchdog.rs` | `tokio::select!` / `spawn` / `sync::mpsc` / `time` —— SSE 泵 + 空闲看门狗。**运行时那半已经逐项 gate 过**（`idle_watchdog.rs:16,62,69,135,178`），只有四个 tokio-free 导出（`DEFAULT_STREAM_IDLE_TIMEOUT` / `IDLE_TIMEOUT_MARKER` / `idle_timeout_error` / `is_idle_timeout`）是无条件的 | **不要 gate 整个模块** —— 那会删掉四个与 adapter 无关的公开导出。只把剩下的无条件项与 adapter feature 对齐 |
| 7 | `uva-session` | `jsonl.rs`：`tokio::sync::Mutex` + `tokio::task::spawn_blocking`；`manager.rs`：`tokio::sync::Mutex`；`compaction.rs`：`tokio::select!` + `tokio::time::timeout`；`search/backend.rs`：`spawn_blocking`；`in_memory.rs:224` 的 `tokio::spawn` 在 `#[cfg(test)]` 内 | 见 §5.4 |

修完 1–7 之后，剩下的唯一来源是 `uva-session`。**session 是 harness 的依赖**（`LogWriter` /
`SessionEvent` / `derive_history` 都在那里），所以 session 必须整体变 runtime-free，或者拆 crate。

**顺带的 server 影响**：去掉 2、3 两条 normal 侧的 `uva-core/tokio-util` 后，server 会失去这个
feature —— 但 server 的 `run_stream.rs:24` 生产代码要 `Arc<tokio_util::sync::CancellationToken>`
（`agent.run` 收 `Arc<dyn CancelToken>`）。server 自己从不请求这个 feature，它今天是**搭
provider / steering 的便车**拿到的。修法：server 显式请求（facade 上加一条 feature），或改用
runtime-free 的 `AtomicCancelToken`（`workflow_host.rs:80` 已经这么用）。这是 W1 里一个会**编译
中断**的隐藏依赖，排在 §7 的第 4 步之前处理。

### 5.4 W1-b：`uva-session` 的 runtime-free 化

session 现在的 tokio 用法分三类，各有不同归宿：

**(a) 摘要超时 —— 设计漂移，必须改。**
`compaction.rs` 用 `tokio::time::timeout` 武装摘要调用。v3 设计 §7.2 自己写的是"harness 运行时无关，
不自己武装定时器；`uva-app-server` 在手动压缩上安装 `DEFAULT_SUMMARY_TIMEOUT`"。代码与设计不一致。

修法：新增 `uva_core::sleeper::Sleeper` 接缝 ——
```rust
pub trait Sleeper: Send + Sync {
    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()>;
}
```
`session` 用它实现超时（`futures::select!` + `sleeper.sleep(d)`，两个 future 竞速，runtime-free）。
tokio 实现（`tokio::time::sleep`）放进兄弟 crate `uva-harness-tokio` 的
`TokioSleeper`，与 `TokioSpawner` 同款先例；`session` 自带一个 `ThreadSleeper`
（std thread + oneshot，测试与无 runtime 消费者可用）。

**(b) 阻塞 IO —— 需要 blocking 接缝。**
`jsonl.rs` 与 `search/backend.rs` 用 `tokio::task::spawn_blocking` 把 fs 写与 fs 读挪出工作线程。

修法：新增 `uva_core::blocking::Blocking` 接缝 ——
```rust
pub trait Blocking: Send + Sync {
    fn run(
        &self,
        f: Box<dyn FnOnce() + Send + 'static>,
    ) -> BoxFuture<'static, ()>;
}
```
这里**不能**写成 `fn run<T: Send + 'static>(&self, f: impl FnOnce() -> T + Send + 'static)
-> BoxFuture<'static, T>`：泛型方法不是对象安全的（object safety），而调用方需要的是
`Arc<dyn Blocking>` —— 一个运行期装进来的 trait 对象。所以闭包的返回值不经过 `run`，由调用方
自己的 oneshot 携带（`let (tx, rx) = oneshot::channel(); blocking.run(Box::new(move || tx.send(f())));`
取 `rx` 就是 `T`）。默认实现 `ThreadBlocking`（`std::thread::spawn` + oneshot），tokio 实现
`TokioBlocking`（`tokio::task::spawn_blocking`）进 `uva-harness-tokio`。

**注意**：`jsonl.rs` 的 `tokio::sync::Mutex` 也要换 —— 它保护的是文件句柄，跨 `.await` 持有；
换成 `futures::lock::Mutex`（session 里 `writer/mod.rs` 已经这么用了）即可 runtime-free。

**(c) `manager.rs` 的 `InputQueue` 与 `in_memory.rs` 的 `tokio::spawn` —— 归位（已决定）。**
`InputQueue` 是 server 用来桥接 submit 与 run loop 的小队列，本质是 server 的东西，不该在
session 的核心里 —— session 不保留多余的语义与作用。**本轮挪进 `uva-app-server`。**
`tokio::spawn` 那处只在 `#[cfg(test)]` 里（并发 append 测试），移到 dev 侧即可。

**这是本轮的一个生态可见破坏**：`InputQueue` 今天在 `uva-session`（`manager.rs:252`），
经 facade 的 glob 导出为 `uva::session::manager::InputQueue`，server 自己也把它当公开 API
用（`session/controller/ops.rs:11` 的注释就写着这个路径）。挪走后**这个名字从 facade 的公开面上
消失** —— 按本轮的规矩不留别名、不留 shim。

连带要处理的：
- `scripts/check-pub-surface-strict.sh` 里 `uva-session` 的 pub-use 基线要更新。
- facade 的 `session` 模块（`crates/uva/src/session.rs`）是 `pub use uva_session::*;`
  的 glob —— glob 会自动跟着 crate 的公开面走，不需要单独改；但要确认删掉后没有别的东西
  顺手消失（`uva_session` 的其他导出不受影响）。
- `uva-session` 里 `manager.rs` 剩下的部分（`SessionManager` 等）仍在 session，只搬
  `InputQueue` 与它的 `PendingEntry`。

**session 的最终形态**：核心里不再有 tokio。具体是"一个 crate，runtime-free 部分无条件、runtime-bound
部分 gate 到 feature"还是"拆成 `uva-session`（核）+ `uva-session-fs`（jsonl/search）"，
由实现者按 §5.3 的余量决定 —— 但**验收标准不变**：`cargo tree -p uva-harness -e normal` 无 tokio。

### 5.5 W1-c：泛型核心 + `dyn` 外壳 —— 复核结论：`dyn` 全部保留

原稿要求 `ReActAgent<P: ModelProvider>` 与"泛型 `ContextManager` + `dyn` 外壳"。**这条路不可行，
本节改为一次逐站点的 `dyn` 复核。**

不可行的原因：`AgentRuntime { pub caps: Arc<AgentCaps> }` 是**按值**传进
`ReasoningStrategy::run(&self, runtime: AgentRuntime, …)` 的，而 `StrategyRegistry` 在运行期
**按名**解析策略。把 `AgentCaps` 泛型化就必然传染
`AgentRuntime<P>` → `ReasoningStrategy<P>` → `StrategyRegistry<P>` → `ReActStrategy<P>`，
**全部三个** strategy crate 一起改（`best-of-n` / `plan-execute` / `cot`，加上 in-harness 的
`ReActStrategy`）—— 正是 §8.2 已经否掉的事件词表那种签名污染
（"会让 `Agent` trait、`ReasoningStrategy`、server、全部 strategy crate 都多一个类型参数"）。
换来的只是每次模型调用省一跳 vtable —— 而模型调用每 **step** 才一次，且是网络受限的。
成本收益为负。

**逐站点复核**（判据：运行期安装？泛型化是否污染值路径的签名？）：

|Site|Count|Why `dyn` is correct|
|---|---|---|
|`Arc<dyn ModelProvider>`|25|runtime-installed by the consumer; genericising propagates as above|
|`Arc<dyn DelegatingTool>`|14|heterogeneous, name-keyed table in the loop|
|`Arc<dyn CancelToken>`|10 + 5 `Option`|per-run value handed in by the caller|
|`Arc<dyn Tool>`|7 (+12 in `uva-tool`)|heterogeneous registry; count unknown at compile time|
|`Arc<dyn ReasoningStrategy>`|7 + 3 `Option`|resolved from `StrategyRegistry` by name|
|`Arc<dyn Compactor>`|7 + 3 `Option`|optionally installed at construction|
|`Arc<dyn Checkpointer>`|6 + 1 `Option`|optional accelerator|
|`Arc<dyn ContextManager>`|4|installed at construction; same propagation blocker|
|`Arc<dyn Agent>`|4|`AgentRegistry` holds heterogeneous agents|
|`Arc<dyn ResultTransformer>` / `RequestTransformer` / `Hook` / `Gate`|3 each|`Vec<Entry<Arc<dyn …>>>` in `Host` — heterogeneous, runtime-registered|
|`Arc<dyn Spawner>` / `SubagentWriters`|1 each|installed at construction|
|`Arc<dyn FullOutputStore>`|**0 here** (5 in `uva-tool`'s `full_output.rs`)|the store is the tool crate's dependency, not the harness's|
|`Box<dyn Tool>`|1|**not production** — a `Send + Sync` assertion in `tool_test.rs:104`|

**结论：没有代码改动。** 判据是**两条**，不是三问全 yes：该站点由下游在运行期装上（构造期、
每个 run、或每张按名解析的表），**并且**把具体类型推到签名上会污染值路径
（`AgentRuntime` 那条传染链）。集合异构与数量未知只是其中几行的额外理由 —— `CancelToken`、
`Compactor`、`Checkpointer` 三行只靠第一条成立（`ContextManager` 那条的第二条也成立：
泛型化它会让 `ReActAgent` 多一个类型参数并传染到 `Agent` trait）。"泛型核心"的意图在有收益的地方
**已经落地** —— `Registry<T>` / `RegistryList<T>`、`NameTable<T>`、`Sensitive<T>`、
`paginate_registry_list<T: RegistryItem>`、`Host` 的 `Entry<T>`、`DelegationCall<'a>`。
vtable 静态分派是这些泛型容器内部的实现细节，不需要把类型参数推到公开签名上。

### 5.6 W1-d：热路径 —— 实测与结论

基准（`cargo bench -p uva-harness --bench hot_paths`，本机实测）。原稿的 "386.9 µs" 与
"3 次 clone" 两个说法都已被本分支的实测推翻；下面是要点。

**基准自己错了，两处：**

1. 旧的两行 `TruncatingContextManager` 把 `Arc::new(messages.clone())` 放在被计时的闭包
   **内部**，于是每轮迭代深拷一份 **401** 条消息的 fixture 并记在 manager 账上。标签写的是
   200 条消息 / 4 KB，fixture 实际是 401 条消息与 `nested_tool_args(120)` ≈ **7,701 B
   ≈ 7.5 KiB**。
2. 把拷贝挪出计时区之后：**无驱逐 79.9 → 19.1 µs**（60.8 µs 的差就是那次拷贝 —— 现在由它
   自己的 `Vec<Message> clone (401 msgs)` 行报价：57.8 µs）；**超窗 386.7 → 381.9 µs，
   在噪声内不变**（−1.2%，落在未被触碰的 turn 行的 2.3% 漂移之内）。

**超窗那一行的"分支"是错的，量级没错。** 旧 bench 交给 manager 一个**唯一所有**的 `Arc`，
`Arc::try_unwrap` 因此成功，manager 直接拿走那个 vector、不发生拷贝；生产路径传的是
`Arc::clone(&session.messages)` 且在持有它的情况下丢弃（`loop/prepare.rs:408`），
`try_unwrap` 失败，manager 必须拷。bench 现在模拟循环的形状。

**`call.input` 的结论 —— 原稿的验收标准在测空气：**

- `guard.rs` 的 `call.input.clone()` **已经是懒的**：它在传给 `dispatch_tool_gate` 的闭包里，
  而没有 hook、没有 host 注册时该函数在 `hooks.rs:51-57` 就返回了。默认路径零成本，所以
  "审批门控路径上 ≤1 次拷贝"这条标准什么都没测。
- `route.rs` 剩下那一处 clone 是结构性的：`resolve_registry_tool` 按值收 `input`，
  `resolve_dispatch` 消费它，而 `call: &ToolUse` 是从模型消息一路借上来的。
- 报价：小工具入参上 `serde_json::Value::clone` = **81 ns**，对 **254 µs** 的 turn 是 **0.03%**。
  为常见形状做 ~250 站点的 `Arc<Value>` 重构，不值得。

**超窗数字里剩下的成本没有单独测过。** 现有 bench 行无法推出它：381.9 − 57.8（拷贝）− 19.1
（快路径）≈ 305 µs，但这个减法把三种不同的工作混在一起，不能当归因。读代码能确定的是超窗路径
的形状 —— `uva-context` 的 `TruncatingContextManager` 在**整表**上做三次 token 估计
（`prepare_arc` 入口一次、`prepare` 入口一次、丢弃后再一次），丢弃循环里再对**每条被丢消息**
单独估一次，并用 O(n) 的 `Vec::remove` 搬移（`context_manager.rs:527`、`:536`）——
都在 `uva-context`，本轮没碰。**未测量，成因候选就是这些**，下一轮先加一行能隔离它们的
bench 再谈降本。

**bench 门禁的边界**：`--check` 只把优化路径与同一份工作的内联参考实现做比值，所以
`TruncatingContextManager` 不可能变成比值 invariant —— 不要把它写进门禁。

### 5.7 W1-e：公开面与死代码

- 逐个复验后删除零外部引用的 `pub`。已确认的候选（本轮必须逐个复验，每个在全仓恰好只有
  自身定义一处引用）：
  `uva_session::as_value`（`typed_event_sink.rs:57`）、
  `uva_session::with_prune_threshold`（`compaction.rs:244`）、
  `uva_provider::create_default_provider`（`config.rs:299`）、
  `uva_provider::openai_compat`（`profile/openai.rs:69`）、
  `uva_context::with_archive`（`summarizing/manager.rs:97`）。
  例外：`hash_to_u64` 的 10 处引用**全部在本模块的测试里**，删除会逼四条断言各自重推哈希 ——
  改成 `#[cfg(test)]` 的私有函数（收窄而非删除），`ahash` 随之降到 dev-dependencies。
- `uva-provider` 里另有两处**死公开 async API**：`retry_with_backoff`（`retry.rs:97`）与
  `retry_with_retry_after`（`:145`）全仓零调用点，只剩 `lib.rs:101,103` 的无条件 re-export。
  一并删除（注意 `uva-resilience` 有个同名但无关的函数）。
- **只有 `uva-app-server` 标 `publish = false`**：它是应用 crate，40k+ 行不该发到 crates.io。
  **facade `uva` 必须保持可发布** —— 它是文档化的消费入口（README 与 crate 文档里的
  `cargo add uva`），标成不可发布就等于砍掉项目的主分发路径。全仓 39 个 crate 目前无一标
  `publish = false`，只有两个 example consumer 标了。
- `uva-app-server` 那 374 个 `pub` **不**做批量降级（原计划的清理已撤销）：`tests/` 下 14 个
  integration test 是独立 crate，直接 `use uva_server::…`；`main.rs` 也用
  `build_info` / `config::server::BindOverrides` / `server::boot_server`。这些路径触达的模块必须
  保持 `pub`。server 既已不可发布，它的 `pub` 面就不再是出厂契约，为没有外部消费者的模块逐个降级
  是纯 churn。只在碰到"既无测试/`main` 引用、其模块也无人 import"的条目时才顺手降。
- 删 `crates/uva-app-server/src/config/server.rs.tmp` —— 已入 git 的 **0 字节占位文件**
  （blob `e69de29b…`），不是配置文件的旧副本。
- `crates/uva-harness/src/agent/re_act/loop/tools/mod.rs` 等处的注释漂移随改动一并修正。
- `check-harness-docs` 的注释预算基线只许降不许升。

### 5.8 W2-a：动态插件协议

**传输**：JSON-RPC 2.0 over stdio 或 HTTP，复用 `uva-mcp` 已有的 `McpTransport`
抽象（`request` / `notify` / `describe`）。不另立传输栈。可复用的实现是 `StdioTransport`
（`stdio.rs:135`）、`StreamableHttpTransport`（`http.rs:154`），测试用 `LoopbackTransport` /
`InMemoryTransport`。**注意**：`McpSupervisor`（`supervisor.rs:232`）本身**不实现** `McpTransport`
—— 它是通过 `TransportFactory` **创建**传输的上层生命周期/重连管理器（`supervisor.rs:107-111`）。
所以 supervisor 只能在新传输也走 `TransportFactory` 时复用；否则 `uva-plugin-host` 自己管进程
生命周期。

**方法集**（B 权限下的最终形态）：

| 方法 | 方向 | 语义 |
|---|---|---|
| `uva/initialize` | host → plugin | 能力协商。plugin 声明 `tools` / `observe` / `transform` / `gate` 四项支持与否 |
| `uva/tools/list` | host → plugin | 工具描述符：`name` / `description` / `inputSchema`（沿用 MCP 形状） |
| `uva/tools/call` | host → plugin | 调用远端工具，参数与结果都是 JSON |
| `uva/transform` | host → plugin | 交还改写后的工具结果；host 应用它 |
| `uva/event` | host → plugin（notification） | 观察事件，无返回值 |
| `uva/gate` | host → plugin | **默认不存在**。仅当 config 把该插件提权到 `authority: full` **且**它在握手时声明了 `gate` 才启用；返回 `Block` / `Ask` |

`authority: full` 与 `gate` 是**两个条件**，不是一个：authority 是 operator 的授权，`gate` 是插件声明的实现。
只满足其一就说话，等于让一个不实现 `uva/gate` 的插件装成功、然后每次工具调用都 fail-closed ——
那是从一个配置词换来的整机停摆。所以 `install` 拒绝 `full` 而插件未声明 `gate` 的组合，并给出修法。

**协议 crate 的内容**（`uva-plugin-protocol`，serde-only）：
- 方法名常量（避免各处硬编码字符串）。
- 请求/响应结构体，全部 `serde` 派生。
- 能力协商结构：`PluginCapabilities { tools: bool, observe: bool, transform: bool, gate: bool }`
  —— 整结构 `#[serde(default)]`，缺省键即 `false`（fail-closed：未声明或拼错的能力不授予任何东西）。
- 错误码：`Unavailable` / `Timeout` / `Denied` / `BadRequest` / `Internal`。
- 一份 JSON Schema 或等价文档，供非 Rust 作者对照。

**关键类型问题**：内部事件词表目前**没有 serde 派生** —— `HookEvent`
（`uva-steering/src/hook_map.rs:26`，`Debug, Clone`）、`HookDecision`（同文件 `:174`，
`Debug, Clone, Default, PartialEq`）、`RequestParams`（**在 `request_params.rs:28`，不在
`hook_map.rs`**，`Debug, Clone, PartialEq`）。`uva-steering` 目前只依赖 `serde_json`、
**不依赖 `serde`**，所以给它们加派生要先按 AGENTS.md §3.1 把 `serde` 引进根
`[workspace.dependencies]`。

决策：**wire 上不直接复用 Rust 的 `HookEvent`**。理由 —— Rust 枚举的变体形状会随内部实现演进而变，
把它当 wire 契约等于把内部重构变成协议破坏。改为在 `uva-plugin-protocol` 里定义**独立的、
稳定的 wire 事件与决策类型**（`PluginEvent` / `PluginDecision`），由 `uva-plugin-host` 做
双向映射。代价是多一层映射代码，收益是协议与内部类型解耦 —— 这是"基座"必须有的分寸。

**wire 词表的覆盖范围**（builder 提出、实现者定）：内部 `HookEvent` 有 11 个变体。远端插件在
降权下只能观察、不能否决，所以 wire 词表**不必镜像全部 11 个** —— 只需覆盖"远端能感知的那部分"
（工具相关 + 步骤/轮次边界），`BeforeRequest` / `AfterProviderCall` 这类内部遥测可以不上 wire。
`PluginDecision` 同理：降权下远端永不返回 `Block` / `Ask`，但 wire 类型仍应定义完整（提权路径
要用），由宿主侧按该插件的 `authority` 决定**是否接受**这两个变体。

**`PluginEvent` 的现状**：v3 设计 §8.2 定过它的形状，但**从未实现** —— `crates/` 与 `scripts/`
里零命中，只有 v3 文档里有提案。本轮落地它，并把 v3 的一处签名改掉：`kind` 是
**`String`，不是 `Cow<'static, str>`** —— 插件的 kind 是运行期取到的值，`'static` 除了把插件
发来的字符串泄漏掉之外无法满足，所以 `Cow<'static, str>` 是个不可满足的契约。

`PluginEvent` 同时补上 v3 §8.2 承诺过但从未实现的**逃生舱**：插件可以发自己的领域事件。
两条路径是**两种承诺**，不能混：

- **durable**：`SessionEvent::Plugin { kind, payload }` —— 追加进日志，崩了也在，但
  **log-only**：不进模型历史、不进 surface、不进 `fold`。边界在 **writer**（不是构造器）：
  `LogWriter` 在每一条 append 路径上（异步、同步、拷贝）对 `type: "plugin"` 的行做约束，
  因为变体字段是公开的，只在构造器里卡等于没卡。约束是：
  `kind` 的编码形式 ≤ **256 B**（空 kind 记作 `"unnamed"`）；`payload` 的编码形式 ≤
  **64 KiB**（`MAX_PLUGIN_PAYLOAD_BYTES`，按转义后的字节算 —— 存的字符串在行里会被二次转义，
  按原始长度卡会给出 1.5 倍的空头承诺）。超限的 payload 被换成**一个 JSON 字符串**，内容是原
  编码的前缀（截在字符边界上），原始字节数记在该行的 `original_bytes` 里 —— wire 消费者读到的
  是字符串，不是对象，必须按 `original_bytes` 判断是否被截断。
- **live**：`SystemEvent::Plugin { kind, payload }` —— 只是通知，**永不持久化**，背压时可丢弃。
  任何"消费方不能错过"的事实走上面的日志行，不走这里。

  **它今天没有生产者**：harness 与 server 都不构造它，`install` 路径也不发。将来的生产者
  必须在 **plugin-host 边界**（事件变成 `AgentEvent` 之前）把 live `kind` 与 `payload` 都收口，
  因为 server 会把每个 run event 序列化后扇出到集群总线 —— 一个不设界的 live payload 是每人
  一份的带宽，而 `kind` 是同样的自由文本，不设界就等于让插件把 body 换个字段名搬进去。
  live 的上界**可以**与 durable 的不同：它可丢，所以不需要与日志的可回读性对齐。

### 5.9 W2-b：权限矩阵（B）

| 能力 | 静态插件 | 动态插件（默认） | 动态插件（config 提权） |
|---|---|---|---|
| 注册工具 | ✅ | ✅ | ✅ |
| 观察事件（只读） | ✅ | ✅ | ✅ |
| 改写工具结果 | ✅ | ✅ | ✅ |
| 改写请求参数 | ✅ | ❌ | ✅ |
| 否决工具调用（block） | ✅ | ❌ | ✅ |
| 发起审批（ask） | ✅ | ❌ | ✅ |
| 结束整轮 run（terminate） | ✅ | ❌ | ✅ |
| 贡献 system prompt 片段 | ✅（本轮链接上，见下） | ❌ | ✅（本轮链接上，见下） |
| 委派子 agent | ✅ | ❌ | ❌（见下） |

**委派永远不进 wire。** 子 agent 要在本进程跑（`DelegatingTool` 拿到 `ToolContext`、
`depth`、`emit`），跨进程就换了语义。远端插件如果要"调用另一个 agent"，那是**远程 agent 端点**，
不是 `DelegatingTool`，本轮不做。

**`prompt_section` 本轮链接上（已决定）**。现状是：注册 API 在（`host.rs:515`），读取口在
（`host.rs:856`），但 `prompt_sections()` 在**全仓只有一个调用者 —— 一个测试**
（`host/tests.rs:437`）。prompt 组装器（`prompt/mod.rs:138,172,187,227`）**从不查 host**。
也就是说今天**没有任何插件的 prompt 片段能到达 system prompt** —— 这一格之前是空的。

本轮把它链接上：prompt 组装器读 host 提供的 section 列表。改动面很小，但要注意三件事：

1. **顺序要确定**。section 按注册顺序渲染（`Host` 已有安装顺序契约，见 `host.rs:447-448`），
   不能让顺序依赖哈希或并发。
2. **不能破坏现有 system prompt**。现有 prompt 组装（`prompt/mod.rs`）有确定的段落顺序与
   身份段，section 插在哪一段之后要有明确规则 —— 建议追加在身份段之后、steering hint 之前，
   避免插件片段被 hint 挤掉或反过来。
3. **这是 harness 改动**，但**不违反"动态通道对 harness 不可见"** —— 那条说的是 harness 不需要
   知道"远端插件"这个概念；prompt section 是 `Host` 的既有能力，只是终于被渲染了。

远端插件的 prompt section 走**提权路径**才有（默认降权不含它），因为它直接进 system prompt，
是比 transform 更强的控制面。

**三个必须写进协议文档的后果**：

1. **远端工具的 risk 由宿主策略决定，插件自报的 `annotations` 只作展示。**
   一个插件可以把 `destructive_hint: false` 写进 `tools/list` 来躲审批 —— 那正是降权要堵的洞。
   宿主侧对远端工具一律按 `Risk::Unknown` 或 config 指定值判定审批。
   **落点**：`Tool::risk()` 今天默认是 `Risk::from_annotations(self.annotations())`
   （`uva-tool/src/traits.rs:154-157`），而 `Risk` 恰好只有 `Low` / `High` / `Unknown`
   三个变体、`Unknown` 是 `#[default]`（同文件 `:38-49`）—— 所以 `Risk::Unknown` 现成可用。
   `uva-plugin-host` 的远端 `Tool` 实现**覆写 `fn risk()`** 返回宿主配置的值即可，
   **不需要 harness 或 tool crate 改动**（§5.9 的"harness 一行不改"成立）。
2. **transformer 的改写必须落审计行。** 宿主在 session log 里追加
   `tool_result_transformed`（transformer 名 + 改写前后摘要）。
   **这行的意义是溯源（provenance），不是对账（reconciliation）**：transformer 在
   `execute_gated` 里运行，之后 `commit_all_results` 才用**改写后的**输出构造
   `tool_result` 行（`commit.rs`），所以日志与模型看到的内容本来就是同一份 ——
   "日志是真源"由此成立，而不是被这行修补。这一行回答的是另一个问题：
   这个被提交的结果，在提交路上被哪个注册的 transformer 改过。
   写入点必须是 harness（调用点），不是 transformer 自己：后者拿不到 context、
   也拿不到 writer，改了什么也无法自证；"有没有变"由宿主对比改写前后的聚合
   `ToolOutput` 测得，不读任何自报标志。
3. **`transform` 是热路径上的一次 IPC 往返。** 每个工具结果多一个 RTT，因此它**按插件 opt-in**，
   且宿主侧有超时与降级：超时 = 保留原结果 + 记诊断，而不是让整个 turn 卡住。
   超时用的是连接自身的 deadline/reaper（每条消息一个 slot + 一个 reaper 线程），不是 §5.4 的
   `Sleeper` 接缝 —— 后者的 wait 不可取消，建在它上面的 deadline 要么每请求一个线程、要么拖长
   关闭时间；reaper 超时即杀进程，这也正是解开被阻塞的写的那一步。

### 5.10 W2-c：运行时组合层

**问题**：facade 现在 45 条 feature（`default` + 44 个具名），其中大半承担的是"组合"职责 ——
`tool-read` / `tool-web` / `strategy-cot` … 决定的是"这次部署带哪些能力"。cargo feature 是
**编译期**、不可 patch、组合爆炸（2^n）。dsh 的解法是 profile / bundle / patch —— **运行期配置**。

**做法**：把两条轴分开。

- **依赖轴（cargo feature）**：这个 crate 在不在编译产物里。保留，但**收窄**。
- **激活轴（运行时组合）**：这次部署开哪些、按什么顺序、怎么接线。新增。

运行时组合层的形状：

```text
# config.yaml
plugins:
  - name: audit          # 静态插件（Rust crate）
    kind: static         # 只按名字点名 —— 它实现了 Plugin，能力由它自己声明
  - name: metrics        # 静态插件
    kind: static
  - name: eraser         # 动态插件（独立进程）
    kind: remote
    command: ["python", "-m", "uva_eraser"]
    transport: stdio
    capabilities: [tools, observe]
  - name: strict-gate    # 提权到完全平权
    kind: remote
    command: ["/opt/policy-bin"]
    authority: full      # ← 唯一能拿到 veto 权的配置位；且插件必须声明 `gate`
```

静态条目**只带 `name`**：它是链接进本二进制的 Rust 类型，`tools` / `observe` 这类能力由
`Plugin` 实现自己声明，在配置里再写一遍就是第二个真源 —— 而且会和真实实现漂移。
所以配置层对未知字段是**拒绝**的（`deny_unknown_fields`），不是忽略。

组合层在 server 侧（app crate），**不进 harness**。它是"这个部署长什么样"的答案，与 harness 的
"循环怎么跑"无关。

**feature 表的实测结论（推翻了本节早先的草案，实施以此为准）**

早先本节断言 `checkpoint` / `transcript` 是"指向不存在目标的空转发 feature"，应删除。
**这是错的，已实测推翻：**

- `checkpoint = ["harness"]` 门控 `crates/uva/src/checkpoint.rs`（34 行）—— 它把
  `uva_harness` 的 `Checkpointer` / `InMemoryCheckpointer` / `CheckpointSnapshot` 收在
  一个入口下，模块文档明说 "a consumer writing a resumable agent does not need to import
  from two paths"。
- `transcript = ["eval"]` 门控 `crates/uva/src/transcript.rs`（38 行）—— 同理，是
  `uva_eval` 的便捷入口。

它们"不拉新 crate"是因为门控的是 **facade 自己的模块**，不是转发下层 feature。按
"是否门控某个模块"做全表审计（`grep -rc 'feature = "<name>"' crates/uva/src/`），
**只有 `cron` 一条门控不到任何模块** —— 而它正是 AGENTS.md 明确记载的跨 crate 能力开关
（"facade 的 `cron` feature 把这条路径**两半一起**打开"）。

**因此：facade 的 feature 表没有死条目。** 现状也不是"42 条 feature 拉 ≥2 个 crate 的组合
爆炸"—— 拉多个 crate 是 `AGENTS.md` 既有规则的正常结果："Each feature pulls exactly the
crate behind its module **and the crates that module's types refer to**, so enabling one
module never leaves a type from a sibling module unnameable"。`harness` 拉 `provider` /
`tool`，是因为 `ReActAgent::new(Arc<dyn ModelProvider>, Arc<ToolRegistry>)` 的参数类型必须
可命名 —— 那是模块可用性，不是组合意图。

**所以本轮的 feature 工作只有两件，都不改现有语义：**

1. **`default` 保持现状**（16 条）。它的职责由 AGENTS.md 与 README 定义：`cargo add uva`
   就得到一个能跑的 agent。工具插件留在 default 里是**库的便利性**，不是部署组合 —— 后者是
   server config 的事。把工具移出 default 会破坏文档承诺的 5 分钟 hello-world，收益为零。
2. **加一条 `plugin-host`**（`plugin-host = ["dep:uva-plugin-host"]`，单 crate 轴），
   随 W2 的 `uva-plugin-host` 落地；`plugin-protocol` 已在 Task 10 加了同样形状的一条。
   两条都是"模块 + 它的类型所引用的 crate"规则的正常应用。

**"组合在运行时"这句话的落点不在 feature 表，在 server。** 一个部署想开哪些插件、按什么顺序、
给谁提权，全部由本节的 config 表达；`cargo feature` 只回答"这个模块编不编译得进来"。两条轴
本来不冲突 —— 早先的草案把它们对立起来是误判。

**唯一值得补的验证**：46 条 feature 里没有任何一条被单独验证过能否独立编译。做一次
`for f in <每条 feature>; do cargo check -p uva --no-default-features --features "$f"; done`
的矩阵作为回归基线；这条矩阵在加 `plugin-host` 时一起跑。

### 5.11 W3：server 无状态化

**现状**：这六个字段挂在 `ControllerInner`（`session/controller/inner.rs`）上，
`SessionController` 只持有 `inner: Option<Arc<ControllerInner>>`（`controller/mod.rs:87`）。

| 字段 | 内容 | 无状态化后 |
|---|---|---|
| `live: parking_lot::Mutex<LiveTurn>`（`inner.rs:136`） | 在途 turn 的瞬时投影 | **可以留在进程内** —— 它本来就是"live 可丢"的瞬时投影，durable 行才是真源 |
| `pending_resume: parking_lot::Mutex<Option<PendingResume>>`（`:139`） | 挂起的 resume | 见下方"已经做了一半"一节 |
| `pending_multimodal: parking_lot::Mutex<VecDeque<ParkedPrompt>>`（`:109`） | 排队的多模态 prompt | **必须落盘** —— 现在进程崩溃就丢（`dispatch.rs:263` 只写 `tracing::info!`，不写日志行） |
| `pending_provider: parking_lot::Mutex<Option<Arc<dyn ModelProvider>>>`（`:119`） | 本轮 provider 选择 | 本轮内的选择，崩溃后重新按 config 解析即可 |
| `steer_gate: tokio::sync::Mutex<()>`（`:157`） | steer 与 run 结算的串行化 | 随 controller 一起，不跨节点 |
| `state: Arc<Mutex<SessionState>>`（`:71`） | 会话状态机 | 从日志派生（v3 已确立"日志是权威"） |

**`pending_resume` 与"挂起跨重启"已经做了一半**（builder 复核确认）：
`SessionController::session_state()`（`controller/mod.rs:436-451`）**已经从日志派生**
`AwaitingApproval` —— 判据是"最后一段 suspended 里有 `approval_asked` 而没有 `approval_decided`"，
注释写得很清楚："whatever this process remembers. That is what lets a restarted server report the
same state."。`inner.awaiting_approval()`（`inner.rs:316`）也是从 `open_interrupts()` 派生的。
durable 词表也已存在：`SessionEvent::{ApprovalAsked, ApprovalDecided, ApprovalGranted,
ToolDispatch, RequestSnapshot}`（`uva-session/src/events.rs:381,401,420,441,459`）。

真正缺的是 `PendingResume` **本身**的重建 —— 它是客户端 POST 进来的
`{ token, decisions, allow_drift }`（`dispatch.rs:496-500`），不是服务端推导出来的。所以节点在
"客户端 POST 了 resume、run 还没消费"之间死掉时，**丢的是那批决策**（token 可由日志重算）。
修法二选一：(a) 客户端重发（resume 是幂等的，`ResumeToken` 按最后 durable seq + 未决集合摘要
派生，重复提交不产生第二次工具执行）；(b) 决策本身落成 durable 行。**倾向 (a)** —— 决策是客户端
的意图，服务端替它记住反而多一份真源；正确性靠 token 校验保证。

**无状态化的真实含义**：计算节点不持有会话真源。真源 = workspace 文件系统上的 JSONL 日志 +
cluster bus 上的租约。节点崩了，另一个节点接手时，从日志重建状态（`repair` → `fold` →
`SessionState`），不需要前一个节点的内存。

**要落的改动**：
1. `pending_multimodal` 与 `pending_resume` 变成 durable 行（新增 `SessionEvent` 变体），
   进程重启后由日志恢复。**这条是本段的核心** —— 现在排队的多模态 prompt 崩溃即丢。
2. **多模态载荷的字节不进日志，只进 attachment store。** 现有约定已经这么做了：
   `run_task.rs:309` 的注释说得很清楚 —— 原始图片/音频字节不序列化进 JSONL（5 MB 图片
   base64 化会把每轮日志撑爆，而没有读者把它读回成图片字节）；日志行里放的是
   `attachment_refs_of_parts` 产出的**元数据**（`kind` / `name` / `mime_type` / `byte_len`，
   见 `run_log.rs:100`）。所以"多模态排队要落盘"的正确形状是：
   - 字节 → `AttachmentStore`（内容寻址，`hash` 做键，fs backend 持久，`lib.rs:305`）；
   - 日志行 → 存**可回读的 ref**（含 `hash`），不是现在的纯元数据 —— 现在的 ref 没有
     `hash`，**回读不了**，这是要补的一处；
   - 恢复时按 `hash` 从 store 取回字节，重建 `ContentPart`。
   这条链自洽且不与"日志不存大字节"的既有约定冲突。
3. `live` 投影降级为"尽力而为的缓存"：节点切换后重建，客户端靠 `follow` 基线整窗替换，
   不依赖它存活。
4. 多节点：沿用现有 `cluster` 的 lease + bus 语义（`session_lease` / `SessionBus` / `ToolMutation`），
   增加"租约转移后从日志重建 controller"路径。`cluster-redis` feature 已存在，本段只补重建逻辑。
   **注意**：今天**没有租约转移路径**，只有 TTL 过期 + 重新获取
   （`cluster/redis_bus.rs:151-181`、`cluster/local.rs:111-143`）—— 转移语义是本段新增的。
5. `follow` 基线的**派生**已经是"从日志快照来"（`routes/follow.rs:202` 的 `build_baseline`），
   这条路子是对的。

**性能：这里有一个必须先说清的事实 —— `follow` 基线现在是整文件读。**

`FOLLOW_WINDOW = 200`（`routes/follow.rs:87`）只限制**基线携带多少行**，不限制**读多少**。
实际读取是 `sink.read()` → `JsonlSessionSink::read()` → `read_filtered(0, false)` → 对整个
`events.jsonl` 做 **`fs::read`**（`uva-session/src/jsonl.rs:395-397, 435-436`），
200 行的截断是**读完之后在内存里切的**（`follow.rs:210-214`）。

这对 W3 有直接后果：

- W3 要往同一个文件里加 durable 行，**这个文件会变大**，而每次 `follow` 订阅都整文件读一遍 ——
  成本随会话长度线性增长，长会话下会变成主导开销。
- 基线还混入了 `controller.live_snapshot()`（`follow.rs:231`），所以"基线只从日志派生"只对了一半。

**因此 W3 必须包含"尾读"**：按 offset / 尾部窗口读日志，而不是整文件读。这是性能修复，
也是"无状态化不牺牲吞吐"这个前提的兑现方式 —— 否则本段的性能承诺是假的。

### 5.12 与 pi / dsh 的位置对照

| 维度 | pi | dsh | Uva 目标 |
|---|---|---|---|
| 扩展单元 | 进程内 TS 扩展 | Cordis 插件（服务 + typed events + effect） | **两条通道**：Rust 静态插件 + 任意语言动态插件 |
| 事件词表 | 固定枚举 | 固定核心 + declaration merging | 封闭核心 + `PluginEvent` 逃生舱（wire 层独立类型） |
| 卸载 | `pi.on()` 返回 unsubscribe | 每个注册返回 disposer | `Registration` drop 即撤销（已有） |
| 组合方式 | npm 包 + 会话选择 | profile / bundle / patch | **运行时组合层**（W2-c）+ 窄化的 feature 轴 |
| 写者 | commit 单线 | `SessionHandle` 单写者 | `LogWriter`（已有，append/flush 分离） |
| 崩溃恢复 | task checkpoint | 日志只追加，读者补 closers | 日志是真源 + repair（已有） |
| 模型历史 | entries → provider 消息 | `deriveMessages()` | `derive_history` → `HistoryProjection`（已有） |
| 压缩 | `pi.compaction` entry | 可选 seam + replace surface + pruner | `Compactor` seam + pruner + provenance（已有） |
| 运行时依赖 | Node | Node | **零 tokio**（W1）+ 运行时中立公开 API |
| 权限模型 | 扩展与宿主同权 | 插件与宿主同权 | **远端默认降权**（B）—— 比两者都保守 |

---

## 6. 验收

### 6.1 机械验收（可脚本化）

1. `cargo tree -p uva-harness -e normal | grep -c tokio` == **0**。
2. `cargo tree -p uva-harness -e normal | grep -cE "reqwest|hyper|rustls|h2|tower"` == 0（保持）。
3. `cargo tree -p uva-plugin-protocol -e normal` 只含 `serde` / `serde_json` / `proc-macro2` 之类，
   **不含 tokio**。
4. `make check-harness-features` 绿；`make check-no-runtime` 绿（且把 `uva-harness`、
   `uva-session` 加入断言名单）。
5. `make check-harness-shape` / `check-harness-docs` / `check-plugin-shape` / `check-pub-surface*`
   / `check-clock` / `check-mvp-deps` 全绿。
6. `cargo +nightly fmt --all --check` 绿；`cargo clippy --all-targets --all-features --tests --all
   -- -D warnings` 绿。
7. `cargo doc --no-deps --workspace` 无 warning。

### 6.2 行为验收

**W1**
- **零 tokio 的验收 fixture 必须是"只依赖 harness"的消费方**。现有的两个 example 都不合格：
  `docs/examples/minimal-consumer` 依赖的是 **`uva` facade**（7 个 feature）且**自己声明了
  `tokio`**（`Cargo.toml:32`，为了跑 `#[tokio::main]`）；`docs/examples/external-consumer`
  直接依赖 `uva-harness`，但同时依赖另外 12 个 uva crate 且也声明了 `tokio`（`:38`）。
  → **新增一个最小 fixture**：只依赖 `uva-harness` + `futures` + `serde_json`，
  用 `futures::executor` 跑（照 `cargo run --example runtime_agnostic -p uva-harness`
  的既有形状），**不声明 tokio**。它的 `cargo tree` 必须零 tokio —— 这才是 §10 第 4 条的证据。
- **基准已修（本轮做掉了）**：`TruncatingContextManager` 的 fixture 克隆移出计时闭包、
  规模写实（401 条）。基线：无驱逐 **19.1 µs**、超窗 **381.9 µs**（相对旧的 386.7 µs 在噪声
  内不变）。`make bench --check` 只比"同一份工作的内联参考实现"，因此
  `TruncatingContextManager` **不**作为比值门禁条目。
- **`call.input` 不作验收标准**：`guard.rs` 的 clone 已在惰性闭包里（无 hook / 无 host 注册时
  不执行），默认路径零成本；`route.rs` 剩的一处是"按值收、被消费、上游是借用"的结构性拷贝。
  小入参 `Value::clone` = 81 ns，对 254 µs 的 turn 是 0.03%。超窗行里剩下的成本**未测量**：
  候选是 `uva-context` 的整表三次估计 + 逐条重估 + O(n) 搬移（§5.6），下一轮先加隔离
  bench 再谈降本。
- `uva-context` 与 `uva-steering` 也加入零 tokio 断言名单（它们是 harness 的依赖，
  闭包断言要覆盖到，否则 feature 统一化会把 tokio 悄悄带回来）。
- **`prompt_section` 真的到达 system prompt**：一个静态插件注册一段 section 后，跑一轮，
  断言该 section 的文本出现在模型收到的 system prompt 里（不是只断言 `host.prompt_sections()`
  返回了它 —— 那是今天的既有行为，且只有测试在看）。
  另外两条：多个 section 按**注册顺序**渲染（不依赖哈希或并发）；section 出现在身份段**之后**。

**W2**
- 一个用 Rust 写的静态插件 crate：不改 harness 一行，`cargo add uva-harness` 即可 `install`。
- 一个用**非 Rust** 写的动态插件进程：按协议实现 3 个方法，宿主按名从 config 装上，
  工具可被模型调用，事件可被观察，工具结果可被改写并留下审计行。
- 降权验证：远端插件**无法** block 一次工具调用（宿主侧拒绝其 `uva/gate`）。
- 提权验证：config 里 `authority: full` 的远端插件**可以** block。
- 远端插件自报 `destructive_hint: false` 的工具，仍按宿主的 risk 策略走审批。
- 远端 transformer 超时：turn 不卡死，保留原结果 + 记诊断。
- 远端 transformer 的改写在日志里留下 `tool_result_transformed` 行，且崩溃恢复后重放得到**相同**的
  工具结果（这是"日志是真源"的可观测断言）。

**W3**
- 多模态 prompt 排队期间杀掉节点 → 重启后该 prompt 仍在（现在会丢 —— `dispatch.rs:263` 不写日志）。
- 挂起（awaiting approval）期间杀掉节点 → 重启后仍处于挂起态，resume 仍可执行。
  **注意这条今天已经部分成立**（`session_state()` 已从日志派生 `AwaitingApproval`）——
  缺的是那批客户端决策的恢复（§5.11 已说明取舍：靠客户端重发）。
- 两个节点共享 workspace：会话在 A 上跑，B 能 `follow` 到同一份 durable 行。
- **`follow` 基线不再整文件读**：长会话（≥ 10k 行）下订阅的读取量与文件全长**不成正比** ——
  这条要有可观测断言（读了多少字节），否则"性能不退化"无法验证。

### 6.3 端到端剧本

沿用 `docs/tutorial/complete-agent.md` 的 A–G 剧本，重跑并追加 H（远端插件）、I（无状态重启）。
每条要可观察断言，不以"接口返回 200"代替。

---

## 7. 实施顺序（每步工作区可编译）

| 步 | 内容 | 破坏性 | 依赖 |
|---|---|---|---|
| 1 | `uva-plugin-protocol` 骨架：方法名常量、能力协商、错误码、wire 类型、serde 派生 | 无（新增 crate） | — |
| 2 | `uva-core` 新增 `Sleeper` + `Blocking` 接缝 + 默认实现；`uva-harness-tokio` 加 tokio 实现 | 中（新增 trait） | — |
| 3 | session runtime-free 化：摘要超时换 `Sleeper`；fs IO 换 `Blocking`；**`InputQueue` 挪进 server**（公开面移除 `uva::session::manager::InputQueue`，更新 `check-pub-surface-strict` 基线） | 中 | 2 |
| 4 | provider / steering 的 tokio 依赖下沉或 gate 到 adapter feature；**`provider` 的 `tokio` 改 `optional`**；server 显式取得 `uva-core/tokio-util`（或改用 `AtomicCancelToken`） | 中 | — |
| 5 | **tokio 闭包清零**（`cargo tree` 断言 0） | 无（内部） | 3, 4 |
| 6 | `dyn` 逐站点复核（§5.5）——**结论：不泛型化**，无代码改动 | 无 | 5 |
| 7 | 热路径：基准先修（fixture 移出计时区、规模写实）再取基线；`call.input` 复核后**不改**（§5.6） | 无 | 6 |
| 8 | 公开面收窄：零引用 `pub` 删除、`hash_to_u64` 收窄到测试内部、`publish = false`（仅 server）、删 `.tmp`。**server 降 `pub(crate)` 的全量 sweep 已撤销** —— 14 个集成测试是独立 crate，且 server 已不可发布，其 `pub` 不再是契约（见 §5.7） | 中 | 7 |
| 8b | **`prompt_section` 链接上**：prompt 组装器读 host 的 sections（注册顺序、插在身份段之后 / hint 之前） | 中（harness 行为改动） | 5 |
| 9 | `uva-plugin-host`：进程管理、JSON-RPC 客户端、能力协商、远端工具 → `Host` 注册 | 无（新增 crate） | 1, 2 |
| 10 | wire 双向映射：Rust `HookEvent` ↔ wire 事件；审计行 `tool_result_transformed` | 中 | 9 |
| 11 | 远端权限矩阵落地：默认降权 + `authority: full` 提权位 + 远端 risk 由宿主判定 | 中 | 10 |
| 12 | `PluginEvent` 逃生舱：`SessionEvent::Plugin` + live 事件扩展 | 中 | 9 |
| 13 | 运行时组合层：config schema + profile 解析 + 组装器（静态 + 动态混合） | 大 | 9, 11 |
| 14 | facade feature 收窄：删 `checkpoint` / `transcript` 两条空转发，其余按"每条一个 crate 依赖"重写 | 大（生态可见） | 13 |
| 15 | server 无状态化：`pending_multimodal` 落盘（字节进 attachment store、日志存可回读 ref）+ 租约转移重建 + **`follow` 基线改尾读** | 大 | 3, 13 |
| 16 | 前后端贯通：前端接运行时组合层与远端插件状态；H/I 剧本 | 中 | 13, 15 |
| 17 | 教程与验收：`docs/tutorial/` 补双通道插件与无状态部署两章；新增"只依赖 harness 的最小消费方" fixture | 无 | 全部 |

第 14 步与第 15 步各是一次切换，不留双写：feature 表收缩与组合层上线同一次提交；
durable 化与旧的内存队列同一次切换。第 15 步里的**尾读**与 durable 化同一次切换 ——
两者都改日志的读写路径，分两次切会留下两套读法。

---

## 8. 我代做的决定（可否决）

| 决定 | 理由 |
|---|---|
| 动态插件走独立进程而非 WASM / dylib | harness 零依赖这条最硬；进程隔离同时给了崩溃隔离与语言无关 |
| wire 复用 MCP 的 JSON-RPC 与传输抽象，不另立传输栈 | 已有 `McpTransport` / stdio / http / supervisor 可复用 |
| wire 上不直接复用 Rust 的 `HookEvent` | 内部枚举形状会演进；当 wire 契约等于协议破坏 |
| 远端插件默认无否决权 | 进程隔离已经是增量；再叠否决权是"权力更大但更不可信" |
| 远端工具的 risk 由宿主判定 | 插件自报注解即可绕过审批，那正是降权要堵的洞 |
| 远端 transformer 必须落审计行 | 否则模型所见与日志不一致，破坏"日志是真源" |
| 委派不进 wire | 子 agent 要在本进程跑；跨进程是"远程 agent 端点"，语义不同 |
| 组合层在 server 侧，不进 harness | "这个部署长什么样"与"循环怎么跑"是两件事 |
| feature 收窄而非删除 | feature 是依赖轴，Rust 无法在运行期凭空多出一个依赖 |
| 摘要超时改走 `Sleeper` 接缝 | v3 设计已声明 harness 不武装定时器；代码是漂移 |
| `SessionEvent::Plugin` 用 `kind: String` + JSON payload | 插件的 kind 是运行期值，`'static` 不可满足（除非泄漏）；泛型事件词表会污染全链路签名 |
| `live` 投影保留在进程内 | 它本来就是"live 可丢"的瞬时投影；durable 行才是真源 |
| `publish = false` 只加到 server | 应用 crate 不该发到 crates.io；facade 是 `cargo add uva` 的消费入口，必须可发布 |
| 泛型化不落地：`dyn` 站点一个不收敛 | 逐站点复核（§5.5）全部过三问；`AgentCaps` 按值进 `ReasoningStrategy::run` 会传染整条签名链，收益是每 step 一跳 vtable |
| `prompt_section` 本轮链接上（对话已定） | 这一格若永远为空，插件就不是一等能力；改动面小，且不违反"动态通道对 harness 不可见" |
| facade feature 按"每条只表达一个 crate 依赖"收窄（对话已定） | feature 是编译期轴，组合意图放不进它；dsh 的 profile/bundle 就是为此 |
| `checkpoint` / `transcript` 两条 feature 直接删 | 它们指向**不存在的目标**（`uva-harness` 与 `uva-eval` 都没有 `[features]` 表）—— 空转发，真死代码 |
| `InputQueue` 挪进 `uva-app-server`（对话已定） | session 不保留多余的语义与作用；它本质是 server 桥接 submit 与 run loop 的东西 |

---

## 9. 留给实现者的部分

- `uva-session` 是"一个 crate 内 gate"还是"拆 `uva-session` + `uva-session-fs`"。
- `Sleeper` 的实现取舍（`BoxFuture` vs `impl Future` in trait，默认实现里 `ThreadSleeper`
  的取舍）—— **已结**：形状由 §5.4 定，`Blocking::run` 的对象安全形状由 §5.4(b) 定
  （泛型方法不是对象安全的，所以闭包结果由调用方携带）。`ThreadSleeper` 只是默认实现，
  不构成待决项。
- `ReActAgent` 泛型参数 —— **已结**：不加（§5.5 的逐站点复核，结论是没有代码改动）。
- `TruncatingContextManager` 超窗成本 —— **未结**：现有 bench 行不隔离它，成因候选是整表三次
  估计 + 逐条重估 + O(n) 搬移（§5.6）。下一轮先补一行能隔离它们的 bench。
- wire 事件的粒度（一次 `uva/event` 携带多个事件，还是一事件一消息）。
- 组合层的 config schema 细节与 profile 的继承/覆盖规则。
- `pending_multimodal` 落盘后与"prompt 在 run 开始时持久化"这一现有约定的交互。
  **注意**：`dispatch.rs:229-237` 的注释说 prompt 是"persisted at run completion"，**这是错的** ——
  `run_task.rs:299-307` 写明是"AFTER reading the sink … and BEFORE invoking the agent"，
  当前轮的 `UserInput` 行在 `run_task.rs:392` 写入，即 **run 开始时**就落盘了。
  多模态**文本**部分也落盘（`prompt_text_of_parts`，`run_task.rs:346-353`）；
  只有多模态的**字节**是有意不落的。所以"排队中的多模态 prompt 崩溃即丢"丢的是**排队期间那一份**，
  不是已开始的轮次。实现时顺带修掉 `dispatch.rs` 那条 stale 注释。
- 多节点租约转移的具体重建顺序。
- 测试夹具与 fake plugin 进程的组织。

---

## 10. 验收定义

"完成"指：

1. §6.1 的 7 条机械验收全绿。
2. §6.2 的 7 条行为验收有可复现的观察记录。
3. §6.3 的 A–I 剧本在真实 `uva-app-server` + `uva-web` 上跑通。
4. §6.2 W1 里新增的那个**只依赖 `uva-harness` 的最小消费方**（不声明 tokio、用
   `futures::executor` 跑）能装一个自有插件并跑通一轮，且 `cargo tree` 零 tokio ——
   证明基座不是 server 专用。（现有的 `minimal-consumer` / `external-consumer` 都不作为这个
   证据：前者依赖 facade 且自带 tokio，后者依赖 13 个 uva crate 且自带 tokio。）
5. 一个**非 Rust** 的动态插件能按协议装上并被模型调用 —— 证明动态通道真的语言无关。
6. `docs/tutorial/` 有"写一个静态插件"与"写一个动态插件"两章，从空环境照做，不需要读源码。

---

## 11. 三条已拍板的事项

其余问题已在文中定稿或归入 §9（留给实现者）。下面三条改的是**产品语义**，已由对话确定，
正文相应章节已按此定稿。

**① `prompt_section` 链接上**（§5.9）。今天插件注册的 prompt 片段从不到达 system prompt
（唯一读取口是一个测试）。本轮把 prompt 组装器与 host 的 sections 接上，让这一格真正可用。
顺序规则（注册顺序、插在身份段之后 / steering hint 之前）见 §5.9。

**② facade feature 按"每条只表达一个 crate 依赖"收窄**（§5.10）。具体清单与四条判定规则见
§5.10；其中 `checkpoint` 与 `transcript` 两条**指向不存在目标的空转发 feature 直接删除**。

**③ `InputQueue` 挪进 `uva-app-server`**（§5.4(c)）。session 不保留多余的语义与作用。
连带后果：`uva::session::manager::InputQueue` 从 facade 公开面上消失（本轮唯一的
公开面移除，不留别名），`check-pub-surface-strict` 的 session 基线要更新。
