# The seam map

Elysper is assembled from **traits**, not from a framework object. This
page is the index: every point where a consumer can substitute their own
implementation, what the workspace already ships for it, and the line of
code that installs a replacement.

Read it as a menu. Nothing here is mandatory: an agent needs the pieces
in §1 and nothing else, and every piece in §2–§4 is opt-in
(§3 pieces are separate crates, feature-gated through the `elysper`
facade — see [`MINIMAL.md`](MINIMAL.md)).

## 1. The pieces of the loop

| Seam | Trait | Ships in-tree | Install yours |
|---|---|---|---|
| the model | `ModelProvider` | `AnthropicProvider`, `OpenAICompatibleProvider` (feature-gated), `ModelProviderStub` (test-support) | `ReActAgent::new(provider, registry)` (the harness is the builder) |
| the tools | `Tool` | plugin crates, one tool each: `elysper-tool-read` / `-write` / `-shell` / `-todo` / `-web` / `-task` / `-scheduler` / `-search` (`elysper-tool` is the paradigm: registry + trait + `workspace` path confinement; a boundary beyond a path, like the shell's process policy or the search tool's `Registry`, belongs to the plugin that enforces it) | `registry.register_entry(ToolEntry::new(Arc::new(my_tool)))`; derive it with `#[derive(Tool)]` from `elysper-macros` |
| the context window | `ContextManager` | `TruncatingContextManager`, `NoopContextManager` | `.with_context_manager(Arc<dyn ContextManager>)` — a manager only projects or trims the request it is handed |
| compacting history | `Compactor` | `SummaryCompactor` (summarises with the provider you hand it) | `.with_compactor(Arc::new(SummaryCompactor::new(provider, model)))`; without one the loop never compacts, and the summary's `Replace` row carries the seqs the fold actually shadowed |
| the policy layer | `Host` — one registry of `Hook`, `Gate`, `Guard`, `Hint`, `Tracker`, `OutputTransformer`, request / result transformer and prompt sections | `Host::new()` (neutral), `Host::default_policy(root)` (the defensive set) | `agent.with_host(host)?` — a registration is reversible: dropping its handle withdraws it |
| the durable log | `SessionSink` (backend) / `SessionLog` (the object-safe view a run holds) | `JsonlSessionSink`, `InMemorySessionSink`, plus `ReplayProvider` for reading one back | the sink is installed where the run is owned (the server's controller, your own run factory) and written through the session's one writer (`LogWriter`, which implements `SessionLog`); `.with_log_writer(writer)` wires the agent's side |
| cancellation | `CancelToken` | `AtomicCancelToken` (std-only); a tokio `CancellationToken` satisfies the trait with no adapter | `agent.run(input, cancel)` — an `Arc<dyn CancelToken>` |
| **what one agent may call** | `ToolRestriction` (allow / deny) | none — the deployment's `[tools]` surface is a separate, coarser layer | `.with_tool_restriction(ToolRestriction::deny(["shell"]))` (or `agents.<name>.denied_tools`); enforced at both ends — excluded tools are not advertised *and* a call is refused with a model-facing error |
| **the reasoning loop** | `ReasoningStrategy` | `ReActStrategy` (think → act → observe, the harness's own); `ChainOfThoughtStrategy` and `BestOfNStrategy` ship as the separate `elysper-strategy-cot` / `elysper-strategy-best-of-n` crates behind the facade features `strategy-cot` / `strategy-best-of-n` | `.with_strategy(Arc::new(MyStrategy))` — the strategy receives an `AgentRuntime` (provider, tools, steering, context manager, clock, cancel token, executor) and publishes through an `EventSink`. Reachable **from config** too: `StrategyRegistry::resolve` answers the harness's own `"react"` / `"re-act"` / `"re_act"` without a register call, so an operator naming `chain-of-thought` resolves it through the same registry, where the plugin registered it at boot. `cargo run --example strategy_swap -p elysper-harness` drives the swap against one runtime |
| the loop's outer contract | `Agent` | `ReActAgent` (which delegates to its `ReasoningStrategy`) | implement `Agent` when you need a different *session* contract rather than a different loop; register it in `AgentRegistry` for routing |

## 2. Runtime and clock

| Seam | Trait | Ships in-tree | Install yours |
|---|---|---|---|
| detached work | `Spawner` | `InlineSpawner` (std thread + block_on, the `elysper_core::spawn` default); the tokio adapter ships in the sibling crate `elysper-harness-tokio` | `.with_spawner(Arc::new(my_spawner))`. Tool dispatch detaches nothing — `ToolRegistry::run_stream` drives every call from the caller's poll |
| wall clock | `Clock` | `SystemClock`, `FixedClock` (tests) | `SharedClock` handles threaded into the component; public "stamp now" constructors have an `_at(..., now)` sibling |
| delays | `Sleeper` | `ThreadSleeper` (std thread + oneshot, the runtime-free implementation); `FixedSleeper` (tests) | an `Arc<dyn Sleeper>` threaded into whatever arms a timeout — the trait is object-safe, so a consumer substitutes its own delay without the framework naming a runtime (`SummaryCompactor::with_sleeper`) |
| blocking work | `Blocking` | `ThreadBlocking` (std thread + oneshot, the runtime-free implementation) | an `Arc<dyn Blocking>` threaded into whatever offloads a blocking call — the trait is object-safe, so a consumer substitutes its own offload (a tokio blocking pool) without the framework naming a runtime (`JsonlSessionSink::with_blocking`, `JsonlSessionSearch::with_blocking`, `SessionRegistry::with_blocking`, `RemotePlugin::with_blocking`) |
| ids | `IdGen` | `UlidGenerator`, `SequenceGenerator` (tests) | `SharedIdGen` handles, same pattern |

The runtime contract — what is neutral, what is a tokio-bound plugin,
and what needs a reactor today — is stated once in
[`crates/elysper-core/src/spawn.rs`](crates/elysper-core/src/spawn.rs)
and proven by `cargo run --example runtime_agnostic -p elysper-harness`.

## 3. Extensions (each is a crate, each is opt-in)

| Seam | Trait | Ships in-tree | Install yours |
|---|---|---|---|
| long-term / working memory | `Memory` | `InMemoryMemory`, `FileMemory`, `SqliteMemory` (feature `sqlite`) | hand an `Arc<dyn Memory>` to the component that talks to it (the server's state, or your own) |
| embeddings | `Embedder` | `ModelProviderEmbedder` (any `ModelProvider` becomes one) | construct the embedder where you build the index |
| skills | `SkillProvider` | `FileSkillProvider`, `InMemorySkillProvider`, `RemoteSkillProvider` | `SkillRegistry::new()` + `register(provider)`; surface them with `PromptContext::with_skill(name, description)` and expose the `skill` tool via `register_skill_tool(&registry)` |
| delegation / peers | `elysper_harness::DelegatingTool` + `AgentRegistry` | `elysper-tool-task`'s `TaskDelegator` (the `task` tool) | `.with_delegator(Arc::new(TaskDelegator::new(registry)))` — the loop then offers the `task` tool — or `host.delegator(...)` for a reversible registration |
| candidate scoring | `CandidateScorer` | `LongestAnswer` (default), `LlmJudgeScorer` — both in `elysper-strategy-best-of-n` | `elysper_strategy_best_of_n::BestOfNStrategy::new(n, Arc::new(my_scorer))` — the strategy samples, your scorer decides; an LLM judge is just a scorer that calls a provider |
| interactive steering | `RunInbox` | `ChannelInbox` | `let (inbox, sender) = ChannelInbox::channel();` + `AgentInput::with_inbox(inbox)`; the host writes the steer row first, then sends the message |
| remote tools | `McpTransport`, `TransportFactory` | `StdioTransport`, `StreamableHttpTransport`, `InMemoryTransport` | `McpClient::new(transport)`; `McpSupervisor::supervise(name, factory)` for reconnect. Two model-facing paths over that client: `register_mcp_tools(&registry, .., NamingPolicy::Namespaced)` publishes one local tool per remote tool, `register_mcp_control_tool(&registry, supervisor)` publishes the single `mcp` tool that lists and calls remote tools at run time. Both want the same *instance* the agent loop dispatches from (a clone is a deep copy); the control tool takes an `&Arc<ToolRegistry>` because it keeps a weak handle for the privacy lookup, so it refuses a tool the registry marks hidden rather than being a way around the guard |
| command confinement | `SandboxBackend` (+ `ExecutionPolicy`), both in `elysper-tool-shell` | `BwrapBackend`, `NoopBackend` | `ShellTool::with_policy(ExecutionPolicy::…)` + `.with_sandbox(backend)`; the tool holds the policy, so what it advertises and what it enforces cannot diverge, and a confined policy without a backend fails closed |
| content scanning (injection, jailbreak, exfiltration) | `Hook` (registered on the harness `Host`) | — (no crate ships a scanner) | safety scanning is a `Host::hook` registration, not a built-in feature: install your own observer on the before-tool / before-request event and decide there |
| workflow effects | `WorkflowHost` | — (the host is the consumer's) | `WorkflowRuntime::new(host)`; the host owns `spawn_agent` / `run_gate` |
| branch scoring | `Scorer` | `HeuristicScorer` | register yours under the document's wire name: `HeuristicScorer::new(name, Arc<dyn Scorer>)` |
| eval metrics | `Metric`, `AsyncMetric`, `JudgeProvider` | `KeywordMetric`, `SchemaValidationMetric`, `LlmJudgeMetric<P>` | `EvalRunner::new().metric(Box::new(my_metric))`, `LlmJudgeMetric::new(my_judge)` |
| job store | `ScheduleStore` | the PID-locked JSON store (`ScheduleStore::new(root)`) | `Scheduler::new(store)`; the timer is the caller's (`tick(now)`). A tool-side write (or a `ScheduleStore::load`) reaches the next `tick` / `next_deadline` on its own — the pass notices the store's revision move and rebuilds its wheel — and `resync(now)` is the explicit rebuild for between passes. Share one `Arc<ScheduleStore>` with `SchedulerTool::new(store)` to put the same schedule under the model's control |
| attachment bytes | `AttachmentBackend` | `FilesystemBackend`, `InMemoryBackend` | `AttachmentStore::new(backend)` / `.with_backend(backend)` |
| **multimodal payloads** | `Tool` (returns `ToolOutput` of `ContentPart`s) | `McpTool` (MCP `image`/`audio` blocks → `ContentPart::{Image, Audio}`), `attachment_to_content_part` (a stored image → `ContentPart::Image`) | a tool that returns an image calls `ToolOutput::from_parts(vec![text, ContentPart::Image(..)])`; the loop's output transformer rewrites only the *text* projection, so the binary survives to the provider (both the Anthropic and OpenAI adapters emit a real image block). The user → model direction is `AttachmentStore::save_image` → `attachment_to_content_part` → `ContentPart::Image` on the message |
| oversized tool output | `FullOutputStore` | `InMemoryFullOutputStore` | `full_output_tool(store)` / `RetrieveFullOutputTool::new(store)` |
| provider tokenizers | `TokenCounter` | the two adapters' impls | implement it for a provider that counts more precisely |
| security redaction | `SensitiveData` | `Sensitive<T>` wrappers | implement on your config type so `Debug` never prints secrets |

## 4. Application-crate seams

`elysper-server` is the one *application* crate; its internals are not
part of the library contract, but they follow the same shape if you
replace the server with your own binary:

| Seam | Trait | Purpose |
|---|---|---|
| `RunStreamFactory` | turn a run request into an agent event stream | the place a custom deployment swaps the loop |
| `SessionLane` | admit / queue a session's work | bounded concurrency policies |
| the session controller | session state machine + broadcast | the server's equivalent of `ReActAgent::run` (a type, not a trait) |

Two more seams exist but are **crate-internal** by design, so they are
not part of the consumer contract: `CommandRunner` (the `elysper-tool-task`
gate's process runner — `run_subagent_with_runner` is `pub(crate)`;
tests inject a recording fake) and the `Reader` trait inside the JSONL
sink. If you need either, that is a signal to shape the feature as an
issue rather than to patch around it.

## Verifying a substitution

Every seam above has an executable proof in the example ladder, and the
repo's gates keep the map honest:

```bash
make examples        # every example + both standalone consumer crates
make check-mvp-deps  # the 7-feature subset pulls no HTTP/OTel/DB dependency
make check-no-runtime# the runtime-free crates stay tokio-free
make check-clock     # library code reads the wall clock through Clock
```

Trait-by-trait source of truth: each crate's `lib.rs` (module docs) and
[`docs/examples/README.md`](docs/examples/README.md) (the index with
what to look at in each example).
