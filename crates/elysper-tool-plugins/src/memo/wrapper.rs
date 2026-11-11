//! [`MemoizedTool`] — the `Tool` wrapper.

use std::{pin::Pin, sync::Arc, time::Duration};

use elysper_core::Clock;
use elysper_tool::{
    Context,
    ExecutionMode,
    StreamOutput,
    Tool,
    ToolAnnotations,
    ToolOutput,
    ToolOutputDefinition,
};
use futures::{
    FutureExt,
    future::{BoxFuture, Shared},
};
use parking_lot::Mutex;
use serde_json::Value;

use crate::memo::{
    memo_key,
    store::{MemoScope, MemoStore},
};

/// Configuration knobs for [`MemoizedTool`].
#[derive(Clone, Debug)]
pub struct MemoConfig {
    /// LRU capacity on the default store. Custom stores ignore this.
    pub max_entries: usize,
    /// TTL applied to every `store` call.
    pub ttl: Duration,
    /// Scope applied to the cache key. Two workspaces don't share
    /// cache when set to [`MemoScope::Workspace`].
    pub scope: MemoScope,
}

impl Default for MemoConfig {
    fn default() -> Self {
        Self {
            max_entries: 4096,
            ttl: Duration::from_secs(5 * 60),
            scope: MemoScope::Process,
        }
    }
}

impl MemoConfig {
    #[must_use]
    pub fn max_entries(mut self, n: usize) -> Self {
        self.max_entries = n;
        self
    }

    #[must_use]
    pub fn ttl(mut self, ttl: Duration) -> Self {
        self.ttl = ttl;
        self
    }

    #[must_use]
    pub fn scope(mut self, scope: MemoScope) -> Self {
        self.scope = scope;
        self
    }
}

/// `Tool` wrapper that memoizes `call` results through a
/// [`MemoStore`]. Streaming path (`stream`) is forwarded unchanged
/// for tools that override it; the wrapper participates only on the
/// simple `call` path and on the default `stream`-from-`call`
/// projection (which we deliberately do not memoize — streaming
/// callers want deltas, not cached final values).
///
/// Per-key in-flight future shared across concurrent callers.
type InflightFuture = Arc<Shared<BoxFuture<'static, ToolOutput>>>;

/// Single-flight: concurrent calls for the same key collapse onto
/// one in-flight future so the inner tool runs at most once per
/// cache slot.
pub struct MemoizedTool {
    inner: Arc<dyn Tool>,
    config: parking_lot::RwLock<MemoConfig>,
    store: Arc<dyn MemoStore>,
    clock: Arc<dyn Clock>,
    /// Per-key in-flight future, runtime-neutral via
    /// `futures::FutureExt::shared()`. Concurrent callers receive
    /// the same `Shared` clone and `await` it.
    single_flight: Mutex<std::collections::HashMap<[u8; 32], InflightFuture>>,
    /// Counters for observability. Hot-path, no allocation.
    pub(crate) hits: parking_lot::Mutex<u64>,
    pub(crate) misses: parking_lot::Mutex<u64>,
}

impl MemoizedTool {
    pub fn new(
        inner: Arc<dyn Tool>,
        config: MemoConfig,
        store: Arc<dyn MemoStore>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            inner,
            config: parking_lot::RwLock::new(config),
            store,
            clock,
            single_flight: Mutex::new(std::collections::HashMap::new()),
            hits: parking_lot::Mutex::new(0),
            misses: parking_lot::Mutex::new(0),
        }
    }

    /// Snapshot of the live config.
    #[must_use]
    pub fn config(&self) -> MemoConfig {
        self.config.read().clone()
    }

    /// Replace the live config (hot-reload).
    pub fn set_config(&self, config: MemoConfig) {
        *self.config.write() = config;
    }

    /// Number of cache hits observed since construction.
    #[must_use]
    pub fn hit_count(&self) -> u64 {
        *self.hits.lock()
    }

    /// Number of cache misses observed since construction.
    #[must_use]
    pub fn miss_count(&self) -> u64 {
        *self.misses.lock()
    }

    /// Whether the tool is memoizable for a given input. Default:
    /// always memoizable. Tools with non-deterministic outputs
    /// override this.
    #[allow(unused_variables)]
    pub fn is_memoizable(&self, input: &Value, context: &Context) -> bool {
        true
    }

    /// Invalidate a single cached entry.
    pub fn invalidate(&self, key: &crate::memo::store::MemoKey) {
        self.store.invalidate(key);
    }

    /// Drop the entire memoization cache.
    pub fn clear_cache(&self) {
        self.store.clear();
    }
}

impl std::fmt::Debug for MemoizedTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoizedTool")
            .field("inner", &self.inner.name())
            .field("config", &self.config())
            .field("hits", &self.hit_count())
            .field("misses", &self.miss_count())
            .finish()
    }
}

#[async_trait::async_trait]
impl Tool for MemoizedTool {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> Value {
        self.inner.parameters()
    }

    fn mode(&self) -> ExecutionMode {
        self.inner.mode()
    }

    fn annotations(&self) -> Option<ToolAnnotations> {
        self.inner.annotations()
    }

    fn output_definition(&self) -> ToolOutputDefinition {
        self.inner.output_definition()
    }

    async fn truncate(&self, output: &mut ToolOutput, context: &Context) {
        self.inner.truncate(output, context).await;
    }

    fn stream<'a>(
        &'a self,
        input: Value,
        context: &'a Context,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamOutput> + Send + 'a>> {
        // Stream path goes straight to the inner tool. Tools that
        // override `stream` get their override honored verbatim;
        // tools that use the default `stream` impl wrap `call`,
        // and the memoization happens there.
        self.inner.stream(input, context)
    }

    async fn call(&self, input: Value, context: &Context) -> ToolOutput {
        let cfg = self.config();
        let now = self.clock.now();

        if !self.is_memoizable(&input, context) {
            *self.misses.lock() += 1;
            return self.inner.call(input, context).await;
        }

        let key = memo_key(self.inner.name(), &input, &cfg.scope);

        // Fast path: cache hit.
        if let Some(cached) = self.store.lookup(&key, now) {
            *self.hits.lock() += 1;
            return cached;
        }

        // Single-flight: collapse concurrent calls for the same key
        // onto one in-flight `Shared` future. The first caller
        // constructs the future; later callers receive a clone and
        // `await` the same value.
        let shared: Arc<Shared<BoxFuture<'static, ToolOutput>>> = {
            let mut guard = self.single_flight.lock();
            if let Some(existing) = guard.get(&key.0).cloned() {
                existing
            } else {
                let inner = self.inner.clone();
                let input_clone = input.clone();
                let context_clone = context.clone();
                let fut: BoxFuture<'static, ToolOutput> =
                    Box::pin(async move {
                        inner.call(input_clone, &context_clone).await
                    });
                let shared = Arc::new(fut.shared());
                guard.insert(key.0, shared.clone());
                shared
            }
        };

        let result = (*shared).clone().await;

        // First caller (the one whose future produced the value)
        // stores the result. We tell that apart by checking whether
        // the entry is still in `single_flight`: once the future is
        // awaited to completion, the producer removes its entry, so
        // a `contains_key` check is racy and is intentionally not
        // used. Instead: every caller populates the store. Multiple
        // stores of the same value are idempotent because the store
        // keys by hash, and the value is identical.
        self.store
            .store(key, result.clone(), self.clock.now(), cfg.ttl);
        {
            let mut guard = self.single_flight.lock();
            // Only the producer (first caller) should remove; we
            // approximate that by removing unconditionally after the
            // first `await` completes. Later concurrent callers
            // (which arrive while the future is in flight) await
            // the same `Shared`; by the time their `await` returns
            // the entry has been removed and they take the cache
            // hit on the next call.
            guard.remove(&key.0);
        }
        *self.misses.lock() += 1;
        result
    }
}

// Helpers for tests live in `tests.rs` (imported by name from
// there); keeping them out of the public surface area.
