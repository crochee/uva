//! Parallel / sequential tool execution + descriptor-pinning
//! regression tests.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uva_core::{CancelToken, Error};
use uva_provider::{
    CompletionRequest,
    CompletionResponse,
    SamplingResult,
    StreamChunk,
    traits::ModelProvider,
    types::ModelConfig,
};
use uva_tool::{Context, Tool, ToolOutput, ToolRegistry};

use super::{support::*, *};

#[tokio::test]
async fn default_execution_mode_is_parallel() {
    // The Tool trait's mode() defaults to Parallel; this
    // verifies the contract every tool implicitly relies on.
    struct GenericTool;
    #[async_trait]
    impl uva_tool::Tool for GenericTool {
        fn name(&self) -> &str {
            "g"
        }

        fn description(&self) -> &str {
            "g"
        }

        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }

        async fn call(&self, _: serde_json::Value, _: &Context) -> ToolOutput {
            ToolOutput::text("ok")
        }
    }
    assert_eq!(
        GenericTool.mode(),
        uva_tool::traits::ExecutionMode::Parallel
    );
}

#[test]
fn with_descriptor_installs_descriptor_verbatim() {
    // Regression test: `with_descriptor` used to call
    // `with_options` internally, which built a default
    // descriptor (name="agent", capabilities=["tools",
    // "streaming", "cancellation"], etc.) and then
    // shadowed it with the caller's descriptor. The wasted
    // allocation was benign but the field-by-field overwrite
    // hid a bug class — if `with_options` ever changed to
    // mutate the descriptor, callers would silently lose
    // their customizations. The fix bypasses `with_options`
    // and installs the caller's descriptor verbatim. Verify
    // every field round-trips.
    let custom = AgentDescriptor {
        name: "custom-judge".to_string(),
        description: "Test judge that aggregates votes".to_string(),
        kind: "judge".to_string(),
        version: "2.0.0".to_string(),
        instructions: "You are a strict judge".to_string(),
        capabilities: vec!["judging".to_string()],
        tools: vec!["foo".to_string()],
        model_hint: Some("gpt-x".to_string()),
        handoffs: vec!["agent".to_string()],
        handoff_hint: Some("Use as final aggregator".to_string()),
        output_schema: None,
        owner: Some("team-a".to_string()),
        domain: Some("review".to_string()),

        persona: Some("Skeptical auditor".to_string()),
        display_name: None,
        max_iterations: None,
    };

    let provider: Arc<dyn ModelProvider> =
        Arc::new(ScriptedStreamProvider::new(vec![]));
    let agent = ReActAgent::with_descriptor(
        provider,
        Arc::new(ToolRegistry::new()),
        PathBuf::from("/tmp"),
        custom.clone(),
        Arc::new(PromptContext::default()),
    );

    let got = agent.descriptor();
    assert_eq!(got.name, "custom-judge");
    assert_eq!(got.description, "Test judge that aggregates votes");
    assert_eq!(got.kind, "judge");
    assert_eq!(got.version, "2.0.0");
    assert_eq!(got.instructions, "You are a strict judge");
    assert_eq!(got.capabilities, vec!["judging".to_string()]);
    assert_eq!(got.tools, vec!["foo".to_string()]);
    assert_eq!(got.model_hint.as_deref(), Some("gpt-x"));
    assert_eq!(got.handoffs, vec!["agent".to_string()]);
    assert_eq!(got.handoff_hint.as_deref(), Some("Use as final aggregator"));
    assert_eq!(got.owner.as_deref(), Some("team-a"));
    assert_eq!(got.domain.as_deref(), Some("review"));
    assert_eq!(got.persona.as_deref(), Some("Skeptical auditor"));
}

#[test]
fn with_descriptor_preserves_caller_fields_after_panel_removal() {
    let descriptor = AgentDescriptor {
        name: "judge".into(),
        description: "judge".into(),
        kind: "judge".into(),
        version: "1.0.0".into(),
        instructions: "judge".into(),
        capabilities: vec![],
        tools: vec![],
        model_hint: None,
        handoffs: vec![],
        handoff_hint: None,
        output_schema: None,
        owner: None,
        domain: None,
        persona: Some("Skeptical auditor".into()),
        display_name: None,
        max_iterations: None,
    };
    let provider: Arc<dyn ModelProvider> =
        Arc::new(ScriptedStreamProvider::new(vec![]));
    let agent = ReActAgent::with_descriptor(
        provider,
        Arc::new(ToolRegistry::new()),
        PathBuf::from("."),
        descriptor,
        Arc::new(PromptContext::default()),
    );
    assert_eq!(
        agent.descriptor().persona.as_deref(),
        Some("Skeptical auditor")
    );
    assert_eq!(agent.descriptor().name, "judge");
}

struct TinyWindowProvider;

#[async_trait]
impl ModelProvider for TinyWindowProvider {
    async fn initialize(
        &mut self,
        _config: uva_provider::ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "tiny-window"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "tiny".to_string(),
            provider: "test".to_string(),
            context_window: 30,
            max_output_tokens: 64,
            supports_tools: false,
            supports_streaming: true,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        unreachable!("streaming path")
    }

    async fn complete_with_stream(
        &self,
        _request: CompletionRequest,
        _cancel_token: Option<Arc<dyn CancelToken>>,
        mut on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<SamplingResult, Error> {
        let turn = SamplingResult {
            text: "ok".to_string(),
            ..Default::default()
        };
        let mut assembler = uva_provider::BlockAssembler::new();
        let done = StreamChunk::IsDone {
            result: Box::new(turn),
        };
        assembler.push(done.clone());
        on_delta(done);
        let (turn, _parts, _incomplete) = assembler.finalize_assistant();
        Ok(turn)
    }
}

#[tokio::test]
async fn context_manager_prunes_overflowing_history_and_warns() {
    // Six 400-char history turns (≈600 tokens) against a
    // 30-token window: the default TruncatingContextManager
    // must evict pairs until the history fits and the loop
    // must surface a ContextCompaction warning.
    let history: Vec<Message> = (0..6)
        .flat_map(|i| {
            vec![
                Message::user(format!("{}{}", "q".repeat(399), i)),
                Message::assistant(format!("{}{}", "a".repeat(399), i)),
            ]
        })
        .collect();

    let agent = ReActAgent::with_descriptor(
        Arc::new(TinyWindowProvider),
        Arc::new(ToolRegistry::new()),
        PathBuf::new(),
        crate::default_descriptor(String::new()),
        Arc::new(PromptContext::default()),
    );

    let mut stream = agent
        .run(
            AgentInput::history(history, "hi"),
            Arc::new(CancellationToken::new()),
        )
        .await;

    let mut saw_compaction_warning = false;
    while let Some(ev) = stream.next().await {
        if let AgentEvent::System(SystemEvent::Warning { kind, .. }) = &ev
            && *kind == WarningKind::ContextCompaction
        {
            saw_compaction_warning = true;
        }
    }
    assert!(
        saw_compaction_warning,
        "expected a ContextCompaction warning for the overflowing history"
    );
}
