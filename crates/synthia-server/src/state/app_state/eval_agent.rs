//! `HarnessEvalAgent` — the real `EvalAgent` implementation that
//! drives a one-shot `ReActAgent` per eval case.
//!
//! Every `respond` call assembles a fresh agent from the server's
//! default provider + a snapshot of the live tool registry (the
//! minimal subset of the boot assembly chain: name, instructions,
//! workspace), runs it to completion on the case input with a
//! private cancel token, and returns the drained assistant text.
//! The drain is the same shape `ServerWorkflowHost::spawn_agent`
//! uses: accumulate `AgentEvent::Model` text parts, require a
//! `Completed` terminal event, and treat anything else as an error.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use futures::StreamExt;
use synthia::{
    core::{AtomicCancelToken, CancelToken},
    eval::{EvalAgent, EvalError},
    harness::{Agent, AgentEvent, AgentInput, SessionEndReason, SystemEvent},
    provider::traits::ModelProvider,
    tool::ToolRegistry,
};
use tokio::sync::RwLock;

/// A callable agent under test backed by the real harness loop.
pub struct HarnessEvalAgent {
    provider: Arc<dyn ModelProvider>,
    tools: Arc<RwLock<ToolRegistry>>,
    instructions: String,
    workspace_root: PathBuf,
}

impl HarnessEvalAgent {
    /// Build the eval agent from the same pieces the interactive
    /// routes resolve through.
    #[must_use]
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        tools: Arc<RwLock<ToolRegistry>>,
        instructions: impl Into<String>,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            provider,
            tools,
            instructions: instructions.into(),
            workspace_root,
        }
    }

    async fn snapshot_tools(&self) -> Arc<ToolRegistry> {
        let guard = self.tools.read().await;
        Arc::new(guard.clone())
    }
}

#[async_trait]
impl EvalAgent for HarnessEvalAgent {
    async fn respond(&self, input: &str) -> Result<String, EvalError> {
        let descriptor = {
            let mut d = synthia::harness::default_descriptor(String::new());
            d.name = "eval".to_string();
            d.instructions = self.instructions.clone();
            d
        };
        let agent =
            crate::agent_assembly::AgentAssembly {
                provider: Arc::clone(&self.provider),
                tool_registry: self.snapshot_tools().await,
                workspace_root: self.workspace_root.clone(),
                descriptor,
                prompt_context: Arc::new(
                    synthia::harness::PromptContext::default(),
                ),
                steering: Arc::new(synthia::steering::Steering::noop()),
                max_iterations: synthia::harness::DEFAULT_MAX_ITERATIONS,
                synthetic: Vec::new(),
                tool_surface: None,
                strategy: None,
                tool_restriction: None,
                typed_event_sink: None,
                context_manager: None,
            }
            .build();
        // A fresh token per case: eval cases run to completion and
        // there is no live control layer to inherit a cancel from.
        let cancel: Arc<dyn CancelToken> = AtomicCancelToken::shared();
        let mut stream =
            agent.run(AgentInput::text(input.to_string()), cancel).await;

        let mut text = String::new();
        let mut completed = false;
        while let Some(event) = stream.next().await {
            match &event {
                AgentEvent::Model(synthia::provider::ContentPart::Text(
                    text_part,
                )) => {
                    if !text_part.text.is_empty() {
                        if !text.is_empty() {
                            text.push('\n');
                        }
                        text.push_str(&text_part.text);
                    }
                }
                AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
                    completed = matches!(reason, SessionEndReason::Completed);
                }
                _ => {}
            }
        }
        if !completed {
            return Err(EvalError::Agent(
                "eval agent session ended without completing".to_string(),
            ));
        }
        if text.trim().is_empty() {
            return Err(EvalError::Agent(
                "eval agent completed without producing text".to_string(),
            ));
        }
        Ok(text)
    }
}
