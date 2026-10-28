//! [`MemoStore`] — pluggable storage for memoized tool outputs.
//!
//! The default [`InMemoryMemoStore`] is LRU + TTL. Custom
//! implementations plug in here (SQLite, Redis, …); the wrapper
//! only depends on the trait.

use std::{path::PathBuf, time::Duration};

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use elysper_tool::ToolOutput;

/// Scope applied to the memo key. Two tools with the same name and
/// input but different scopes do not share cache.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum MemoScope {
    /// Process-global cache (the default).
    Process,
    /// Cache is partitioned by the canonicalised workspace path.
    Workspace(PathBuf),
}

/// 32-byte BLAKE3 key.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MemoKey(pub [u8; 32]);

/// One memoized entry: the original output, when it was stored, and
/// the TTL.
#[derive(Clone, Debug)]
pub struct MemoEntry {
    pub output: ToolOutput,
    pub stored_at: DateTime<Utc>,
    pub ttl: Duration,
}

impl MemoEntry {
    #[must_use]
    pub fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        let age = now.signed_duration_since(self.stored_at);
        let chrono_ttl = chrono::Duration::from_std(self.ttl)
            .unwrap_or_else(|_| chrono::Duration::seconds(i64::MAX));
        age >= chrono::Duration::zero() && age <= chrono_ttl
    }
}

/// Pluggable memo store.
pub trait MemoStore: Send + Sync {
    fn lookup(&self, key: &MemoKey, now: DateTime<Utc>) -> Option<ToolOutput>;
    fn store(
        &self,
        key: MemoKey,
        output: ToolOutput,
        stored_at: DateTime<Utc>,
        ttl: Duration,
    );
    fn invalidate(&self, key: &MemoKey);
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
    fn clear(&self);
}

/// LRU + TTL in-memory store. Identical shape to the
/// `elysper-semantic-cache` store, kept as a standalone
/// implementation here so the two crates never share an internal
/// type.
pub struct InMemoryMemoStore {
    inner: RwLock<Inner>,
    max_entries: usize,
}

struct Inner {
    entries: indexmap_lite::IndexMap<[u8; 32], Row>,
}

struct Row {
    output: ToolOutput,
    stored_at: DateTime<Utc>,
    ttl: Duration,
    #[allow(dead_code)]
    last_touched: u64,
    /// Monotonic counter for LRU. Bumped on every successful
    /// `lookup`/`store` so eviction hits the oldest untouched entry.
    counter: u64,
}

impl Row {
    fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        let age = now.signed_duration_since(self.stored_at);
        let chrono_ttl = chrono::Duration::from_std(self.ttl)
            .unwrap_or_else(|_| chrono::Duration::seconds(i64::MAX));
        age >= chrono::Duration::zero() && age <= chrono_ttl
    }
}

impl Default for InMemoryMemoStore {
    fn default() -> Self {
        Self::with_capacity(4096)
    }
}

impl InMemoryMemoStore {
    #[must_use]
    pub fn with_capacity(max_entries: usize) -> Self {
        Self {
            inner: RwLock::new(Inner {
                entries: indexmap_lite::IndexMap::new(),
            }),
            max_entries: max_entries.max(1),
        }
    }
}

impl MemoStore for InMemoryMemoStore {
    fn lookup(&self, key: &MemoKey, now: DateTime<Utc>) -> Option<ToolOutput> {
        let mut inner = self.inner.write();
        let hex: [u8; 32] = key.0;
        if let Some(row) = inner.entries.get_mut(&hex)
            && row.is_fresh(now)
        {
            row.counter = row.counter.saturating_add(1);
            return Some(clone_output(&row.output));
        }
        None
    }

    fn store(
        &self,
        key: MemoKey,
        output: ToolOutput,
        stored_at: DateTime<Utc>,
        ttl: Duration,
    ) {
        let mut inner = self.inner.write();
        let hex = key.0;
        let counter = inner.entries.len() as u64 + 1;
        if inner.entries.has_key(&hex) {
            if let Some(row) = inner.entries.get_mut(hex.as_slice()) {
                row.output = output;
                row.stored_at = stored_at;
                row.ttl = ttl;
                row.counter = counter;
            }
            return;
        }
        if inner.entries.len() >= self.max_entries
            && let Some(lru_key) = inner
                .entries
                .iter()
                .min_by_key(|(_, row)| row.counter)
                .map(|(k, _)| *k)
        {
            inner.entries.shift_remove(&lru_key);
        }
        inner.entries.insert(
            hex,
            Row {
                output,
                stored_at,
                ttl,
                last_touched: 0,
                counter,
            },
        );
    }

    fn invalidate(&self, key: &MemoKey) {
        let mut inner = self.inner.write();
        let hex = key.0;
        inner.entries.shift_remove(&hex);
    }

    fn len(&self) -> usize {
        self.inner.read().entries.len()
    }

    fn clear(&self) {
        self.inner.write().entries.clear();
    }
}

fn clone_output(output: &ToolOutput) -> ToolOutput {
    // `ToolOutput` is `Clone`; this helper exists so the rest of
    // the module can refer to one canonical clone site.
    output.clone()
}

/// Standalone `IndexMap` shim — mirrors the one in
/// `elysper-semantic-cache`. Kept local so the two crates don't
/// share an internal type (and so either can change shape without
/// dragging the other).
mod indexmap_lite {
    use std::collections::HashMap;

    pub(crate) struct IndexMap<K, V> {
        order: Vec<K>,
        index: HashMap<K, usize>,
        values: Vec<Option<V>>,
    }

    impl<K, V> IndexMap<K, V>
    where
        K: Clone + Eq + std::hash::Hash,
    {
        pub(crate) fn new() -> Self {
            Self {
                order: Vec::new(),
                index: HashMap::new(),
                values: Vec::new(),
            }
        }

        pub(crate) fn len(&self) -> usize {
            self.order.len()
        }

        #[allow(dead_code)]
        pub(crate) fn is_empty(&self) -> bool {
            self.order.is_empty()
        }

        pub(crate) fn clear(&mut self) {
            self.order.clear();
            self.index.clear();
            self.values.clear();
        }

        pub(crate) fn has_key(&self, k: &K) -> bool {
            self.index.contains_key(k)
        }

        #[allow(dead_code)]
        pub(crate) fn get(&self, k: &K) -> Option<&V> {
            self.index.get(k).and_then(|i| self.values[*i].as_ref())
        }

        pub(crate) fn get_mut<Q>(&mut self, k: &Q) -> Option<&mut V>
        where
            K: std::borrow::Borrow<Q>,
            Q: std::hash::Hash + Eq + ?Sized,
        {
            let i = *self.index.get(k)?;
            self.values[i].as_mut()
        }

        pub(crate) fn iter(&self) -> impl Iterator<Item = (&K, &V)> {
            self.order
                .iter()
                .zip(self.values.iter())
                .filter_map(|(k, v)| v.as_ref().map(|v| (k, v)))
        }

        pub(crate) fn insert(&mut self, k: K, v: V) {
            if let Some(i) = self.index.get(&k).copied() {
                self.values[i] = Some(v);
                return;
            }
            let i = self.values.len();
            self.values.push(Some(v));
            self.order.push(k.clone());
            self.index.insert(k, i);
        }

        pub(crate) fn shift_remove(&mut self, k: &K) -> Option<V> {
            let i = self.index.remove(k)?;
            let slot = self.values[i].take();
            if let Some(pos) = self.order.iter().position(|x| x == k) {
                self.order.remove(pos);
            }
            slot
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_entry_is_returned() {
        let store = InMemoryMemoStore::default();
        let now = chrono::Utc::now();
        let key = MemoKey([1u8; 32]);
        store.store(key, ToolOutput::text("hi"), now, Duration::from_secs(60));
        assert!(store.lookup(&key, now).is_some());
    }

    #[test]
    fn stale_entry_returns_none() {
        let store = InMemoryMemoStore::default();
        let now = chrono::Utc::now();
        let key = MemoKey([2u8; 32]);
        store.store(key, ToolOutput::text("hi"), now, Duration::from_secs(60));
        let later = now + chrono::Duration::seconds(120);
        assert!(store.lookup(&key, later).is_none());
    }

    #[test]
    fn lru_eviction_is_under_capacity() {
        let store = InMemoryMemoStore::with_capacity(2);
        let now = chrono::Utc::now();
        for i in 0u8..3u8 {
            store.store(
                MemoKey([i; 32]),
                ToolOutput::text("x"),
                now,
                Duration::from_secs(60),
            );
        }
        assert_eq!(store.len(), 2);
    }
}
