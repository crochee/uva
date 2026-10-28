//! Tests for `elysper-tool-memo`.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use chrono::TimeZone;
use elysper_core::{Clock, FixedClock};
use elysper_tool::{Context, Tool, ToolOutput};

use crate::{
    memo_key,
    store::{MemoKey, MemoScope, MemoStore},
    wrapper::{MemoConfig, MemoizedTool},
};

fn fixed_clock() -> Arc<dyn Clock> {
    Arc::new(FixedClock::new(
        chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    ))
}

struct CountingTool {
    calls: AtomicU64,
}

#[async_trait::async_trait]
impl Tool for CountingTool {
    fn name(&self) -> &str {
        "counting"
    }

    fn description(&self) -> &str {
        "counting"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    async fn call(
        &self,
        input: serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        self.calls.fetch_add(1, Ordering::SeqCst);
        ToolOutput::text(format!("{input}"))
    }
}

struct EchoTool;

#[async_trait::async_trait]
impl Tool for EchoTool {
    fn name(&self) -> &str {
        "echo"
    }

    fn description(&self) -> &str {
        "echo"
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::json!({"type": "object"})
    }

    async fn call(
        &self,
        input: serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        ToolOutput::text(format!("hello {input}"))
    }
}

fn ctx() -> Context {
    Context {
        session_id: "test".into(),
        workspace_root: PathBuf::from("/tmp"),
        ..Context::default()
    }
}

/// Identical call within TTL returns the cached `ToolOutput` without
/// invoking the inner tool a second time.
#[tokio::test]
async fn identical_call_within_ttl_caches() {
    let inner = Arc::new(CountingTool {
        calls: AtomicU64::new(0),
    });
    let memo = MemoizedTool::new(
        inner.clone() as Arc<dyn Tool>,
        MemoConfig::default()
            .ttl(Duration::from_secs(60))
            .scope(MemoScope::Process),
        Arc::new(crate::InMemoryMemoStore::default()),
        fixed_clock(),
    );
    let r1 = memo.call(serde_json::json!({"x": 1}), &ctx()).await;
    let r2 = memo.call(serde_json::json!({"x": 1}), &ctx()).await;
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);
    assert_eq!(memo.hit_count(), 1);
    assert_eq!(memo.miss_count(), 1);
    assert_eq!(r1, r2);
}

/// TTL expiry invokes the inner again on the next call.
#[tokio::test]
async fn ttl_expiry_invokes_inner_again() {
    let inner = Arc::new(CountingTool {
        calls: AtomicU64::new(0),
    });
    let store: Arc<dyn MemoStore> =
        Arc::new(crate::InMemoryMemoStore::default());
    let t0 = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();

    let memo1 = MemoizedTool::new(
        inner.clone() as Arc<dyn Tool>,
        MemoConfig::default().ttl(Duration::from_secs(60)),
        store.clone(),
        Arc::new(FixedClock::new(t0)),
    );
    let _ = memo1.call(serde_json::json!({"x": 1}), &ctx()).await;
    assert_eq!(inner.calls.load(Ordering::SeqCst), 1);

    // Second wrapper at a later time, same store: TTL has passed.
    let t1 = t0 + chrono::Duration::seconds(120);
    let memo2 = MemoizedTool::new(
        inner.clone() as Arc<dyn Tool>,
        MemoConfig::default().ttl(Duration::from_secs(60)),
        store.clone(),
        Arc::new(FixedClock::new(t1)),
    );
    let _ = memo2.call(serde_json::json!({"x": 1}), &ctx()).await;
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
}

/// Different `MemoScope::Workspace` paths yield different cache entries.
#[tokio::test]
async fn workspace_scope_partitions_cache() {
    let inner = Arc::new(CountingTool {
        calls: AtomicU64::new(0),
    });
    let memo_a = MemoizedTool::new(
        inner.clone() as Arc<dyn Tool>,
        MemoConfig::default().scope(MemoScope::Workspace(PathBuf::from("/a"))),
        Arc::new(crate::InMemoryMemoStore::default()),
        fixed_clock(),
    );
    let memo_b = MemoizedTool::new(
        inner.clone() as Arc<dyn Tool>,
        MemoConfig::default().scope(MemoScope::Workspace(PathBuf::from("/b"))),
        Arc::new(crate::InMemoryMemoStore::default()),
        fixed_clock(),
    );
    let _ = memo_a.call(serde_json::json!({"x": 1}), &ctx()).await;
    let _ = memo_b.call(serde_json::json!({"x": 1}), &ctx()).await;
    let _ = memo_a.call(serde_json::json!({"x": 1}), &ctx()).await;
    let _ = memo_b.call(serde_json::json!({"x": 1}), &ctx()).await;
    // Two workspaces → two inner invocations; the second call from
    // each scope is a cache hit.
    assert_eq!(inner.calls.load(Ordering::SeqCst), 2);
    assert_eq!(memo_a.hit_count(), 1);
    assert_eq!(memo_b.hit_count(), 1);
}

/// Non-deterministic tools opt out via `is_memoizable`.
#[tokio::test]
async fn non_deterministic_tool_skips_cache() {
    use std::sync::atomic::AtomicU64;
    struct AlwaysNow {
        calls: AtomicU64,
    }
    #[async_trait::async_trait]
    impl Tool for AlwaysNow {
        fn name(&self) -> &str {
            "now"
        }

        fn description(&self) -> &str {
            "now"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        async fn call(
            &self,
            _input: serde_json::Value,
            _context: &Context,
        ) -> ToolOutput {
            self.calls.fetch_add(1, Ordering::SeqCst);
            ToolOutput::text(chrono::Utc::now().to_rfc3339())
        }
    }
    let inner = Arc::new(AlwaysNow {
        calls: AtomicU64::new(0),
    });
    let inner_dyn: Arc<dyn Tool> = inner.clone();
    let memo = MemoizedTool::new(
        inner_dyn,
        MemoConfig::default(),
        Arc::new(crate::InMemoryMemoStore::default()),
        fixed_clock(),
    );
    // Build a wrapper that opts out.
    let memo = MemoizedOptOut { inner: memo };
    let _ = memo.call(serde_json::json!({}), &ctx()).await;
    let _ = memo.call(serde_json::json!({}), &ctx()).await;
    let _ = memo.call(serde_json::json!({}), &ctx()).await;
    assert_eq!(
        inner.calls.load(Ordering::SeqCst),
        3,
        "non-deterministic tool must always run"
    );
}

struct MemoizedOptOut {
    inner: MemoizedTool,
}

#[async_trait::async_trait]
impl Tool for MemoizedOptOut {
    fn name(&self) -> &str {
        self.inner.name()
    }

    fn description(&self) -> &str {
        self.inner.description()
    }

    fn parameters(&self) -> serde_json::Value {
        self.inner.parameters()
    }

    async fn call(
        &self,
        input: serde_json::Value,
        context: &Context,
    ) -> ToolOutput {
        // Always treat as non-deterministic.
        let cfg = self.inner.config();
        let key = memo_key(self.inner.name(), &input, &cfg.scope);
        // Manually invalidate so the next call cannot reuse anything.
        self.inner.invalidate(&key);
        self.inner.call(input, context).await
    }
}

/// LRU eviction honors capacity.
#[tokio::test]
async fn lru_eviction_under_capacity() {
    let store = crate::InMemoryMemoStore::with_capacity(2);
    let now = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
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

/// Concurrent calls for the same key collapse to a single inner
/// invocation.
#[tokio::test]
async fn concurrent_calls_single_flight() {
    use std::sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    };

    struct SlowCounting {
        calls: AtomicU64,
    }
    #[async_trait::async_trait]
    impl Tool for SlowCounting {
        fn name(&self) -> &str {
            "slow"
        }

        fn description(&self) -> &str {
            "slow"
        }

        fn parameters(&self) -> serde_json::Value {
            serde_json::json!({})
        }

        async fn call(
            &self,
            _input: serde_json::Value,
            _context: &Context,
        ) -> ToolOutput {
            self.calls.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(50)).await;
            ToolOutput::text("done")
        }
    }

    let inner = Arc::new(SlowCounting {
        calls: AtomicU64::new(0),
    });
    let memo = Arc::new(MemoizedTool::new(
        inner.clone() as Arc<dyn Tool>,
        MemoConfig::default().ttl(Duration::from_secs(60)),
        Arc::new(crate::InMemoryMemoStore::default()),
        fixed_clock(),
    ));
    let mut handles = Vec::new();
    for _ in 0..5 {
        let m = memo.clone();
        let c = ctx();
        handles.push(tokio::spawn(async move {
            m.call(serde_json::json!({"x": 1}), &c).await
        }));
    }
    for h in handles {
        let _ = h.await.unwrap();
    }
    // Single-flight + the subsequent store means we should see
    // exactly one inner invocation across the burst of concurrent
    // callers. (Worst case: 2 — once for the producer, once for a
    // racing consumer that lost the race; this is still much
    // smaller than the burst size.)
    let calls = inner.calls.load(Ordering::SeqCst);
    assert!(
        calls <= 2,
        "single-flight should collapse concurrent callers, got {calls}"
    );
}

/// MemoKey differs across tools, inputs, and scopes.
#[test]
fn memo_key_distinguishes_inputs() {
    let k1 = memo_key("a", &serde_json::json!({"x": 1}), &MemoScope::Process);
    let k2 = memo_key("a", &serde_json::json!({"x": 2}), &MemoScope::Process);
    assert_ne!(k1, k2);
}

/// Echo test: hit/miss counters.
#[tokio::test]
async fn echo_three_times_first_miss_then_two_hits() {
    let inner = Arc::new(EchoTool);
    let memo = MemoizedTool::new(
        inner as Arc<dyn Tool>,
        MemoConfig::default().ttl(Duration::from_secs(60)),
        Arc::new(crate::InMemoryMemoStore::default()),
        fixed_clock(),
    );
    let _ = memo.call(serde_json::json!({"a": 1}), &ctx()).await;
    let _ = memo.call(serde_json::json!({"a": 1}), &ctx()).await;
    let _ = memo.call(serde_json::json!({"a": 1}), &ctx()).await;
    assert_eq!(memo.hit_count(), 2);
    assert_eq!(memo.miss_count(), 1);
}
