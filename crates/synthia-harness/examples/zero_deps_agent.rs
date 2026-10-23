//! # Minimum-deps agent assembly via the canonical constructor
//!
//! ```bash
//! cargo run --example zero_deps_agent -p synthia-harness
//! ```
//!
//! Expected tail: `ZERO-DEPS-AGENT: OK`
//!
//! Smoke test for the canonical [`ReActAgent::new`] path:
//! the consumer brings only the *required* dependencies
//! (synthia-harness + a provider + a tool registry) and gets a
//! working agent without touching any of the optional knobs
//! (interceptor, lifecycle, checkpointer, run inbox, …).
//!
//! The example uses [`ReActAgent::new`] to assemble a
//! [`ReActAgent`] from a scripted provider + an empty tool
//! registry. The program prints its proof line after the
//! constructor returns; no real LLM call is made (the
//! provider is scripted to emit a final text answer).

use std::sync::Arc;

use synthia_harness::ReActAgent;
use synthia_provider::ModelProvider;
use synthia_test_support::FakeProvider;
use synthia_tool::ToolRegistry;

fn main() {
    let provider: Arc<dyn ModelProvider> = Arc::new(FakeProvider::text("ok"));
    let tools: Arc<ToolRegistry> = Arc::new(ToolRegistry::new());

    // The canonical constructor: provider + tools is all that is
    // required; every other knob keeps its default.
    let _agent: ReActAgent = ReActAgent::new(provider, tools);

    println!("ZERO-DEPS-AGENT: OK");
}
