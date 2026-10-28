//! Reflection demo.
//!
//! A `ScriptedCritic` returns an `ERROR` critique on the first
//! tool call, then `INFO` on subsequent calls. The
//! `ReflectionHook` stages a hint on the first call and the demo
//! prints the staged hint body and reflection counters.
//!
//! Run with:
//! `cargo run --example reflection_demo -p elysper-reflection`

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use chrono::TimeZone;
use elysper_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    TextContent,
};
use elysper_reflection::{
    HookDecision,
    HookEvent,
    ReflectionHook,
    ReflectorConfig,
};

struct ScriptedCritic {
    replies: Vec<String>,
    pos: AtomicU64,
}

#[async_trait::async_trait]
impl ModelProvider for ScriptedCritic {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), elysper_core::Error> {
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
    ) -> Result<CompletionResponse, elysper_core::Error> {
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
            usage: elysper_provider::TokenUsage::default(),
            cached: false,
            stop_reason: None,
            replay_state: None,
        })
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let critic = Arc::new(ScriptedCritic {
        replies: vec![
            "ERROR drift detected at step 2 | focus on the task".to_string(),
            "INFO on track".to_string(),
        ],
        pos: AtomicU64::new(0),
    });
    let clock = Arc::new(elysper_core::FixedClock::new(
        chrono::Utc.with_ymd_and_hms(2026, 1, 1, 0, 0, 0).unwrap(),
    ));
    let hook = Arc::new(ReflectionHook::new(
        ReflectorConfig::default()
            .critic_provider(critic)
            .every_n_steps(1),
        clock,
    ));

    for step in 0..2 {
        let decision = hook
            .on_event(&HookEvent::AfterTool {
                tool_name: "read".into(),
                call_id: format!("c{step}"),
                is_error: false,
            })
            .await;
        assert_eq!(decision, HookDecision::Allow);
        if let Some(text) = hook.pending_hint_text() {
            println!("[step {step}] STAGED HINT: {text}");
        } else {
            println!("[step {step}] no hint");
        }
    }
    println!(
        "STATS: reflections={} hints_injected={}",
        hook.reflection_count(),
        hook.hint_count()
    );
    assert_eq!(hook.reflection_count(), 2);
    assert_eq!(hook.hint_count(), 1);
    println!("OK: drift caught at step 2");
    Ok(())
}
