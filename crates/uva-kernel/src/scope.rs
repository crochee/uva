//! Scope isolation and the `ui.` / core-key namespace boundary.
//!
//! [`Context::isolate`] gives a child its own service scope: a service a
//! child provides is invisible to its parent and to sibling isolates,
//! while the event bus stays shared with the root. [`namespace_allows`]
//! is the assembly-time check that keeps the two layers from claiming
//! each other's keys: core keys must not carry the `ui.` prefix, and UI
//! keys must carry it.

use std::sync::Arc;

use crate::{Context, PluginError};

/// The prefix reserved for services owned by the UI layer.
pub const UI_PREFIX: &str = "ui.";

/// The two sides of the `ui.` namespace boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Namespace {
    /// The core service tree (`llm`, `tools`, `sessions`, ...).
    Core,
    /// The UI service tree (`ui.theme`, `ui.panels`, ...).
    Ui,
}

/// Whether `namespace` may own `key`.
///
/// The rule is the prefix alone: [`Namespace::Core`] rejects keys that
/// start with [`UI_PREFIX`], [`Namespace::Ui`] requires it. An empty key
/// belongs to neither namespace.
pub fn namespace_allows(namespace: Namespace, key: &str) -> bool {
    if key.is_empty() {
        return false;
    }
    match namespace {
        Namespace::Core => !key.starts_with(UI_PREFIX),
        Namespace::Ui => key.starts_with(UI_PREFIX),
    }
}

/// A service resolved once and kept for the life of the handle.
///
/// Resolving through [`ServiceHandle::resolve`] caches one `Arc<T>` so a
/// hot loop does not re-enter the service registry per call, and the
/// instance stays alive even if its provider is later withdrawn. A
/// withdrawal is still visible to fresh lookups through the context.
pub struct ServiceHandle<T> {
    value: Arc<T>,
}

impl<T> ServiceHandle<T> {
    /// Resolve `key` from `ctx` and cache the instance.
    ///
    /// # Errors
    ///
    /// Returns [`PluginError::Failed`] when the service is unavailable.
    pub fn resolve(ctx: &Context, key: &str) -> Result<Self, PluginError>
    where
        T: Send + Sync + 'static,
    {
        Ok(Self {
            value: ctx.require(key)?,
        })
    }

    /// The cached instance.
    pub fn get(&self) -> &Arc<T> {
        &self.value
    }
}

impl<T> Clone for ServiceHandle<T> {
    fn clone(&self) -> Self {
        Self {
            value: Arc::clone(&self.value),
        }
    }
}

impl<T> std::fmt::Debug for ServiceHandle<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServiceHandle").finish_non_exhaustive()
    }
}
