use super::*;
use crate::types::{
    CompletionRequest,
    Content,
    TextContent,
    ToolChoice,
    ToolDefinition,
};

#[test]
fn cache_policy_default_aligns_with_opencode_auto() {
    let p = CachePolicy::default();
    assert!(p.tools);
    assert!(p.system);
    assert_eq!(p.messages, MessageCacheStrategy::LatestUserMessage);
    assert_eq!(p.ttl_seconds, None);
}

fn make_request_with_tools(n: usize) -> CompletionRequest {
    CompletionRequest {
        model: "claude-3".to_string(),
        messages: Arc::new(vec![]),
        tools: Arc::new(
            (0..n)
                .map(|i| {
                    ToolDefinition::new(
                        format!("tool_{i}"),
                        format!("Tool {i}"),
                        serde_json::json!({"type": "object"}),
                    )
                })
                .collect(),
        ),
        tool_choice: ToolChoice::Auto,
        temperature: None,
        max_tokens: None,
        stop_sequences: vec![],
        extra_body: None,
        cache_policy: None,
        replay_state: None,
    }
}

#[test]
fn apply_policy_marks_last_tool_only() {
    let mut req = make_request_with_tools(3);
    let policy = CachePolicy {
        tools: true,
        system: false,
        messages: MessageCacheStrategy::None,
        ttl_seconds: None,
    };
    apply_cache_policy(&mut req, &policy);
    assert!(req.tools[0].cache_control.is_none());
    assert!(req.tools[1].cache_control.is_none());
    assert!(req.tools[2].cache_control.is_some());
}

#[test]
fn apply_policy_empty_tools_is_noop() {
    let mut req = make_request_with_tools(0);
    let policy = CachePolicy {
        tools: true,
        system: false,
        messages: MessageCacheStrategy::None,
        ttl_seconds: None,
    };
    apply_cache_policy(&mut req, &policy);
    assert!(req.tools.is_empty());
}

#[test]
fn apply_policy_all_disabled_is_noop() {
    let mut req = make_request_with_tools(3);
    let original = req.clone();
    let policy = CachePolicy {
        tools: false,
        system: false,
        messages: MessageCacheStrategy::None,
        ttl_seconds: None,
    };
    apply_cache_policy(&mut req, &policy);
    assert_eq!(req.tools, original.tools);
}

#[test]
fn apply_policy_is_idempotent() {
    let mut req = make_request_with_tools(3);
    let policy = CachePolicy::default();
    apply_cache_policy(&mut req, &policy);
    let after_first = req.clone();
    apply_cache_policy(&mut req, &policy);
    assert_eq!(req.tools, after_first.tools);
}

#[test]
fn apply_policy_marks_last_user_message_single_content() {
    let mut req = make_request_with_tools(0);
    req.messages = Arc::new(vec![
        Message::user("hi"),
        Message::assistant("hello"),
        Message::user("bye"),
    ]);
    let policy = CachePolicy {
        tools: false,
        system: false,
        messages: MessageCacheStrategy::LatestUserMessage,
        ttl_seconds: None,
    };
    apply_cache_policy(&mut req, &policy);
    // Last user message is messages[2]; its TextContent is marked.
    if let Content::Single(ContentPart::Text(tc)) = &req.messages[2].content {
        assert!(tc.cache_control.is_some());
    } else {
        panic!("expected Single(Text)");
    }
    // Earlier user message must NOT be marked.
    if let Content::Single(ContentPart::Text(tc)) = &req.messages[0].content {
        assert!(tc.cache_control.is_none());
    } else {
        panic!("expected Single(Text)");
    }
}

#[test]
fn apply_policy_marks_last_user_message_multi_content() {
    let mut req = make_request_with_tools(0);
    req.messages = Arc::new(vec![Message::new(
        Role::User,
        Content::Multi(vec![
            ContentPart::Text(TextContent {
                text: "first".into(),
                cache_control: None,
            }),
            ContentPart::Text(TextContent {
                text: "second".into(),
                cache_control: None,
            }),
        ]),
    )]);
    let policy = CachePolicy {
        tools: false,
        system: false,
        messages: MessageCacheStrategy::LatestUserMessage,
        ttl_seconds: None,
    };
    apply_cache_policy(&mut req, &policy);
    if let Content::Multi(parts) = &req.messages[0].content {
        assert!(parts[0].cache_control().is_none());
        assert!(parts[1].cache_control().is_some());
    } else {
        panic!("expected Multi");
    }
}

/// Empty tool list must not panic or insert a phantom
/// `cache_control`. `tools.last_mut()` on an empty
/// vector returns `None`, so the marker branch is
/// skipped silently. This pins the safety contract so
/// future refactors (e.g. adding a "first tool" branch)
/// do not silently regress agent loop startup when an
/// agent has no registered tools.
#[test]
fn cache_policy_with_empty_tool_list_is_safe_noop() {
    use std::sync::Arc;
    let mut req = make_request_with_tools(0);
    let policy = CachePolicy::default();
    apply_cache_policy(&mut req, &policy);
    assert!(
        req.tools.is_empty(),
        "empty tool list must remain empty after apply_cache_policy"
    );
    // Specifically: no panic, no phantom entry, no
    // `cache_control` reference created.
    assert_eq!(
        Arc::strong_count(&req.tools),
        1,
        "the tool Arc must not have been duplicated by a no-op"
    );
}

/// `CachePolicyApplier::apply` is the
/// stateful short-circuit path: when both
/// `tools` and `messages` Arc references are
/// ptr-equal to the previous call, the
/// function returns `true` and skips
/// re-application. This is the contract that
/// keeps the provider's prompt cache prefix
/// valid across multiple agent-loop turns.
///
/// No existing test exercises this path —
/// `apply_cache_policy` (the free function) is
/// always called, so the short-circuit logic
/// is dead code as far as CI is concerned.
/// Pin the 4 critical invariants.
fn messages_one_user() -> Arc<Vec<Message>> {
    Arc::new(vec![Message::user("hi")])
}

#[test]
fn applier_first_call_returns_false_and_stores_state() {
    let mut applier = CachePolicyApplier::new();
    let mut req = make_request_with_tools(2);
    req.messages = messages_one_user();
    let policy = CachePolicy::default();
    let short_circuited = applier.apply(&mut req, &policy);
    assert!(
        !short_circuited,
        "first call MUST return false (full evaluation path)"
    );
}

#[test]
fn applier_second_call_same_arcs_short_circuits() {
    let mut applier = CachePolicyApplier::new();
    let mut req = make_request_with_tools(2);
    req.messages = messages_one_user();
    let policy = CachePolicy::default();
    // First call — full evaluation.
    applier.apply(&mut req, &policy);
    // Second call — same Arc pointers — short-circuit.
    let short_circuited = applier.apply(&mut req, &policy);
    assert!(
        short_circuited,
        "second call with identical Arc pointers MUST short-circuit"
    );
}

#[test]
fn applier_second_call_with_only_tools_changed_re_evaluates() {
    let mut applier = CachePolicyApplier::new();
    let mut req = make_request_with_tools(2);
    req.messages = messages_one_user();
    let policy = CachePolicy::default();
    applier.apply(&mut req, &policy);
    // Re-build request with NEW tools Arc but
    // SAME messages Arc (via Arc::clone).
    let original_messages = Arc::clone(&req.messages);
    req.tools = Arc::new(vec![ToolDefinition::new(
        "new_tool".to_string(),
        "New Tool".to_string(),
        serde_json::json!({"type": "object"}),
    )]);
    req.messages = original_messages;
    let short_circuited = applier.apply(&mut req, &policy);
    assert!(
        !short_circuited,
        "tools Arc changed — short-circuit MUST NOT fire"
    );
}

#[test]
fn applier_second_call_with_only_messages_changed_re_evaluates() {
    let mut applier = CachePolicyApplier::new();
    let mut req = make_request_with_tools(2);
    req.messages = messages_one_user();
    let policy = CachePolicy::default();
    applier.apply(&mut req, &policy);
    // Re-build request with NEW messages Arc but
    // SAME tools Arc (via Arc::clone).
    let original_tools = Arc::clone(&req.tools);
    req.messages = Arc::new(vec![Message::user("bye")]);
    req.tools = original_tools;
    let short_circuited = applier.apply(&mut req, &policy);
    assert!(
        !short_circuited,
        "messages Arc changed — short-circuit MUST NOT fire"
    );
}

#[test]
fn applier_second_call_with_both_arcs_changed_re_evaluates() {
    let mut applier = CachePolicyApplier::new();
    let mut req = make_request_with_tools(2);
    req.messages = messages_one_user();
    let policy = CachePolicy::default();
    applier.apply(&mut req, &policy);
    // Both Arcs replaced with brand-new allocations.
    req.tools = Arc::new(vec![ToolDefinition::new(
        "fresh_tool".to_string(),
        "Fresh".to_string(),
        serde_json::json!({"type": "object"}),
    )]);
    req.messages = Arc::new(vec![Message::user("goodbye")]);
    let short_circuited = applier.apply(&mut req, &policy);
    assert!(
        !short_circuited,
        "both Arcs changed — short-circuit MUST NOT fire"
    );
}

#[test]
fn applier_short_circuit_does_not_double_mark_tools() {
    // The most subtle invariant: short-circuit
    // returns `true` (signaling "marks already
    // present") and must NOT re-apply. If a
    // refactor accidentally re-applies during
    // short-circuit, the second call would
    // overwrite the mark on the last tool with
    // a fresh one (still a mark, but wastes
    // work and could matter if future code
    // distinguishes mark identity, e.g. for
    // cache invalidation).
    let mut applier = CachePolicyApplier::new();
    let mut req = make_request_with_tools(2);
    req.messages = messages_one_user();
    let policy = CachePolicy::default();
    applier.apply(&mut req, &policy);
    // Capture the mark on the last tool.
    let mark_after_first = req.tools.last().unwrap().cache_control.clone();
    // Second call — short-circuit.
    let short_circuited = applier.apply(&mut req, &policy);
    assert!(short_circuited);
    let mark_after_second = req.tools.last().unwrap().cache_control.clone();
    assert_eq!(
        mark_after_first, mark_after_second,
        "short-circuit MUST NOT mutate existing marks"
    );
}
