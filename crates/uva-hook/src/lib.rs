//! The interception contract under the agent loop.
//!
//! This crate owns what a third-party registration needs to
//! intercept a uva agent run without depending on the harness:
//!
//! - the four interception traits — [`Hook`], [`Gate`],
//!   [`RequestTransformer`], [`ResultTransformer`];
//! - the seven mount-point keys ([`seam::STEP_PRE`] …
//!   [`seam::PROMPT_SECTIONS`]) the loop dispatches its kernel
//!   waterfalls under;
//! - the decoupled [`HookContext`] — the run state and cancellation
//!   token a registration reads, and nothing else;
//! - [`HookPayload`], the payload every seam chain carries for one
//!   pass;
//! - [`HookRegistry`], the revocable registration table keyed by
//!   mount point, and its deny-only [`Arbitration`];
//! - [`HookPlugin`], the kernel plugin that publishes the registry as
//!   the `hooks` service the agent loop injects.
//!
//! The decision vocabulary is not duplicated here: a registration
//! returns [`HookDecision`] (five ways, including `ReplaceParams`),
//! and the payload's event is [`HookEvent`] — the event *is* the
//! payload (`BeforeTool` carries the tool name, the call id and the
//! arguments). Both are re-exported below, so an external plugin needs
//! this crate and no other to implement the traits; the
//! `async_trait` macro those traits are declared with is re-exported
//! for the same reason.
//!
//! `uva-harness` re-exports every name this crate owns, so the
//! crate-root paths (`uva_harness::Hook`, …) and the deep paths
//! (`uva_harness::agent::host::Hook`, `uva_harness::seams::TOOL_PRE`,
//! …) keep working; the curated frozen surface does not move. Why
//! the contract lives below the loop — and why the criterion is the
//! dependency tree, not the publicity of the keys — is recorded in
//! `docs/adr/0004-hook-contract-sinks-below-harness.md` in the
//! repository.

mod context;
mod plugin;
pub mod registry;
pub mod seam;
mod traits;

/// The attribute macro the interception traits are declared with.
///
/// A third-party plugin that implements [`Hook`], [`Gate`] or either
/// transformer needs this macro. Re-exporting it is what keeps
/// `uva-hook` the plugin's only normal dependency (the same rule the
/// `uva` facade applies to its own traits).
pub use async_trait::async_trait;
pub use context::HookContext;
pub use plugin::{HookPlugin, KEY_HOOKS};
pub use registry::{
    Arbitration,
    HookRegistration,
    HookRegistry,
    RegistryError,
    RegistryPass,
    is_mount_point,
};
pub use seam::HookPayload;
pub use traits::{Gate, Hook, RequestTransformer, ResultTransformer};
/// The decision vocabulary a registration returns.
pub use uva_steering::{HookDecision, HookEvent, RequestParams};
