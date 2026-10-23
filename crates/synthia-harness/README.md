# synthia-harness

The agent runtime: the core ReAct loop, `Agent` / `SyntheticTool` /
strategy seams, `AgentRegistry`, type definitions. One crate turns a
model + a tool set into a streaming `AgentEvent` flow; everything is
runtime-agnostic so the same loop runs on tokio, async-std, smol, or
test-threaded executors.

公开 API 与 seam 规则见 [`src/lib.rs`](src/lib.rs)；7 大组件的可替换点见
> [`SEAMS.md`](../../SEAMS.md)。

## 5-minute tour

### 1. Assemble an agent

```rust
use std::sync::Arc;
use synthia_harness::ReActAgent;
use synthia_provider::ModelProvider;
use synthia_tool::ToolRegistry;

let provider: Arc<dyn ModelProvider> = /* your model */;
let registry = Arc::new(ToolRegistry::new());

let agent = ReActAgent::new(provider, registry);
```

`ReActAgent` **is** the builder: `new` pre-populates the harness's
defaults (system prompt, `TruncatingContextManager`,
`Steering::noop()`, `InlineSpawner`, `SharedClock::system()`) and
every `with_*` setter mutates one `AgentRuntime` field. Use
`with_descriptor` when you supply your own descriptor,
`PromptContext`, or workspace root.

### 2. Run a session

```rust
use futures::StreamExt;

let cancel = synthia_core::AtomicCancelToken::shared();
let mut stream = agent
    .run(AgentInput::text("hello"), cancel)
    .await;
while let Some(event) = stream.next().await {
    // Process `AgentEvent`s in real time.
}
```

`AgentInput::text` takes text; `AgentInput::new` also takes a
multimodal `Vec<ContentPart>`, and `multi_with_history` resumes a
session. Optional fields (`prompt_context`, `thread_id`) chain:

```rust
let input = AgentInput::text("hi")
    .prompt_context(Arc::new(PromptContext::default()))
    .thread_id("thread-1");
```

### 3. Swap the reasoning strategy

```rust
use synthia_harness::{StrategyRegistry, ReasoningStrategy};
let strategies = StrategyRegistry::new();
strategies.register("my-strategy", Arc::new(MyStrategy) as Arc<dyn ReasoningStrategy>);

let agent = ReActAgent::new(provider, registry)
    .with_strategy(strategies.resolve("my-strategy").unwrap());
```

Or by the built-in's name at boot:

```rust
let strategies = StrategyRegistry::new();
strategies.register("react", Arc::new(ReActStrategy) as Arc<dyn ReasoningStrategy>);
let agent = ReActAgent::new(provider, registry)
    .with_strategy(strategies.resolve("react").unwrap());
```

(`StrategyRegistry::resolve` also falls back to the shipped
`"react"` / `"re-act"` / `"re_act"` built-ins without a `register`
call, so a config string resolves the default loop directly.)

The crate ships the canonical `ReActStrategy`; the alternative
strategies live in the sibling plugin crates
(`synthia-strategy-cot` → `ChainOfThoughtStrategy`,
`synthia-strategy-best-of-n` → `BestOfNStrategy` +
`LlmJudgeScorer`), each gated by its own facade feature
(`strategy-cot` / `strategy-best-of-n`). Pick the strategy via
`StrategyRegistry::resolve` (a config string) or the builder's
`with_strategy`; a name you registered through
`StrategyRegistry::register` resolves the same way.

### 4. Plug into the tool dispatch seams

```rust
// Synthetic-tool plugin: implement `SyntheticTool` (multi-agent
// delegation lives in `synthia-tool-task`).
let agent = ReActAgent::new(provider, registry)
    .with_synthetic_tool(Arc::new(MyTaskDelegator))
    .expect("no synthetic tool already advertises this name");
```

Vetoing and observing a run uses the `HookMap` — see
[Extending the loop](#extending-the-loop).

Install fails with the conflicting name when two synthetic tools
advertise the same tool — same-name dispatch has no defined
precedence.

### 5. Run on any runtime

```rust
// Default: `synthia_core::spawn::InlineSpawner` (in-process).
// A tokio deployment installs the adapter from the sibling
// `synthia-harness-tokio` crate:
use synthia_harness_tokio::TokioSpawner;
let agent = ReActAgent::new(provider, registry)
    .with_spawner(Arc::new(TokioSpawner));
// A smol / async-std deployment installs its own `Spawner` impl.
```

## Design philosophy

Two reference harnesses set the bar this crate aims at, for
different reasons:

- **pi (badlogic/pi-mono)** — a minimal loop with one strict wire
  protocol. Its lesson is *protocol discipline*: the event stream
  is the product, so `AgentEvent` + `EventSink` (one publish
  point) + the JSONL session format are the contract everything
  else serves.
- **DeepSeek Harness (dsh)** — everything-is-a-plugin. Its lesson
  is *seam completeness*: the seven swap points
  (`ModelProvider` / `Tool` / `ContextManager` / `Steering` /
  `SessionSink` / `CancelToken` / `Agent`, plus
  `SyntheticTool` / `Checkpointer` / `Spawner`)
  cover the extension surface without runtime patching.

synthia takes the intersection and adds two constraints neither
has:

1. **Zero runtime** — no tokio in the dependency list; the loop,
   dispatch, and streams run on any executor (§5 above), enforced
   by `make check-no-runtime` / `check-harness-features`.
2. **Compile-time shape budget** — production functions stay
   ≤ 100 lines and ≤ 4 nesting levels (`make check-harness-shape`),
   so "read the whole loop in one sitting" stays true.

## Design decisions

Three costs are deliberate wire decisions, kept so the next round inherits
the measurement instead of re-deriving it:

- **`ContentPart` cloned per streaming delta** (`src/agent/re_act/stream.rs`)
  — `AgentEvent`'s ownership shape is a wire decision, not an optimisation.
- **Terminal `SamplingResult` cloned** (`src/agent/re_act/stream.rs`) — same:
  removing it changes what the event stream hands the consumer.
- **`serde_json::Value` tree built per delta for the typed sink**
  (`src/agent/re_act/stream.rs`) — the payload `SessionEvent` carries is a
  public contract.

## Extending the loop

Interception is one vocabulary: `synthia_steering::{HookMap, HookEvent,
HookDecision}`. A `HookMap` is installed through `Steering::hooks` and
every handler is `async`, so a handler may await its own I/O.

| `HookEvent` | Fires |
|---|---|
| `BeforeRun` | once per run, before the first sample |
| `BeforeProviderCall` | before every provider request |
| `AfterProviderCall` | after every provider response |
| `BeforeTool` | before each tool dispatch — **can veto** |
| `AfterTool` | after each tool dispatch |
| `AfterStep` | at the end of each loop iteration, after the iteration's tool results are committed — **can terminate** (`Block` is meaningless here and is treated as `Allow`) |
| `OnCompaction` | when the context manager pruned history |
| `OnError` | a provider or tool failure ends the run |
| `AfterRun` | once per run, after the terminal `SessionEnded` |

`HookDecision` has three arms: `Allow` (keep going), `Block { reason }`
(the tool call is refused and `reason` becomes the model-visible result),
and `Terminate { reason }` (end the run now with
`SessionEndReason::Error(reason)`).

Handlers registered with `HookMap::on_with_state` additionally receive the
run's `AgentState`, which is how a policy that depends on iteration count or
context utilisation reads it without capturing the loop.

```rust
use std::sync::Arc;
use synthia_steering::{HookDecision, HookEvent, HookMap};

let mut hooks = HookMap::new();
hooks.on_with_state(Arc::new(|event: &HookEvent, _state: &synthia_context::AgentState| {
    Box::pin(async move {
        match event {
            HookEvent::BeforeTool { tool_name, .. } if tool_name == "shell" => {
                HookDecision::Block { reason: "shell is disabled in this profile".into() }
            }
            _ => HookDecision::Allow,
        }
    })
}));

let steering = synthia_steering::Steering { hooks, ..synthia_steering::Steering::noop() };
let agent = ReActAgent::new(provider, registry).with_steering(Arc::new(steering));
```

A zero-hook run allocates nothing at these dispatch sites: each site checks
`hooks.is_empty()` before constructing an event.

## Reference

| Surface | Where |
|---|---|
| `ReActAgent::new` | The single entry point; `with_*` setters are the builder. |
| `with_descriptor` | Supply your own descriptor / `PromptContext` / workspace root. |
| `AgentCaps` | The capability bundle the agent owns, grouped into planes: `model` (provider, descriptor, prompt context), `tools` (registry, surface, restriction, synthetic tools), `control` (steering, iteration cap, context manager), `env` (workspace, clock, spawner, checkpointer, typed sink). Every `with_*` setter writes to exactly one plane. |
| `AgentRuntime` | Per-call bundle handed to the strategy: an owned `Arc<AgentCaps>` plus the run's cancel token and subagent depth. |
| `ReasoningStrategy` | Trait every strategy implements. |
| `SyntheticTool` | Synthetic-tool plugin (multi-agent delegation lives in `synthia-tool-task`). |
| `AgentRegistry` | Multi-agent registry. |
| `Checkpointer` / `InMemoryCheckpointer` | The durability seam — `agent::checkpoint`. |
| `LogWriter` / `Durability` | The run's log writer — stamps each row's ordinal, buffers it, and flushes at the step boundary; `with_log_writer` installs it. |
| `HookMap` / `HookEvent` / `HookDecision` | Interception fan-out — from `synthia-steering`, `Steering::hooks`. |
| `Guard` / `Hint` | Veto + prompt steering — from `synthia-steering`, `with_steering`. |
| `Spawner` | Detached-work seam — `with_spawner`. |
| `TokioSpawner` | The runtime adapter — in `synthia-harness-tokio`, not here. |

## Where new behavior goes

| You want | Install |
|---|---|
| A new model adapter | Implement `ModelProvider` (`synthia-provider`); pass it to `ReActAgent::new`. |
| A different loop / paradigm | Implement `ReasoningStrategy`; `with_strategy(Arc::new(...))`, or resolve by name through `StrategyRegistry`. |
| A cross-cutting synthetic tool | Implement `SyntheticTool`; `with_synthetic_tool` (install fails on a duplicate advertised name). |
| Durable resume | Implement `Checkpointer`; `with_checkpointer` — `InMemoryCheckpointer` ships for tests. |
| A durable run log | Build a `LogWriter` over your `SessionSink`; `with_log_writer` — the loop stamps each row, buffers it, and flushes at the step boundary. |
| Different prompt composition | `PromptContext`; per agent via `with_descriptor`, per dispatch via `AgentInput::prompt_context`. |
| Interception / policy | Steering: `HookEvent` hooks + tool `Guard`s + `ToolRestriction`; `with_steering`, `with_tool_restriction`. |
| Multi-agent | `AgentRegistry` for the catalog; the `task` delegation tool lives in `synthia-tool-task`. |

## Reasoning strategies

The harness ships the canonical `ReActStrategy`. The alternative
paradigms are sibling plugin crates, each behind its own facade
feature:

| Strategy | Crate | Facade feature |
|---|---|---|
| `ReActStrategy` | this crate | always on |
| `ChainOfThoughtStrategy` | `synthia-strategy-cot` | `strategy-cot` |
| `BestOfNStrategy` + `CandidateScorer` + `LlmJudgeScorer` | `synthia-strategy-best-of-n` | `strategy-best-of-n` |
| `PlanExecuteStrategy` | `synthia-strategy-plan-execute` | `strategy-plan-execute` |

Pick a strategy through `StrategyRegistry::register` + `resolve`
(a registered name shadows the built-in `"react"`) or the
builder's `with_strategy(Arc<...>)` setter.

## CI 契约

- `cargo test -p synthia-harness --lib` 绿；
- **形状预算**：生产代码函数 ≤ 100 行、嵌套 ≤ 4 层
  （`make check-harness-shape`）；
- 不引入运行时类型到 lib 公共 API
  （`make check-public-api-runtime`）。

## Examples（5 个，全部离线 + 跨 crate）

起点：[`lib_grade`](examples/lib_grade.rs)（五行跑通 5 大可插拔面）、
[`assemble_with_builder`](examples/assemble_with_builder.rs)（7 大组件链式装配）、
[`runtime_agnostic`](examples/runtime_agnostic.rs)（非 tokio 执行器）、
[`zero_deps_agent`](examples/zero_deps_agent.rs)（零依赖装配）、
[`durable_hitl`](examples/durable_hitl.rs)（Checkpointer 持久化 + 人机回环）。

策略类 example 移到了各自的插件 crate：
`cargo run --example chain_of_thought -p synthia-strategy-cot`（`COT: OK`）、
`cargo run --example best_of_n_demo -p synthia-strategy-best-of-n`（`BON: OK`）。
完整 proof line 索引见
[`docs/examples/README.md`](../../docs/examples/README.md) §1。

跑：`cargo run --example <name> -p synthia-harness`。
