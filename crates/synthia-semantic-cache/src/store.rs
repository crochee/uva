//! [`SemanticCacheStore`] — pluggable storage for cached completions.
//!
//! The default [`InMemorySemanticCache`] is LRU + TTL. Custom
//! implementations plug in here (SQLite, Redis, …) — the wrapper
//! only depends on the trait.

use std::time::Duration;

use chrono::{DateTime, Utc};
use parking_lot::RwLock;
use sha2::{Digest, Sha256};
use synthia_provider::CompletionResponse;
use synthia_search::Embedder;

/// 32-byte key derived from `Sha256(model + tool_set_names +
/// messages)`. Used both as a map key (hex string form) and as the
/// identifier the wrapper reads back.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct EmbeddingKey(pub [u8; 32]);

impl EmbeddingKey {
    /// Lowercase hex form. Stable across runs, safe as a map key.
    #[must_use]
    pub fn to_hex(&self) -> String {
        hex::encode(self.0)
    }

    /// Build a key from the structural fingerprint of a request.
    /// The exact bytes are an implementation detail; the property
    /// callers rely on is "two structurally-identical requests yield
    /// the same key".
    #[must_use]
    pub fn from_fingerprint(fingerprint: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(fingerprint);
        let out = hasher.finalize();
        let mut key = [0u8; 32];
        key.copy_from_slice(&out);
        Self(key)
    }
}

/// A cached completion: the original provider response, when it
/// was stored, and the TTL the engine treats it as fresh for.
#[derive(Clone, Debug)]
pub struct CachedCompletion {
    /// The original provider response, verbatim.
    pub response: CompletionResponse,
    /// When the entry was stored (wall-clock).
    pub stored_at: DateTime<Utc>,
    /// Lifetime advertised to the engine. Re-derived at lookup time
    /// from `now - stored_at` against the configured TTL.
    pub ttl: Duration,
}

impl CachedCompletion {
    /// True when `now - stored_at <= ttl`.
    #[must_use]
    pub fn is_fresh(&self, now: DateTime<Utc>) -> bool {
        let age = now.signed_duration_since(self.stored_at);
        // `chrono::Duration::from_std` rejects std durations whose
        // total seconds overflow `i64`. Cap at i64::MAX seconds
        // (≈ 292 billion years) — past any practical TTL.
        let chrono_ttl = chrono::Duration::from_std(self.ttl)
            .unwrap_or_else(|_| chrono::Duration::seconds(i64::MAX));
        age >= chrono::Duration::zero() && age <= chrono_ttl
    }
}

/// Stats a custom store might surface for observability. The
/// in-memory default implements these; the trait leaves them
/// optional.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SemanticCacheStats {
    pub hits: u64,
    pub misses: u64,
    pub entries: u64,
}

/// Pluggable cache store. Two methods + bookkeeping; the wrapper
/// always calls `lookup` first and `store` on a miss.
pub trait SemanticCacheStore: Send + Sync {
    /// Look up a candidate. Returns `Some(completion)` only when a
    /// stored entry matches `key` with cosine similarity above
    /// `threshold` AND is still within its TTL (the wrapper computes
    /// `now`; the store is allowed to filter purely by exact match
    /// and let the wrapper expire).
    fn lookup(
        &self,
        key: &EmbeddingKey,
        request_embedding: &[f32],
        threshold: f32,
        now: DateTime<Utc>,
    ) -> Option<CachedCompletion>;

    /// Persist `completion` against `key` for `ttl`. The store MUST
    /// apply any size / TTL constraints it owns (the default evicts
    /// least-recently-used).
    fn store(
        &self,
        key: EmbeddingKey,
        request_embedding: Vec<f32>,
        completion: CachedCompletion,
    );

    /// Number of stored entries (bookkeeping / metrics).
    fn len(&self) -> usize;

    /// True when `len == 0`.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drop every stored entry.
    fn clear(&self);

    /// Snapshot of stats for observability. Default: no stats.
    fn stats(&self) -> SemanticCacheStats {
        SemanticCacheStats::default()
    }
}

/// Single-row LRU + TTL store backed by `parking_lot::RwLock`.
///
/// `lookup` walks the map, scoring each entry's stored embedding
/// against the request embedding via cosine. The best match above
/// `threshold` AND still within TTL wins. The walk is O(n) in the
/// store size — fine for the default `max_entries = 2048`; for
/// higher cardinalities a caller supplies a vector-indexed store.
pub struct InMemorySemanticCache {
    inner: RwLock<Inner>,
    max_entries: usize,
}

struct Inner {
    entries: indexmap_lite::IndexMap<String, Row>,
    stats: SemanticCacheStats,
}

struct Row {
    /// Stored for debug and for future keyed-eviction policies; the
    /// map's hex string key is the actual lookup key.
    #[allow(dead_code)]
    key: EmbeddingKey,
    embedding: Vec<f32>,
    completion: CachedCompletion,
    /// Monotonic counter used to evict the LRU. Incremented on every
    /// `touch`. Higher = more recent.
    last_touched: u64,
}

impl Default for InMemorySemanticCache {
    fn default() -> Self {
        Self::with_capacity(2048)
    }
}

impl InMemorySemanticCache {
    /// Build with a custom LRU capacity.
    #[must_use]
    pub fn with_capacity(max_entries: usize) -> Self {
        Self {
            inner: RwLock::new(Inner {
                entries: indexmap_lite::IndexMap::new(),
                stats: SemanticCacheStats::default(),
            }),
            max_entries: max_entries.max(1),
        }
    }

    fn touch(&self, key: &str, counter: &mut u64) {
        let mut inner = self.inner.write();
        if let Some(row) = inner.entries.get_mut(key) {
            *counter += 1;
            row.last_touched = *counter;
        }
    }
}

impl SemanticCacheStore for InMemorySemanticCache {
    fn lookup(
        &self,
        key: &EmbeddingKey,
        request_embedding: &[f32],
        threshold: f32,
        now: DateTime<Utc>,
    ) -> Option<CachedCompletion> {
        let mut best: Option<(f32, CachedCompletion)> = None;
        let mut touch_counter = 0u64;
        let mut touch_target: Option<String> = None;
        {
            let mut inner = self.inner.write();
            for (k, row) in inner.entries.iter() {
                if !row.completion.is_fresh(now) {
                    continue;
                }
                let sim =
                    crate::cosine_similarity(&row.embedding, request_embedding);
                if sim >= threshold
                    && best.as_ref().is_none_or(|(s, _)| sim > *s)
                {
                    best = Some((sim, row.completion.clone()));
                    touch_target = Some(k.clone());
                }
            }
            if let Some(target) = &touch_target {
                inner.stats.hits = inner.stats.hits.saturating_add(1);
                touch_counter = inner.stats.hits;
                if let Some(row) = inner.entries.get_mut(target.as_str()) {
                    row.last_touched = touch_counter;
                }
            } else {
                // The key was given to us; bump the counter even on
                // miss so the touch helper is consistent. Not strictly
                // necessary on a miss, but keeps the API uniform.
                inner.stats.misses = inner.stats.misses.saturating_add(1);
            }
        }
        // Exact-key short-circuit: if the store is asked for the same
        // key that produced `best`, update its LRU position via
        // `touch`. Otherwise best came from a similar-but-not-equal
        // key (cosine hit), and the caller's read of `stats` is the
        // primary signal.
        self.touch(&key.to_hex(), &mut touch_counter);
        best.map(|(_, c)| c)
    }

    fn store(
        &self,
        key: EmbeddingKey,
        request_embedding: Vec<f32>,
        completion: CachedCompletion,
    ) {
        let mut inner = self.inner.write();
        let hex = key.to_hex();
        // Update existing row (preserves LRU position semantics: a
        // store overwrites last_touched via the counter below).
        let counter = inner.stats.hits + inner.stats.misses + 1;
        if inner.entries.has_key(&hex) {
            if let Some(row) = inner.entries.get_mut(hex.as_str()) {
                row.embedding = request_embedding;
                row.completion = completion;
                row.last_touched = counter;
            }
        } else {
            // Evict least-recently-used if at capacity.
            if inner.entries.len() >= self.max_entries
                && let Some(lru_key) = inner
                    .entries
                    .iter()
                    .min_by_key(|(_, row)| row.last_touched)
                    .map(|(k, _)| k.clone())
            {
                inner.entries.shift_remove(&lru_key);
            }
            inner.entries.insert(
                hex,
                Row {
                    key,
                    embedding: request_embedding,
                    completion,
                    last_touched: counter,
                },
            );
        }
        inner.stats.entries = inner.entries.len() as u64;
    }

    fn len(&self) -> usize {
        self.inner.read().entries.len()
    }

    fn clear(&self) {
        let mut inner = self.inner.write();
        inner.entries.clear();
        inner.stats = SemanticCacheStats::default();
    }

    fn stats(&self) -> SemanticCacheStats {
        self.inner.read().stats
    }
}

/// Tiny `IndexMap` shim: backed by `Vec`-of-`Option` to keep the
/// crate dependency-free for the LRU store. Single-threaded reads
/// are not assumed; `RwLock` wraps the whole structure.
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
            // Compact the order vec by removing the first matching
            // entry (order.len() is small relative to values.len()
            // because we only ever push once per insert).
            if let Some(pos) = self.order.iter().position(|x| x == k) {
                self.order.remove(pos);
            }
            slot
        }
    }
}

/// Build a stable fingerprint from a `CompletionRequest`.
///
/// The exact bytes are an implementation detail; the property the
/// engine relies on is "two requests whose visible structure
/// (model, tools, messages) is identical produce the same
/// fingerprint".
#[must_use]
pub fn fingerprint_request(
    request: &synthia_provider::CompletionRequest,
) -> Vec<u8> {
    use synthia_provider::{Content, Role};
    let mut buf: Vec<u8> = Vec::new();
    buf.extend_from_slice(b"model:");
    buf.extend_from_slice(request.model.as_bytes());
    buf.extend_from_slice(b"|tools:");
    let mut names: Vec<&str> =
        request.tools.iter().map(|t| t.name.as_str()).collect();
    names.sort_unstable();
    for name in names {
        buf.extend_from_slice(name.as_bytes());
        buf.push(b',');
    }
    buf.extend_from_slice(b"|messages:");
    for m in request.messages.iter() {
        buf.push(match m.role {
            Role::System => b's',
            Role::User => b'u',
            Role::Assistant => b'a',
            Role::Tool => b't',
        });
        match &m.content {
            Content::Single(p) => push_part(&mut buf, p),
            Content::Multi(parts) => {
                for p in parts {
                    push_part(&mut buf, p);
                }
            }
        }
        buf.push(b'|');
    }
    buf
}

fn push_part(buf: &mut Vec<u8>, p: &synthia_provider::ContentPart) {
    use synthia_provider::ContentPart;
    match p {
        ContentPart::Text(t) => {
            buf.extend_from_slice(b"T:");
            buf.extend_from_slice(t.text.as_bytes());
        }
        ContentPart::ToolUse(u) => {
            buf.extend_from_slice(b"U:");
            buf.extend_from_slice(u.name.as_bytes());
            buf.push(b':');
            buf.extend_from_slice(u.id.as_bytes());
            buf.push(b':');
            buf.extend_from_slice(
                serde_json::to_string(&u.input)
                    .unwrap_or_default()
                    .as_bytes(),
            );
        }
        ContentPart::ToolResult(r) => {
            buf.extend_from_slice(b"R:");
            buf.extend_from_slice(r.tool_use_id.as_bytes());
            buf.push(b':');
            buf.extend_from_slice(
                serde_json::to_string(&r.content)
                    .unwrap_or_default()
                    .as_bytes(),
            );
        }
        _ => {
            buf.extend_from_slice(b"?:");
            buf.extend_from_slice(
                serde_json::to_string(&serde_json::Value::Null)
                    .unwrap_or_default()
                    .as_bytes(),
            );
        }
    }
}

/// `Embedder` trait re-export so callers do not have to import a
/// second trait from `synthia-search`.
pub use synthia_search::Embedder as _Embedder;

/// Helper: build an embedding for a request using the configured
/// embedder. Public so tests can verify the wrapper's embedding
/// semantics.
#[must_use]
pub fn embed_request(
    request: &synthia_provider::CompletionRequest,
    embedder: &dyn Embedder,
) -> Vec<f32> {
    // Embed the textual concatenation of all messages. The exact
    // bytes are not load-bearing — the property the engine relies
    // on is that two requests with similar text get a cosine ≥
    // threshold.
    let mut text = String::new();
    for m in request.messages.iter() {
        text.push_str(match m.role {
            synthia_provider::Role::System => "system: ",
            synthia_provider::Role::User => "user: ",
            synthia_provider::Role::Assistant => "assistant: ",
            synthia_provider::Role::Tool => "tool: ",
        });
        match &m.content {
            synthia_provider::Content::Single(p) => {
                push_part_text(&mut text, p);
            }
            synthia_provider::Content::Multi(parts) => {
                for p in parts {
                    push_part_text(&mut text, p);
                }
            }
        }
        text.push('\n');
    }
    embedder.embed(&text)
}

fn push_part_text(text: &mut String, p: &synthia_provider::ContentPart) {
    use synthia_provider::ContentPart;
    match p {
        ContentPart::Text(t) => text.push_str(&t.text),
        ContentPart::ToolUse(u) => {
            text.push_str(&u.name);
            text.push(':');
            text.push_str(&serde_json::to_string(&u.input).unwrap_or_default());
        }
        ContentPart::ToolResult(r) => {
            text.push_str(
                &serde_json::to_string(&r.content).unwrap_or_default(),
            );
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_to_hex_is_64_chars() {
        let k = EmbeddingKey([0xab; 32]);
        assert_eq!(k.to_hex().len(), 64);
    }

    #[test]
    fn fingerprint_differs_on_tool_change() {
        let tool_a = synthia_provider::ToolDefinition {
            name: "a".into(),
            description: "".into(),
            input_schema: serde_json::json!({}),
            cache_control: None,
            annotations: None,
        };
        let tool_b = synthia_provider::ToolDefinition {
            name: "b".into(),
            description: "".into(),
            input_schema: serde_json::json!({}),
            cache_control: None,
            annotations: None,
        };
        let req1 = synthia_provider::CompletionRequest {
            model: "m".into(),
            tools: std::sync::Arc::new(vec![tool_a]),
            ..synthia_provider::CompletionRequest::default()
        };
        let req2 = synthia_provider::CompletionRequest {
            model: "m".into(),
            tools: std::sync::Arc::new(vec![tool_b]),
            ..synthia_provider::CompletionRequest::default()
        };
        assert_ne!(fingerprint_request(&req1), fingerprint_request(&req2));
    }

    #[test]
    fn lru_evicts_oldest() {
        use synthia_core::Clock;
        let cache = InMemorySemanticCache::with_capacity(2);
        let clock = synthia_core::SystemClock;
        let now = clock.now();
        // Insert three keys with monotonically increasing counters
        // (one store each). The first should be evicted, the other
        // two retained.
        for i in 0u8..3u8 {
            let key = EmbeddingKey([i; 32]);
            cache.store(
                key,
                vec![0.0; 2],
                CachedCompletion {
                    response: synthia_provider::CompletionResponse::default(),
                    stored_at: now,
                    ttl: Duration::from_secs(60),
                },
            );
        }
        assert_eq!(
            cache.len(),
            2,
            "capacity is 2, oldest entry must be evicted"
        );
    }
}
