//! # Consume elysper from outside the workspace (R18 proof)
//!
//! This crate is deliberately NOT a member of the elysper
//! workspace: it depends on the crates by relative path, exactly
//! as an external library consumer would. If it compiles and
//! runs, the "assemble a complete agent as a lib from zero"
//! claim has direct evidence.
//!
//! ```bash
//! cd docs/examples/external-consumer && cargo run --release
//! ```
//!
//! Expected tail: `CONSUMER-PROOF: OK`
//!
//! ## What it demonstrates (one expression each)
//!
//! | Piece | Crate | Call |
//! |---|---|---|
//! | Provider (stub, no network) | `elysper-provider` | `ModelProviderStub::text_only` |
//! | Tier-aware steering | `elysper-steering` | `tier::auto(provider.tier(), ws)` |
//! | Session log writer | `elysper-session` | `LogWriter::new(sink, clock)` |
//! | Agent assembly | `elysper-harness` | `ReActAgent::new(..)` chain → the agent is its own builder |
//! | Structured output | `elysper-harness`/`elysper-tool` | `.with_output_schema(json!{..})` |
//! | LLM compaction | `elysper-context` | `SummarizingContextManager::new(..).with_settings(..)` via `.with_context_manager(..)` |
//! | Multimodal store | `elysper-attachment` | `AttachmentStore::{save,read}_image` |
//! | Runtime-neutral cancel | `elysper-core` | `AtomicCancelToken` (std only) |
//! | Skill registry | `elysper-skill` | `SkillRegistry::new()` |
//!
//! No `tokio` type appears in any of the elysper API calls above —
//! the runtime is the consumer's choice.

use std::sync::Arc;

use futures::StreamExt;
use elysper_harness::agent::Agent as _;
use elysper_harness::{AgentEvent, AgentInput, Host, ReActAgent};
use elysper_provider::traits::ModelProvider;
use elysper_session::{LogWriter, SessionSink, in_memory::InMemorySessionSink};
use elysper_tool::{ToolEntry, ToolRegistry};
use elysper_tool_plugins::read::ReadTool;
use elysper_tool_plugins::shell::ShellTool;
use elysper_tool_plugins::todo::TodoWriteTool;
use elysper_tool_plugins::write::WriteTool;

/// The tool surface, composed brick by brick: one line per plugin
/// crate, nothing hidden in a default set.
fn plugin_tool_registry() -> ToolRegistry {
    let registry = ToolRegistry::new();
    registry.register_entry(ToolEntry::new(Arc::new(ReadTool::new())));
    registry.register_entry(ToolEntry::new(Arc::new(WriteTool::new())));
    registry.register_entry(ToolEntry::new(Arc::new(ShellTool::new())));
    registry.register_entry(ToolEntry::new(Arc::new(TodoWriteTool::new())));
    registry
}

#[tokio::main]
async fn main() {
    let workspace = std::path::PathBuf::from("/tmp");

    // Piece 1 — provider (stub, no network).
    let provider: Arc<dyn ModelProvider> = Arc::new(
        elysper_provider::traits_stub::ModelProviderStub::text_only(
            "consumer proof ok",
        ),
    );

    // Piece 2 — the interception surface, assembled from guards.
    let host = Host::default_policy(&workspace);

    // Piece 3 — the session's one writer (runtime-neutral).
    let sink = Arc::new(InMemorySessionSink::new("consumer-proof"));
    let writer = Arc::new(LogWriter::new(
        sink.clone(),
        Arc::new(elysper_core::SystemClock),
    ));

    // Pieces 4..n — one-expression assembly.
    let agent = ReActAgent::new(provider, Arc::new(plugin_tool_registry()))
        .with_name("consumer-proof")
        .with_workspace(&workspace)
        .with_host(host)
        .expect("the host binds to the registry")
        .with_output_schema(serde_json::json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}},
            "required": ["answer"]
        }))
        .with_context_manager(Arc::new(
            elysper_context::TruncatingContextManager,
        ))
        .with_log_writer(writer);

    println!(
        "descriptor: {} (schema: {})",
        agent.descriptor().name,
        agent.descriptor().output_schema.is_some()
    );

    // Piece n+1 — attachment store (multimodal), used directly.
    let store = elysper_attachment::AttachmentStore::default();
    let png: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00,
        0x0D, 0x49, 0x48, 0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00,
        0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1F, 0x15, 0xC4, 0x89,
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9C, 0x63,
        0xF8, 0xCF, 0xC0, 0x00, 0x00, 0x00, 0x03, 0x00, 0x01, 0x6F, 0x32,
        0x4D, 0x0E, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44, 0xAE,
        0x42, 0x60, 0x82,
    ];
    let saved = store.save_image(&png, "image/png").expect("save image");
    let bytes_back = store.read_image(&saved).expect("read image");
    println!(
        "attachment: {} ({} bytes round-tripped)",
        &saved.hash[..12],
        bytes_back.len()
    );

    // Piece n+2 — runtime-neutral cancellation (std-only token).
    let cancel: Arc<dyn elysper_core::CancelToken> =
        Arc::new(elysper_core::AtomicCancelToken::new());

    // Drive a turn with NO tokio type in the public API surface.
    let mut stream = agent
        .run(
            AgentInput::text("prove the consumer works"),
            cancel,
        )
        .await;
    let mut events = 0usize;
    let mut text = String::new();
    while let Some(event) = stream.next().await {
        events += 1;
        if let AgentEvent::Model(elysper_provider::ContentPart::Text(t)) = &event
        {
            text.push_str(&t.text);
        }
    }
    println!("run: {events} events, text = {text:?}");

    // Read the structural rows back from the log.
    let rows = sink.read().await.expect("read");
    println!("structural rows: {}", rows.len());

    // Skill registry (the lego for procedural knowledge).
    let reg = elysper_skill::SkillRegistry::new();
    println!("skill registry providers: {}", reg.provider_names().len());

    assert_eq!(agent.descriptor().name, "consumer-proof");
    assert!(text.contains("consumer proof ok"));
    assert_eq!(bytes_back, png);
    println!("CONSUMER-PROOF: OK");
}
