//! # uva-steering
//!
//! The steering layer of the Uva agent runtime: the traits
//! and built-ins that observe, nudge, and veto an agent run
//! without the agent loop knowing any concrete policy.
//!
//! Adopted from the TraitClaw trait vocabulary and the dsh
//! (deepseek-harness) interception semantics, with traitclaw's
//! own wiring defects deliberately fixed:
//!
//! | traitclaw defect | uva fix |
//! |---|---|
//! | `Tracker::on_tool_call` never invoked | invoked at every dispatch boundary |
//! | guard state (budget/loop counters) held in interior-mutability shared across runs | per-run [`uva_context::AgentState`] is the single source of truth |
//! | `Sanitize` result silently discarded | sanitized action is adopted before dispatch |
//! | `inject_hints` ignores `InjectionPoint`/priority | all four injection points + priority gating honoured |
//! | severity never consumed | severity rides the denial message the model reads and the loop's structured log line |
//!
//! ## Layering
//!
//! This crate is the **vocabulary**, not the wiring: the harness's
//! `Host` registers one of each and dispatches it at the matching
//! boundary.
//!
//! ```text
//! agent loop (single writer of AgentState)
//!   ├─ Hook      async interception: observe, veto, or terminate the run
//!   ├─ Hint      advisory [reminder] injections (before each LLM call)
//!   ├─ Tracker   sync observation + concurrency recommendation
//!   ├─ Guard     sync fail-closed policy pipeline (per tool call)
//!   └─ OutputTransformer  async tool-output rewrite (commit path)
//! ```
//!
//! Errors-as-results contract: a guard denial or hook block is
//! surfaced to the model as the tool result (an error string with
//! actionable coaching), never as a run abort — the model adapts,
//! the session survives.

pub mod action;
pub mod effect_gate;
pub mod event;
pub mod guard;
pub mod guards;
pub mod hint;
pub mod hook;
pub mod output_transformer;
pub mod request_params;
pub mod tracker;
pub mod transformer;
pub use action::{Action, tool_fingerprint};
pub use effect_gate::{AdmissionError, Gate, GateControl, GateState};
pub use event::{HookDecision, HookEvent, event_name};
pub use guard::{
    Guard,
    GuardOutcome,
    GuardResult,
    GuardSeverity,
    NoopGuard,
    run_guards,
};
pub use guards::{
    CATASTROPHIC_SHELL_PATTERNS,
    LoopDetectionGuard,
    PromptInjectionGuard,
    ShellDenyGuard,
    ToolBudgetGuard,
    WorkspaceBoundaryGuard,
};
pub use hint::{
    ContextBudgetHint,
    Hint,
    HintMessage,
    HintPriority,
    InjectionPoint,
    IterationReminderHint,
    NoopHint,
    TruncationHint,
};
pub use hook::LoggingHook;
pub use output_transformer::{NoopOutputTransformer, OutputTransformer};
pub use request_params::RequestParams;
pub use transformer::{BudgetAwareTruncator, JsonExtractor, TransformerChain};
