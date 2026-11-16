//! [`uva_approval`] — the approval capability: the held-call
//! vocabulary (`ApprovalRequest` / `Suspension` / `ResumeToken`), the
//! policy that decides which calls are held, the log-derived decision
//! functions a resume is checked with, and the kernel plugin that
//! publishes the deployment's stance as the `approval` service.
//!
//! The `tool/pre-execute` gate stays in `uva-harness` (as
//! [`ApprovalGate`](crate::harness::ApprovalGate)) until the
//! interception contract moves under `uva-approval`; `uva::harness`
//! carries the same names this module does, because the harness's
//! frozen facade re-exports the crate.
pub use uva_approval::*;
