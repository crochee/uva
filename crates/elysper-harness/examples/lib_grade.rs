//! # lib-grade starter — five lines, three paradigms
//!
//! Run it:
//!
//! ```text
//! cargo run --example lib_grade -p elysper-harness
//! ```
//!
//! A one-file tour that exercises the harness's lib-grade
//! shape: compose a [`ReActAgent`] from independent pieces,
//! register a consumer-side
//! [`StrategyRegistry`](elysper_harness::StrategyRegistry)
//! entry for custom reasoning, and turn the dial —
//! `chain-of-thought` vs `react` vs a custom strategy — by
//! changing one line.

use std::sync::Arc;

use async_trait::async_trait;
use elysper_harness::{
    EventSink,
    ReActAgent,
    ReasoningStrategy,
    StrategyRegistry,
};
use elysper_test_support::FakeProvider;
use elysper_tool::ToolRegistry;

/// A trivial consumer-side reasoning strategy. The
/// [`StrategyRegistry`] resolves it by name (`"empty"`)
/// without the harness knowing what it is.
struct EmptyStrategy;

#[async_trait]
impl ReasoningStrategy for EmptyStrategy {
    fn name(&self) -> &str {
        "empty"
    }

    async fn run(
        &self,
        _runtime: elysper_harness::AgentRuntime,
        _input: elysper_harness::AgentInput,
        sink: &EventSink,
    ) {
        // Publish a single terminal event so a caller that
        // collects events sees a well-formed transcript.
        sink.emit_typed(elysper_session::step_start(0, 0));
        sink.system(elysper_harness::SystemEvent::SessionEnded {
            reason: elysper_harness::SessionEndReason::Completed,
        });
    }
}

/// A minimal non-tokio spawner so the example runs offline.
/// Tests + benches ship the same fixture under different
/// names; reproducing it inline keeps the example self-contained.
struct InlineSpawner;

impl elysper_core::spawn::Spawner for InlineSpawner {
    fn spawn(&self, _fut: elysper_core::spawn::BoxFuture) {}
}

fn main() {
    let provider = Arc::new(FakeProvider::text("ok"));
    let tools = Arc::new(ToolRegistry::new());

    // Step 1: register a foreign strategy.
    let registry = StrategyRegistry::new();
    registry.register("empty", Arc::new(EmptyStrategy));

    // Step 2: resolve it back by name — a registered name (or a
    // harness built-in like "react") resolves through the same
    // registry that registered it.
    let strategy = registry.resolve("empty").expect("just registered");

    // Step 3: install the resolved strategy.
    let _agent = ReActAgent::new(
        Arc::clone(&provider) as Arc<dyn elysper_provider::ModelProvider>,
        Arc::clone(&tools),
    )
    .with_strategy(strategy)
    .with_spawner(Arc::new(InlineSpawner));

    println!("LIB-GRADE: OK");
}
