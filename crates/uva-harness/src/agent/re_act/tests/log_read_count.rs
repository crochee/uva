//! The run reads the session log once per run: the setup's repair and
//! fold read it between them, and no step boundary reads it again —
//! the transcript the run holds in memory is the only source a step
//! consults.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::{StreamExt, future::BoxFuture};
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use uva_core::{CancelToken, SystemClock};
use uva_provider::{Message, StreamChunk, ToolUse};
use uva_session::{
    CompactionError,
    CompactionOutcome,
    CompactionPlan,
    Compactor,
    LogWriter,
    SessionError,
    SessionSink,
    SessionSnapshot,
    TokenBudget,
    in_memory::InMemorySessionSink,
};

use super::support::*;
use crate::{
    agent::{Agent, re_act::ReActAgent},
    input::AgentInput,
};

// The run this test measures: three iterations, each one a tool
// call, so every boundary is crossed.
const ITERATIONS: usize = 3;

// A sink that counts the whole-log reads. Every other method is the
// in-memory sink's, so the run's rows land where a reader sees them.
struct CountingSink {
    inner: InMemorySessionSink,
    reads: AtomicUsize,
}

impl CountingSink {
    fn new(id: &str) -> Self {
        Self {
            inner: InMemorySessionSink::new(id),
            reads: AtomicUsize::new(0),
        }
    }

    fn read_count(&self) -> usize {
        self.reads.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl SessionSink for CountingSink {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn alloc_seq(&self) -> u64 {
        self.inner.alloc_seq()
    }

    async fn append(&self, event: &Value) -> Result<(), SessionError> {
        self.inner.append(event).await
    }

    async fn read(&self) -> Result<Vec<Value>, SessionError> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.inner.read().await
    }

    async fn read_from(&self, from: u64) -> Result<Vec<Value>, SessionError> {
        self.inner.read_from(from).await
    }

    async fn snapshot(&self) -> Result<SessionSnapshot, SessionError> {
        self.inner.snapshot().await
    }

    async fn close(
        &self,
        reason: uva_session::SessionEndReason,
    ) -> Result<(), SessionError> {
        self.inner.close(reason).await
    }
}

// A compactor that never plans: it makes the loop plan a compaction
// pass over the log — and decline — without writing a replace row.
struct NeverCompactor;

impl Compactor for NeverCompactor {
    fn plan(
        &self,
        _history: &[Message],
        _budget: TokenBudget,
    ) -> Option<CompactionPlan> {
        None
    }

    fn summarize(
        &self,
        _plan: &CompactionPlan,
        _cancel: Arc<dyn CancelToken>,
    ) -> BoxFuture<'static, Result<CompactionOutcome, CompactionError>> {
        Box::pin(async { Err(CompactionError::EmptySummary) })
    }
}

// The run reads the log twice, both in setup: once inside `repair`
// (to find an interrupted tail) and once for the fold that seeds the
// transcript. A step boundary reads nothing, so the count does not
// grow with the number of iterations.
#[tokio::test]
async fn log_reads_once_per_run() {
    let sink = Arc::new(CountingSink::new("read-count"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let scripted: Vec<Vec<StreamChunk>> = (0..ITERATIONS)
        .map(|index| {
            tool_call_response(vec![ToolUse {
                id: format!("c{index}"),
                name: "echo".to_string(),
                input: serde_json::json!({}),
            }])
        })
        .collect();
    let provider = Arc::new(ScriptedStreamProvider::new(scripted));
    let (registry, _executions) = echo_registry();

    let agent = ReActAgent::new(provider, registry)
        .with_log_writer(writer)
        .with_compactor(Arc::new(NeverCompactor))
        .with_max_iterations(ITERATIONS);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let reads = sink.read_count();
    assert_eq!(
        reads, 2,
        "setup's repair and fold are the only reads: {reads} reads for \
         {ITERATIONS} iterations"
    );
    assert!(
        reads < ITERATIONS,
        "a step boundary must never read the log: {reads} reads for \
         {ITERATIONS} iterations"
    );
}
