//! `assemble_with_builder` — the modern one-expression assembly.
//!
//! R14 capstone tutorial. Every earlier `assemble_*` example
//! wires the bricks by hand (each `with_*` call held by the
//! caller). `ReActAgent::new(provider, registry)` collapses that
//! into one chained expression (R12's `AgentBuilder` folded into
//! the agent itself in R110); R13's `output_schema` setter rides
//! the same chain. This file is the "best-assembly paradigm" the
//! project goal names: a complete, production-shaped agent in one
//! readable block.
//!
//! Demonstrated bricks:
//!
//! | Brick | Round | API |
//! |---|---|---|
//! | `ReActAgent` (the builder is the harness) | R12 / R110 | `ReActAgent::new(provider, registry)` chain |
//! | The production policy | 0.2 | `uva_harness::Host::default_policy(root)` |
//! | Structured output | R12+R13 | `.with_output_schema(schema)` → auto tool |
//! | LLM compaction | R13 | `uva-app-server::agent_run_config::context_manager_for_compaction_with_emitters` |
//! | Session log writer | R6 | `.with_log_writer(writer)` |
//!
//! Run (no network, no API key):
//!
//! ```bash
//! cargo run --example assemble_with_builder -p uva-harness
//! ```

use std::sync::Arc;

use uva_harness::{Host, ReActAgent, agent::Agent as _};
use uva_provider::traits::ModelProvider;
use uva_session::{LogWriter, SessionSink, in_memory::InMemorySessionSink};
use uva_tool::{ToolEntry, ToolRegistry};
use uva_tool_plugins::{
    read::ReadTool,
    shell::ShellTool,
    todo::TodoWriteTool,
    write::WriteTool,
};

/// The default surface, composed explicitly: one line per plugin
/// crate, exactly the bricks this example wants.
fn plugin_tool_registry() -> Arc<ToolRegistry> {
    let registry = ToolRegistry::new();
    registry.register_entry(ToolEntry::new(Arc::new(ReadTool::new())));
    registry.register_entry(ToolEntry::new(Arc::new(WriteTool::new())));
    registry.register_entry(ToolEntry::new(Arc::new(ShellTool::new())));
    registry.register_entry(ToolEntry::new(Arc::new(TodoWriteTool::new())));
    Arc::new(registry)
}

#[tokio::main]
async fn main() {
    println!("== uva R14: assemble with ReActAgent ==\n");

    // 1. Provider — the stub answers locally (no network).
    let provider: Arc<dyn ModelProvider> =
        Arc::new(uva_provider::traits_stub::ModelProviderStub::text_only(
            "Structured answer captured via the structured_output tool.",
        ));
    println!("[1/6] provider      : {}", provider.name());

    // 2. The production policy (0.2): one `Host` carries every
    //    guard / hint / tracker registration the deployment wants.
    //    The base library installs none of them by default.
    let workspace = std::path::PathBuf::from(".");
    let host = Host::default_policy(&workspace);
    println!(
        "[2/6] host policy   : {} registrations",
        host.registration_count()
    );

    // 3. The session's one writer (R6): the structural rows the loop
    //    produces go straight into it, and it stamps each row's
    //    ordinal.
    let sink = Arc::new(InMemorySessionSink::new("assemble-with-builder"));
    let writer = Arc::new(LogWriter::new(
        sink.clone(),
        Arc::new(uva_core::SystemClock),
    ));
    println!("[3/6] log writer   : session writer installed");

    // 4-6. THE BUILD: one chained expression consumes the bricks.
    //    - `.output_schema(..)` (R13) auto-registers the
    //      `structured_output` tool AND stamps the descriptor.
    //    - LLM compaction is wired by the server-side factory
    //      `uva_server::agent_run_config::context_manager_for_compaction_with_emitters`
    //      (R126 moved it out of the harness); the harness only
    //      owns the contextual wires it composes here.
    //    - `.with_host(..)` / `.log_writer(..)` / `.name()`
    //      complete the wiring.
    let agent = ReActAgent::new(provider, plugin_tool_registry())
        .with_name("tutorial-agent")
        .with_instructions("Answer through the structured_output tool.")
        .with_workspace(&workspace)
        .with_host(host)
        .expect("bind the host")
        .with_output_schema(serde_json::json!({
            "type": "object",
            "properties": {
                "answer": {"type": "string"},
                "confidence": {"type": "number"}
            },
            "required": ["answer"]
        }))
        .with_log_writer(writer);

    println!(
        "[4/6] descriptor    : name={} (schema {})",
        agent.descriptor().name,
        if agent.descriptor().output_schema.is_some() {
            "declared"
        } else {
            "none"
        }
    );
    println!(
        "[5/6] structured out: tool auto-injected (output_schema declared)"
    );
    println!(
        "[6/6] compaction    : server factory owns the LLM summariser wiring"
    );

    // Drive one turn so the whole assembly is proven live.
    use futures::StreamExt;
    let cancel = Arc::new(uva_core::AtomicCancelToken::new());
    let mut stream = agent
        .run(
            uva_harness::AgentInput::text("Give me the structured answer."),
            cancel,
        )
        .await;
    let mut answer = String::new();
    let mut events = 0usize;
    while let Some(event) = stream.next().await {
        events += 1;
        if let uva_harness::AgentEvent::Model(
            uva_provider::ContentPart::Text(t),
        ) = &event
        {
            answer.push_str(&t.text);
        }
    }
    println!("\nrun              : {events} events, final: {answer}");

    // Read the structural rows back to prove the writer was wired.
    let rows = sink.read().await.expect("read");
    println!("structural rows  : {} written to the log", rows.len());

    println!("\n== done ==");
}
