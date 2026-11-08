# Elysper - AI Agent Framework

<p align="left">
  <img src="elysper-web/public/logo.svg" alt="Elysper" height="56">
</p>

<!-- Badges: 集成状态一目了然 -->
<p align="left">
  <a href=".github/workflows/ci.yml"><img alt="CI" src="https://img.shields.io/badge/CI-passing-2ea043?style=flat-square&logo=github-actions&logoColor=white"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg?style=flat-square"></a>
  <a href="https://www.rust-lang.org"><img alt="Rust 1.98+" src="https://img.shields.io/badge/Rust-1.98%2B-orange.svg?style=flat-square&logo=rust&logoColor=white"></a>
  <a href="https://github.com/crochee/elysper/releases/latest"><img alt="Releases" src="https://img.shields.io/badge/release-v0.1.0-blueviolet.svg?style=flat-square&logo=github-releases"></a>
  <a href="https://github.com/crochee/elysper/discussions"><img alt="Discussions" src="https://img.shields.io/badge/discussions-GitHub-ff7043.svg?style=flat-square&logo=github"></a>
  <a href="SECURITY.md"><img alt="Security policy" src="https://img.shields.io/badge/security-policy-blue.svg?style=flat-square&logo=security&logoColor=white"></a>
  <a href="deny.toml"><img alt="cargo-deny" src="https://img.shields.io/badge/cargo--deny-enforced-brightgreen.svg?style=flat-square&logo=rust"></a>
</p>

**Elysper** is pronounced **/ɛˈlɪspər/** ("eh-LISS-per").

Elysper is a modular, high-performance AI Agent framework written in Rust. It implements the ReAct (Reasoning + Acting) pattern with comprehensive tool execution, session management, and observability hooks.

> **Just want a 5-minute hello-world?** [`docs/QUICKSTART.md`](docs/QUICKSTART.md) — one dependency, one provider, one agent, no API key.
> **Adopting for production / commercial use?** [`docs/COMMERCIAL.md`](docs/COMMERCIAL.md) — SLA, security, supply-chain, and governance checklist.
> **New here?** [`MINIMAL.md`](MINIMAL.md) — the 4-step MVP guide (provider → registry → steering → agent).
> **Replacing a piece?** [`SEAMS.md`](SEAMS.md) — every trait a consumer can substitute, what ships for it, and the line that installs yours.
> **How decisions are made?** [`GOVERNANCE.md`](GOVERNANCE.md) — BDFL today, council evolution path.
> **Looking for one specific document?** [`docs/README.md`](docs/README.md) — the docs tree entry, [`docs/INDEX.md`](docs/INDEX.md) — the live doc map, and [`docs/ARCHIVE.md`](docs/ARCHIVE.md) — the frozen record of every past round.

## 30 秒看懂 Elysper

```text
┌──────────────────────────────────────────────────────────────────────┐
│                       Consumer Application                           │
│            (HTTP server, CLI, embedded widget, robot …)              │
└──────────────────────────────┬───────────────────────────────────────┘
                               │ depends on ONE crate: `elysper`
                               ▼
┌──────────────────────────────────────────────────────────────────────┐
│  elysper (facade: feature-gated re-exports + curated `prelude`)      │
└──┬──────────┬──────────┬──────────┬──────────┬──────────┬──────────┬──┘
   │          │          │          │          │          │          │
   ▼          ▼          ▼          ▼          ▼          ▼          ▼
┌──────┐ ┌────────┐ ┌──────┐ ┌────────┐ ┌──────┐ ┌──────────┐ ┌────────┐
│Provider│ │  Tool  │ │Context│ │Steering│ │Session│ │Scheduler │ │  MCP   │
│ OpenAI │ │read,…  │ │window │ │ guard/ │ │lifecycle│ │ cron    │ │ client │
│ Anthro │ │ plugins│ │policy │ │hook/hint│ │ sink   │ │ interval│ │ tools  │
└──────┘ └────────┘ └──────┘ └────────┘ └──────┘ └──────────┘ └────────┘
   └────────┬────────┴─────┬─────┴────┬─────┴─────┬──────┴──────┬─────┘
            │              │          │           │             │
            └──────────────┴──────────┴───────────┴─────────────┘
                                       ▼
                ┌────────────────────────────────────────┐
                │     elysper-harness: ReAct loop        │
                │  (Agent + DelegatingTool + strategy) │
                └─────────────────────┬──────────────────┘
                                      ▼
                ┌────────────────────────────────────────┐
                │      elysper-core: primitives          │
                │  Clock · IdGen · CancelToken · Spawner │
                │  Registry · paths · schemas · errors    │
                └────────────────────────────────────────┘
```

**两个最高级目标**（决定下面的所有约定）：

1. **可作为 lib** — `cargo add elysper` 是消费端入口；下游只依赖
   `elysper` / `elysper-*` crates，永远不回指消费端应用。
2. **乐高式组装** — 七大组件（Provider / Tool / ContextManager /
   Steering / SessionSink / CancelToken / Agent）每一个都是可换
   trait + 默认实现；外部消费者可以从一个空的 `cargo new` 仓库
   拼出一个完整的 AI agent。

## 5 分钟跑起来

一个依赖、一段脚本、一行 proof —— 离线、无 API key、无 env：

```bash
git clone https://github.com/crochee/elysper && cd elysper
cd docs/examples/minimal-consumer && cargo run
# → MVP-OK
```

整条链路（scripted provider → tool registry → steering → ReAct
loop → session writer）在这一份代码里真实跑一遍，并断言工具确实被
调用了一次。完整阅读路径见 [`docs/examples/README.md`](docs/examples/README.md)，
跨 41 个可运行示例与两个独立 consumer crate。

## 适用场景

| 场景 | Elysper 的对应能力 |
|---|---|
| **生产级 SaaS / 内部平台** | 25 个 crate 独立 `cargo add`，按需拉起 provider / tool / telemetry，无未使用依赖 |
| **本地 CLI / IDE 插件** | `CancelToken` 与 `Spawner` 让 stdout / LSP 长生命周期进程安全取消 |
| **嵌入式 / 机器人** | 无 tokio 依赖的 lib 公共 API（`make check-public-api-runtime` 保证），可跑在 `std::thread + futures::executor` |
| **多智能体工作流** | `elysper-tool-task`（多智能体委派）+ `elysper-workflow`（声明式 DAG），JSONL journal 可断点续跑 |
| **审计 / 合规** | `elysper-steering`（Guard / Hook / Hint）+ `elysper-telemetry/otlp`（OTel trace），事件流可接企业 SIEM |
| **商业化 / 私有 fork** | MIT 许可 + 双轨发布（cargo + raw binary）+ `cargo-deny` 锁供应链 + SLSA Build L1 来源证明（见 [RELEASE.md](RELEASE.md)） |

## 状态与治理

| 维度 | 状态 |
|---|---|
| 测试 | `cargo test -p <crate>` 逐 crate 通过（`make test-crates`）；CI 5 段：fmt / clippy / tests / examples / bench |
| 静态分析 | `cargo clippy -D warnings --all-targets --all-features --tests --all`（`make lint-rust`） |
| 文档 | rustdoc (`-D warnings`)，`MINIMAL.md` 4 步 MVP guide 编译为 doctest，`SEAMS.md` 7 大组件可替换点索引 |
| 安全 | `cargo-audit`（rustsec/advisory-db）+ `cargo-deny`（许可证 + 来源 + ban），每周一跑 + 每次 PR 跑 |
| 供应链 | `Cargo.lock` 入库；Dependabot 周一开 PR（patch 自动、minor / major 走 review）；release 产物为多架构容器镜像，OCI labels 自证身份 |
| 治理 | [SECURITY.md](SECURITY.md) / [CONTRIBUTING.md](CONTRIBUTING.md) / [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) / [RELEASE.md](RELEASE.md) / [MAINTAINERS.md](MAINTAINERS.md) / [.github/CODEOWNERS](.github/CODEOWNERS) 完整 |

## 仓库分层

The framework is organized as a Rust workspace with **40 members**, all under `crates/`, every one of them a `elysper-*` crate except the `elysper` facade. Of those, the libraries are meant to be composed by consumers; `elysper-app-server` is the application crate and `elysper-mcp-server` the stdio binary — the two places where `tokio` / `axum` may appear in a public signature. The front-end sits next to the workspace as `elysper-web/` with its own `package.json`.

### Core Crates

| Crate | Description |
|-------|-------------|
| [elysper-core](crates/elysper-core/) | Common utilities (ID generation, time, paths, schemas) |
| [elysper-provider](crates/elysper-provider/) | LLM provider abstraction (OpenAI, Anthropic) |
| [elysper-context](crates/elysper-context/) | Context-window management (truncating / summarizing / DAG strategies) + 3-layer `Memory` tier (conversation / working / long-term) over the session sink |
| [elysper-tool](crates/elysper-tool/) | Tool paradigm: `Tool` trait, registry, exposure projection, `workspace` path confinement, and the two synthetic contracts (`__get_full_output`, `structured_output`) — **no agent-facing tool set** |
| [elysper-tool-read](crates/elysper-tool-read/) · [-write](crates/elysper-tool-write/) · [-shell](crates/elysper-tool-shell/) · [-todo](crates/elysper-tool-todo/) · [-web](crates/elysper-tool-web/) · [-task](crates/elysper-tool-task/) · [-scheduler](crates/elysper-tool-scheduler/) · [-search](crates/elysper-tool-search/) | The tool plugins, one brick each (the shell owns its OS execution policy; `-task` is the multi-agent `task` tool; `-scheduler` is the `schedule` tool over `elysper-scheduler`; `-search` is the cross-domain `search` tool over a host-built `elysper-search` registry) |
| [elysper-session](crates/elysper-session/) | Session lifecycle and state management |
| [elysper-harness](crates/elysper-harness/README.md) | The harness: core ReAct loop, `Agent` / `DelegatingTool` / strategy seams, `AgentRegistry`, type definitions |
| [elysper-steering](crates/elysper-steering/) | Steering layer: guards / hooks / hints / tracker / output transformer + `HookMap` typed registry |
| [elysper-telemetry](crates/elysper-telemetry/) | Observability in two switchable halves: console/file logging always, Prometheus (`metrics`) and OTLP export (`otlp`) as features |
| [elysper-attachment](crates/elysper-attachment/) | Multimodal attachment store (image / audio / file bytes) with filesystem + in-memory backends |
| [elysper-mcp](crates/elysper-mcp/) | MCP client: drive any MCP server over a pluggable transport and publish its tools into the local registry |
| [elysper-scheduler](crates/elysper-scheduler/) | Runtime-neutral cron / interval / once dispatcher with PID-locked atomic JSON persistence |
| [elysper-macros](crates/elysper-macros/) | `#[derive(Tool)]` + `#[tool(name, description, mode)]` — generate a `elysper_tool::Tool` impl from a struct + inherent `execute` |
| [elysper-eval](crates/elysper-eval/) | Evaluation framework: suites, sync/async metrics, runner, judge + schema validation, JSON/CSV export |
| [elysper-workflow](crates/elysper-workflow/) | Declarative multi-agent workflow runtime: serde `WorkflowSpec` (agent / fan-out / pipeline), one `WorkflowHost` effect seam, caps + concurrency + live control, JSONL journal with prefix replay |
| [elysper](crates/elysper/) | The facade: every piece behind feature flags plus a curated `prelude`, so a consumer can depend on one crate and assemble from zero |
### Interface Crates

| Crate | Description |
|-------|-------------|
| [elysper-app-server](crates/elysper-app-server/) | The application crate: `core` (headless JSON-RPC 2.0 on stdio) + `serve` (the gateway: one WebSocket at `/rpc`, HTTP probes, static web) |

### Support Crates

| Crate | Description |
|-------|-------------|
| [elysper-test-support](crates/elysper-test-support/) | Shared mock implementations for cross-crate testing |
| [elysper-web](elysper-web/) | React/Vite frontend speaking JSON-RPC 2.0 over the gateway's `/rpc` WebSocket |

### Protocol / Cache / Skill Crates

| Crate | Description |
|-------|-------------|
| [elysper-skill](crates/elysper-skill/) | Skill registry (slash-command, prompt, and tool bundles) |
| [elysper-agents](crates/elysper-agents/) | Agent definitions as files (`agents/<name>/agent.md` + optional `config.json`), discovered over the two-layer `~/.agents` layout |

### Workspace Members

Every member of the `[workspace.members]` array in the root `Cargo.toml` is
listed below, and every path is a real on-disk directory:

| Path | One-line responsibility |
|------|-------------------------|
| `crates/elysper-core` | Cross-cutting utilities (IDs, time, paths, error schemas) |
| `crates/elysper-telemetry` | Tracing + optional OTel pipeline |
| `crates/elysper-provider` | LLM provider trait, OpenAI / Anthropic adapters |
| `crates/elysper-context` | Context-window management (truncating / summarizing / DAG strategies) |
| `crates/elysper-tool` | Tool paradigm: trait, registry, exposure projection, `workspace` path confinement |
| `crates/elysper-tool-*` | Tool plugins (`-read`, `-write`, `-shell`, `-todo`, `-web`, `-task`, `-scheduler`, `-search`), one tool each |
| `crates/elysper-skill` | Skill registry + loader |
| `crates/elysper-agents` | Agent definitions as files: `agents/<name>/agent.md` + `config.json`, discovered over the `~/.agents` layout |
| `crates/elysper-session` | Session lifecycle + cleanup daemon |
| `crates/elysper-harness` | ReAct loop agent (the "AI" of Elysper) |
| `crates/elysper-kernel` | Plugin kernel: manifests, dependency-ordered `mount_all`, reversible `EffectHandle`s, per-plugin `dispose_plugin`, and the `ui.` / core namespace boundary |
| `crates/elysper-app-server` | The application crate: headless JSON-RPC 2.0 core (`core`) + WebSocket gateway (`serve`) — the human entry point; boots the native plugin set through the kernel, polls `profile.toml` for HMR, and serves `ui/schema` |
| `crates/elysper-attachment` | Multimodal attachment store (filesystem + in-memory backends) |
| `crates/elysper-mcp` | MCP client (stdio + in-memory transports) and tool publication |
| `crates/elysper-scheduler` | Cron / interval / once dispatcher (caller owns the timer) + atomic JSON job store |
| `crates/elysper-macros` | `#[derive(Tool)]` proc-macro: derive `elysper_tool::Tool` from a struct + inherent `execute` |
| `crates/elysper-eval` | Evaluation framework: suites, sync/async metrics, runner, JSON/CSV export |
| `crates/elysper-workflow` | Declarative workflow runtime: `WorkflowSpec`, `WorkflowHost`, caps + concurrency + control, JSONL journal prefix replay |
| `crates/elysper-test-support` | Mock fixtures + `ReplayProvider` for golden-transcript regression tests |
| `crates/elysper` | Facade: feature-gated re-exports of every piece + a curated `prelude` + the assembly tutorial |
## Use as a Library (assemble an agent from scratch)

Every crate is a lego brick with a runtime-neutral public API: the
agent takes `Arc<dyn elysper_core::CancelToken>` (a std-only
`AtomicCancelToken` ships in-tree; tokio users' `CancellationToken`
coerces with no adapter), streams are `futures::Stream`, and the
typed-event channel is `futures::channel::mpsc`. The executor is the
consumer's choice — including the one place the loop used to hard-code
it: detached per-turn work now goes through
[`elysper_core::spawn::Spawner`](crates/elysper-core/src/spawn.rs),
installed via `ReActAgent::with_spawner`. (Tool dispatch needs no
spawner at all: `run_stream` drives every call from the caller's
poll.)

```bash
cargo run --example runtime_agnostic -p elysper-harness
# → RUNTIME-AGNOSTIC: OK
# a full turn (tool call included) on std::thread + futures::executor::block_on
```

The boundary is documented rather than implied: the tool plugin crates
(`elysper-tool-read` / `-write` / `-shell` use `tokio::fs` /
`tokio::process`; `elysper-tool-web` uses `reqwest`), the provider HTTP
adapters, and the JSONL session sink are tokio-bound *plugins* — a
consumer on another runtime does not depend on them, or supplies their
own `Tool` / `ModelProvider` / `SessionSink`, and the loop still runs
on their executor. Timers are the remaining coupling (provider retry
backoff, the streaming idle watchdog): those paths need a reactor
today.

The runtime *features* are declared per crate, not inherited: the
workspace entry carries no `full`, so a piece asks for the tokio
subsystems its own code uses (`elysper-provider` → `macros` + `time`,
`elysper-session` → `rt` + `sync` + `time` + `macros`, …) and the
pieces that only *test* with tokio — `elysper-core`,
`elysper-scheduler`, `elysper-macros`, `elysper-eval`,
`elysper-workflow`, `elysper-search`, `elysper-telemetry` without
`otlp` — pull no runtime into a consumer's build at all:

```bash
make check-no-runtime   # asserts exactly that, for exactly those crates
```

**One dependency is enough.** The `elysper` facade crate re-exports
every piece behind feature flags and curates a `prelude`, so a
consumer can depend on `elysper` alone and read the assembly tutorial
in its crate docs (`cargo doc -p elysper --open`) instead of choosing
between nine crates:

```toml
[dependencies]
elysper = { path = "crates/elysper" }            # default features assemble a basic agent
# elysper = { path = "crates/elysper", features = ["workflow", "sqlite", "mcp"] }
```

```rust,ignore
use elysper::prelude::*;   // ReActAgent, ModelProvider, Tool, Steering, …

let agent = ReActAgent::new(provider, registry)
    .with_steering(steering);

The runnable tutorial assembles all seven pieces and runs them
end-to-end (no network, no API key) — through the facade, or crate by
crate if you prefer to see each brick:

```bash
cargo run --example assemble_from_zero      -p elysper         # prelude only
cargo run --example assemble_with_builder   -p elysper-harness # each crate named
```

Sketch of the same assembly:

```rust,ignore
use std::sync::Arc;
use elysper_harness::{Agent, AgentInput, ReActAgent};
use elysper_core::{AtomicCancelToken, CancelToken, SystemClock};
use elysper_session::{LogWriter, SessionSink, in_memory::InMemorySessionSink};
use elysper_steering::Steering;
use elysper_tool::{ToolEntry, ToolRegistry};
use elysper_tool_read::ReadTool;   // tool implementations are plugins:
use elysper_tool_shell::ShellTool; // depend on the bricks you want

let provider = Arc::new(MyProvider::new());               // 1. any ModelProvider
let registry = ToolRegistry::new();                       // 2. tools: compose
registry.register_entry(ToolEntry::new(Arc::new(ReadTool::new())));
registry.register_entry(ToolEntry::new(Arc::new(ShellTool::new())));
let registry = Arc::new(registry);
let steering = Arc::new(Steering::default_policy("."));    // 3. guards/hooks/hints
let sink = Arc::new(InMemorySessionSink::new("demo"));     // 5. the durable log
let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
let cancel: Arc<dyn CancelToken> = AtomicCancelToken::shared(); // 6. runtime-free

let agent = ReActAgent::new(provider, registry)            // 7. the loop
    .with_steering(steering)
    .with_log_writer(writer);

let mut stream = agent.run(AgentInput::text("hi"), cancel).await;
while let Some(event) = stream.next().await { /* SSE-style events */ }
let rows = sink.read().await.unwrap();       /* request_header / step / … */

Per-crate responsibilities and the composition contracts are
documented in each crate's `lib.rs` and `README.md`. The
per-round design records — what was adopted from traitclaw / dsh /
pi / pi-subagents, and why — were written as
`docs/optimization-report-R*.md` (one file per round, R4 onward,
with R14–R28 consolidated) plus `docs/reference-parity.md`; those
files are not part of this repository, so the rounds are named here
rather than linked.

### Composition primitives added in R9

The workspace also ships four small lego primitives
that turn the assembly above into a typed configuration, layered
skill registry, and runtime-neutral spawn surface:

| # | Primitive | Crate | What it gives you |
|---|---|---|---|
| 1 | `ProviderProfile` + `ProviderRegistry` | `elysper-provider` | Typed `enum` for OpenAI / Anthropic / Stub profiles; `ProviderRegistry::build(name)` returns `Box<dyn ModelProvider>` |
| 2 | `SkillProvider` + `SkillRegistry` | `elysper-skill` | Plug any source of skills (filesystem, in-memory, remote) into one deduplicated registry |
| 3 | `SkillApplication` / `SkillApplicationContext` | `elysper-skill` | Opt-in structured I/O for procedural skills: `SkillRequest → SkillResponse` closure |
| 4 | `HookMap::on_with_state` / `dispatch_with_state` | `elysper-steering` | Hook handlers that read `&AgentState` (pi `harness/hooks.ts` parity) |
| 5 | `OperationSnapshot` / `OperationState` | `elysper-session` | Read-only status record (running / completing / failed / cancelled + iteration + usage + last error / compaction) |

### Composition primitives added in R10–R13

Later rounds kept growing the same lego axes:

| # | Primitive | Crate | What it gives you |
|---|---|---|---|
| 7 | `ModelTier` + `TierLimits` | `elysper-provider` | Coarse capacity bucket (Small/Medium/Large) with per-tier presets (tool budget, concurrency, hints) |
| 8 | `Steering::for_tier` / `tier::auto` | `elysper-steering` | One-line tier-tuned guard/hint/tracker bundle (traitclaw parity) |
| 9 | `AdaptiveRegistry` | `elysper-tool` | Tier-capped LLM-visible tool list over any `Arc<ToolRegistry>` |
| 10 | `ToolOutputDefinition` + `present_call` / `present_result` | `elysper-tool` | Tool-owned canonical render contract for LLM + UI surfaces (dsh parity) |
| 11 | `Router` + `LeaderRouter` + `mentions_to_task_specs` | `elysper-tool-task` | `@agent: prompt` text mentions → delegation `TaskSpec`s |
| 12 | `AttachmentStore` + `ImageAttachmentRef` | `elysper-attachment` | Content-addressed multimodal store (sha256, hash re-verify, FS + memory backends) |
| 13 | `SnapshotBus` + `subscribe_snapshots` | `elysper-session` / `elysper-app-server` | Publish/subscribe channel for run lifecycle snapshots; surfaced by the `sessions/status` method |
| 14 | `ReActAgent` + `with_*` chain | `elysper-harness` | One-expression assembly of a complete agent (`new(provider, registry)` + chained setters; the harness *is* the builder) |
| 15 | `validate_against_schema` + `SchemaViolation` | `elysper-core` | JSON-Schema-subset validator with dotted-path, all-at-once violations |
| 16 | `StructuredOutputTool` | `elysper-tool` | Schema-validated structured-output capture; `with_output_schema(json)` on the agent auto-injects it |
| 23 | `GroupJoin` | `elysper-tool-task` | Batch background-agent completions into one notification, with a timeout so a straggler can't stall the batch (clock injected — runtime-neutral) |
| 24 | `ToolRestriction` + `RestrictedRegistry` | `elysper-tool` | Per-scope allow / deny tool visibility; ancestor scopes intersect (dsh parity) |
| 25 | `ToolSchemaBuilder` + `ToolFeatures` | `elysper-tool` | Feature-gated JSON-Schema fields — a refused capability is never described to the model |
| 26 | `credential::normalize_api_key` | `elysper-provider` | Pre-send credential classifier; a malformed key fails before it reaches the transport (dsh parity) |
| 27 | `MentionClone` + `LeaderRouter::mention_clone_mode` | `elysper-tool-task` | Route a `@agent:` mention through an invisible throwaway clone, not the leader's own tool call (pi-subagents parity) |
| 28 | `Scheduler` + `Job` + `ScheduleStore` | `elysper-scheduler` | Cron / interval / once dispatch with the timer owned by the caller and atomic JSON persistence |
| 29 | `OperationRequest` + gated operation dispatch | `elysper-app-server` | One discriminated-union request (`prompt | skill | prompt_template | compaction | navigation`) behind `[operations] enabled`, served by the `chat/operation` method |
| 30 | `CompactionLifecycle` emitter + `compaction_outcomes` | `elysper-context`, `elysper-session` | Pair `compaction_start`/`end` in the durable log; an orphan is `Interrupted`, not an ambiguous gap |
| 31 | `AssistantChunk` + `assemble_chunks` | `elysper-session`, `elysper-harness` | Preserve every streamed delta verbatim so replay reproduces the sequence, not just the concatenation |
| 32 | `ContextManager::set_compaction_details` + `CompactionDetails` | `elysper-context`, `elysper-harness` | Name the files a run touched in the next compaction's summariser prompt (read / write only — no shell guessing) |
| 33 | `RetryClass::{EmptyResponse, Quota}` + `validation::is_empty_terminal_completion` + shared `streaming::idle_watchdog::pump_sse` | `elysper-provider` | Retry taxonomy completion (dsh llm-retry); terminal empty completions become typed errors instead of silent success; per-read stream idle watchdog with shared cancel-drain grace across Anthropic + OpenAI |
| 34 | `public_tool_name` + `NamingPolicy` + `McpSupervisor` + `StreamableHttpTransport` + `AppState::mcp_health` | `elysper-mcp`, `elysper-app-server` | Deterministic `mcp__<server>__<raw>` namespacing with sha-suffix on lossy normalization (two servers with the same raw name now coexist); supervisor tick(now) reconnect, generation-swap resync on `tools/list_changed`; streamable-HTTP transport (wiremock tested); per-server health |
| 35 | `token_meter::TokenMeter` + `UsageBuckets` + `ContextPressure` + `CompactionSettings::should_compact_anchored` + `OperationSnapshot::usage_buckets / context_pressure` | `elysper-session`, `elysper-context` | Usage-anchored token meter fold (dsh token-meter): provider-usage anchoring, `max(0, anchor + signed surface_delta)` projection; opt-in anchor-aware compaction gating; serde-default back-compat for old logs |
| 36 | `#[derive(Tool)]` + `#[tool(name, description, mode)]` | `elysper-macros` (new) | Derive a full `impl elysper_tool::Tool` from a struct + an inherent `async fn execute(&self, &Context) -> ToolOutput`; schemars-driven `parameters()`; compile-error diagnostics for misuse (non-struct, generics, missing `execute`, missing description, bad attribute) |
| 37 | `EvalSuite` / `TestCase` / `Metric` (sync) / `AsyncMetric` / `EvalRunner` / `KeywordMetric` / `LlmJudgeMetric` / `SchemaValidationMetric` / `EvalReport` (JSON + CSV) | `elysper-eval` (new) | Quality measurement: case passes iff ALL metric scores ≥ threshold; LLM judge with local `JudgeProvider` seam and clamped score parse; schema-validation reuses `elysper_core::schema::validate_against_schema`; RFC4180 CSV escaping |
| 38 | `ReplayProvider` (`from_script` / `from_events_jsonl` + `assert_consumed`) | `elysper-test-support` | Deterministic `ModelProvider` for golden-transcript regression tests over the full loop (compaction, steering, delegation, repair); dsh `assertConsumed` parity (cancellation aborts undrained cursor) |
| 39 | `RunInbox` trait + `MpscInbox` + `RunInboxHandle` + `SteeringInjected` / `SteeringSource::{Steering, FollowUp}` + length-stop guard | `elysper-harness` | Interactive seams: messages typed mid-run drain between tool rounds; follow-ups revive a stopping run; length-stop tool batches fail with model-facing error `ToolResult`s instead of executing truncated JSON (pi parity) |
| 40 | `prompt::RuntimeContext` (chrono clock injected) + `task::gate` (`GateSpec` + injectable `CommandRunner`) + `task::worktree` (`WorktreeSpec` + `elysper/agent-<ulid>` branch) + `task_tool_definition()` schema fields | `elysper-harness`, `elysper-tool-task` | Cache-stable system prompt — volatile env facts become a user-role snapshot appended only when the rendered body changed (dsh `renderContextSnapshot`); verification gate on `task` runs exactly-once post-child (failed gates refuse the result); git-worktree isolation for delegated children (the `worktree_isolation` feature name `elysper-tool/schema_builder` R29 advertised is now real) |
| 41 | `FileMemory` + `FileMemoryConfig` + `MemoryScope::{Local, Project, User}` + `FileMemoryEntry` + `MemoryKind` | `elysper-context` | File-backed scoped long-term memory (pi-subagents parity): `MEMORY.md` index + frontmatter entry files, deterministic keyword recall (3×/2×/1× name/description/body, newest-id tie-break), ULID identity, read-only mode, agent-name whitelist + symlink/traversal refusal |
| 42 | `ExecutionPolicy` + `SandboxBackend` + `BwrapBackend` + `NoopBackend` + `resolve_confined_command` + `effective_policy` + `SANDBOX_UNAVAILABLE` | `elysper-tool-shell` | OS-level execution policy (dsh sandbox parity), owned by the plugin that runs processes: confinement is **argv construction** (the backend returns the command to spawn instead), `ReadOnly`/`WorkspaceWrite`/`DangerFullAccess`, bubblewrap profile with `--ro-bind /` + `--die-with-parent`, availability probed once and **fail-closed** (a confined policy with no backend refuses instead of running unconfined), `grant > session mode > deployment default` fold |
| 43 | `SubagentPool` + `Slot` + `Admission`/`AdmissionTicket`/`QueuedToken` + `InvocationRecord` + `InvocationStatus` | `elysper-tool-task` | Two-lane subagent admission state machine (pi-subagents `agent-manager` parity): bounded background lane with FIFO queue, unbounded foreground, nested children uncharged (the deadlock guard), 100 tombstones addressable by id, requested-vs-effective model/tier substitution visible after the fact. No threads, no timers — the caller admits, spawns, releases |
| 44 | `WorkflowSpec` / `Step::{Agent, FanOut, Pipeline}` / `WorkflowRuntime` / `WorkflowHost` / `WorkflowCaps` / `WorkflowControl` / `WorkflowJournal` | `elysper-workflow` (new) | Declarative workflow runtime (pi-subagents §workflow, JS VM replaced by serde data): one injected `WorkflowHost` effect seam (`spawn_agent` / `run_gate`) keeps caps + concurrency + abort enforceable and tests host-free; JSONL journal keyed by sha256 of each call's identity, torn-tail tolerant, replays the longest matching prefix and re-runs the tail; typed pause/resume/skip/retry/abort control |
| 45 | `GroupedRegistry` + `GroupError` | `elysper-tool` | Named tool groups with independent activation (traitclaw `GroupedRegistry`): `visible_tool_names()` offers only active groups (plus tools no group claims) while dispatch is untouched, so a tool in a deactivated group **stays executable** — the visibility ≠ executability split skills, workflow steps and subagents rely on. Declarations are validated (unknown tool, duplicate, tool claimed twice) and one tool belongs to one group |
| 46 | `FullOutputStore` + `InMemoryFullOutputStore` + `__get_full_output` tool + `TransformerChain` / `JsonExtractor` / `BudgetAwareTruncator` | `elysper-core`, `elysper-tool`, `elysper-steering` | Output too large to commit is transformed then *stashed* rather than dropped (traitclaw transformers): the store is bounded by entries **and** bytes with true LRU (a `get` refreshes recency, handles are never reused), the transformer chain extracts JSON before budgeting, and the published marker names a handle the model retrieves through `__get_full_output` |
| 47 | Layered `elysper-harness`: private submodules + curated re-exports; `agent::{strategy, run}` | `elysper-harness`, `elysper-tool-task` | R122's harness cleanup. Every submodule of `elysper-harness` is **private** and re-exports only the names it is for, so internal structure is no longer public API and adding an item to an internal module cannot silently widen the contract. The three reasoning strategies moved under `agent::strategy` beside the seam they implement (with `CandidateScorer` its own file and one shared single-shot request builder), and run control — `RunInbox`/`MpscInbox` under `agent::run`; the `AgentHandle`/`DetachedAgent` deferred-run pair that sat beside them has since been **deleted** (no in-tree consumer — `Agent::run`'s event stream is the single run entry point). The out-of-band **team** layer (`BoundAgent` / `VerificationChain` / `RoundRobinGroupChat`) was **deleted**: a `prompt -> text` team chat has no caller in a harness whose delegation is the `task` tool, verify-then-retry is a `CandidateScorer` (or a `elysper-workflow` gate), and a stop condition is the loop's own |
| 48 | `ConditionalRouter` + `RoutePatternError` | `elysper-tool-task` | Ordered regex routing rules, first match wins, with a default fallback (traitclaw `ConditionalRouter`) — implemented against the existing `Router` trait, so rules and `@agent:` mentions are two strategies behind one seam. A bad pattern is a typed error, never a panic |
| 49 | `SqliteMemory` + `SqliteMemoryError` (feature `sqlite`) | `elysper-context` | Single-file durable memory (traitclaw-memory-sqlite): FTS5/BM25 long-term recall (queries are sanitised into quoted terms so `NEAR(` and `*` are text, not syntax), a durable working-memory table, a session registry, read-only opens, and typed errors that keep `SQLite`'s own message. The conversation tier still delegates to the session sink — one source of truth |
| 50 | `ToolEntry::with_exposure` + `ToolDescriptor::exposure` + `project_tool_definitions` + `called_tool_names` + `ToolRegistry::descriptors` | `elysper-tool`, `elysper-harness`, `elysper-app-server` | The tool surface the model is offered, in one place: `Direct` sends the full schema, `Deferred` sends name + description with a permissive schema until the transcript shows the tool was called (then it is promoted), `Hidden`/`is_hidden` sends nothing. Promotion is derived from the transcript, so it is replay-stable and needs no mutable registry state; the same projection serves the agent loop and the operator listing, and the visible-name filter is how `AdaptiveRegistry`'s tier cap and `GroupedRegistry`'s groups plug in |
| 51 | `json_repair::{repair_json, parse_tool_input_reported, parse_tool_input_logged, ToolArgsQuality}` + `error_body::{ProviderErrorBody, parse_provider_error_body}` + `RetryClass::{ContextOverflow, Auth}` + `classify_provider_error_body` | `elysper-provider` | Wire hygiene: malformed tool-call arguments are salvaged by repair instead of silently degrading, and *how* they were parsed is reported (strict / repaired / raw fallback) so a production log shows it. Provider error bodies are parsed into a typed signal, so quota exhaustion, context overflow, capacity overload and auth failures classify distinctly — including when they share an HTTP status (429 quota vs 429 throttling) |
| 52 | `Step::BestOf` + `BestOfStep` + `CallStatus::Superseded` + `WorkflowRun::winner_of` | `elysper-workflow` | Selection as a workflow step (traitclaw MCTS-lite, declarative): one call per candidate under the existing caps and concurrency bound, the first candidate whose gate passes wins and its text becomes the step's output. Losers are `Superseded` — recorded, never able to drag a passing run down — and an all-failed selection fails the step while still handing the caller every failure text |
| 53 | the `elysper` facade: `prelude` + one feature per piece | `elysper` (new) | One dependency instead of nine: `elysper::agent`, `elysper::tool`, … are feature-gated re-exports, and `elysper::prelude::*` is a 27-name curated subset with every collision decided and documented (`Result`, `Tool`, `Context`, and the deliberately excluded `SessionEndReason`). The crate docs are the assembly tutorial; 11 `compile_fail` doc tests pin the feature gating |
| 54 | `ToolSurfacePolicy` + `SurfacePolicyError` + `ToolRegistry::{set_exposure, set_hidden, exposure}` + `[tools]` server config | `elysper-tool`, `elysper-harness`, `elysper-app-server` | The R33 tool surface, reachable from a deployment: groups and an active-group set (a member of an inactive group is simply not advertised), `max_visible` cap, deferred advertisement — validated with typed errors before any write, applied at boot from `[tools]`, and an unknown tool name is a warning rather than a startup failure. A tool nobody advertises still executes |
| 55 | `WorkflowHost::select_candidate` + `SelectionRequest` / `SelectionCandidate` / `CandidateOutcome` / `GateVerdict` + `JournalEntry.winner` | `elysper-workflow` | The host may judge candidates (a rubric, an LLM, the longest answer) instead of taking the first passing gate; the default implementation keeps R33's rule, the choice is **recorded** so a replay never re-asks the host, and naming a candidate that did not succeed is a typed error rather than a plausible-looking wrong winner |
| 56 | `CompactionCheckpoint` + `SurfaceLedger` + `fold_log_surface` / `try_fold_log_surface` | `elysper-session`, `elysper-context`, `elysper-harness`, `elysper-app-server` | Durable compaction checkpoints, end to end: provenance is resolved through the log (the manager's in-memory indices cannot address the surface), the lifecycle pair is handed to the session's writer as soon as it fires and reaches the log at the writer's next flush, and a record whose span cannot be proven is dropped with a warning instead of writing a checkpoint with wrong provenance. The production resume projection folds the log, so a resumed session rebuilds the compacted surface instead of replaying (and re-paying for) the pre-compaction history |

| 57 | `Spawner` + `SharedSpawner` + `ReActAgent::with_spawner` | `elysper-core`, `elysper-harness` | The one operation that used to hard-code tokio — detached work — behind a trait with no runtime in its signature. The loop still detaches one task per run (a caller that stops polling must not cancel the run), but *where* that task goes is the consumer's choice: `cargo run --example runtime_agnostic -p elysper-harness` runs a full turn on `std::thread` + `futures::executor::block_on`. Tool dispatch is spawn-free entirely (`run_stream` drives every call from the caller's poll); the tool plugin crates, the provider HTTP adapters and the JSONL sink remain tokio-bound plugins, and the crate docs say so |
| 58 | Per-crate features: tool plugins (`elysper-tool-read` / `-write` / `-shell` / `-todo` / `-web` / `-task` / `-scheduler` / `-search`, one tool each) + `elysper-telemetry` (`metrics`, `otlp`), `elysper-provider` (`anthropic`, `openai`), facade features (`provider-anthropic`, `provider-openai`, `tool-*`) + `make check-mvp-deps` | workspace-wide | Transport and tools are opt-in and *separable*: the seven-feature MVP subset compiles the `Tool` paradigm and **no agent-facing tool** (no HTTP client at all — `reqwest` / `hyper` / `rustls` / `h2` / `tower` absent), each tool plugin is a crate you either depend on or don't, the OTel exporter arrives only with `otlp`, and the gate asserts both the MVP subset and the seven offline tool crates stay clean |
| 59 | `ReasoningStrategy` + `AgentRuntime` + `EventSink` + `ReActAgent::with_strategy` | `elysper-harness` | The reasoning loop as a swappable part: a strategy receives every assembled piece (provider, tools, steering, context manager, clock, cancel token, executor) and publishes events, so `ReActStrategy` and `ChainOfThoughtStrategy` run against one runtime — `cargo run --example chain_of_thought -p elysper-strategy-cot` exercises the chain-of-thought strategy against the same runtime. One method, streaming-first; errors travel as `SessionEnded { reason }` |
| 60 | `McpControlTool` + `register_mcp_control_tool` (`MCP_TOOL_NAME`) | `elysper-mcp` | The `mcp` tool: the **dynamic** counterpart of the per-tool `McpTool` registration. Three actions (`servers` / `tools` / `call`) over a shared `McpSupervisor`, so a model can read the live catalog — including each remote tool's argument schema — and call a remote tool by its server-side name without the host publishing every remote schema up front. Unknown server vs disconnected server get different, corrective messages; a remote tool's `isError` and its image/audio blocks travel through the same projection as the pre-registered path (`cargo run --example mcp_control_tool -p elysper-mcp`) |
| 61 | `SchedulerTool` + `register_scheduler_tool` (`SCHEDULE_TOOL_NAME`) | `elysper-tool-scheduler` (new) | The `schedule` tool: `list` / `create` / `pause` / `resume` / `remove` over a shared `ScheduleStore`, addressed by job id **or** name. It plans, the host delivers — `create` never fires its own payload, so there is one delivery path (`Scheduler::tick`). `cron` is refused by the schema unless the `cron` feature is enabled — with it, `kind: "cron"` plus a 5-field POSIX `cron_expr` is accepted and the first fire is the expression's own next occurrence; without it the scheduler crate cannot compute a recurrence, so an accepted job is always one the crate can honour (`cargo run --example schedule_jobs -p elysper-tool-scheduler`) |


```bash
# Assemble an agent
cargo run --example assemble_with_builder -p elysper-harness
cargo run --example zero_deps_agent -p elysper-harness
cargo run --example durable_hitl -p elysper-harness
cargo run --example lib_grade -p elysper-harness

# Tools
cargo run --example deferred_tools -p elysper-tool
cargo run --example read_ranges -p elysper-tool-read
cargo run --example write_modes -p elysper-tool-write
cargo run --example todo_list -p elysper-tool-todo
cargo run --example allowlist -p elysper-tool-web
cargo run --example sandbox_policy -p elysper-tool-shell
cargo run --example schedule_jobs -p elysper-tool-scheduler
cargo run --example search_tool_demo -p elysper-tool-search
cargo run --example derive_tool -p elysper-macros

# Teams and workflows
cargo run --example delegation_gate -p elysper-tool-task
cargo run --example worktree_isolation -p elysper-tool-task
cargo run --example workflow_fanout -p elysper-workflow
cargo run --example workflow_best_of -p elysper-workflow

# Runtime choice and reasoning loop
cargo run --example runtime_agnostic -p elysper-harness
cargo run --example chain_of_thought -p elysper-strategy-cot
cargo run --example best_of_n_demo -p elysper-strategy-best-of-n

# Memory and remote tools
cargo run --example sqlite_memory -p elysper-context
cargo run --example register_remote_tools -p elysper-mcp
cargo run --example mcp_control_tool -p elysper-mcp

# Measuring and replaying
cargo run --example eval_suite -p elysper-eval
cargo run --example replay_provider -p elysper-test-support
```

The `assemble_with_builder` example is the recommended starting
point: it wires the tier-aware steering, structured output,
LLM compaction, the session's log writer, and the `@agent` router in a
single chained expression and runs end-to-end with no network.

### Consume elysper from your own crate (no workspace needed)

`docs/examples/external-consumer/` is a standalone crate that is
**not** a workspace member — it depends on the elysper crates by
relative path, exactly as an external library consumer does. It
assembles a complete agent (provider + tier steering + structured
output + LLM compaction + session writer + attachment store +
runtime-neutral cancel) and runs a turn:

```bash
cd docs/examples/external-consumer && cargo run
# → CONSUMER-PROOF: OK
```

Copy that directory anywhere and repoint the `path = …`
dependencies (or switch them to version deps) to start your own
project. No tokio type appears in any elysper API call it
makes — the async runtime is your choice.

### The minimal agent (one dependency, seven features)

`docs/examples/minimal-consumer/` is the smallest assembly that
still runs a full turn — one `elysper` dependency with
`default-features = false` and the feature set from
[`MINIMAL.md`](MINIMAL.md). It implements a scripted provider and one
hand-written tool, asserts the loop really called it, and prints a
proof line:

```bash
cd docs/examples/minimal-consumer && cargo run
# → MVP-OK
```

`make check-mvp-deps` asserts the same feature subset never pulls an
HTTP client (`reqwest` / `hyper` / `rustls` / `h2` / `tower`), the OTLP
exporter, the gRPC transport, `axum`, or a database driver. The subset
keeps the `ModelProvider` trait rather than the bundled HTTP adapters
(`provider-anthropic` / `provider-openai`) and the `Tool` **paradigm**
rather than any tool implementation — the MVP subset compiles zero
tools, so there is nothing that could talk to the network. Add the
plugin features (`tool-read`, `tool-shell`, `tool-web`, …) when you
want them.

## Quick Start

### Prerequisites

- Rust 1.98+
- cargo
- Node.js 20+ (for `elysper-web`)
- Docker (optional, for containerized runs)

### Develop

```bash
make dev   # boots elysper-app-server + elysper-web (:5173)
```

Open `http://localhost:5173`.

### Build

```bash
make build           # both server and web (debug)
make build-release   # release binaries
```

### Test

```bash
make ci              # fast gate: fmt --check, clippy -D warnings, rustdoc -D warnings, the compiled-guide doctests, and every invariant below
make test-crates     # every member's suite, one crate at a time (never --workspace)
make test-sqlite     # the optional SQLite memory tier
make test-guides     # MINIMAL.md's fences, default features and the MVP subset
make examples        # 41 examples + both standalone consumer crates
make check-web       # frontend gates: tsc --noEmit, eslint, prettier --check

# Invariants (all part of `make ci` except the ratios)
make check-mvp-deps            # the MVP subset pulls no HTTP/DB/exporter stack
make check-no-runtime          # core/scheduler/macros/eval/workflow are tokio-free
make check-public-api-runtime  # no library public API names a runtime type
make check-pub-surface         # no glob re-export inside a elysper-* crate
make check-claim-language      # current-state docs make no absolute claim
make check-clock               # wall-clock reads go through Clock
make check-harness-shape       # no elysper-harness fn over 100 lines / 4 nesting
make check-test-layout         # inline test blocks stay in their ratchet band
make doc-check                 # rustdoc warnings are errors
make bench           # hot-path benchmarks (build the table, compare before/after)
make bench-check     # enforce the hot-path ratios (CI gate)
```

`.github/workflows/rust-quality.yml` runs exactly those targets on
every push and pull request — five jobs (`gates`, `tests`, `examples`,
`web`, `bench`), no API keys required, because the suite and every example
are credential-free by construction. The `bench` job runs
`make bench-check`, which enforces the hot-path **ratios** (each
memoised path against the expression it replaced, both measured in one
process), so a performance regression fails the build without any
dependence on the runner's absolute speed. The workflow calls `make` rather
than spelling the commands out, so a red CI step is reproducible
locally with the same line.

### Lint & Format

```bash
make lint            # clippy + tsc
make fmt             # cargo +nightly fmt + prettier
```

## Running

One binary, two modes — `elysper-app-server`:

```bash
# the gateway (the human entry point): spawns a core subprocess and
# serves it — WebSocket /rpc, probes, and the web front-end
cargo run -p elysper-app-server -- serve --port 8080

# the headless core: JSON-RPC 2.0 on stdin/stdout (LF-framed), for a
# TUI / desktop shell / any host that wants a sidecar
cargo run -p elysper-app-server -- core --directory .
```

### Server + Web

```bash
make dev   # boots both with hot reload
```

See [DEPLOYMENT.md](./DEPLOYMENT.md) for production deployment,
Docker Compose, and environment variables.

## Makefile

The root `Makefile` is the single entry point for development,
build, test, format, lint, deploy, and Docker operations.
Run `make help` for the full list of targets.

## Project Structure

```
.
├── Cargo.toml                  # Rust workspace root
├── Makefile                    # unified dev/build/test/deploy entry point
├── Dockerfile.server           # elysper-app-server production image
├── docker-compose.yml          # development compose
├── DEPLOYMENT.md               # deployment guide
├── crates/                     # the 40 workspace members
│   ├── elysper/                # the facade: one dependency, one prelude
│   ├── elysper-core/           # error, cancel, clock, idgen, spawn, text, schema
│   ├── elysper-telemetry/      # console logging always; metrics / otlp as features
│   ├── elysper-provider/       # ModelProvider + wire types; adapters as features
│   ├── elysper-context/        # window strategies, memory tiers, compaction
│   ├── elysper-tool/           # the tool paradigm: trait, registry, path policy
│   ├── elysper-tool-read/      # one tool per plugin crate: read, write, shell (with its
│   │                           #   OS execution policy), todo, web, task (multi-agent),
│   │                           #   scheduler (the `schedule` tool), search (cross-domain
│   │                           #   over a host-built elysper-search registry)
│   ├── elysper-skill/          # procedural skills bound to tools
│   ├── elysper-agents/         # agent definitions as files (agent.md + config.json)
│   ├── elysper-session/        # session lifecycle, sinks, typed events
│   ├── elysper-steering/       # guards / hooks / hints / tracker / transformers
│   ├── elysper-harness/          # the harness: ReAct loop, reasoning strategies,
│   │                           #   run control (inbox), seams
│   ├── elysper-attachment/     # content-addressed multimodal store
│   ├── elysper-mcp/            # MCP client (stdio + HTTP transports)
│   ├── elysper-scheduler/      # runtime-free cron / interval / once dispatcher
│   ├── elysper-workflow/       # declarative multi-agent workflow runtime
│   ├── elysper-eval/           # eval suites, metrics, judge, reports
│   ├── elysper-macros/         # #[derive(Tool)]
│   ├── elysper-test-support/   # deterministic fakes (ReplayProvider, …)
│   └── elysper-app-server/     # the application crate: stdio JSON-RPC core + gateway
└── elysper-web/                # React frontend
    │   ├── api/                # /rpc WebSocket JSON-RPC client modules
    │   ├── components/         # UI components (ui/, layout/)
    │   ├── pages/              # Top-level pages
    │   ├── styles/             # Design tokens
    │   └── hooks/              # React hooks (useServerHealth, ...)
```

## 文档导航

| 我想看… | 入口 |
| :--- | :--- |
| 5 分钟 hello-world | [`docs/QUICKSTART.md`](docs/QUICKSTART.md) |
| 4 步 MVP guide | [`MINIMAL.md`](MINIMAL.md) |
| 7 大可替换组件 | [`SEAMS.md`](SEAMS.md) |
| 可运行示例（41 个 + 2 个独立 consumer） | [`docs/examples/README.md`](docs/examples/README.md) |
| 完整 agent 对话教程（server + web，含 A–G 验收） | [`docs/tutorial/complete-agent.md`](docs/tutorial/complete-agent.md) |
| 顶层文档导航 | [`docs/INDEX.md`](docs/INDEX.md) |
| 历史 / 冻结的 spec 与优化报告 | [`docs/ARCHIVE.md`](docs/ARCHIVE.md) |
| 当前路线 | [`docs/ROADMAP.md`](docs/ROADMAP.md) |
| 当前变更 / 历史 release | [`CHANGELOG.md`](CHANGELOG.md) `[Unreleased]` |
| 商业 / SLA / 供应链 | [`docs/COMMERCIAL.md`](docs/COMMERCIAL.md) |
