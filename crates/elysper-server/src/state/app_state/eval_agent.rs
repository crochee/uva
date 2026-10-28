//! `HarnessEvalAgent` — the real `EvalAgent` implementation that
//! drives a one-shot `ReActAgent` per eval case.
//!
//! Every `respond` call assembles a fresh agent from the server's
//! default provider + the node's shared tool registry (the same
//! handle a chat run binds, plugin tools included) and the
//! deployment's plugin host (a clone; the host is already bound to
//! that registry, so binding again is a no-op), runs it to
//! completion on the case input with a private cancel token, and
//! returns the drained assistant text. The drain is the same shape
//! `ServerWorkflowHost::spawn_agent` uses: accumulate
//! `AgentEvent::Model` text parts, require a `Completed` terminal
//! event, and treat anything else as an error.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use futures::StreamExt;
use elysper::{
    core::{AtomicCancelToken, CancelToken},
    eval::{EvalAgent, EvalError},
    harness::{Agent, AgentEvent, AgentInput, SessionEndReason, SystemEvent},
    provider::traits::ModelProvider,
    tool::ToolRegistry,
};

/// A callable agent under test backed by the real harness loop.
pub struct HarnessEvalAgent {
    provider: Arc<dyn ModelProvider>,
    tools: Arc<ToolRegistry>,
    host: elysper::harness::Host,
    instructions: String,
    workspace_root: PathBuf,
}

impl HarnessEvalAgent {
    /// Build the eval agent from the same pieces the interactive
    /// routes resolve through.
    #[must_use]
    pub fn new(
        provider: Arc<dyn ModelProvider>,
        tools: Arc<ToolRegistry>,
        host: elysper::harness::Host,
        instructions: impl Into<String>,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            provider,
            tools,
            host,
            instructions: instructions.into(),
            workspace_root,
        }
    }
}

#[async_trait]
impl EvalAgent for HarnessEvalAgent {
    async fn respond(&self, input: &str) -> Result<String, EvalError> {
        let descriptor = {
            let mut d = elysper::harness::default_descriptor(String::new());
            d.name = "eval".to_string();
            d.instructions = self.instructions.clone();
            d
        };
        let agent =
            crate::agent_assembly::AgentAssembly {
                provider: Arc::clone(&self.provider),
                tool_registry: Arc::clone(&self.tools),
                workspace_root: self.workspace_root.clone(),
                descriptor,
                prompt_context: Arc::new(
                    elysper::harness::PromptContext::default(),
                ),
                max_iterations: elysper::harness::DEFAULT_MAX_ITERATIONS,
                delegators: Vec::new(),
                tool_surface: None,
                strategy: None,
                tool_restriction: None,
                log_writer: None,
                subagent_writers: None,
                context_manager: None,
                compactor: None,
                // The deployment's host, so an eval case runs the
                // same plugin surface chat does. The host is shared
                // (a clone), so no plugin process is spawned here.
                host: Some(self.host.clone()),
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
                AgentEvent::Model(elysper::provider::ContentPart::Text(
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
