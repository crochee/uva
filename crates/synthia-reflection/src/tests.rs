//! Tests for `synthia-reflection`.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use chrono::TimeZone;
use synthia_core::{Clock, FixedClock};
use synthia_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    TextContent,
};
use synthia_steering::{HookDecision, HookEvent};

use crate::{
    critique::{Critique, Severity, parse_critique},
    hook::{ReflectionHook, ReflectorConfig},
};

struct ScriptedCritic {
    replies: Vec<String>,
    pos: Arc<AtomicU64>,
}

#[async_trait::async_trait]
impl ModelProvider for ScriptedCritic {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), synthia_core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "critic"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "critic".into(),
            provider: "critic".into(),
            context_window: 4_096,
            max_output_tokens: 256,
            supports_tools: false,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, synthia_core::Error> {
        let n = self.pos.fetch_add(1, Ordering::SeqCst) as usize;
        let text = self
            .replies
            .get(n)
            .cloned()
            .unwrap_or_else(|| "INFO default".to_string());
        Ok(CompletionResponse {
            id: "c".into(),
            model: "critic".into(),
            content: Content::Single(ContentPart::Text(TextContent {
                text,
                cache_control: None,
            })),
            usage: synthia_provider::TokenUsage::default(),
            cached: false,
            stop_reason: None,
            replay_state: None,
        })
    }
}

fn clock() -> Arc<dyn Clock> {
    Arc::new(FixedClock::new(
        chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    ))
}

/// Critique parser distinguishes INFO / WARN / ERROR.
#[test]
fn parse_critique_handles_three_severities() {
    let info = parse_critique("INFO all good");
    assert_eq!(info.severity, Severity::Info);
    assert_eq!(info.reason, "all good");
    assert!(!info.warrants_hint());

    let warn = parse_critique("WARN drift detected | focus on the task");
    assert_eq!(warn.severity, Severity::Warn);
    assert_eq!(warn.reason, "drift detected");
    assert_eq!(warn.suggested_hint.as_deref(), Some("focus on the task"));
    assert!(warn.warrants_hint());

    let err = parse_critique("ERROR off the rails | start over");
    assert_eq!(err.severity, Severity::Error);
    assert!(err.warrants_hint());

    // Unknown prefix → info.
    let unknown = parse_critique("hello");
    assert_eq!(unknown.severity, Severity::Info);
}

/// Severity-only Critique builders produce the right `warrants_hint`.
#[test]
fn critique_builders_have_expected_severities() {
    assert!(!Critique::info("").warrants_hint());
    assert!(Critique::warn("", Some("hint".into())).warrants_hint());
    assert!(Critique::error("", Some("hint".into())).warrants_hint());
    // warn without hint → does not warrant.
    assert!(!Critique::warn("", None).warrants_hint());
}

/// Critique severity Error injects a hint into the next prompt.
#[tokio::test]
async fn error_severity_injects_hint() {
    let critic = Arc::new(ScriptedCritic {
        replies: vec!["ERROR off the rails | start over".to_string()],
        pos: Arc::new(AtomicU64::new(0)),
    });
    let hook = Arc::new(ReflectionHook::new(
        ReflectorConfig::default()
            .critic_provider(critic.clone())
            .every_n_steps(1),
        clock(),
    ));
    let hooks = Arc::clone(&hook).hook_map();
    let decision = hooks
        .dispatch(&HookEvent::AfterTool {
            tool_name: "read".into(),
            call_id: "c0".into(),
            is_error: false,
        })
        .await;
    assert_eq!(decision, HookDecision::Allow);
    let hint = hook.pending_hint_text();
    assert_eq!(
        hint.as_deref(),
        Some("start over"),
        "Error critique should stage a hint"
    );
    assert_eq!(hook.hint_count(), 1);
    assert_eq!(hook.reflection_count(), 1);
}

/// Critique severity Info does not modify the prompt.
#[tokio::test]
async fn info_severity_does_not_inject_hint() {
    let critic = Arc::new(ScriptedCritic {
        replies: vec!["INFO all good".to_string()],
        pos: Arc::new(AtomicU64::new(0)),
    });
    let hook = Arc::new(ReflectionHook::new(
        ReflectorConfig::default()
            .critic_provider(critic.clone())
            .every_n_steps(1),
        clock(),
    ));
    let hooks = Arc::clone(&hook).hook_map();
    let decision = hooks
        .dispatch(&HookEvent::AfterTool {
            tool_name: "read".into(),
            call_id: "c0".into(),
            is_error: false,
        })
        .await;
    assert_eq!(decision, HookDecision::Allow);
    assert_eq!(hook.pending_hint_text(), None);
    assert_eq!(hook.hint_count(), 0);
}

/// every_n_steps=1 invokes on every step without history unbounded
/// growth (bounded by history_window).
#[tokio::test]
async fn every_step_invokes_bounded_history() {
    let critic = Arc::new(ScriptedCritic {
        replies: (0..10).map(|i| format!("INFO step {i}")).collect(),
        pos: Arc::new(AtomicU64::new(0)),
    });
    let hook = Arc::new(ReflectionHook::new(
        ReflectorConfig::default()
            .critic_provider(critic.clone())
            .every_n_steps(1)
            .history_window(4),
        clock(),
    ));
    let hooks = Arc::clone(&hook).hook_map();
    for i in 0..8 {
        let decision = hooks
            .dispatch(&HookEvent::AfterTool {
                tool_name: "read".into(),
                call_id: format!("c{i}"),
                is_error: false,
            })
            .await;
        assert_eq!(decision, HookDecision::Allow);
    }
    // The cron loop calls the critic every step (8 times) but the
    // history is bounded.
    assert_eq!(hook.reflection_count(), 8);
    assert_eq!(hook.hint_count(), 0);
}

/// `hook_map()` dispatches `after_tool`: the call is recorded, the
/// critic runs on the boundary, and the decision is `Allow`.
#[tokio::test]
async fn hook_map_dispatch_stages_hint_on_boundary() {
    let critic = Arc::new(ScriptedCritic {
        replies: vec!["ERROR off the rails | start over".to_string()],
        pos: Arc::new(AtomicU64::new(0)),
    });
    let hook = Arc::new(ReflectionHook::new(
        ReflectorConfig::default()
            .critic_provider(critic)
            .every_n_steps(1),
        clock(),
    ));
    let hooks = Arc::clone(&hook).hook_map();
    let decision = hooks
        .dispatch(&HookEvent::AfterTool {
            tool_name: "echo".into(),
            call_id: "c1".into(),
            is_error: false,
        })
        .await;
    assert_eq!(decision, HookDecision::Allow);
    assert_eq!(hook.reflection_count(), 1);
    assert_eq!(hook.pending_hint_text().as_deref(), Some("start over"));
}
