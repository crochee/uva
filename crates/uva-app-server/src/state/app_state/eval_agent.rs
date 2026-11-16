//! `HarnessEvalAgent` — the real `EvalAgent` implementation that
//! drives a one-shot `ReActAgent` per eval case.
//!
//! Every `respond` call asks the mounted `agentLoop` service for a
//! fresh agent (the same factory a chat run resolves through, so an
//! eval case runs the deployment's configured loop), runs it to
//! completion on the case input with a private cancel token, and
//! returns the drained assistant text. The drain accumulates
//! `AgentEvent::Model` text parts, requires a `Completed` terminal
//! event, and treats anything else as an error.
//!
//! The factory is the service, not a copy of the assembly: an
//! unmounted `agentLoop` leaves the caller without an agent, which is
//! the capability gate the kernel exists to make visible.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use futures::StreamExt;
use uva::{
    core::{AtomicCancelToken, CancelToken},
    eval::{EvalAgent, EvalError},
    harness::{
        AgentEvent,
        AgentInput,
        SessionEndReason,
        SystemEvent,
        plugin::{AgentLoopFactory, RunConfig},
    },
    provider::traits::ModelProvider,
    tool::ToolRegistry,
};

/// A callable agent under test backed by the real harness loop.
pub struct HarnessEvalAgent {
    /// The mounted `agentLoop` service: one fresh agent per case.
    factory: Arc<dyn AgentLoopFactory>,
    provider: Arc<dyn ModelProvider>,
    tools: Arc<ToolRegistry>,
    host: uva::harness::Host,
    instructions: String,
    workspace_root: PathBuf,
}

impl HarnessEvalAgent {
    /// Build the eval agent from the same pieces the interactive
    /// routes resolve through.
    #[must_use]
    pub fn new(
        factory: Arc<dyn AgentLoopFactory>,
        provider: Arc<dyn ModelProvider>,
        tools: Arc<ToolRegistry>,
        host: uva::harness::Host,
        instructions: impl Into<String>,
        workspace_root: PathBuf,
    ) -> Self {
        Self {
            factory,
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
            let mut d = uva::harness::default_descriptor(String::new());
            d.name = "eval".to_string();
            d.instructions = self.instructions.clone();
            d
        };
        let run = RunConfig {
            provider: Arc::clone(&self.provider),
            tool_registry: Arc::clone(&self.tools),
            workspace_root: self.workspace_root.clone(),
            descriptor,
            prompt_context: Arc::new(uva::agent::PromptContext::default()),
            max_iterations: uva::harness::DEFAULT_MAX_ITERATIONS,
            tool_surface: None,
            strategy: None,
            tool_restriction: None,
            log_writer: None,
            context_manager: None,
            compactor: None,
            // The deployment's host, so an eval case runs the same
            // plugin surface chat does. The host is shared (a
            // clone), so no plugin process is spawned here.
            host: Some(self.host.clone()),
            spawner: Some(crate::spawner::server_spawner()),
        };
        let agent = self.factory.create(&run);
        // A fresh token per case: eval cases run to completion and
        // there is no live control layer to inherit a cancel from.
        let cancel: Arc<dyn CancelToken> = AtomicCancelToken::shared();
        let mut stream =
            agent.run(AgentInput::text(input.to_string()), cancel).await;

        let mut text = String::new();
        let mut completed = false;
        while let Some(event) = stream.next().await {
            match &event {
                AgentEvent::Model(uva::provider::ContentPart::Text(
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
