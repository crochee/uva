//! Agent-level tests: descriptor shape, smoke runs, system-prompt
//! plumbing. No streaming provider used here — these are the
//! cheapest entry-point checks.

use std::sync::Arc;

use elysper_tool::ToolRegistry;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use super::{support::*, *};

#[tokio::test]
async fn system_prompt_is_injected_as_first_message() {
    use elysper_provider::Role;

    let provider = Arc::new(CapturingProvider::new(vec![empty_response()]));
    let agent = ReActAgent::with_descriptor(
        provider.clone(),
        Arc::new(ToolRegistry::new()),
        PathBuf::new(),
        crate::default_descriptor("TEST-PROMPT".to_string()),
        Arc::new(PromptContext::default()),
    );

    let mut stream = agent
        .run(AgentInput::text("hi"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let captured = provider.captured.lock().await;
    let first = &captured[0][0];
    assert_eq!(
        first.role,
        Role::System,
        "the assembled system prompt must be the first request message"
    );
    let text = match &first.content {
        elysper_provider::Content::Single(ContentPart::Text(t)) => &t.text,
        _ => panic!("expected single text content"),
    };
    assert!(
        text.contains("TEST-PROMPT"),
        "system message must carry the configured prompt; got {text:?}"
    );
}

#[tokio::test]
async fn descriptor_is_stable() {
    let agent = ReActAgent::new(
        Arc::new(elysper_provider::traits_stub::ModelProviderStub::new()),
        Arc::new(ToolRegistry::new()),
    );
    assert_eq!(agent.descriptor().name, "agent");
    assert_eq!(agent.descriptor().kind, "react");
    assert_eq!(agent.descriptor().version, "1.0.0");
    assert!(
        agent
            .descriptor()
            .capabilities
            .contains(&"tools".to_string())
    );
    // Industry-aligned fields default to sensible values.
    assert_eq!(
        agent.descriptor().instructions,
        crate::DEFAULT_SYSTEM_PROMPT
    );
    // Brand fields the default uses to fill are
    // `None` once the descriptor is slimmed — callers
    // set them explicitly via `with_descriptor`.
    assert_eq!(agent.descriptor().owner, None);
    assert_eq!(agent.descriptor().domain, None);
    // The human-readable label is "Elysper" — the
    // programmatic `name` ("agent") stays as the
    // routing slug.
    assert_eq!(agent.descriptor().display_name(), "Elysper");
    assert_eq!(agent.descriptor().display_name.as_deref(), Some("Elysper"));
}

#[tokio::test]
async fn descriptor_surfaces_custom_system_prompt() {
    let agent = ReActAgent::with_descriptor(
        Arc::new(elysper_provider::traits_stub::ModelProviderStub::new()),
        Arc::new(ToolRegistry::new()),
        PathBuf::new(),
        crate::default_descriptor("CUSTOM".to_string()),
        Arc::new(PromptContext::default()),
    );
    // `system_prompt` argument is now exposed via
    // `descriptor.instructions` so callers can introspect
    // the agent's role without holding a second handle.
    assert_eq!(agent.descriptor().instructions, "CUSTOM");
}

#[tokio::test]
async fn react_agent_setters_thread_through_to_descriptor_and_registry() {
    let registry = Arc::new(ToolRegistry::new());
    let schema = serde_json::json!({
        "type": "object",
        "properties": {"answer": {"type": "string"}},
        "required": ["answer"]
    });
    let mut descriptor = crate::default_descriptor("SYSTEM".to_string());
    descriptor.model_hint = Some("claude-opus-4".to_string());
    let agent = ReActAgent::with_descriptor(
        Arc::new(elysper_provider::traits_stub::ModelProviderStub::new()),
        Arc::clone(&registry),
        PathBuf::new(),
        descriptor,
        Arc::new(PromptContext::default()),
    )
    .with_name("researcher")
    .with_instructions("Find things.")
    .with_output_schema(schema.clone());

    assert_eq!(agent.descriptor().name, "researcher");
    assert_eq!(agent.descriptor().instructions, "Find things.");
    assert_eq!(
        agent.descriptor().model_hint.as_deref(),
        Some("claude-opus-4")
    );

    // The output schema landed on the descriptor…
    let desc_schema = agent
        .descriptor()
        .output_schema
        .as_deref()
        .expect("descriptor carries the schema string");
    let parsed: serde_json::Value = serde_json::from_str(desc_schema).unwrap();
    assert_eq!(parsed, schema);

    // …and the structured_output tool was auto-injected
    // into the registry by `with_output_schema`.
    registry
        .get(elysper_tool::structured_output::STRUCTURED_OUTPUT_TOOL_NAME)
        .expect("with_output_schema must install structured_output");
}

#[test]
fn default_system_prompt_covers_canonical_sections() {
    let p = crate::DEFAULT_SYSTEM_PROMPT;

    for tag in [
        "action_safety",
        "tool_calling",
        "output_efficiency",
        "formatting",
    ] {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        assert!(
            p.contains(&open) && p.contains(&close),
            "missing `{open}` / `{close}` in DEFAULT_SYSTEM_PROMPT"
        );
    }
}

#[tokio::test]
async fn descriptor_defaults_are_on_brand() {
    let agent = ReActAgent::new(
        Arc::new(elysper_provider::traits_stub::ModelProviderStub::new()),
        Arc::new(ToolRegistry::new()),
    );
    let d = agent.descriptor();

    assert_eq!(d.kind, "react");
    assert_eq!(d.version, "1.0.0");
    assert_eq!(d.display_name(), "Elysper");
    // Brand fields the default does not fill.
    assert_eq!(d.persona, None);
    assert_eq!(d.handoff_hint, None);
}
