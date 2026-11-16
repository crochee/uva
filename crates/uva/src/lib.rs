//! # uva
//!
//! The facade for the uva agent framework: **one dependency, one
//! prelude, zero logic**. Every module here is exactly one underlying
//! [`harness`] is `uva-harness`, [`tool`]
//! is `uva-tool`, [`provider`] is `uva-provider`, … — so
//! nothing in this crate can drift from the pieces it names, every
//! long-tail type stays reachable under a path that says where it came
//! from, and a consumer who needs to disambiguate can always do so
//! (`uva::harness::ReActAgent`).
//!
//! ```toml
//! [dependencies]
//! uva = "0.1"
//! ```
//!
//! ## Assemble an agent from zero
//!
//! The framework is lego: seven pieces, wired by the consumer, none of
//! them hidden inside the loop. This is the order a consumer meets
//! them in.
//!
//! **1. The provider** — the model, and the one piece with no
//! default. `uva-provider` ships adapters
//! ([`AnthropicProvider`](crate::provider::AnthropicProvider),
//! [`OpenAICompatibleProvider`](crate::provider::OpenAICompatibleProvider),
//! behind the `provider-anthropic` / `provider-openai` features),
//! but the loop only knows the [`ModelProvider`] trait. Its other job
//! is the wire format: [`Message`] and [`ContentPart`] are the
//! provider-agnostic vocabulary every other piece speaks.
//!
//! **2. The tools** — a [`ToolRegistry`], holding [`Tool`]
//! implementations. The framework ships the **paradigm**, not an
//! agent-facing set: the shipped tools live in one plugin family
//! (`uva-tool-plugins`), one module per tool behind its own
//! feature (`tool-read` / `tool-write` / `tool-shell` / `tool-todo` /
//! `tool-web` / `tool-task` / `tool-scheduler` / `tool-search` /
//! `tool-memo`), so the surface is composed brick by brick —
//! register the plugin tools you want, or implement
//! [`Tool`] for the rest. (The two
//! synthetic contracts the harness injects itself — `__get_full_output`
//! and `structured_output` — come with the registry.) Each
//! invocation receives a [`Context`] (session id, workspace root,
//! truncation config) and returns a [`ToolOutput`].
//!
//! **3. The host** — the one registry of policy: guards that can veto
//! a tool call, hooks that observe the lifecycle, hints injected
//! before a model call, trackers that recommend concurrency,
//! transformers that rewrite what a tool returns. A registration is
//! reversible (dropping its handle withdraws it) and the empty host
//! is neutral.
//!
//! **4. The context manager** — what fits in the next prompt. The
//! agent default is [`TruncatingContextManager`] (drop the oldest
//! pairs until the window fits). Compression is the session's
//! `Compactor`, not a context manager: a manager only projects the
//! request it is handed.
//!
//! **5. The session sink** — `uva-session`'s log, written through
//! its one writer: the structural boundary events (iteration, step,
//! request header, usage) go straight into the writer, which stamps
//! their ordinal and writes them with the rest of the batch. Leave the
//! writer out and the agent simply keeps no structural log.
//!
//! **6. The cancel token** — [`CancelToken`] is the whole cancellation
//! vocabulary: `is_cancelled`, `cancel`, `cancelled().await`.
//! [`AtomicCancelToken`] is a std-only implementation, so a library
//! consumer never inherits a runtime type; tokio users can pass their
//! `CancellationToken` instead.
//!
//! **7. The agent** — [`ReActAgent`], the canonical
//! [`crate::harness::Agent`] loop.
//! Drive it with an [`AgentInput`] and an
//! `Arc<dyn CancelToken>`; it streams [`AgentEvent`]s (model deltas,
//! the model's final result, lifecycle events, sub-agent traces)
//! until the session ends.
//!
//! The loop itself is a piece too: `ReActAgent::with_strategy(...)`
//! picks the reasoning paradigm, `ReActStrategy` (the default) or a
//! plugin strategy you supply as an `Arc<dyn ReasoningStrategy>`.
//! `uva::harness::StrategyRegistry` resolves names: the
//! harness's own `"react"` / `"re-act"` / `"re_act"` built-ins
//! resolve without a register call, every other paradigm is
//! registered by name first — which is how `uva-app-server`
//! exposes the choice as `agents.<name>.strategy`.
//!
//! ### The whole assembly, using only the prelude
//!
//! ```rust
//! # #![allow(unused_imports)]
//! # #[cfg(not(all(feature = "core", feature = "provider", feature = "context", feature = "tool", feature = "session", feature = "steering", feature = "agent")))]
//! # fn main() {}
//! # #[cfg(all(feature = "core", feature = "provider", feature = "context", feature = "tool", feature = "session", feature = "steering", feature = "agent"))]
//! # fn main() {
//! use std::sync::Arc;
//!
//! use uva::prelude::*;
//!
//! /// The one piece with no default: implement `ModelProvider`.
//! struct ScriptedProvider;
//!
//! #[async_trait]
//! impl ModelProvider for ScriptedProvider {
//!     async fn initialize(&mut self, _config: ProviderConfig) -> Result<(), Error> {
//!         Ok(())
//!     }
//!
//!     fn name(&self) -> &str {
//!         "scripted"
//!     }
//!
//!     fn model_config(&self) -> ModelConfig {
//!         ModelConfig {
//!             name: "scripted-1".to_string(),
//!             provider: "scripted".to_string(),
//!             context_window: 8_192,
//!             max_output_tokens: 1_024,
//!             supports_tools: true,
//!             supports_streaming: true,
//!             supports_reasoning: false,
//!         }
//!     }
//!
//!     async fn complete(
//!         &self,
//!         _request: CompletionRequest,
//!     ) -> Result<CompletionResponse, Error> {
//!         Ok(CompletionResponse {
//!             content: Content::text("assembled from the prelude"),
//!             ..CompletionResponse::default()
//!         })
//!     }
//!
//!     // `embed` has a default impl: a provider that doesn't drive
//!     // any embedder does not need to write it. Only override
//!     // when you actually have an embedding endpoint.
//! }
//!
//! let agent = ReActAgent::new(Arc::new(ScriptedProvider), Arc::new(ToolRegistry::new()))
//!     .with_workspace(".")
//!     .with_host(uva::harness::Host::new())
//!     .expect("the empty host binds to the agent's registry")
//!     .with_context_manager(Arc::new(TruncatingContextManager))
//!     .with_name("tutorial-agent")
//!     .with_instructions("Answer in one short sentence.");
//!
//! assert_eq!(agent.descriptor().name, "tutorial-agent");
//! # }
//! ```
//!
//! The same agent, driven end to end (a turn, its events, the final
//! answer), is the runnable example
//! [`examples/assemble_from_zero.rs`](https://github.com/crochee/uva/blob/master/crates/uva/examples/assemble_from_zero.rs);
//! run it with `cargo run -p uva --example assemble_from_zero`.
//! The full example ladder — one program per seam, each ending in a
//! proof line — is indexed in
//! [`docs/examples/README.md`](https://github.com/crochee/uva/blob/master/docs/examples/README.md).
//!
//! ## Features
//!
//! The default set is exactly what assembling a basic agent needs.
//! Everything else is opt-in, so the facade never forces a heavy
//! dependency (SQLite, axum, OTLP) on a consumer who does not want it.
//!
//! Three kinds of entry keep the *transport* and the *parts*
//! optional: `provider` is the [`ModelProvider`](crate::provider::ModelProvider)
//! trait and the wire types, while the shipped adapters are
//! `provider-anthropic` / `provider-openai`; `tool` is the
//! registry + trait paradigm with **no** implementations, while
//! each builtin is a `tool-*` feature (`tool-task` adds the
//! multi-agent `task` tool as a plugin, `tool-scheduler` the
//! `schedule` tool, `tool-search` the cross-domain `search` tool). The
//! features whose host must first build a runtime object — `mcp` (a
//! supervisor), `tool-scheduler` (a schedule store) and `tool-search`
//! (the `Registry` it searches) — are opt-in rather than default. A
//! consumer with their own provider and no outbound fetch compiles no
//! HTTP client at all.
//!
//! | Feature | Module | Underlying crate | Default |
//! |---|---|---|---|
//! | `core` | [`core`] | `uva-core` | yes |
//! | `provider` | [`provider`] | `uva-provider` | yes |
//! | `provider-anthropic` | [`provider`] | `uva-provider/anthropic` | yes |
//! | `provider-openai` | [`provider`] | `uva-provider/openai` | yes |
//! | `context` | [`context`] | `uva-context` | yes |
//! | `tool` | [`tool`] | `uva-tool` | yes |
//! | `tool-read` | [`tool_read`] | `uva-tool-plugins` | yes |
//! | `tool-write` | [`tool_write`] | `uva-tool-plugins` | yes |
//! | `tool-shell` | [`tool_shell`] | `uva-tool-plugins` | yes |
//! | `tool-todo` | [`tool_todo`] | `uva-tool-plugins` | yes |
//! | `tool-web` | [`tool_web`] | `uva-tool-plugins` | yes |
//! | `tool-task` | [`tool_task`] | `uva-tool-task` | yes |
//! | `session` | [`session`] | `uva-session` | yes |
//! | `steering` | [`steering`] | `uva-steering` | yes |
//! | `approval` | [`approval`] | `uva-approval` | yes |
//! | `agent` | [`agent`] | `uva-agent` | yes |
//! | `harness` | [`harness`] | `uva-harness` | yes |
//! | `harness-tokio` | `uva::harness_tokio` | `uva-core-tokio` | no |
//! | `agents` | `uva::agents` | `uva-core` (the `agent_def` module) | no |
//! | `skill` | `uva::skill` | `uva-skill` | no |
//! | `attachment` | `uva::attachment` | `uva-attachment` | no |
//! | `mcp` | `uva::mcp` | `uva-mcp` | no |
//! | `tool-scheduler` | `uva::tool_scheduler` | `uva-tool-plugins` | no |
//! | `tool-search` | `uva::tool_search` | `uva-tool-plugins` | no |
//! | `scheduler` | `uva::scheduler` | `uva-scheduler` | no |
//! | `search` | `uva::search` | `uva-search` | no |
//! | `cron` | `uva::scheduler` / `uva::tool_scheduler` | `uva-scheduler/cron` + `uva-tool-plugins/cron` | no |
//! | `eval` | `uva::eval` | `uva-eval` | no |
//! | `telemetry` | `uva::telemetry` | `uva-telemetry` | no |
//! | `semantic-cache` | [`semantic_cache`] | `uva-policy-plugins` | no |
//! | `tool-memo` | [`tool_memo`] | `uva-tool-plugins` | no |
//! | `resilience` | [`resilience`] | `uva-policy-plugins` | no |
//! | `strategy-plan-execute` | [`strategy_plan_execute`] | `uva-policy-plugins` | no |
//! | `reflection` | `uva::reflection` | `uva-hook-plugins` | no |
//! | `redact` | `uva::redact` | `uva-hook-plugins` | no |
//! | `traffic-light` | `uva::traffic_light` | `uva-hook-plugins` | no |
//! | `budget` | `uva::budget` | `uva-policy-plugins` | no |
//! | `hpc-pool` | `uva::hpc_pool` | `uva-provider/hpc-pool` | no |
//! | `hpc-simd` | `uva::hpc_simd` | `uva-search/simd` | no |
//! | `sqlite` | `uva::context::memory` | `uva-context/sqlite` | no |
//! | `test-support` | `uva::test_support` | `uva-test-support` | no |
//! | `kernel` | `uva::kernel` | `uva-kernel` | no |
//! | `hnsw` | `uva::hnsw` | `uva-search/hnsw` | no |
//! | `runtime` | `uva::runtime` | `uva-runtime` | no |
//!
//! Enabling a feature enables the modules its types refer to, so a
//! name from a sibling module is never left unnameable. The `prelude`
//! is always present; its items appear as their features are enabled
//! (see [`prelude`] for the collision decisions behind each name).
//!
//! The `kernel` feature re-exports `uva-kernel`'s whole surface:
//! `Plugin`, `PluginManifest`, `mount_all`, `MountHandle`, `Context`,
//! `EffectHandle`, `PluginState`, and the `ui.` / core namespace
//! boundary. The built-in plugin crates (`uva-tool`,
//! `uva-skill`, `uva-mcp`, …) publish kernel plugins from their
//! own modules, so a consumer assembles a runtime through the same
//! `mount_all` the app server uses.
//!
//! ## Layout
//!
//! One module per underlying crate, named after it — `uva::harness`
//! for `uva-harness`, `uva::tool` for `uva-tool`, and so on.
//! The table above is the authoritative list, including which entries
//! are in the default set.
//!
//! ## Runtime neutrality
//!
//! No crate below the facade names a runtime: cancellation is the
//! [`CancelToken`] trait, streams are `futures::Stream`, and the
//! typed-event channel is `futures::channel::mpsc`. An application
//! picks a runtime; the examples use tokio only because a `main`
//! needs an executor.
//!
//! The one runtime the facade *can* name is opt-in: the
//! `harness-tokio` feature exposes `uva::harness_tokio` (the
//! tokio `Spawner` / `Sleeper` / `Blocking` adapters), so a tokio
//! host installs its runtime's implementations of the runtime-
//! neutral seams through the single dependency — e.g.
//! `ResilientProvider::with_sleeper(Arc::new(TokioSleeper))`, so a
//! retry backoff costs a timer entry instead of an OS thread.
//!
//! [`AnthropicProvider`]: crate::provider::AnthropicProvider
//! [`OpenAICompatibleProvider`]: crate::provider::OpenAICompatibleProvider
//! [`ModelProvider`]: crate::prelude::ModelProvider
//! [`Message`]: crate::prelude::Message
//! [`ContentPart`]: crate::prelude::ContentPart
//! [`ToolRegistry`]: crate::prelude::ToolRegistry
//! [`Tool`]: crate::prelude::Tool
//! [`Context`]: crate::prelude::Context
//! [`ToolOutput`]: crate::prelude::ToolOutput
//! [`Host`]: crate::prelude::Host
//! [`TruncatingContextManager`]: crate::prelude::TruncatingContextManager
//! [`ContextManager`]: crate::prelude::ContextManager
//! [`CancelToken`]: crate::prelude::CancelToken
//! [`AtomicCancelToken`]: crate::prelude::AtomicCancelToken
//! [`ReActAgent`]: crate::prelude::ReActAgent
//! [`AgentInput`]: crate::prelude::AgentInput
//! [`AgentEvent`]: crate::prelude::AgentEvent

// ---------------------------------------------------------------------
// Opt-in modules are proven absent from a default build.
//
// Each attribute below adds a `compile_fail` doc test that names an
// item from the module its feature gates. Under default features the
// module does not exist, the snippet does not compile, and the doc
// test passes; under `--all-features` the attribute is not applied and
// no such test is emitted. `cargo test -p uva --doc` therefore
// fails the moment a feature stops gating its module.
// ---------------------------------------------------------------------
#![cfg_attr(
    not(feature = "skill"),
    doc = r#"```compile_fail
// The `skill` feature is off: this module must not exist.
use uva::skill::SkillRegistry;
```"#
)]
#![cfg_attr(
    not(feature = "attachment"),
    doc = r#"```compile_fail
// The `attachment` feature is off: this module must not exist.
use uva::attachment::AttachmentStore;
```"#
)]
#![cfg_attr(
    not(feature = "mcp"),
    doc = r#"```compile_fail
// The `mcp` feature is off: this module must not exist.
use uva::mcp::McpClient;
```"#
)]
#![cfg_attr(
    not(feature = "scheduler"),
    doc = r#"```compile_fail
// The `scheduler` feature is off: this module must not exist.
use uva::scheduler::Scheduler;
```"#
)]
#![cfg_attr(
    not(feature = "search"),
    doc = r#"```compile_fail
// The `search` feature is off: this module must not exist.
use uva::search::SearchEngine;
```"#
)]
#![cfg_attr(
    not(feature = "eval"),
    doc = r#"```compile_fail
// The `eval` feature is off: this module must not exist.
use uva::eval::EvalSuite;
```"#
)]
#![cfg_attr(
    not(feature = "telemetry"),
    doc = r#"```compile_fail
// The `telemetry` feature is off: this module must not exist.
use uva::telemetry::init_tracing;
```"#
)]
#![cfg_attr(
    not(feature = "test-support"),
    doc = r#"```compile_fail
// The `test-support` feature is off: this module must not exist.
use uva::test_support::ReplayProvider;
```"#
)]
#![cfg_attr(
    not(feature = "sqlite"),
    doc = r#"```compile_fail
// The `sqlite` feature is off: the SQLite memory tier must not exist.
use uva::context::memory::SqliteMemory;
```"#
)]
#![cfg_attr(
    not(feature = "provider-anthropic"),
    doc = r#"```compile_fail
// The `provider-anthropic` feature is off: the adapter (and with it
// the HTTP client) must not exist. This proof is live in any build
// that drops the default set, e.g. the seven-feature MVP subset.
use uva::provider::AnthropicProvider;
```"#
)]
#![cfg_attr(
    not(feature = "provider-openai"),
    doc = r#"```compile_fail
// The `provider-openai` feature is off: the adapter must not exist.
use uva::provider::OpenAICompatibleProvider;
```"#
)]
#![cfg_attr(
    not(feature = "tool-web"),
    doc = r#"```compile_fail
// The `tool-web` feature is off: `web_fetch` (the one tool that
// talks to the network) must not exist.
use uva::tool_web::WebFetchTool;
```"#
)]
#![cfg_attr(
    not(feature = "tool-read"),
    doc = r#"```compile_fail
// The `tool-read` feature is off: that plugin crate (and its module)
// must not exist. Live in any build that drops the default set — the
// seven-feature MVP subset, where the framework compiles no
// agent-facing tool.
use uva::tool_read::ReadTool;
```"#
)]
#![cfg_attr(
    not(feature = "tool-write"),
    doc = r#"```compile_fail
// The `tool-write` feature is off: that plugin crate must not exist.
use uva::tool_write::WriteTool;
```"#
)]
#![cfg_attr(
    not(feature = "tool-shell"),
    doc = r#"```compile_fail
// The `tool-shell` feature is off: that plugin crate must not exist.
use uva::tool_shell::ShellTool;
```"#
)]
#![cfg_attr(
    not(feature = "tool-todo"),
    doc = r#"```compile_fail
// The `tool-todo` feature is off: that plugin crate must not exist.
use uva::tool_todo::TodoWriteTool;
```"#
)]
#![cfg_attr(
    not(feature = "tool-task"),
    doc = r#"```compile_fail
// The `tool-task` feature is off: the `task` tool plugin must
// not exist.
use uva::tool_task::TaskTool;
```"#
)]
#![cfg_attr(
    not(feature = "tool-scheduler"),
    doc = r#"```compile_fail
// The `tool-scheduler` feature is off: the `schedule` tool plugin
// must not exist. It is opt-in (like `mcp`) because the host
// constructs the `ScheduleStore` it adapts.
use uva::tool_scheduler::SchedulerTool;
```"#
)]
#![cfg_attr(
    not(feature = "tool-search"),
    doc = r#"```compile_fail
// The `tool-search` feature is off: the `search` tool plugin must not
// exist. It is opt-in (like `mcp`) because the host constructs the
// `Registry` it searches.
use uva::tool_search::SearchTool;
```"#
)]
#![cfg_attr(
    not(feature = "hpc-pool"),
    doc = r#"```compile_fail
// The `hpc-pool` feature is off: the connection-pool module must
// not exist.
use uva::hpc_pool::PooledHttpClient;
```"#
)]
#![cfg_attr(
    not(feature = "hpc-simd"),
    doc = r#"```compile_fail
// The `hpc-simd` feature is off: the arch-specific SIMD dot
// must not exist.
use uva::hpc_simd::DotRoute;
```"#
)]
#![cfg_attr(
    not(feature = "harness-tokio"),
    doc = r#"```compile_fail
// The `harness-tokio` feature is off: the tokio adapters must not
// exist — the facade names no runtime unless the consumer asks for
// one. Live in the default build and the seven-feature MVP subset.
use uva::harness_tokio::TokioSleeper;
```"#
)]

// The facade's async traits (`ModelProvider`, `Tool`, `Agent`,
// `ContextManager`) are implemented with this attribute macro, so the
// facade re-exports it: a consumer that has to add a second dependency
// to implement the traits it just imported has not been freed from
// dependency archaeology.
//
// `async-trait` is the facade's only non-uva dependency. It is a
// proc macro already in every library crate's graph.
pub use async_trait::async_trait;
/// The exact `serde_json` the framework's wire types use.
///
/// A plugin author implements [`tool::Tool`], whose signature speaks
/// `serde_json::Value` — naming that type from a *different*
/// `serde_json` version would not satisfy the trait. The facade hands
/// over the version it was built against, so a plugin author needs one
/// dependency entry instead of two (and cannot pin the wrong one).
pub use serde_json;

/// The tutorial, **compiled**.
///
/// The `rust` fences in [`MINIMAL.md`](../../../MINIMAL.md) are doctests:
/// a recipe that stops compiling fails `cargo test --doc`, and CI runs
/// that command twice — once with the default features and once with the
/// seven-feature MVP subset the guide promises is sufficient.
#[cfg(doctest)]
#[doc = include_str!("../../../MINIMAL.md")]
pub struct MinimalGuide;

#[cfg(feature = "agent")]
pub mod agent;
#[cfg(feature = "agents")]
pub mod agents;
#[cfg(feature = "approval")]
pub mod approval;
#[cfg(feature = "attachment")]
pub mod attachment;
#[cfg(feature = "budget")]
pub mod budget;
#[cfg(feature = "context")]
pub mod context;
#[cfg(feature = "core")]
pub mod core;
#[cfg(feature = "eval")]
pub mod eval;
#[cfg(feature = "events")]
pub mod events;
#[cfg(feature = "harness")]
pub mod harness;
#[cfg(feature = "harness-tokio")]
pub mod harness_tokio;
#[cfg(feature = "hnsw")]
pub mod hnsw;
#[cfg(feature = "hook")]
pub mod hook;
#[cfg(feature = "hpc-pool")]
pub mod hpc_pool;
#[cfg(feature = "hpc-simd")]
pub mod hpc_simd;
#[cfg(feature = "kernel")]
pub mod kernel;
#[cfg(feature = "mcp")]
pub mod mcp;
pub mod prelude;
#[cfg(feature = "provider")]
pub mod provider;
#[cfg(feature = "redact")]
pub mod redact;
#[cfg(feature = "reflection")]
pub mod reflection;
#[cfg(feature = "resilience")]
pub mod resilience;
#[cfg(feature = "runtime")]
pub mod runtime;
#[cfg(feature = "scheduler")]
pub mod scheduler;
#[cfg(feature = "search")]
pub mod search;
#[cfg(feature = "semantic-cache")]
pub mod semantic_cache;
#[cfg(feature = "session")]
pub mod session;
#[cfg(feature = "skill")]
pub mod skill;
#[cfg(feature = "steering")]
pub mod steering;
#[cfg(feature = "strategy-best-of-n")]
pub mod strategy_best_of_n;
#[cfg(feature = "strategy-cot")]
pub mod strategy_cot;
#[cfg(feature = "strategy-plan-execute")]
pub mod strategy_plan_execute;
#[cfg(feature = "telemetry")]
pub mod telemetry;
#[cfg(feature = "test-support")]
pub mod test_support;
#[cfg(feature = "tool")]
pub mod tool;
#[cfg(feature = "tool-memo")]
pub mod tool_memo;
#[cfg(feature = "tool-read")]
pub mod tool_read;
#[cfg(feature = "tool-scheduler")]
pub mod tool_scheduler;
#[cfg(feature = "tool-search")]
pub mod tool_search;
#[cfg(feature = "tool-shell")]
pub mod tool_shell;
#[cfg(feature = "tool-task")]
pub mod tool_task;
#[cfg(feature = "tool-todo")]
pub mod tool_todo;
#[cfg(feature = "tool-web")]
pub mod tool_web;
#[cfg(feature = "tool-write")]
pub mod tool_write;
#[cfg(feature = "traffic-light")]
pub mod traffic_light;
