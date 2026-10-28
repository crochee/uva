//! [`ReflectionHook`] — the `after_tool` observer.

use std::sync::{
    Arc,
    atomic::{AtomicU32, Ordering},
};

use elysper_core::Clock;
use elysper_harness::agent::host::{Hook, HookContext};
use elysper_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    Message,
    ModelProvider,
    Role,
    TextContent,
};
use elysper_steering::{
    Hint,
    HintMessage,
    HintPriority,
    HookDecision,
    HookEvent,
    InjectionPoint,
    OutputTransformer,
};

use crate::critique::{CriticProvider, HistoryEntry, ModelCritic};

/// Configuration for [`ReflectionHook`].
#[derive(Clone)]
pub struct ReflectorConfig {
    /// Critic model (cheap + fast). Required.
    pub critic_provider: Arc<dyn ModelProvider>,
    /// How often to invoke the critic (per tool call).
    pub every_n_steps: u32,
    /// Bound the history snapshot to the last N tool calls.
    pub history_window: usize,
    /// Critic impl. `None` ⇒ [`ModelCritic`] over `critic_provider`.
    pub critic: Option<Arc<dyn CriticProvider>>,
}

impl std::fmt::Debug for ReflectorConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReflectorConfig")
            .field("critic_provider", &self.critic_provider.name())
            .field("every_n_steps", &self.every_n_steps)
            .field("history_window", &self.history_window)
            .field("critic", &"<dyn CriticProvider>")
            .finish()
    }
}

impl ReflectorConfig {
    #[must_use]
    pub fn critic_provider(mut self, p: Arc<dyn ModelProvider>) -> Self {
        self.critic_provider = p;
        self
    }

    #[must_use]
    pub fn every_n_steps(mut self, n: u32) -> Self {
        self.every_n_steps = n;
        self
    }

    #[must_use]
    pub fn history_window(mut self, n: usize) -> Self {
        self.history_window = n;
        self
    }

    #[must_use]
    pub fn critic(mut self, c: Arc<dyn CriticProvider>) -> Self {
        self.critic = Some(c);
        self
    }
}

impl Default for ReflectorConfig {
    fn default() -> Self {
        // `critic_provider` defaults to a stub provider that
        // returns `INFO …`. Production code MUST set a real one
        // before instantiating the hook.
        struct Stub;
        #[async_trait::async_trait]
        impl ModelProvider for Stub {
            async fn initialize(
                &mut self,
                _config: elysper_provider::ProviderConfig,
            ) -> Result<(), elysper_core::Error> {
                Ok(())
            }

            fn name(&self) -> &str {
                "reflection-stub"
            }

            fn model_config(&self) -> elysper_provider::ModelConfig {
                elysper_provider::ModelConfig {
                    name: "stub".into(),
                    provider: "stub".into(),
                    context_window: 1,
                    max_output_tokens: 1,
                    supports_tools: false,
                    supports_streaming: false,
                    supports_reasoning: false,
                }
            }

            async fn complete(
                &self,
                _request: CompletionRequest,
            ) -> Result<CompletionResponse, elysper_core::Error> {
                Ok(CompletionResponse {
                    id: "stub".into(),
                    model: "stub".into(),
                    content: Content::Single(ContentPart::Text(TextContent {
                        text: "INFO stub".into(),
                        cache_control: None,
                    })),
                    usage: elysper_provider::TokenUsage::default(),
                    cached: false,
                    stop_reason: None,
                    replay_state: None,
                })
            }
        }
        Self {
            critic_provider: Arc::new(Stub),
            every_n_steps: 3,
            history_window: 4,
            critic: None,
        }
    }
}

/// Reflection hook that asks a critic model to flag drift every N tool
/// calls, and stages a [`Hint`] when drift is severe enough.
pub struct ReflectionHook {
    pub config: ReflectorConfig,
    pub clock: Arc<dyn Clock>,
    pub last_reflected_step: AtomicU32,
    /// Bounded history. Populated by the `after_tool` handler,
    /// drained by the periodic reflection pass.
    history: parking_lot::Mutex<Vec<HistoryEntry>>,
    /// The most recent hint to inject into the next prompt.
    pending_hint: parking_lot::Mutex<Option<Box<dyn Hint>>>,
    /// Mirrored text for `pending_hint_text` (cheap peek).
    pending_text: parking_lot::Mutex<Option<String>>,
    /// Counters for observability.
    pub(crate) reflections: AtomicU32,
    pub(crate) hints_injected: AtomicU32,
}

impl ReflectionHook {
    pub fn new(config: ReflectorConfig, clock: Arc<dyn Clock>) -> Self {
        Self {
            config,
            clock,
            last_reflected_step: AtomicU32::new(0),
            history: parking_lot::Mutex::new(Vec::new()),
            pending_hint: parking_lot::Mutex::new(None),
            pending_text: parking_lot::Mutex::new(None),
            reflections: AtomicU32::new(0),
            hints_injected: AtomicU32::new(0),
        }
    }

    /// Number of reflection passes run so far.
    #[must_use]
    pub fn reflection_count(&self) -> u32 {
        self.reflections.load(Ordering::SeqCst)
    }

    /// Number of hints injected so far.
    #[must_use]
    pub fn hint_count(&self) -> u32 {
        self.hints_injected.load(Ordering::SeqCst)
    }

    /// Snapshot the pending hint body text (if any).
    #[must_use]
    pub fn pending_hint_text(&self) -> Option<String> {
        self.pending_text.lock().clone()
    }

    /// Drain the pending hint, returning it.
    pub fn take_pending_hint(&self) -> Option<Box<dyn Hint>> {
        *self.pending_text.lock() = None;
        self.pending_hint.lock().take()
    }

    fn critic(&self) -> Arc<dyn CriticProvider> {
        self.config.critic.clone().unwrap_or_else(|| {
            Arc::new(ModelCritic::new(self.config.critic_provider.clone()))
                as Arc<dyn CriticProvider>
        })
    }

    fn history_snapshot(&self) -> Vec<HistoryEntry> {
        let h = self.history.lock();
        let n = self.config.history_window.max(1);
        h.iter().rev().take(n).cloned().collect::<Vec<_>>()
    }

    fn append_history(&self, entry: HistoryEntry) {
        let mut h = self.history.lock();
        let cap = self.config.history_window.max(4);
        h.push(entry);
        while h.len() > cap * 2 {
            h.remove(0);
        }
    }

    async fn maybe_reflect(&self, step: u32) {
        let every = self.config.every_n_steps.max(1);
        if !step.is_multiple_of(every) {
            return;
        }
        let last = self.last_reflected_step.load(Ordering::SeqCst);
        if step <= last {
            return;
        }
        self.last_reflected_step.store(step, Ordering::SeqCst);
        self.reflections.fetch_add(1, Ordering::SeqCst);

        let snapshot = self.history_snapshot();
        let critique = self.critic().critique(&snapshot).await;
        if critique.warrants_hint()
            && let Some(text) = critique.suggested_hint.clone()
        {
            let hint = TextHint::new(text.clone());
            *self.pending_hint.lock() = Some(Box::new(hint));
            *self.pending_text.lock() = Some(text);
            self.hints_injected.fetch_add(1, Ordering::SeqCst);
        }
    }
}

/// Concrete [`Hint`] impl carrying the suggested hint body.
pub struct TextHint {
    content: String,
}

impl TextHint {
    pub fn new(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
        }
    }
}

#[async_trait::async_trait]
impl Hint for TextHint {
    fn name(&self) -> &str {
        "reflection"
    }

    fn should_trigger(&self, _state: &elysper_context::AgentState) -> bool {
        true
    }

    fn generate(&self, _state: &elysper_context::AgentState) -> HintMessage {
        HintMessage {
            content: self.content.clone(),
            priority: HintPriority::Normal,
        }
    }

    fn injection_point(&self) -> InjectionPoint {
        InjectionPoint::BeforeNextLlmCall
    }
}

impl ReflectionHook {
    /// Observe one event: record the tool call and, on the
    /// `every_n_steps` boundary, await the critic and stage a hint.
    ///
    /// Drain the staged hint with
    /// [`ReflectionHook::take_pending_hint`]; the host's hint
    /// dispatch asks for it before the next model call.
    pub async fn on_event(&self, event: &HookEvent) -> HookDecision {
        let HookEvent::AfterTool { tool_name, .. } = event else {
            return HookDecision::Allow;
        };
        let step = self.reflections.load(Ordering::SeqCst) + 1;
        self.append_history(HistoryEntry {
            tool_name: tool_name.clone(),
            arguments: serde_json::Value::Null,
            result_preview: String::new(),
        });
        self.maybe_reflect(step).await;
        HookDecision::Allow
    }
}

#[async_trait::async_trait]
impl Hook for ReflectionHook {
    fn name(&self) -> &str {
        "reflection"
    }

    async fn observe(
        &self,
        event: &HookEvent,
        _ctx: &HookContext<'_>,
    ) -> HookDecision {
        self.on_event(event).await
    }
}

/// Output transformer that wraps the final answer into a
/// "self-critique + revised answer" pair. Optional integration;
/// the harness does not require it.
pub struct ReflectiveOutputTransformer {
    pub critic_provider: Arc<dyn ModelProvider>,
}

#[async_trait::async_trait]
impl OutputTransformer for ReflectiveOutputTransformer {
    async fn transform(
        &self,
        output: String,
        _tool_name: &str,
        _state: &elysper_context::AgentState,
    ) -> String {
        // Skip the cost when the answer is short.
        if output.len() < 32 {
            return output;
        }
        let request = CompletionRequest {
            model: self.critic_provider.model_config().name.clone(),
            messages: Arc::new(vec![Message {
                role: Role::System,
                content: Content::Single(ContentPart::Text(TextContent {
                    text: "Reply with `OK <answer>` if the answer is fine, or `REVISE <new_answer>` if it should be improved.".into(),
                    cache_control: None,
                })),
                tool_call_id: None,
                name: None,
                tool_result_cleared_at: None,
            }, Message {
                role: Role::User,
                content: Content::Single(ContentPart::Text(TextContent {
                    text: format!("Answer: {output}\n\nCritique:"),
                    cache_control: None,
                })),
                tool_call_id: None,
                name: None,
                tool_result_cleared_at: None,
            }]),
            tools: Arc::new(Vec::new()),
            ..CompletionRequest::default()
        };
        let response = match self.critic_provider.complete(request).await {
            Ok(r) => r,
            Err(_) => return output,
        };
        let text = match &response.content {
            Content::Single(ContentPart::Text(t)) => t.text.clone(),
            _ => return output,
        };
        if let Some(rest) = text.strip_prefix("REVISE ") {
            rest.to_string()
        } else {
            output
        }
    }
}
