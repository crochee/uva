//! The approval capability: the vocabulary a held tool call becomes,
//! the decision functions a resume is checked with, and the kernel
//! plugin that publishes the deployment's stance as the `approval`
//! service.
//!
//! # What lives here
//!
//! The request, the policy, the resume token and the decisions are
//! this crate's own vocabulary; the risk a call carries is
//! `uva-tool`'s (a tool declares its own). The open set, the token
//! and the coverage check all derive from `SessionEvent` rows — the
//! paused state is a property of the log, not of any in-memory flag —
//! so the dependency on `uva-session` is by design, not accident.
//!
//! The `tool/pre-execute` gate that turns a held call into an `Ask`
//! is not here yet: it implements the harness `Gate` trait, whose
//! context still carries a harness-private sink, so it stays in
//! `uva-harness` as a thin adapter until the interception contract
//! moves under this crate. `uva-harness` re-exports this crate's
//! names at its own root, so a consumer of the frozen surface keeps
//! its imports unchanged.
//!
//! # Runtime neutrality
//!
//! The crate is runtime-free: no async, no executor, no clock. The
//! decision functions are pure folds over rows the caller already
//! holds.

mod approval;
mod plugin;

pub use approval::{
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
    merge_grants,
    open_requests,
    request_snapshot_hash,
    suspended_segment,
    wants_session_grant,
    workspace_hash,
};
pub use plugin::{ApprovalPlugin, KEY_APPROVAL, RiskOf};
