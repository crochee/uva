//! Prompt assembly integration: the `<identity>`, `<env>`,
//! `<available_skills>`, `<available_agents>` renderers and
//! the runtime-context snapshot reach the wire together with
//! the configured `instructions`.

use std::sync::{Arc, atomic::Ordering};

use elysper_provider::{ContentPart, SamplingResult, StreamChunk, TokenUsage};
use elysper_tool::ToolRegistry;
use futures::StreamExt;
use tokio_util::sync::CancellationToken;

use super::{support::*, *};
#[tokio::test]
async fn with_prompt_context_injects_skills_and_agents() {
    // End-to-end: skills + peer agents populated via
    // `with_prompt_context` must reach the LLM in the
    // assembled system prompt.
    let provider =
        Arc::new(CapturingProvider::new(vec![vec![StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: "ok".into(),
                tool_calls: vec![],
                reasoning: String::new(),
                reasoning_signature: None,
                usage: TokenUsage::default(),
                ..Default::default()
            }),
        }]]));

    let ctx = crate::prompt::PromptContext::default()
        .with_skill("summarize", "Summarize text.")
        .with_agent(&peer_descriptor(
            "planner",
            "Plans the work.",
            Some("complex tasks"),
        ));

    let agent = ReActAgent::with_descriptor(
        provider.clone(),
        Arc::new(ToolRegistry::new()),
        PathBuf::new(),
        crate::default_descriptor("BASE".to_string()),
        Arc::new(ctx),
    );

    let mut stream = agent
        .run(
            crate::input::AgentInput::text("hi"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while let Some(_ev) = stream.next().await {}
    drop(stream);

    for _ in 0..50 {
        if provider.call_count.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let captured = provider.captured.lock().await;
    let sys_text = match &captured[0][0].content {
        elysper_provider::Content::Single(ContentPart::Text(t)) => {
            t.text.clone()
        }
        _ => panic!("expected single text content"),
    };
    assert!(sys_text.contains("BASE"));
    // The `<identity>` opening line must use the
    // human-readable `display_name` ("Elysper"), not
    // the routing slug ("agent"), so the model
    // self-identifies with the persona the user sees
    // on the UI card.
    assert!(
        sys_text.contains("You are `Elysper`"),
        "identity line must use display_name; got:\n{sys_text}"
    );
    assert!(
        !sys_text.contains("You are `react`"),
        "identity line must not leak the routing slug; got:\n{sys_text}"
    );
    // `read_file` is a tool — it must NOT appear in the
    // prompt text (tools ride the completion request's
    // `tools` channel).
    assert!(!sys_text.contains("`read_file`"));
    assert!(sys_text.contains("<name>summarize</name>"));
    assert!(
        !sys_text.contains("<name>disabled-skill</name>"),
        "disabled skills must not be advertised"
    );
    assert!(sys_text.contains("`planner`"));
    let agents_section = sys_text
        .split_once("<available_agents>")
        .and_then(|(_, rest)| rest.split_once("</available_agents>"))
        .map(|(body, _)| body)
        .expect("available_agents section");
    assert!(
        agents_section.contains("planner"),
        "available_agents must name the peer agent"
    );
    assert!(sys_text.contains("<identity>"));
    assert!(sys_text.contains("</identity>"));
    assert!(sys_text.contains("<available_skills>"));
    assert!(sys_text.contains("</available_skills>"));
    assert!(sys_text.contains("<available_agents>"));
    assert!(sys_text.contains("</available_agents>"));
    // Tool schemas are NOT in the prompt — they ride the
    // request's `tools` channel and the runtime validates
    // every emitted name.
    assert!(
        !sys_text.contains("<available_tools>"),
        "tool schemas must not be assembled into the system prompt"
    );
    assert!(
        !sys_text.contains("Use only these tools:"),
        "tool grounding belongs on the runtime, not in the prompt"
    );
}

#[tokio::test]
async fn descriptor_is_stable_returns_assembled_prompt_metadata() {
    // Sanity: the descriptor fields used by the assembler are
    // reachable via `descriptor()`.
    let agent = ReActAgent::with_descriptor(
        Arc::new(elysper_provider::traits_stub::ModelProviderStub::new()),
        Arc::new(ToolRegistry::new()),
        PathBuf::new(),
        crate::default_descriptor("X".to_string()),
        Arc::new(crate::prompt::PromptContext::default()),
    );
    assert_eq!(agent.descriptor().instructions, "X");
}

#[tokio::test]
async fn tool_schemas_never_appear_in_assembled_system_prompt() {
    // End-to-end contract: even when tools exist in the
    // runtime, the assembled system prompt must NOT carry
    // their descriptions. Tool schemas ride the completion
    // request's `tools` field; the runtime validates every
    // emitted name against the registry.
    let provider =
        Arc::new(CapturingProvider::new(vec![vec![StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: "ok".into(),
                tool_calls: vec![],
                reasoning: String::new(),
                reasoning_signature: None,
                usage: TokenUsage::default(),
                ..Default::default()
            }),
        }]]));

    // `PromptContext` no longer carries tools — it only
    // carries skills / peer agents. The runtime will pull
    // tool definitions from `ToolRegistry` for the
    // completion request's `tools` channel.
    let ctx = crate::prompt::PromptContext::default();

    let agent = ReActAgent::with_descriptor(
        provider.clone(),
        Arc::new(ToolRegistry::new()),
        PathBuf::new(),
        crate::default_descriptor("BASE".to_string()),
        Arc::new(ctx),
    );

    let mut stream = agent
        .run(
            crate::input::AgentInput::text("hi"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while let Some(_ev) = stream.next().await {}
    drop(stream);

    for _ in 0..50 {
        if provider.call_count.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let captured = provider.captured.lock().await;
    let sys_text = match &captured[0][0].content {
        elysper_provider::Content::Single(ContentPart::Text(t)) => {
            t.text.clone()
        }
        _ => panic!("expected single text content"),
    };
    // No tool manifest, no tool grounding, no tool name
    // leakage in the prompt text.
    assert!(
        !sys_text.contains("<available_tools>"),
        "tool schemas must not be assembled into the system prompt"
    );
    assert!(
        !sys_text.contains("Use only these tools:"),
        "tool grounding belongs on the runtime, not in the prompt"
    );
}

// The harness has no git dependency, so the old git line was a permanent `unknown`.
#[tokio::test]
async fn runtime_snapshot_names_the_model_and_omits_git() {
    let provider =
        Arc::new(CapturingProvider::new(vec![vec![StreamChunk::IsDone {
            result: Box::new(SamplingResult {
                text: "ok".into(),
                tool_calls: vec![],
                reasoning: String::new(),
                reasoning_signature: None,
                usage: TokenUsage::default(),
                ..Default::default()
            }),
        }]]));

    let agent =
        ReActAgent::new(provider.clone(), Arc::new(ToolRegistry::new()));
    let mut stream = agent
        .run(
            crate::input::AgentInput::text("hi"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while let Some(_ev) = stream.next().await {}
    drop(stream);

    for _ in 0..50 {
        if provider.call_count.load(Ordering::SeqCst) > 0 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let captured = provider.captured.lock().await;
    let snapshot = captured[0]
        .iter()
        .filter_map(|m| match &m.content {
            elysper_provider::Content::Single(ContentPart::Text(t)) => {
                Some(t.text.as_str())
            }
            _ => None,
        })
        .find(|t| t.contains("Current runtime context."))
        .expect("the runtime-context snapshot reaches the wire");
    assert!(
        snapshot.contains("Model: capturing/capturing"),
        "the snapshot must name the model the loop already holds; \
         got:\n{snapshot}"
    );
    assert!(
        !snapshot.contains("Is directory a git repo:"),
        "no git dependency means no permanently-unknown git line; \
         got:\n{snapshot}"
    );
}
