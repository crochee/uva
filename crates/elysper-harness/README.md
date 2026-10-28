# elysper-harness

The agent runtime: the ReAct loop, the `Agent` / `ReasoningStrategy` /
`DelegatingTool` seams, the one `Host` registry every interception
point reads, and `AgentRegistry`. One crate turns a model + a tool set
into a streaming `AgentEvent` flow; nothing here names a runtime.

公开 API 与 seam 规则见 [`src/lib.rs`](src/lib.rs)；可替换点见
[`SEAMS.md`](../../SEAMS.md)。

## What it is

- **One transcript.** A run derives its history from the session log
  once at setup, then reads and writes an in-memory transcript; every
  committed message carries the ordinal of the row it was written
  under, so a compaction can cite provenance without a re-read.
- **One interception registry.** `Host` holds hooks, gates, guards,
  hints, trackers, transformers, prompt sections, tools and
  delegators. Each registration is reversible: dropping its handle
  withdraws it. `Host::new()` is neutral; `Host::default_policy(root)`
  installs the defensive set.
- **One compaction engine.** The `Compactor` seam in `elysper-session`
  is the only writer of a `compaction` row; a `ContextManager` only
  projects or trims the request it is handed.
- **Zero features, zero runtime.** The crate declares no `[features]`
  table and pulls no tokio; `make check-harness-features` and
  `make check-no-runtime` enforce both. Composition happens through
  crate boundaries and `Host` registrations.

## 5-minute tour

### 1. Assemble an agent

```rust,no_run
use std::sync::Arc;

use elysper_harness::{AgentInput, Host, ReActAgent};
use elysper_tool::ToolRegistry;

# async fn assemble(
#     provider: Arc<dyn elysper_provider::ModelProvider>,
# ) -> Result<(), Box<dyn std::error::Error>> {
let registry = Arc::new(ToolRegistry::new());
let agent = ReActAgent::new(provider, registry)
    .with_workspace(".")
    .with_host(Host::default_policy("."))?;

let mut stream = agent
    .run(
        AgentInput::text("hello"),
        Arc::new(elysper_core::AtomicCancelToken::shared()),
    )
    .await;
while let Some(event) = futures::StreamExt::next(&mut stream).await {
    let _ = event; // AgentEvent::Model / ModelDone / System
}
# Ok(())
# }
```

`with_host` returns `Result<Self, Vec<host::Diagnostic>>`: a host that
is already bound to a different registry, or a tool whose name a core
tool owns, is reported instead of silently dropped.

### 2. Register a policy

```rust,no_run
use std::sync::Arc;

use elysper_harness::{Host, agent::host::PromptSection};

let mut host = Host::new();
// The handle is the registration: dropping it withdraws the policy.
let _section = host.prompt_section(PromptSection {
    title: "Audit".to_string(),
    body: "Always cite the file you read.".to_string(),
});
```

### 3. Steer a live run

```rust,no_run
# use std::sync::Arc;
use elysper_harness::{AgentInput, ChannelInbox};

let (inbox, sender) = ChannelInbox::channel();
// The host writes the `user_message` row and flushes it first; the
// loop drains the channel at the next iteration and never re-reads
// the log to find it.
let _input = AgentInput::text("hello").with_inbox(inbox);
let _ = sender.unbounded_send(elysper_provider::Message::user("steer"));
```

### 4. Swap the reasoning strategy

```rust,ignore
let agent = agent.with_strategy(Arc::new(MyStrategy));
```

A strategy receives an `AgentRuntime` (caps, cancel token, sub-agent
depth) and publishes through an `EventSink`; it is the seam for a
different loop, not a different session contract.

## What a consumer depends on

The harness hands back traits it does not own and does not re-export
them. A consumer declares four crates — `elysper-harness`,
`elysper-core`, `elysper-provider` and `elysper-tool` — plus
`async-trait`, the proc macro those traits are declared with. That is
7 direct dependencies in total. `ModelProvider` stays in `elysper-provider` and
`Tool` in `elysper-tool`, so naming them says *which* traits you
implement; it does not grow the tree.

That holds because of one premise: `elysper-provider`'s default feature
set turns on the two HTTP adapters, and each carries `reqwest` and
tokio. A consumer writes the same line the harness writes:

```toml
elysper-provider = { version = "0.1", default-features = false }
```

## The two public layers

The crate root re-exports a curated facade of 60 names. That facade is
the promised surface: depend on it, and a release will not move your
names. The crate is public further down, under
`elysper_harness::agent::…` and `elysper_harness::events::…`. Only a
deep path reaches 57 names: rustdoc records 109 public paths under
those two modules, 53 of them being facade names at their defining
path, which leaves 56 names the facade does not carry — plus `Risk`,
re-exported from `elysper-tool`, which has no path of its own inside
the harness. None of those 57 is promised.

## Seams

| Seam | Trait / type | Installed with |
|---|---|---|
| the model | `ModelProvider` (from `elysper-provider`) | `ReActAgent::new(provider, registry)` |
| the tools | `Tool` + `ToolRegistry` (from `elysper-tool`) | `registry.register_entry(..)`, or `Host::tool` |
| the context window | `ContextManager` (from `elysper-context`) | `with_context_manager` |
| compression | `Compactor` (from `elysper-session`) | `with_compactor` |
| policy | `Host` (this crate) | `with_host` |
| live input | `RunInbox` / `ChannelInbox` | `AgentInput::with_inbox` |
| the durable log | `SessionLog` (from `elysper-session`) | `with_log_writer` |
| cancellation | `CancelToken` (from `elysper-core`) | `agent.run(input, cancel)` |
| detached work | `Spawner` (from `elysper-core`) | `with_spawner` |
| delegation | `DelegatingTool` | `with_delegator`, or `Host::delegator` |
| the loop's outer contract | `Agent` | implement it and register it in `AgentRegistry` |

## Verify it

```bash
cargo test -p elysper-harness --lib
cargo run --example assemble_with_builder -p elysper-harness
cargo run --example runtime_agnostic -p elysper-harness
make check-no-runtime check-harness-features check-harness-shape
```

## Further reading

- [`docs/api-surface.md`](../../docs/api-surface.md) — the public
  surface as a tour: dependency counts, the two public layers, the
  policy table and the example index.
- [`docs/api-frozen.md`](../../docs/api-frozen.md) — the promise: the
  names the seven library crates keep importable.
- [`docs/log-format.md`](../../docs/log-format.md) — the session log's
  on-disk shape and its version gate.
- [`docs/contributing/first-steps.md`](../../docs/contributing/first-steps.md)
  — from a clone to one full turn.
