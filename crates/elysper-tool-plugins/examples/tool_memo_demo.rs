//! Tool-memo demo.
//!
//! A trivial `Echo` tool returns `"hello {input}"` and increments a
//! counter on every `call`. The `MemoizedTool` wrapper caches the
//! first call's result and serves subsequent identical invocations
//! from cache.
//!
//! Run with:
//! `cargo run --example tool_memo_demo -p elysper-tool-memo`

use std::{
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
    time::Duration,
};

use chrono::TimeZone;
use elysper_core::{Clock, FixedClock};
use elysper_tool::{Context, Tool, ToolOutput};
use elysper_tool_plugins::memo::{
    InMemoryMemoStore,
    MemoConfig,
    MemoScope,
    MemoizedTool,
};

struct Echo {
    calls: AtomicU64,
}

#[async_trait::async_trait]
impl Tool for Echo {
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
        self.calls.fetch_add(1, Ordering::SeqCst);
        ToolOutput::text(format!("hello {input}"))
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let inner = Arc::new(Echo {
        calls: AtomicU64::new(0),
    });
    let t0 = chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap();
    let clock: Arc<dyn Clock> = Arc::new(FixedClock::new(t0));
    let memo = MemoizedTool::new(
        inner.clone(),
        MemoConfig::default()
            .max_entries(64)
            .ttl(Duration::from_secs(60))
            .scope(MemoScope::Process),
        Arc::new(InMemoryMemoStore::default()),
        clock,
    );

    let ctx = Context {
        session_id: "demo".into(),
        workspace_root: std::path::PathBuf::from("/tmp"),
        ..Context::default()
    };
    // 5 identical invocations → 1 inner call + 4 cache hits.
    for i in 0..5 {
        let r = memo.call(serde_json::json!({"prompt": "hi"}), &ctx).await;
        println!("[#{i}] {r:?}");
    }

    let calls = inner.calls.load(Ordering::SeqCst);
    let hits = memo.hit_count();
    let misses = memo.miss_count();
    println!("STATS: inner_invocations={calls} hits={hits} misses={misses}");
    assert_eq!(calls, 1, "5 identical calls must collapse to 1 inner");
    assert_eq!(hits, 4);
    assert_eq!(misses, 1);
    println!("OK: 1 invocation, 4 cache-hits");
    Ok(())
}
