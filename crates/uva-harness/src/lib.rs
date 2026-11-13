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
//! `uva-harness` is not a dependency a consumer can hold alone:
//! the harness hands back traits it does not own, and it does not
//! re-export them. A consumer declares four crates — `uva-harness`,
//! `uva-core`, `uva-provider` and `uva-tool` — plus
//! `async-trait`, the proc macro those traits are declared with:
//!
//! | Crate | What the consumer takes from it |
//! |---|---|
//! | `uva-harness` | the loop, the seams, the event stream |
//! | `uva-core` | `CancelToken`, `Spawner`, `Clock` |
//! | `uva-provider` | the `ModelProvider` trait, the message types |
//! | `uva-tool` | the `Tool` trait, `ToolRegistry` |
//!
//! `Spawner`, `ModelProvider` and `Tool` stay in the crates that
//! define them. Re-exporting them would put names the harness does
//! not own on the surface it promises, and would tie the harness's
//! runtime-free promise to feature sets it cannot vouch for — the
//! harness takes `uva-provider` with `default-features = false`
//! for exactly that reason (`uva-tool` has no feature set at
//! all, so the flag on it is a no-op).
//!
//! Those three crates are already inside the harness's own closure (it
//! names them in its signatures), so naming them in a consumer's own
//! manifest says *which* traits the consumer implements — it does not
//! grow the consumer's tree.
//!
//! That last sentence has one premise, and it is about
//! `uva-provider`: its default feature set (`default =
//! ["anthropic", "openai"]`) turns on the two shipped HTTP adapters,
//! and each adapter carries `reqwest`, `tokio` and `tokio-util`. A
//! consumer that writes `uva-provider = "0.1"` — the plain
//! crates.io spelling — gets all three into its tree. The harness
//! inherits the provider with the defaults off; a consumer who wants
//! the same tree writes the same line:
//!
//! ```toml
//! uva-provider = { version = "0.1", default-features = false }
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
//! The crate root re-exports a curated facade of 64 names. That
//! facade is the promised surface: depend on it, and a release will
//! not move your names.
//!
//! The crate is public further down too — under `uva_harness::agent::…`
//! and `uva_harness::events::…`, plus the `plugin` module (the
//! `agentLoop` kernel service) and the `seams` module (the loop's
//! interception keys). The `agent` and `events` deep paths hold 57
//! names the facade does not promise on its own: 53 of them are facade
//! names at their defining path, so 56 are names the facade does not
//! carry — plus `Risk`, re-exported from `uva-tool` into the public
//! `agent::approval` module, which has no path of its own inside the
//! harness. None of these names is promised: they may move or change in
//! any release.

pub mod agent;
/// The `approval` kernel service and the `tool/pre-execute` gate over it.
pub mod approval_plugin;
pub mod events;
mod input;
/// The `agentLoop` service: the per-run agent factory as a kernel plugin.
pub mod plugin;
mod prompt;
/// The four loop interception seams, by their kernel keys.
pub mod seams;

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
    EventStream,
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
        suspended_segment,
        workspace_hash,
    },
    descriptor::default_descriptor,
    host::{
        Arbitration,
        Diagnostic,
        DiagnosticKind,
        Gate,
        Hook,
        HookContext,
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
pub use approval_plugin::{ApprovalGate, ApprovalPlugin, KEY_APPROVAL, RiskOf};
pub use events::{
    AgentEvent,
    AgentMeta,
    SessionEndReason,
    SystemEvent,
    WarningKind,
};
pub use input::{AgentInput, ChannelInbox, RunInbox, SubagentWriters};
pub use prompt::{DEFAULT_SYSTEM_PROMPT, PromptContext};
