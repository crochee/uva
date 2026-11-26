//! Model-order scheduling: the batch runs in the order the model
//! wrote it, and an exclusive call is a barrier between the shared
//! calls around it.
//!
//! The fixtures gate every call on a channel the test opens, so the
//! order is observed rather than inferred from a timer. The harness
//! runs a call on a runtime-free thread (the default spawner), so a
//! fixture that slept on a tokio timer would panic there.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use async_trait::async_trait;
use futures::channel::oneshot;
use parking_lot::Mutex;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uva_tool::{Context, ToolOutput, ToolRegistry};

use super::{support::*, *};

// The log every fixture tool appends to: `name:start` when it
// begins, `name:end` when it finishes.
type Log = Arc<Mutex<Vec<String>>>;

// How long a test waits for the run to reach the next step before
// it calls the order wrong.
const WAIT: Duration = Duration::from_secs(5);

// A tool that records when it starts and when it finishes, and waits
// for the test to open its gate before it finishes.
struct GatedTool {
    name: String,
    mode: uva_tool::traits::ExecutionMode,
    log: Log,
    gate: Mutex<Option<oneshot::Receiver<()>>>,
}

#[async_trait]
impl uva_tool::Tool for GatedTool {
    fn name(&self) -> &str {
        &self.name
    }

    fn description(&self) -> &str {
        "records its own start and end"
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object"})
    }

    fn mode(&self) -> uva_tool::traits::ExecutionMode {
        self.mode
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        self.log.lock().push(format!("{}:start", self.name));
        let gate = self.gate.lock().take();
        if let Some(gate) = gate {
            // The gate is the test's hold on this call: the call
            // waits here until the test opens it.
            let _ = gate.await;
        }
        self.log.lock().push(format!("{}:end", self.name));
        ToolOutput::text(self.name.clone())
    }
}

// A gated tool plus the gate the test opens.
fn gated(
    name: &str,
    mode: uva_tool::traits::ExecutionMode,
    log: &Log,
) -> (Arc<GatedTool>, oneshot::Sender<()>) {
    let (tx, rx) = oneshot::channel();
    let tool = Arc::new(GatedTool {
        name: name.to_string(),
        mode,
        log: log.clone(),
        gate: Mutex::new(Some(rx)),
    });
    (tool, tx)
}

fn call(id: &str, name: &str) -> ToolUse {
    ToolUse {
        id: id.to_string(),
        name: name.to_string(),
        input: json!({}),
    }
}

// The lines the log holds.
fn lines(log: &Log) -> Vec<String> {
    log.lock().clone()
}

// Wait until the log holds `line`; fail the test when it never does.
fn wait_for(log: &Log, line: &str) {
    let deadline = Instant::now() + WAIT;
    while Instant::now() < deadline {
        if lines(log).iter().any(|seen| seen == line) {
            return;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    panic!(
        "the run never reached {line:?}; the log holds {:?}",
        lines(log)
    );
}

// The log must not hold `line` yet.
fn assert_not_yet(log: &Log, line: &str) {
    assert!(
        !lines(log).iter().any(|seen| seen == line),
        "{line:?} ran before the call in front of it; the log holds {:?}",
        lines(log)
    );
}

// The batch runs in the model's order: the exclusive call sits
// between the shared calls the model wrote around it.
//
// The shape the bucket scheduler had — every shared call first, the
// exclusive ones after — would run `shared2` before `exclusive`.
#[tokio::test]
async fn the_batch_runs_in_the_models_order() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let (shared1, open1) =
        gated("shared1", uva_tool::traits::ExecutionMode::Parallel, &log);
    let (exclusive, open_exclusive) = gated(
        "exclusive",
        uva_tool::traits::ExecutionMode::Sequential,
        &log,
    );
    let (shared2, open2) =
        gated("shared2", uva_tool::traits::ExecutionMode::Parallel, &log);
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![
            call("s1", "shared1"),
            call("x", "exclusive"),
            call("s2", "shared2"),
        ]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(shared1));
    registry.register_entry(uva_tool::ToolEntry::new(exclusive));
    registry.register_entry(uva_tool::ToolEntry::new(shared2));

    let agent = ReActAgent::new(provider, registry);
    let mut stream = agent
        .run(AgentInput::text("m"), Arc::new(CancellationToken::new()))
        .await;

    wait_for(&log, "shared1:start");
    assert_not_yet(&log, "shared2:start");
    open1.send(()).ok();
    wait_for(&log, "shared1:end");

    wait_for(&log, "exclusive:start");
    assert_not_yet(&log, "shared2:start");
    open_exclusive.send(()).ok();
    wait_for(&log, "exclusive:end");

    wait_for(&log, "shared2:start");
    open2.send(()).ok();
    wait_for(&log, "shared2:end");

    while futures::StreamExt::next(&mut stream).await.is_some() {}
}

// An exclusive call is a barrier: the shared call behind it starts
// only after the exclusive call has finished.
#[tokio::test]
async fn an_exclusive_call_is_a_barrier_for_the_shared_calls_around_it() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let (exclusive, open_exclusive) = gated(
        "exclusive",
        uva_tool::traits::ExecutionMode::Sequential,
        &log,
    );
    let (shared, open_shared) =
        gated("shared", uva_tool::traits::ExecutionMode::Parallel, &log);
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![call("x", "exclusive"), call("s", "shared")]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(exclusive));
    registry.register_entry(uva_tool::ToolEntry::new(shared));

    let agent = ReActAgent::new(provider, registry);
    let mut stream = agent
        .run(AgentInput::text("m"), Arc::new(CancellationToken::new()))
        .await;

    wait_for(&log, "exclusive:start");
    assert_not_yet(&log, "shared:start");
    open_exclusive.send(()).ok();
    wait_for(&log, "exclusive:end");

    wait_for(&log, "shared:start");
    open_shared.send(()).ok();
    wait_for(&log, "shared:end");

    while futures::StreamExt::next(&mut stream).await.is_some() {}
}

// The shared calls of one wave still overlap: the order is the
// model's, not a serialisation of the batch.
//
// The second shared call only starts while the first is still held
// at its gate — so the two were in flight together.
#[tokio::test]
async fn the_shared_calls_of_one_wave_still_overlap() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let (shared1, open1) =
        gated("shared1", uva_tool::traits::ExecutionMode::Parallel, &log);
    let (shared2, open2) =
        gated("shared2", uva_tool::traits::ExecutionMode::Parallel, &log);
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![call("s1", "shared1"), call("s2", "shared2")]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(shared1));
    registry.register_entry(uva_tool::ToolEntry::new(shared2));

    let agent = ReActAgent::new(provider, registry);
    let mut stream = agent
        .run(AgentInput::text("m"), Arc::new(CancellationToken::new()))
        .await;

    wait_for(&log, "shared1:start");
    // The second call starts while the first is still held: the wave
    // overlaps.
    wait_for(&log, "shared2:start");
    open1.send(()).ok();
    open2.send(()).ok();
    wait_for(&log, "shared1:end");
    wait_for(&log, "shared2:end");

    while futures::StreamExt::next(&mut stream).await.is_some() {}
}
