//! Host wiring: guards, hooks, sanitizers, hints, output
//! transformers, and the context-manager + chunk persistence
//! contracts.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::json;
use uva_provider::ContentPart;
use uva_steering::{HookDecision, HookEvent};
use uva_test_support::FakeTool;
use uva_tool::ToolRegistry;

use super::{support::*, *};
use crate::agent::host::{Hook, HookContext, Host};

// A `before_run` `Terminate` ends the run before the first sample — the
// turn-stopping capability the previous vocabulary lacked.
#[tokio::test]
async fn terminate_before_run_ends_the_run_without_sampling() {
    struct PlanModeGate;

    #[async_trait]
    impl Hook for PlanModeGate {
        fn name(&self) -> &str {
            "plan-mode"
        }

        async fn observe(
            &self,
            _event: &HookEvent,
            _ctx: &HookContext<'_>,
        ) -> HookDecision {
            HookDecision::Terminate {
                reason: "plan mode".to_string(),
            }
        }
    }

    let mut host = Host::new();
    let _gate = host.hook(Arc::new(PlanModeGate));

    let provider =
        Arc::new(CapturingProvider::new(vec![done_chunk("never reached")]));
    let events = run_and_collect_with_host(
        provider.clone(),
        Arc::new(ToolRegistry::new()),
        host,
        AgentInput::text("hi"),
    )
    .await;

    let ended: Vec<&SessionEndReason> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
                Some(reason)
            }
            _ => None,
        })
        .collect();
    assert_eq!(ended.len(), 1, "exactly one SessionEnded");
    assert!(
        matches!(ended[0], SessionEndReason::Error(m) if m == "plan mode"),
        "the terminate reason must reach the terminal event: {ended:?}"
    );
    assert_eq!(
        provider
            .call_count
            .load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a before_run terminate must not sample"
    );
}

#[tokio::test]
async fn terminate_after_step_ends_the_run_after_commit() {
    use parking_lot::Mutex;

    struct StopAfterStep {
        seen: Arc<Mutex<Vec<u32>>>,
    }

    #[async_trait]
    impl Hook for StopAfterStep {
        fn name(&self) -> &str {
            "stop-after-step"
        }

        async fn observe(
            &self,
            event: &HookEvent,
            _ctx: &HookContext<'_>,
        ) -> HookDecision {
            match event {
                HookEvent::AfterStep { iteration, .. } => {
                    self.seen.lock().push(*iteration);
                    HookDecision::Terminate {
                        reason: "policy-stop".to_string(),
                    }
                }
                _ => HookDecision::Allow,
            }
        }
    }

    let seen: Arc<Mutex<Vec<u32>>> = Arc::new(Mutex::new(Vec::new()));
    let mut host = Host::new();
    let _stopper = host.hook(Arc::new(StopAfterStep {
        seen: Arc::clone(&seen),
    }));

    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "c1".to_string(),
            name: "echo".to_string(),
            input: json!({ "text": "hi" }),
        }]),
        done_chunk("never reached"),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(FakeTool::new(
        "echo", "echo ok",
    ))));
    let events = run_and_collect_with_host(
        provider.clone(),
        registry,
        host,
        AgentInput::text("hi"),
    )
    .await;

    let ended: Vec<&SessionEndReason> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
                Some(reason)
            }
            _ => None,
        })
        .collect();
    assert_eq!(ended.len(), 1, "exactly one SessionEnded: {events:?}");
    assert!(
        matches!(ended[0], SessionEndReason::Error(m) if m == "policy-stop"),
        "the after_step terminate reason must reach the terminal event"
    );
    assert_eq!(
        provider
            .call_count
            .load(std::sync::atomic::Ordering::SeqCst),
        1,
        "terminate after step 0 must not sample again"
    );
    assert_eq!(
        *seen.lock(),
        vec![0],
        "the hook must see iteration 0's payload once"
    );
}

#[tokio::test]
async fn after_step_no_hooks_run_completes_unchanged() {
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "c1".to_string(),
            name: "echo".to_string(),
            input: json!({ "text": "hi" }),
        }]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(FakeTool::new(
        "echo", "echo ok",
    ))));
    let events = run_and_collect_with_host(
        provider.clone(),
        registry,
        Host::new(),
        AgentInput::text("hi"),
    )
    .await;

    let ended: Vec<&SessionEndReason> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
                Some(reason)
            }
            _ => None,
        })
        .collect();
    assert_eq!(ended.len(), 1);
    assert!(matches!(ended[0], SessionEndReason::Completed));
    assert_eq!(
        provider
            .call_count
            .load(std::sync::atomic::Ordering::SeqCst),
        2,
        "no hooks: the loop samples the tool turn and the final turn"
    );
}

// Pins the `[blocked by policy]` denial wire string and that the model sees
// the body on its next request.
#[tokio::test]
async fn block_at_before_tool_vetoes_and_reaches_the_next_request() {
    struct BlockEcho;

    #[async_trait]
    impl Hook for BlockEcho {
        fn name(&self) -> &str {
            "block-echo"
        }

        async fn observe(
            &self,
            event: &HookEvent,
            _ctx: &HookContext<'_>,
        ) -> HookDecision {
            match event {
                HookEvent::BeforeTool { tool_name, .. }
                    if tool_name == "echo" =>
                {
                    HookDecision::Block {
                        reason: "echo is not allowed here".to_string(),
                    }
                }
                _ => HookDecision::Allow,
            }
        }
    }

    let mut host = Host::new();
    let _blocker = host.hook(Arc::new(BlockEcho));

    let tool_use = ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![tool_use]),
        done_chunk("blocked noted"),
    ]));
    let (registry, _executions) = echo_registry();

    let events = run_and_collect_with_host(
        provider.clone(),
        registry,
        host,
        AgentInput::text("run echo"),
    )
    .await;

    let committed = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(tr)) => Some(tr),
            _ => None,
        })
        .next()
        .expect("the vetoed call must still commit a tool result");
    assert_eq!(committed.is_error, Some(true));
    assert!(
        committed
            .content
            .iter()
            .any(|p| matches!(p, ContentPart::Text(t)
                if t.text.starts_with("[blocked by policy]"))),
        "the denial must carry the policy prefix: {:?}",
        committed.content
    );

    let captured = provider.captured.lock().await;
    let second = captured
        .get(1)
        .expect("the model must see the denial on the next request");
    let body = second
        .iter()
        .filter_map(|m| match &m.content {
            uva_provider::Content::Single(ContentPart::Text(t)) => {
                Some(t.text.clone())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        body.contains("[blocked by policy]"),
        "the next request must carry the denial body: {body}"
    );
}

// The success path must carry the real call id — `after_tool` used
// to hard-code an empty one, defeating observation correlation.
#[tokio::test]
async fn after_tool_success_carries_the_real_call_id() {
    use parking_lot::Mutex;

    struct RecordAfterTool {
        seen: Arc<Mutex<Vec<(String, String, bool)>>>,
    }

    #[async_trait]
    impl Hook for RecordAfterTool {
        fn name(&self) -> &str {
            "record-after-tool"
        }

        async fn observe(
            &self,
            event: &HookEvent,
            _ctx: &HookContext<'_>,
        ) -> HookDecision {
            if let HookEvent::AfterTool {
                tool_name,
                call_id,
                is_error,
            } = event
            {
                self.seen.lock().push((
                    tool_name.clone(),
                    call_id.clone(),
                    *is_error,
                ));
            }
            HookDecision::Allow
        }
    }

    let seen: Arc<Mutex<Vec<(String, String, bool)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let mut host = Host::new();
    let _recorder = host.hook(Arc::new(RecordAfterTool {
        seen: Arc::clone(&seen),
    }));

    let tool_use = ToolUse {
        id: "call_echo_1".to_string(),
        name: "echo".to_string(),
        input: json!({"text": "hi"}),
    };
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![tool_use]),
        done_chunk("done"),
    ]));
    let (registry, executions) = echo_registry();

    let _events = run_and_collect_with_host(
        provider,
        registry,
        host,
        AgentInput::text("run echo"),
    )
    .await;

    assert_eq!(*executions.lock(), 1, "the call must succeed");
    let captured = seen.lock();
    assert_eq!(
        captured.len(),
        1,
        "after_tool must fire exactly once for the batch"
    );
    assert_eq!(
        captured[0],
        ("echo".to_string(), "call_echo_1".to_string(), false),
        "the success-path AfterTool must carry the real call id"
    );
}
