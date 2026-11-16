//! Per-row mount context derivation: shared isolation realms and
//! intercept overlays.
//!
//! A profile row (the dsh entry model) contributes two context
//! derivations the kernel applies when it mounts the row's fiber: which
//! service keys the row reads through an isolated scope, and which
//! intercept configurations it overlays. Both are data — the host
//! resolves them from the document — and the kernel turns them into
//! derived [`Context`]s, one per row.

use std::{
    collections::BTreeMap,
    sync::{Mutex, PoisonError},
};

use serde_json::Value;

use crate::context::Context;

/// How one service key is isolated for a row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IsolateSpec {
    /// A fresh scope private to the row (dsh `LocalRealm`).
    Local,
    /// One scope shared by every row naming the same label for the same
    /// service (dsh `GlobalRealm`).
    Shared(String),
}

/// The context derivation one profile row contributes to its mount.
///
/// Both fields default empty: a row without an opinion mounts under its
/// parent context unchanged.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MountRow {
    /// Service keys to read through an isolated scope, with the scope
    /// spec for each.
    pub isolate: Vec<(String, IsolateSpec)>,
    /// Intercept configuration overlaid per service key, ancestor
    /// entries first (the mount order supplies the chain).
    pub intercept: Vec<(String, Value)>,
}

/// Process-wide registry of shared isolation labels.
///
/// A label resolves to one [`cordis::Isolation`] per (label, service)
/// pair, allocated on first use and stable for the process lifetime —
/// remounts rejoin the same scope instead of fragmenting it. Interiorly
/// mutable so mounts and group subtrees can share one registry.
#[derive(Debug, Default)]
pub struct Realms {
    scopes: Mutex<BTreeMap<String, cordis::Isolation>>,
}

impl Realms {
    /// The isolation label for `label`/`service`, allocating on first
    /// use.
    ///
    /// # Panics
    ///
    /// Never in practice: the lock is only poisoned if a holder
    /// panicked while holding it, and the critical section cannot
    /// panic.
    pub fn label(
        &self,
        ctx: &Context,
        label: &str,
        service: &str,
    ) -> cordis::Isolation {
        let key = format!("{label}::{service}");
        let mut scopes =
            self.scopes.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(found) = scopes.get(&key) {
            return *found;
        }
        let fresh = ctx.new_isolation();
        scopes.insert(key, fresh);
        fresh
    }
}

/// Derive the context a row mounts under: isolation scopes first, then
/// intercept overlays.
pub(crate) fn derive_context(
    base: &Context,
    realms: &Realms,
    row: &MountRow,
) -> Context {
    let mut ctx = base.clone();
    for (service, spec) in &row.isolate {
        let label = match spec {
            IsolateSpec::Local => ctx.new_isolation(),
            IsolateSpec::Shared(label) => realms.label(&ctx, label, service),
        };
        ctx = ctx.isolate_with(service.clone(), label);
    }
    for (service, config) in &row.intercept {
        ctx = ctx.intercept_value(service.clone(), config.clone());
    }
    ctx
}
