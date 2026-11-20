//! The interception contract under the agent loop.
//!
//! This crate owns what a third-party registration needs to
//! intercept a uva agent run without depending on the harness:
//!
//! - the four interception traits — [`Hook`], [`Gate`],
//!   [`RequestTransformer`], [`ResultTransformer`];
//! - the twelve mount points ([`MountPoint`]) — six pass points, five
//!   observation points and the prompt-section collection — each with
//!   one wire spelling, and the [`RegistrationOrigin`] that says who
//!   registered and which lane it runs in;
//! - the decoupled [`HookContext`] — the run state and cancellation
//!   token a registration reads, and nothing else;
//! - [`HookRegistry`], the revocable registration table keyed by
//!   mount point, the prompt-section collection it carries, and its
//!   deny-only [`Arbitration`];
//! - [`arbitration`], the one rule every pass runs under: what a
//!   registration's decision means at a mount point;
//! - [`HookPlugin`], the kernel plugin that publishes the registry as
//!   the `hooks` service the agent loop injects.
//!
//! The decision vocabulary is not duplicated here: a registration
//! returns [`HookDecision`] (five ways, including `ReplaceParams`) over
//! the dispatching [`HookEvent`] (`BeforeTool` carries the tool name,
//! the call id and the arguments). Both are re-exported below, so an
//! external plugin needs this crate and no other to implement the
//! traits; the `async_trait` macro those traits are declared with is
//! re-exported for the same reason.
//!
//! `uva-harness` re-exports the interception traits, so the crate-root
//! paths (`uva_harness::Hook`, …) and the deep path
//! (`uva_harness::agent::host::Hook`) keep working; the loop's own
//! `Seams` dispatches through the registry held by its `Host`. Why the
//! contract lives below the loop — and why the criterion is the
//! dependency tree, not the publicity of a name — is recorded in
//! `docs/adr/0004-hook-contract-sinks-below-harness.md` in the
//! repository.

pub mod arbitration;
mod context;
mod mount;
mod plugin;
pub mod registry;
mod traits;

pub use arbitration::{
    Arbitration,
    Fact,
    FactKind,
    PanicPolicy,
    Refusal,
    Ruling,
};
/// The attribute macro the interception traits are declared with.
///
/// A third-party plugin that implements [`Hook`], [`Gate`] or either
/// transformer needs this macro. Re-exporting it is what keeps
/// `uva-hook` the plugin's only normal dependency (the same rule the
/// `uva` facade applies to its own traits).
pub use async_trait::async_trait;
pub use context::HookContext;
pub use mount::{MountPoint, RegistrationOrigin};
pub use plugin::{HookPlugin, KEY_HOOKS};
pub use registry::{
    HookRegistration,
    HookRegistry,
    RegistryError,
    RegistryPass,
};
pub use traits::{Gate, Hook, RequestTransformer, ResultTransformer};
/// A system-prompt section a registration contributes.
///
/// The section vocabulary belongs to the prompt manifest in
/// `uva-agent`; the registry only collects sections in table order,
/// so a registration does not introduce a second section type.
pub use uva_agent::PromptSection;
/// The decision vocabulary a registration returns.
pub use uva_steering::{HookDecision, HookEvent, RequestParams};
