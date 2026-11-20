//! Host wiring: guards, hooks, sanitizers, hints, output
//! transformers, and the context-manager + chunk persistence
//! contracts.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use uva_context::AgentState;
use uva_core::SystemClock;
use uva_hook::{MountPoint, RegistrationOrigin};
use uva_provider::{ContentPart, TokenUsage};
use uva_session::{LogWriter, SessionSink, in_memory::InMemorySessionSink};
use uva_steering::{GuardResult, HookDecision, HookEvent};
use uva_test_support::FakeTool;
use uva_tool::ToolRegistry;

use super::{support::*, *};
use crate::agent::host::{Hook, HookContext, Host};

#[tokio::test]
async fn guard_denial_surfaces_as_error_result_without_a_live_warning() {
    let tool_use = ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    };
    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![tool_use.clone()]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(FakeTool::new(
        "echo", "echoed",
    ))));
    let mut host = Host::new();
    let _guard = host.guard(Arc::new(DenyEchoGuard));

    let events = run_and_collect_with_host(
        provider,
        registry,
        host,
        AgentInput::text("run echo"),
    )
    .await;

    let tool_results: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(tr)) => Some(tr.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(tool_results.len(), 1);
    assert!(tool_results[0].is_error.unwrap_or(false));
    let text = match &tool_results[0].content[0] {
        ContentPart::Text(t) => t.text.clone(),
        _ => panic!("expected text content"),
    };
    assert!(
        text.contains("[blocked by guard `deny_echo` (high)]")
            && text.contains("echo is disabled"),
        "denial text should carry guard name + reason, got: {text}"
    );

    // The veto's live `Warning{Guard}` is retired: the durable
    // `hook_block` row says the same veto and reaches every surface
    // through the log — keeping both would state one fact twice, in
    // two vocabularies. The retirement must be real: no Guard warning
    // fires, and no other warning replaces it.
    let warnings: Vec<_> = events
        .iter()
        .filter(|e| {
            matches!(e, AgentEvent::System(SystemEvent::Warning { .. }))
        })
        .collect();
    assert!(
        warnings.is_empty(),
        "a guard veto publishes no live warning — the `hook_block` row \
         is the fact — got {warnings:?}"
    );

    // The session still ends Completed — denials are
    // errors-as-results, never run aborts.
    assert!(matches!(
        events.last(),
        Some(AgentEvent::System(SystemEvent::SessionEnded {
            reason: SessionEndReason::Completed,
        }))
    ));
}

// A guard veto is a policy block too: the log keeps the guard's
// name and its reason, so a reloaded session says which policy
// refused the call.
#[tokio::test]
async fn a_guard_denial_writes_a_hook_block_row() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let tool_use = ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    };
    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![tool_use.clone()]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(FakeTool::new(
        "echo", "echoed",
    ))));
    let mut host = Host::new();
    let _guard = host.guard(Arc::new(DenyEchoGuard));

    let agent = ReActAgent::new(provider, registry)
        .with_host(host)
        .expect("bind the host")
        .with_log_writer(writer);
    let mut stream = agent
        .run(
            AgentInput::text("run echo"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read");
    let blocks: Vec<&Value> = rows
        .iter()
        .filter(|row| row["type"] == "hook_block")
        .collect();
    assert_eq!(
        blocks.len(),
        1,
        "the guard's veto writes exactly one hook_block row: {rows:#?}"
    );
    assert_eq!(blocks[0]["data"]["hook"], "deny_echo");
    assert_eq!(blocks[0]["data"]["reason"], "echo is disabled for this run");
    assert_eq!(blocks[0]["data"]["tool"], "echo");
    assert_eq!(blocks[0]["data"]["call_id"], "c1");
}

// The `hook_block` row is not a reload-only fact: it is queued at the
// veto and flushed with the veto's own tool result, so a client
// reading the log while the run is still going — a `follow`
// subscriber's row poll — already sees it. That is what makes the
// live timeline and the reloaded one show the same notice.
#[tokio::test]
async fn the_hook_block_row_is_on_disk_before_the_run_ends() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let tool_use = ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    };
    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![tool_use.clone()]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(FakeTool::new(
        "echo", "echoed",
    ))));
    let mut host = Host::new();
    let _guard = host.guard(Arc::new(DenyEchoGuard));

    let agent = ReActAgent::new(provider, registry)
        .with_host(host)
        .expect("bind the host")
        .with_log_writer(writer);
    let mut stream = agent
        .run(
            AgentInput::text("run echo"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    // Sample the log at the veto's own tool result — the row the
    // commit path flushes on its way out, one event before the run's
    // final boundary.
    let mut sampled: Option<Vec<Value>> = None;
    while let Some(event) = stream.next().await {
        if matches!(
            &event,
            AgentEvent::Model(ContentPart::ToolResult(result))
                if result.is_error.unwrap_or(false)
        ) && sampled.is_none()
        {
            sampled = Some(sink.read().await.expect("read the log mid-run"));
        }
    }
    let rows = sampled.expect("the veto's tool result event fired");
    assert!(
        rows.iter().any(|row| row["type"] == "hook_block"),
        "the hook_block row is durable while the run is live: {rows:#?}"
    );
}

// Pins the denial wire string and the veto-epilogue order: the tracker must see
// the denied call before `on_error` fans out, or nothing in-tree would notice.
#[tokio::test]
async fn hook_block_surfaces_as_error_result() {
    #[derive(Default)]
    struct CountingTracker {
        calls: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl uva_steering::tracker::Tracker for CountingTracker {
        fn on_iteration(&self, _state: &AgentState) {}

        fn on_tool_call(
            &self,
            _name: &str,
            _arguments: &Value,
            _state: &AgentState,
        ) {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }

        fn on_llm_response(&self, _usage: &TokenUsage, _state: &AgentState) {}

        fn recommended_concurrency(&self, _state: &AgentState) -> usize {
            1
        }
    }

    // Blocks the `echo` call before it dispatches.
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
                        reason: "echo requires user approval".to_string(),
                    }
                }
                _ => HookDecision::Allow,
            }
        }
    }

    // Records how many tracker calls the tracker had seen when the
    // tool result was observed.
    struct RecordTrackerCalls {
        tracker_calls: Arc<std::sync::atomic::AtomicUsize>,
        recorded: Arc<parking_lot::Mutex<Option<usize>>>,
    }

    #[async_trait]
    impl Hook for RecordTrackerCalls {
        fn name(&self) -> &str {
            "record-tracker-calls"
        }

        async fn observe(
            &self,
            event: &HookEvent,
            _ctx: &HookContext<'_>,
        ) -> HookDecision {
            if matches!(event, HookEvent::AfterTool { .. }) {
                let seen = self
                    .tracker_calls
                    .load(std::sync::atomic::Ordering::SeqCst);
                *self.recorded.lock() = Some(seen);
            }
            HookDecision::Allow
        }
    }

    let tracker_calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let recorded = Arc::new(parking_lot::Mutex::new(None));

    let tool_use = ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({}),
    };
    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![tool_use.clone()]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(FakeTool::new(
        "echo", "echoed",
    ))));
    let mut host = Host::new();
    let _blocker = host.hook_registry().register_hook_at(
        MountPoint::ToolPre,
        Arc::new(BlockEcho),
        RegistrationOrigin::InProcess,
    );
    let _recorder = host.hook_registry().register_hook_at(
        MountPoint::ToolPost,
        Arc::new(RecordTrackerCalls {
            tracker_calls: Arc::clone(&tracker_calls),
            recorded: Arc::clone(&recorded),
        }),
        RegistrationOrigin::InProcess,
    );
    let _tracker = host.tracker(Arc::new(CountingTracker {
        calls: Arc::clone(&tracker_calls),
    }));

    let events = run_and_collect_with_host(
        provider,
        registry,
        host,
        AgentInput::text("run echo"),
    )
    .await;

    let text = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(tr)) => {
                Some(tr.content.clone())
            }
            _ => None,
        })
        .next()
        .expect("tool result event");
    assert!(text.iter().any(|p| {
        matches!(p, ContentPart::Text(t)
                if t.text.contains("[blocked by policy]")
                    && t.text.contains("user approval"))
    }));

    let seen = recorded
        .lock()
        .expect("the block must have been observed by after_tool");
    assert!(
        seen >= 1,
        "the tracker must record the denied call before `after_tool` \
         observes it (saw {seen} recorded calls)"
    );
}

#[tokio::test]
async fn sanitized_action_is_adopted_for_dispatch() {
    struct RewriteToAlt;
    impl uva_steering::Guard for RewriteToAlt {
        fn name(&self) -> &str {
            "rewrite_to_alt"
        }

        fn check(
            &self,
            action: &uva_steering::Action,
            _state: &AgentState,
        ) -> GuardResult {
            match action {
                uva_steering::Action::ToolCall { name, arguments }
                    if name == "echo" =>
                {
                    GuardResult::Sanitize {
                        action: uva_steering::Action::ToolCall {
                            name: "alt".to_string(),
                            arguments: arguments.clone(),
                        },
                        warning: "routed to alt".to_string(),
                    }
                }
                _ => GuardResult::Allow,
            }
        }
    }

    let tool_use = ToolUse {
        id: "c1".to_string(),
        name: "echo".to_string(),
        input: json!({"x": 1}),
    };
    let provider = Arc::new(ScriptedStreamProvider::new(vec![
        tool_call_response(vec![tool_use.clone()]),
        empty_response(),
    ]));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(FakeTool::new(
        "echo", "echoed",
    ))));
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(FakeTool::new(
        "alt", "alt-ran",
    ))));
    let mut host = Host::new();
    let _guard = host.guard(Arc::new(RewriteToAlt));

    let events = run_and_collect_with_host(
        provider,
        registry,
        host,
        AgentInput::text("run echo"),
    )
    .await;

    let result_text = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Model(ContentPart::ToolResult(tr)) => {
                Some(tr.content.clone())
            }
            _ => None,
        })
        .next()
        .expect("tool result event");
    assert!(
        result_text.iter().any(|p| {
            matches!(p, ContentPart::Text(t) if t.text == "alt-ran")
        })
    );
}
