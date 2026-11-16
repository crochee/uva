//! [`uva_hook`] — the interception contract under the agent loop: the
//! `Hook` / `Gate` / `RequestTransformer` / `ResultTransformer` traits,
//! the seven mount-point keys, the decoupled `HookContext`, the
//! revocable `HookRegistry` and its deny-only arbitration, and the
//! `hooks` kernel plugin.
//!
//! `uva::harness` carries the same contract names (the harness
//! re-exports the traits and keys), but the registry and the plugin
//! live here: a third-party plugin depends on this module and never on
//! the loop.
pub use uva_hook::*;
