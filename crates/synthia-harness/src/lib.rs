//! Runtime-agnostic AI agent harness: the [`ReActAgent`] loop, the
//! [`ReasoningStrategy`] seam that swaps it, the [`DelegatingTool`]
//! seam, and the [`AgentRegistry`] multi-agent catalog. See
//! `README.md` for the tour and `SEAMS.md` for the integration map.
//!
//! # The seam rule
//!
//! A type is `dyn` iff a downstream crate must install it at runtime
//! — by `Arc`, by name through a registry, or from a config string.
//! Everything the harness owns stays concrete or generic. Read
//! accessors stay when in-tree code or an external consumer's
//! plausible integration path uses them; setters' round-trip
//! getters with zero references are deleted.
//!
//! # What a consumer depends on
//!
//! `synthia-harness` is not a dependency a consumer can hold alone:
//! the harness hands back traits it does not own, and it does not
//! re-export them. A consumer declares four crates — `synthia-harness`,
//! `synthia-core`, `synthia-provider` and `synthia-tool` — plus
//! `async-trait`, the proc macro those traits are declared with:
//!
//! | Crate | What the consumer takes from it |
//! |---|---|
//! | `synthia-harness` | the loop, the seams, the event stream |
//! | `synthia-core` | `CancelToken`, `Spawner`, `Clock` |
//! | `synthia-provider` | the `ModelProvider` trait, the message types |
//! | `synthia-tool` | the `Tool` trait, `ToolRegistry` |
//!
//! `Spawner`, `ModelProvider` and `Tool` stay in the crates that
//! define them. Re-exporting them would put names the harness does
//! not own on the surface it promises, and would tie the harness's
//! runtime-free promise to feature sets it cannot vouch for — the
//! harness takes `synthia-provider` with `default-features = false`
//! for exactly that reason (`synthia-tool` has no feature set at
//! all, so the flag on it is a no-op).
//!
//! Those three crates are already inside the harness's own closure (it
//! names them in its signatures), so naming them in a consumer's own
//! manifest says *which* traits the consumer implements — it does not
//! grow the consumer's tree.
//!
//! That last sentence has one premise, and it is about
//! `synthia-provider`: its default feature set (`default =
//! ["anthropic", "openai"]`) turns on the two shipped HTTP adapters,
//! and each adapter carries `reqwest`, `tokio` and `tokio-util`. A
//! consumer that writes `synthia-provider = "0.1"` — the plain
//! crates.io spelling — gets all three into its tree. The harness
//! inherits the provider with the defaults off; a consumer who wants
//! the same tree writes the same line:
//!
//! ```toml
//! synthia-provider = { version = "0.1", default-features = false }
//! ```
//!
//! The reference consumer (`docs/examples/harness-only-consumer`)
//! declares 7 direct dependencies — those four crates plus
//! `async-trait`, `futures` and `serde_json` — and its `cargo tree`
//! carries no `tokio`: its provider line turns the default features
//! off, so no adapter, no `reqwest` and no runtime come along (the
//! fixture drives the loop on `futures::executor::block_on`).
//!
//! # The two public layers
//!
//! The crate root re-exports a curated facade of 59 names. That
//! facade is the promised surface: depend on it, and a release will
//! not move your names.
//!
//! The crate is public further down, under `synthia_harness::agent::…`
//! and `synthia_harness::events::…`. Only a deep path reaches 64
//! names: rustdoc records 117 public paths under those two modules,
//! 54 of them being facade names at their defining path, which leaves
//! 63 names the facade does not carry — plus `Risk`, re-exported from
//! `synthia-tool` into the public `agent::approval` module, which has
//! no path of its own inside the harness. None of these names is
//! promised: they may move or change in any release.

pub mod agent;
pub mod events;
mod input;
mod prompt;

pub use agent::{
    Agent,
    AgentCaps,
    AgentDescriptor,
    AgentEntry,
    AgentRegistry,
    AgentRuntime,
    DEFAULT_MAX_ITERATIONS,
    DelegatingTool,
    DelegationCall,
    EventSink,
    ReActAgent,
    ReActStrategy,
    ReasoningStrategy,
    StrategyRegistry,
    approval::{
        ApprovalMode,
        ApprovalPolicy,
        ApprovalRequest,
        GrantKey,
        GrantScope,
        ResumeCoverageError,
        ResumeDecision,
        ResumeStatus,
        ResumeToken,
        Suspension,
        align_decisions,
        check_coverage,
        latest_request_snapshot_hash,
        may_retry,
        open_requests,
        workspace_hash,
    },
    checkpoint::{
        CheckpointError,
        CheckpointSnapshot,
        Checkpointer,
        InMemoryCheckpointer,
    },
    descriptor::default_descriptor,
    host::{
        Arbitration,
        Diagnostic,
        DiagnosticKind,
        Gate,
        Hook,
        Host,
        Plugin,
        PluginHandle,
        PromptSection,
        Registration,
        RequestTransformer,
        ResultTransformer,
    },
    strategy::{
        prepare_messages_for_shot,
        single_shot_request,
        single_shot_request_shared,
    },
};
pub use events::{
    AgentEvent,
    AgentMeta,
    SessionEndReason,
    SystemEvent,
    WarningKind,
};
pub use input::{AgentInput, SubagentWriters};
pub use prompt::{DEFAULT_SYSTEM_PROMPT, PromptContext};
