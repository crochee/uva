//! Reasoning-strategy seam: pluggable agent loops.

use std::sync::Arc;

use async_trait::async_trait;
pub use caps::AgentRuntime;

use crate::{
    AgentCaps,
    agent::{
        caps::{self},
        sink::EventSink,
    },
    input::AgentInput,
};

/// Implement this to run a session your way: the runtime hands over every
/// assembled piece and the sink publishes what happens.
#[async_trait]
pub trait ReasoningStrategy: Send + Sync + 'static {
    /// Stable label for logs and introspection (`"react"`, `"cot"`, …).
    fn name(&self) -> &str;

    /// Drive one session to completion, publishing events through `sink`.
    async fn run(
        &self,
        runtime: AgentRuntime,
        input: AgentInput,
        sink: &EventSink,
    );
}

fn builtin_strategy(name: &str) -> Option<Arc<dyn ReasoningStrategy>> {
    match name.trim().to_ascii_lowercase().as_str() {
        "react" | "re-act" | "re_act" => {
            Some(Arc::new(crate::agent::ReActStrategy))
        }
        _ => None,
    }
}

// Shared by both entry points so their messages cannot drift apart.
fn unknown_strategy(name: &str, registered: &[String]) -> String {
    format!(
        "unknown reasoning strategy `{name}`; registered: [{}] \
         (built-ins resolve without a register call: react, re-act, re_act)",
        registered.join(", ")
    )
}

/// The one request constructor both sampling paths use; everything
/// not passed here is the request's own default.
pub(crate) fn build_completion_request(
    model: String,
    prepared: Arc<Vec<uva_provider::Message>>,
    tools: Arc<Vec<uva_provider::ToolDefinition>>,
    tool_choice: uva_provider::ToolChoice,
    max_tokens: Option<usize>,
) -> uva_provider::CompletionRequest {
    uva_provider::CompletionRequest {
        model,
        messages: prepared,
        tools,
        tool_choice,
        max_tokens,
        ..uva_provider::CompletionRequest::default()
    }
}

/// `instruction` is appended to the run's assembled system prompt, for
/// strategies that steer through the prompt.
pub async fn single_shot_request(
    caps: &AgentCaps,
    input: &AgentInput,
    instruction: Option<&str>,
) -> uva_provider::CompletionRequest {
    let prepared = prepare_messages_for_shot(caps, input, instruction).await;
    single_shot_request_shared(caps, prepared)
}

/// Assemble and fit the conversation exactly once.
pub async fn prepare_messages_for_shot(
    caps: &AgentCaps,
    input: &AgentInput,
    instruction: Option<&str>,
) -> Arc<Vec<uva_provider::Message>> {
    use uva_context::AgentState;

    let base = caps
        .run_prompt_context(input.prompt_context.as_ref())
        .assemble(&caps.model.descriptor);
    let system = match instruction {
        Some(instruction) => format!("{base}\n\n{instruction}"),
        None => base,
    };

    // Same transcript assembly the ReAct loop sends: no placeholder
    // system message on an empty prompt, single-part user input as
    // `Content::Single`.
    let messages = crate::prompt::base_transcript(
        &system,
        &input.history,
        Some(input.to_message()),
    );

    let config = caps.model.provider.model_config();
    let mut state = AgentState::from_config(&config);
    caps.control
        .context_manager
        .prepare_arc(Arc::new(messages), &mut state)
        .await
}

/// Build a `CompletionRequest` from an already-prepared conversation.
pub fn single_shot_request_shared(
    caps: &AgentCaps,
    prepared: Arc<Vec<uva_provider::Message>>,
) -> uva_provider::CompletionRequest {
    use uva_provider::ToolChoice;

    let config = caps.model.provider.model_config();
    build_completion_request(
        config.name.clone(),
        prepared,
        Arc::new(Vec::new()),
        ToolChoice::None,
        Some(config.max_output_tokens),
    )
}

// ---------------------------------------------------------------------------
// StrategyRegistry
// ---------------------------------------------------------------------------

use uva_core::Error;

/// The built-in `"react"` resolves without a `register` call; a registration
/// under the same name shadows it.
pub struct StrategyRegistry {
    inner: uva_core::NameTable<Arc<dyn ReasoningStrategy>>,
}

impl StrategyRegistry {
    /// Empty registry. Boot-time `register` calls fill it.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: uva_core::NameTable::new(),
        }
    }

    /// Register `strategy` under `name`, returning the previous binding so a caller
    /// can keep the old strategy alive while installing the new one.
    pub fn register(
        &self,
        name: impl Into<String>,
        strategy: Arc<dyn ReasoningStrategy>,
    ) -> Option<Arc<dyn ReasoningStrategy>> {
        self.inner.insert(name.into(), strategy)
    }

    fn registered(&self, name: &str) -> Option<Arc<dyn ReasoningStrategy>> {
        self.inner.get(name)
    }

    /// Every registered name, sorted for deterministic listing.
    #[must_use]
    pub(crate) fn names(&self) -> Vec<String> {
        self.inner.names()
    }

    /// Registered names first, then built-ins. The miss message names every
    /// registered entry — operators read it in a boot log.
    pub fn resolve(
        &self,
        name: &str,
    ) -> Result<Arc<dyn ReasoningStrategy>, Error> {
        self.registered(name)
            .or_else(|| builtin_strategy(name))
            .ok_or_else(|| {
                uva_core::Error::validation(unknown_strategy(
                    name,
                    &self.names(),
                ))
            })
    }
}

impl Default for StrategyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
