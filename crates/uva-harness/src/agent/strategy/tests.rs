//! Tests for the reasoning-strategy seam: the registry
//! contract (name-keyed resolution, duplicate registration)
//! and the seam-level behavior (foreign strategies, runtime
//! derivations, `StrategyRegistry::resolve`).

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use futures::{StreamExt as _, channel::mpsc};
use uva_provider::ModelProvider;

use super::*;
use crate::{
    agent::{caps::AgentRuntime, sink::EventSink},
    events::{AgentEvent, SessionEndReason, SystemEvent},
    input::AgentInput,
};

struct Counting {
    name: String,
    calls: AtomicUsize,
}

#[async_trait]
impl ReasoningStrategy for Counting {
    fn name(&self) -> &str {
        &self.name
    }

    async fn run(
        &self,
        _runtime: AgentRuntime,
        _input: AgentInput,
        _sink: &EventSink,
    ) {
        self.calls.fetch_add(1, Ordering::SeqCst);
    }
}

fn make(name: &str) -> Arc<dyn ReasoningStrategy> {
    Arc::new(Counting {
        name: name.to_string(),
        calls: AtomicUsize::new(0),
    })
}

#[test]
fn register_then_get_returns_the_strategy() {
    let reg = StrategyRegistry::new();
    let prev = reg.register("x", make("x"));
    assert!(prev.is_none());
    assert_eq!(reg.names(), vec!["x"]);
    assert!(reg.resolve("x").is_ok());
}

#[test]
fn register_returns_the_previous_binding_on_overwrite() {
    let reg = StrategyRegistry::new();
    assert!(reg.register("k", make("a")).is_none());
    let prev = reg.register("k", make("b"));
    assert!(prev.is_some());
}

#[test]
fn names_are_sorted_for_deterministic_listing() {
    let reg = StrategyRegistry::new();
    for name in ["zeta", "alpha", "mu"] {
        reg.register(name, make(name));
    }
    assert_eq!(reg.names(), vec!["alpha", "mu", "zeta"]);
}

#[test]
fn resolve_unknown_returns_validation_error_naming_the_strategy() {
    let reg = StrategyRegistry::new();
    reg.register("alpha", make("alpha"));
    match reg.resolve("mcts") {
        Ok(_) => panic!("mcts should not resolve"),
        Err(err) => {
            let message = err.to_string();
            assert!(
                message.contains("mcts"),
                "error must name the rejected strategy: {message}"
            );
            assert!(
                message.contains("alpha"),
                "error must list the registered names: {message}"
            );
        }
    }
}

#[test]
fn resolve_unknown_returns_uva_core_error_validation() {
    let reg = StrategyRegistry::new();
    match reg.resolve("missing") {
        Err(Error::Validation { .. }) => {}
        Err(_) => panic!("expected Validation envelope"),
        Ok(_) => panic!("missing should not resolve"),
    }
}

// ===== seam level =====

// `"null"` and the model config are pinned by `runtime_derives_model_config` and `runtime_debug`.
struct NullProvider;

#[async_trait]
impl ModelProvider for NullProvider {
    async fn initialize(
        &mut self,
        _config: uva_provider::ProviderConfig,
    ) -> Result<(), uva_core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "null"
    }

    fn model_config(&self) -> uva_provider::ModelConfig {
        uva_provider::ModelConfig {
            name: "null-1".into(),
            provider: "null".into(),
            context_window: 8_192,
            max_output_tokens: 1_024,
            supports_tools: true,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: uva_provider::CompletionRequest,
    ) -> Result<uva_provider::CompletionResponse, uva_core::Error> {
        Ok(uva_provider::CompletionResponse {
            content: uva_provider::Content::text("null"),
            ..uva_provider::CompletionResponse::default()
        })
    }
}

fn caps() -> AgentCaps {
    let provider: Arc<dyn ModelProvider> = Arc::new(NullProvider);
    let mut caps = AgentCaps::for_provider(provider);
    caps.model.descriptor.name = "seam".into();
    caps.model.descriptor.kind = "seam".into();
    caps
}

#[test]
fn a_foreign_strategy_runs_against_the_seam() {
    struct Counting;

    #[async_trait]
    impl ReasoningStrategy for Counting {
        fn name(&self) -> &str {
            "counting"
        }

        async fn run(
            &self,
            runtime: AgentRuntime,
            _input: AgentInput,
            sink: &EventSink,
        ) {
            sink.emit_typed(uva_session::step_start(0, 1));
            sink.emit(AgentEvent::text_delta(format!(
                "{} tools are available",
                runtime.caps.tools.registry.tool_count()
            )));
            sink.emit(AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Completed,
            }));
        }
    }

    let strategy: Arc<dyn ReasoningStrategy> = Arc::new(Counting);
    assert_eq!(strategy.name(), "counting");

    let (tx, mut rx) = mpsc::unbounded();
    let sink = EventSink::new(Arc::new(tx), None);
    let caps = caps();
    let runtime = AgentRuntime {
        caps: Arc::new(caps),
        cancel: uva_core::AtomicCancelToken::shared(),
        subagent_depth: 0,
    };
    let seen = AtomicUsize::new(0);
    futures::executor::block_on(async {
        strategy.run(runtime, AgentInput::text("go"), &sink).await;
        // Drop the sink (and its `Arc<UnboundedSender>`) so the
        // receiver side closes once we have read everything the
        // strategy emitted.
        drop(sink);
        while rx.next().await.is_some() {
            seen.fetch_add(1, Ordering::SeqCst);
        }
    });
    assert_eq!(
        seen.load(Ordering::SeqCst),
        2,
        "the text delta and the terminal event must both be published"
    );
}

#[test]
fn emit_after_the_consumer_left_is_a_no_op() {
    let (tx, rx) = mpsc::unbounded();
    let sink = EventSink::new(Arc::new(tx), None);
    drop(rx);
    sink.emit(AgentEvent::text_delta("into the void"));
}

#[test]
fn runtime_derives_model_config_and_system_prompt() {
    let mut caps = caps();
    caps.model.descriptor.instructions = "answer in one line".into();
    assert_eq!(caps.model.provider.model_config().context_window, 8_192);
    let prompt = caps.model.prompt_context.assemble(&caps.model.descriptor);
    assert!(
        prompt.contains("answer in one line"),
        "the descriptor's instructions must reach the prompt: {prompt}"
    );
}

#[test]
fn runtime_debug_names_the_agent_and_provider() {
    let caps = caps();
    let runtime = AgentRuntime {
        caps: Arc::new(caps),
        cancel: uva_core::AtomicCancelToken::shared(),
        subagent_depth: 0,
    };
    let text = format!("{runtime:?}");
    assert!(text.contains("seam"), "{text}");
    assert!(text.contains("null"), "{text}");
}

#[test]
fn resolve_finds_the_builtin_with_an_empty_registry() {
    let registry = StrategyRegistry::new();
    assert_eq!(
        registry.resolve("react").unwrap().name(),
        "react",
        "the built-in must resolve without a register call"
    );
    assert_eq!(registry.resolve("re-act").unwrap().name(), "react");
    assert_eq!(registry.resolve("ReAct").unwrap().name(), "react");
}

#[test]
fn a_registered_name_shadows_the_builtin() {
    let registry = StrategyRegistry::new();
    registry.register("react", Arc::new(CustomStrategy));
    assert_eq!(
        registry.resolve("react").unwrap().name(),
        "custom",
        "a registration must shadow the built-in"
    );
}

#[test]
fn resolve_names_the_registered_entries_on_a_miss() {
    let registry = StrategyRegistry::new();
    registry.register("cot", Arc::new(CustomStrategy));
    let err = registry
        .resolve("nope")
        .err()
        .expect("nope must not resolve");
    let message = err.to_string();
    assert!(message.contains("nope"), "{message}");
    assert!(
        message.contains("cot"),
        "the error must name the registered entries: {message}"
    );
}

struct CustomStrategy;

#[async_trait::async_trait]
impl ReasoningStrategy for CustomStrategy {
    fn name(&self) -> &'static str {
        "custom"
    }

    async fn run(
        &self,
        _runtime: AgentRuntime,
        _input: crate::AgentInput,
        _sink: &crate::EventSink,
    ) {
    }
}
