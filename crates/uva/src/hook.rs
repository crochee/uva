//! [`uva_hook`] — the interception contract under the agent loop: the
//! `Hook` / `Gate` / `RequestTransformer` / `ResultTransformer` traits,
//! the twelve `MountPoint` variants and the `RegistrationOrigin` lanes,
//! the decoupled `HookContext`, the revocable `HookRegistry` and its
//! deny-only arbitration, and the `hooks` kernel plugin.
//!
//! `uva::harness` carries the same trait and context names (the
//! harness re-exports them), but the registry and the plugin live
//! here: a third-party plugin depends on this module and never on the
//! loop.
pub use uva_hook::*;
