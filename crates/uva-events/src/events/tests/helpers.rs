//! Shared fixtures for the [`events`](super) unit tests.
//! Every other test file pulls its `make_*` / `all_system_events`
//! helpers from here so each concern can stay focused on its
//! own contract.

use uva_provider::{
    ContentPart,
    ImageContent,
    ReasoningContent,
    ResourceLink,
    SamplingResult,
    TextContent,
    TokenUsage,
    ToolResult,
    ToolUse,
};

use super::super::{SessionEndReason, SystemEvent, WarningKind};

pub(super) fn sampling_result_for_test() -> SamplingResult {
    SamplingResult {
        text: "hi".to_string(),
        tool_calls: vec![],
        reasoning: String::new(),
        reasoning_signature: None,
        usage: TokenUsage::default(),
        ..Default::default()
    }
}

pub(super) fn make_text(text: &str) -> ContentPart {
    ContentPart::Text(TextContent {
        text: text.to_string(),
        cache_control: None,
    })
}

pub(super) fn make_reasoning(text: &str) -> ContentPart {
    ContentPart::Reasoning(ReasoningContent {
        text: text.to_string(),
        signature: None,
    })
}

pub(super) fn make_image() -> ContentPart {
    ContentPart::Image(ImageContent {
        data: String::new(),
        mime_type: "image/png".to_string(),
        detail: None,
    })
}

pub(super) fn make_tool_use() -> ContentPart {
    ContentPart::ToolUse(ToolUse {
        id: "call-1".to_string(),
        name: "bash".to_string(),
        input: serde_json::json!({}),
    })
}

pub(super) fn make_tool_result() -> ContentPart {
    ContentPart::ToolResult(ToolResult {
        tool_use_id: "call-1".to_string(),
        tool_name: None,
        content: vec![],
        structured_content: None,
        is_error: None,
        metadata: Default::default(),
        truncated_by: None,
    })
}

pub(super) fn make_resource() -> ContentPart {
    ContentPart::Resource(ResourceLink {
        uri: "file:///tmp/x".to_string(),
        name: "x".to_string(),
        title: None,
        description: None,
        mime_type: None,
    })
}

/// Every `SystemEvent` variant (the suite's fixture source of
/// truth — the count assertions below pin it at 12).
pub(super) fn all_system_events() -> Vec<SystemEvent> {
    vec![
        SystemEvent::SessionStarted {
            session_id: "s".into(),
        },
        SystemEvent::SessionEnded {
            reason: SessionEndReason::Completed,
        },
        SystemEvent::SessionInterrupted {
            reason: "user-cancel".into(),
        },
        SystemEvent::Progress {
            message: "m".into(),
            step: 1,
            total: 5,
        },
        SystemEvent::ToolProgress {
            tool_name: "bash".into(),
            call_id: "c1".into(),
            output: uva_tool::ToolOutput::text("hello"),
        },
        SystemEvent::Warning {
            kind: WarningKind::Loop,
            message: "loop detected".into(),
            iteration: Some(3),
        },
        SystemEvent::Usage {
            input_tokens: 100,
            output_tokens: 50,
            cache_read_tokens: Some(20),
            cache_creation_tokens: None,
        },
        SystemEvent::Suspended(uva_approval::Suspension::AwaitingApproval {
            interrupts: Vec::new(),
            resume: uva_approval::ResumeToken::new(
                7,
                vec!["c1".to_string()],
                "hash",
            ),
        }),
        SystemEvent::CompactionStarted {
            reason: "context_pressure".into(),
            start: 0,
            end: 2,
            first_seq: Some(1),
            last_seq: Some(2),
        },
        SystemEvent::CompactionFinished {
            reason: "context_pressure".into(),
            shadowed_messages: 2,
            shadowed_tokens: 400,
            outcome: uva_session::CompactionLifecycleOutcome::Committed,
            first_seq: Some(1),
            last_seq: Some(2),
        },
        SystemEvent::EventsDropped { count: 3 },
        SystemEvent::Plugin {
            kind: "demo".into(),
            payload: serde_json::json!({"ok": true}),
        },
    ]
}
