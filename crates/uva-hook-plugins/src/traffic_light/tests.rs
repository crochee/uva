//! Tests for the `traffic-light` gate.

use std::sync::Arc;

use serde_json::Value;
use uva_context::AgentState;
use uva_core::{AtomicCancelToken, CancelToken};
use uva_hook::{Gate, HookContext, HookDecision, HookEvent};
use uva_tool::{
    Context,
    Risk,
    Tool,
    ToolEntry,
    ToolOutput,
    registry::ToolRegistry,
};

use super::{TrafficLight, TrafficPolicy};

struct RiskTool {
    name: &'static str,
    risk: Risk,
}

#[async_trait::async_trait]
impl Tool for RiskTool {
    fn name(&self) -> &str {
        self.name
    }

    fn description(&self) -> &str {
        "test tool"
    }

    fn parameters(&self) -> Value {
        serde_json::json!({ "type": "object" })
    }

    fn risk(&self, _input: &Value) -> Risk {
        self.risk
    }

    async fn call(&self, _input: Value, _context: &Context) -> ToolOutput {
        ToolOutput::text("ran")
    }
}

fn registry_with(name: &'static str, risk: Risk) -> Arc<ToolRegistry> {
    let registry = Arc::new(ToolRegistry::new());
    let tool = Arc::new(RiskTool { name, risk });
    registry.register_entry(ToolEntry::new(tool));
    registry
}

fn before_tool(tool: &str) -> HookEvent {
    HookEvent::BeforeTool {
        tool_name: tool.to_string(),
        call_id: "c1".to_string(),
        arguments: serde_json::json!({}),
    }
}

fn context<'a>(
    state: &'a AgentState,
    cancel: &'a Arc<dyn CancelToken>,
) -> HookContext<'a> {
    HookContext { state, cancel }
}

#[tokio::test]
async fn a_high_risk_call_is_blocked_by_default() {
    let state = AgentState::with_window(128);
    let cancel = AtomicCancelToken::shared();
    let ctx = context(&state, &cancel);
    let gate = TrafficLight::new(registry_with("shell", Risk::High));

    let decision = gate
        .check(&before_tool("shell"), &ctx)
        .await
        .expect("gate answers");

    assert_eq!(
        decision,
        HookDecision::Block {
            reason: "traffic-light: `shell` is high risk".to_string(),
        }
    );
}

#[tokio::test]
async fn a_low_risk_call_passes() {
    let state = AgentState::with_window(128);
    let cancel = AtomicCancelToken::shared();
    let ctx = context(&state, &cancel);
    let gate = TrafficLight::new(registry_with("read", Risk::Low));

    let decision = gate
        .check(&before_tool("read"), &ctx)
        .await
        .expect("gate answers");

    assert_eq!(decision, HookDecision::Allow);
}

#[tokio::test]
async fn unknown_risk_passes_by_default_and_can_be_blocked() {
    let state = AgentState::with_window(128);
    let cancel = AtomicCancelToken::shared();
    let ctx = context(&state, &cancel);
    let registry = registry_with("third-party", Risk::Unknown);

    let permissive = TrafficLight::new(registry.clone());
    assert_eq!(
        permissive
            .check(&before_tool("third-party"), &ctx)
            .await
            .expect("gate answers"),
        HookDecision::Allow,
        "the default is fail-open for unannotated tools"
    );

    let strict = TrafficLight::with_policy(
        registry,
        TrafficPolicy::blocking([Risk::High, Risk::Unknown]),
    );
    assert!(matches!(
        strict
            .check(&before_tool("third-party"), &ctx)
            .await
            .expect("gate answers"),
        HookDecision::Block { .. }
    ));
}

#[tokio::test]
async fn an_unregistered_tool_is_unknown() {
    let gate = TrafficLight::new(registry_with("read", Risk::Low));
    assert_eq!(
        gate.risk_of("missing", &serde_json::json!({})),
        Risk::Unknown
    );
}

#[tokio::test]
async fn the_risk_is_read_from_the_call_arguments() {
    struct ArgSensitive;
    #[async_trait::async_trait]
    impl Tool for ArgSensitive {
        fn name(&self) -> &str {
            "guessable"
        }

        fn description(&self) -> &str {
            "test tool"
        }

        fn parameters(&self) -> Value {
            serde_json::json!({ "type": "object" })
        }

        fn risk(&self, input: &Value) -> Risk {
            if input["danger"] == true {
                Risk::High
            } else {
                Risk::Low
            }
        }

        async fn call(&self, _input: Value, _context: &Context) -> ToolOutput {
            ToolOutput::text("ran")
        }
    }

    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(ToolEntry::new(Arc::new(ArgSensitive)));

    let state = AgentState::with_window(128);
    let cancel = AtomicCancelToken::shared();
    let ctx = context(&state, &cancel);
    let gate = TrafficLight::new(registry);

    let benign = HookEvent::BeforeTool {
        tool_name: "guessable".to_string(),
        call_id: "c1".to_string(),
        arguments: serde_json::json!({ "danger": false }),
    };
    assert_eq!(
        gate.check(&benign, &ctx).await.expect("gate answers"),
        HookDecision::Allow
    );

    let dangerous = HookEvent::BeforeTool {
        tool_name: "guessable".to_string(),
        call_id: "c2".to_string(),
        arguments: serde_json::json!({ "danger": true }),
    };
    assert!(matches!(
        gate.check(&dangerous, &ctx).await.expect("gate answers"),
        HookDecision::Block { .. }
    ));
}

#[tokio::test]
async fn other_mount_points_pass_through() {
    let state = AgentState::with_window(128);
    let cancel = AtomicCancelToken::shared();
    let ctx = context(&state, &cancel);
    let gate = TrafficLight::new(registry_with("shell", Risk::High));
    let event = HookEvent::AfterTool {
        tool_name: "shell".to_string(),
        call_id: "c1".to_string(),
        is_error: false,
    };

    assert_eq!(
        gate.check(&event, &ctx).await.expect("gate answers"),
        HookDecision::Allow
    );
}

#[test]
fn the_gate_names_itself_for_the_hook_block_row() {
    let gate = TrafficLight::new(Arc::new(ToolRegistry::new()));
    assert_eq!(gate.name(), "traffic-light");
    assert_eq!(gate.policy().blocked(), &[Risk::High]);
}
