// Allow `result_large_err` for this module: P1b added 4 hidden
// fields to every struct-form variant (frames, backtrace, source,
// and the synthetic source chain), so every `Result<_, Error>` is
// at least 128 bytes. Boxing the error would force every call site
// to `.map_err(|e| *e)` (or accept the allocation), and the existing
// API has no `Box<Error>` in the public surface. Accept the size
// cost; revisit if profiling shows it matters.
#![allow(clippy::result_large_err)]

//! Registry trait definition.
//!
//! # Design
//!
//! A **registry** is a catalog of named items plus their metadata
//! entries. [`Registry`] is a single trait that covers registration
//! (`put` / `delete`), querying (`get`), cursor-paginated listing
//! ([`Registry::list_paginate`]), and an unpaginated convenience
//! ([`Registry::list`], defaulted). The trait uses two associated
//! types:
//!
//! - [`Registry::Item`] — the value the registry stores and returns.
//!   It implements [`RegistryItem`] (giving `name()` / `description()`)
//!   and is `Clone` so the trait can hand copies back to callers.
//! - [`Registry::Filter`] — the list filter type. Use [`EmptyFilter`]
//!   when no filtering is needed.
//!
//! # Listing model
//!
//! Registries expose two listing-shaped methods:
//!
//! - [`Registry::list_paginate`] returns a [`RegistryList<T>`]
//!   envelope (`data`, `next_cursor`, `total`) with cursor-based
//!   keyset pagination. Implementations fetch their full filtered
//!   set themselves and then delegate the slicing/encoding work to
//!   [`paginate_registry_list`].
//! - [`Registry::list`] has a default implementation that calls
//!   [`Registry::list_paginate`] with `limit = u64::MAX` and returns
//!   just the `data` vector. Registries that have a more efficient
//!   "everything" path may override it.
//!
//! # No `Serialize` / `Deserialize` bound
//!
//! `Registry::Item` is intentionally not required to be
//! `(de)serializable`. Most `Item` values are constructed at runtime
//! from in-memory state and cannot be (de)serialized in any
//! meaningful sense. Persistence is a per-implementation concern; it
//! lives on impl-side helper methods, not on the trait contract.

use std::{collections::HashMap, sync::atomic::AtomicU64};

use async_trait::async_trait;
use parking_lot::RwLock;

use crate::error::Error;

/// Maximum page size enforced by [`paginate_registry_list`]. Larger
/// `limit` values are silently truncated to this value.
///
/// Source of truth for both the in-memory registry pagination
/// (`paginate_registry_list` below) and the wire-level cap in
/// `elysper_server::api::v1::page_query::MAX_LIMIT`. The server's
/// constant is sourced from this one (an alias), so the two
/// cannot drift — the previous "MUST stay in sync" doc comment
/// is now a `const ... = elysper_core::registry::MAX_LIMIT;`
/// line that the compiler enforces.
///
/// Pin the value at `100` so a refactor that changed the
/// number would fail the in-crate test
/// (`max_limit_matches_spec`).
pub const MAX_LIMIT: u64 = 100;

/// Lightweight cursor-list envelope returned by
/// [`Registry::list_paginate`].
///
/// This is a mirror of the wire-level
/// `elysper_server::api::v1::List<T>` — same shape, same JSON
/// serialization. We keep an independent copy in core because the
/// registry pagination logic is generic over `Self::Item` and must
/// not depend on the `elysper-server` wire envelope.
///
/// Public so external crates (elysper-harness, elysper-tool, …)
/// that call `Registry::list_paginate` directly can read the
/// fields. `elysper-server` routes wrap the result in their own
/// `api::v1::List<T>` for the wire response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegistryList<T> {
    pub data: Vec<T>,
    pub next_cursor: Option<String>,
    pub total: Option<u64>,
}

impl<T> RegistryList<T> {
    pub fn new(data: Vec<T>) -> Self {
        Self {
            data,
            next_cursor: None,
            total: None,
        }
    }

    pub fn with_next_cursor(mut self, cursor: impl Into<String>) -> Self {
        self.next_cursor = Some(cursor.into());
        self
    }

    pub fn with_total(mut self, total: u64) -> Self {
        self.total = Some(total);
        self
    }
}

/// Decode a registry cursor into the underlying resource ID.
///
/// Thin re-export of [`crate::cursor::decode`] so the
/// `paginate_registry_list` call site reads naturally. The
/// codec itself lives in [`crate::cursor`] — the canonical
/// home; the server's HTTP layer and the in-registry
/// pagination use the same encoding, so a cursor emitted
/// by one round-trips on the other.
#[allow(clippy::result_large_err)] // P1b: `elysper_core::Error` carries 4 hidden fields; result is intentionally large
fn decode_registry_cursor(cursor: &str) -> Result<String, Error> {
    crate::cursor::decode(cursor)
}

/// Encode the last resource ID into an opaque cursor. Mirror
/// of `elysper_server::api::v1::encode_cursor`. Thin
/// re-export of [`crate::cursor::encode`] for the same
/// reason as [`decode_registry_cursor`].
fn encode_registry_cursor(id: &str) -> String {
    crate::cursor::encode(id)
}

/// Slice a fully-fetched `items` vector into the page described by
/// the cursor + limit, returning a [`RegistryList<T>`] envelope.
///
/// This is the shared pagination primitive every
/// [`Registry::list_paginate`] implementation should delegate to:
/// each registry is responsible only for fetching its filtered set
/// (which requires knowledge of the underlying store), and then
/// hands the result here for the cursor arithmetic + envelope
/// construction.
///
/// # Cursor
///
/// The cursor is the opaque base64 encoding of the last
/// resource's [`RegistryItem::name`] on the previous page.
/// Decode failures return [`Error::InvalidItem`]. A cursor that
/// points to a deleted resource yields an empty page (per spec:
/// `data: [], next_cursor: null`). An empty-string cursor is
/// treated as "no cursor" (first page).
///
/// # Limit
///
/// `limit == 0` returns [`Error::Validation`]. `limit` above
/// `MAX_LIMIT` is silently truncated to
/// `MAX_LIMIT`.
///
/// Callers that don't need pagination can pass `limit = u64::MAX`
/// — they get the full set back with `total` populated, or call
/// [`Registry::list`] for the unpaginated convenience.
pub fn paginate_registry_list<T: RegistryItem>(
    items: Vec<T>,
    cursor: Option<&str>,
    limit: u64,
) -> Result<RegistryList<T>, Error> {
    let total = items.len() as u64;

    let effective_limit = if limit == 0 {
        return Err(Error::validation("limit must be greater than 0"));
    } else {
        limit.min(MAX_LIMIT)
    };

    let last_seen_id: Option<String> = match cursor {
        None => None,
        Some("") => None,
        Some(c) => Some(decode_registry_cursor(c)?),
    };

    let start_idx: usize = match &last_seen_id {
        None => 0,
        Some(cursor_id) => items
            .iter()
            .position(|item| item.name() == cursor_id.as_str())
            .map(|p| p + 1)
            .unwrap_or(items.len()),
    };

    if start_idx >= items.len() {
        return Ok(RegistryList::new(Vec::new()).with_total(total));
    }

    let remaining = items.len() - start_idx;
    let limit_usize = effective_limit as usize;
    let take_count = remaining.min(limit_usize);
    let has_more = remaining > limit_usize;

    let page: Vec<T> =
        items.into_iter().skip(start_idx).take(take_count).collect();

    let last_id = page.last().map(|item| item.name()).unwrap_or("");
    let next = if has_more {
        Some(encode_registry_cursor(last_id))
    } else {
        None
    };

    let mut list = RegistryList::new(page);
    if let Some(c) = next {
        list = list.with_next_cursor(c);
    }
    Ok(list.with_total(total))
}

/// No-op filter. Use as [`Registry::Filter`] when the registry has
/// no filterable fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EmptyFilter;

/// Metadata contract for anything stored in a [`Registry`].
///
/// Every `Registry::Item` implements this so the registry can
/// extract a uniform `name()` + `description()` for cursor
/// encoding, listing, and the `paginate_registry_list` helper
/// without knowing the item's concrete type.
pub trait RegistryItem: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
}

// ===== NameTable =========================================================

/// A concurrency-safe `HashMap<String, T>` with a monotonic version counter,
/// shared by every "name → object" catalog in the workspace.
///
/// This is the storage half of a registry; the [`Registry`] trait is the
/// wire-facing half. Holding the map + version in one type means the two
/// cannot drift apart: every successful mutation bumps the version, and a
/// caller keying a cache on [`NameTable::version`] is invalidated exactly
/// when the catalog changed.
///
/// `names()` and `values()` return sorted order so listings are
/// deterministic across lock-acquisition order.
pub struct NameTable<T> {
    inner: RwLock<HashMap<String, T>>,
    version: AtomicU64,
}

impl<T> NameTable<T> {
    /// Empty table at version `0`.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(HashMap::new()),
            version: AtomicU64::new(0),
        }
    }

    /// Insert `value` under `name`, returning the previous binding. Bumps the
    /// version whether or not a previous binding existed.
    pub fn insert(&self, name: String, value: T) -> Option<T> {
        let prev = self.inner.write().insert(name, value);
        self.bump();
        prev
    }

    /// Insert only when `name` is absent, handing the value back on a
    /// duplicate. This is the atomic check-and-set behind a `put`-style
    /// registry whose duplicate rejection must not race with a concurrent
    /// registration.
    pub fn insert_if_absent(&self, name: String, value: T) -> Result<(), T> {
        let mut guard = self.inner.write();
        if guard.contains_key(&name) {
            return Err(value);
        }
        guard.insert(name, value);
        drop(guard);
        self.bump();
        Ok(())
    }

    /// Remove `name`, returning the binding. Does **not** bump the version
    /// when the name was absent: a no-op delete is not a catalog change.
    pub fn remove(&self, name: &str) -> Option<T> {
        let prev = self.inner.write().remove(name);
        if prev.is_some() {
            self.bump();
        }
        prev
    }

    /// Every key, sorted.
    #[must_use]
    pub fn names(&self) -> Vec<String> {
        let mut names: Vec<String> =
            self.inner.read().keys().cloned().collect();
        names.sort();
        names
    }

    /// Number of bindings.
    #[must_use]
    pub fn len(&self) -> usize {
        self.inner.read().len()
    }

    /// Whether the table holds no bindings.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Monotonic counter of successful mutations; cache keys read this.
    #[must_use]
    pub fn version(&self) -> u64 {
        self.version.load(std::sync::atomic::Ordering::Relaxed)
    }

    fn bump(&self) {
        self.version
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

impl<T: Clone> NameTable<T> {
    /// Clone the binding under `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<T> {
        self.inner.read().get(name).cloned()
    }

    /// Clone every binding, in sorted-name order.
    #[must_use]
    pub fn values(&self) -> Vec<T> {
        let guard = self.inner.read();
        let mut pairs: Vec<(String, T)> =
            guard.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        drop(guard);
        pairs.sort_by(|a, b| a.0.cmp(&b.0));
        pairs.into_iter().map(|(_, v)| v).collect()
    }
}

impl<T> Default for NameTable<T> {
    fn default() -> Self {
        Self::new()
    }
}

/// Named-item catalog trait.
///
/// Each implementor declares the item type it stores
/// ([`Registry::Item`]) and the list filter it supports
/// ([`Registry::Filter`]). Implementations of [`Registry::list_paginate`]
/// should fetch the full filtered set themselves and delegate the
/// cursor/limit/envelope construction to [`paginate_registry_list`].
///
/// [`Registry::list`] has a default implementation that calls
/// [`Registry::list_paginate`] with `limit = u64::MAX` and returns
/// the resulting `data` vector.
#[async_trait]
pub trait Registry: Send + Sync {
    type Item: RegistryItem + Clone + Send + Sync + 'static;
    type Filter: Clone + Send + Sync + 'static;

    /// Insert (or replace) an item by its [`RegistryItem::name`].
    /// Returns an error if the registration is rejected
    /// (e.g. duplicate name, missing dependency).
    async fn put(&self, item: Self::Item) -> Result<(), Error>;

    /// Remove an item by metadata name. Returns
    /// `Err(Error::NotFound)` if no item with that name is currently
    /// registered.
    async fn delete(&self, name: &str) -> Result<(), Error>;

    /// Look up a single metadata entry by name.
    async fn get(&self, name: &str) -> Result<Option<Self::Item>, Error>;

    /// Paginated list with cursor-based keyset pagination.
    ///
    /// Callers that don't need pagination should call
    /// [`Registry::list`] instead. The `sort` parameter is accepted
    /// for API compatibility; implementations are free to apply it
    /// or ignore it (in-memory registries typically ignore it and
    /// return insertion order).
    ///
    /// Cursor semantics, `limit` validation, and the
    /// `data`/`next_cursor`/`total` envelope are all handled by
    /// [`paginate_registry_list`] — implementations are expected to
    /// fetch their filtered set and then delegate.
    async fn list_paginate(
        &self,
        cursor: Option<String>,
        limit: u64,
        sort: Option<String>,
        filter: Option<Self::Filter>,
    ) -> Result<RegistryList<Self::Item>, Error>;

    /// Unpaginated list. Returns the full filtered `data` vector of
    /// [`Registry::list_paginate`] with `limit = u64::MAX`.
    ///
    /// Equivalent to
    /// `self.list_paginate(None, u64::MAX, None, filter).await?.data`.
    /// Override only if the implementation has a more efficient
    /// "everything" path.
    async fn list(
        &self,
        filter: Option<Self::Filter>,
    ) -> Result<Vec<Self::Item>, Error> {
        let page = self.list_paginate(None, u64::MAX, None, filter).await?;
        Ok(page.data)
    }

    /// Begin the registry's lifecycle for the named item. The
    /// default is a no-op returning `Ok(())`, suitable for stateless
    /// or fully-in-memory registries (e.g. `ToolRegistry`,
    /// `AgentRegistry`).
    ///
    /// Implementors that own background work, file handles, or
    /// network resources associated with a named item MUST override
    /// this hook. The name is the [`RegistryItem::name`] of the
    /// entry to start.
    async fn start(&self, _name: &str) -> Result<(), Error> {
        Ok(())
    }

    /// End the registry's lifecycle for the named item. The default
    /// is a no-op returning `Ok(())`, symmetric to
    /// [`Registry::start`]. Override only when an implementor
    /// actually holds lifecycle state to release.
    async fn stop(&self, _name: &str) -> Result<(), Error> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
