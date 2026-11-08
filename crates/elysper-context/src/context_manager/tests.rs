use elysper_provider::{Content, ContentPart, TextContent};

use super::*;

fn sys(text: &str) -> Message {
    Message::new(Role::System, Content::text(text))
}

fn user(text: &str) -> Message {
    Message::new(Role::User, Content::text(text))
}

fn assistant(text: &str) -> Message {
    Message::new(Role::Assistant, Content::text(text))
}

fn tool_result(id: &str, text: &str) -> Message {
    Message::new(
        Role::Tool,
        Content::Single(ContentPart::ToolResult(
            elysper_provider::ToolResult {
                tool_use_id: id.to_string(),
                tool_name: None,
                content: vec![ContentPart::Text(TextContent {
                    text: text.to_string(),
                    cache_control: None,
                })],
                structured_content: None,
                is_error: None,
                metadata: serde_json::Map::new(),
                truncated_by: None,
            },
        )),
    )
}
#[tokio::test]
async fn truncating_drops_oldest_user_assistant_pair() {
    // Token math (4 ASCII bytes ≈ 1 token + 5% overhead):
    // - sys: 21 chars → 5 + 0 = 5 tokens
    // - one user+assistant pair of 80 chars each → 20 + 1 + 20 + 1 = 42 tokens
    // A 50-token window drops exactly one pair: 5 + 42 = 47 ≤ 50,
    // so the second pair survives.
    let mut msgs = vec![
        sys("you are elysper, helpful"),
        user(&"q1".repeat(40)),      // 80 chars
        assistant(&"a1".repeat(40)), // 80 chars
        user(&"q2".repeat(40)),      // 80 chars
        assistant(&"a2".repeat(40)), // 80 chars
    ];
    let mut state = AgentState::with_window(50);

    TruncatingContextManager
        .prepare(&mut msgs, &mut state)
        .await;

    assert!(state.last_truncated, "flag must reflect the eviction");
    assert!(state.estimated_tokens <= state.context_window);
    // System + second user/assistant pair survives.
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[0].role, Role::System);
    assert_eq!(msgs[1].role, Role::User);
    assert_eq!(msgs[2].role, Role::Assistant);
}

#[tokio::test]
async fn truncating_never_drops_system_messages() {
    // System prompt alone already exceeds the window; the
    // manager must still keep it and report overflow.
    let mut msgs = vec![sys(&"S".repeat(200)), user("q")];
    let mut state = AgentState::with_window(10);

    TruncatingContextManager
        .prepare(&mut msgs, &mut state)
        .await;

    assert_eq!(msgs[0].role, Role::System);
    assert!(state.estimated_tokens > state.context_window);
    assert!(
        state.last_truncated,
        "user message must still be dropped even when the system prompt itself overflows"
    );
}

#[tokio::test]
async fn truncating_leaves_under_limit_history_untouched() {
    let mut msgs = vec![sys("you are elysper"), user("hi"), assistant("hello")];
    let before = msgs.len();
    let mut state = AgentState::with_window(1_000_000);

    TruncatingContextManager
        .prepare(&mut msgs, &mut state)
        .await;

    assert_eq!(msgs.len(), before, "no eviction when window is huge");
    assert!(!state.last_truncated);
    assert!(state.estimated_tokens <= state.context_window);
}

#[tokio::test]
async fn truncating_keeps_orphaned_tool_results_when_no_assistant_follows() {
    // A trailing tool_result without a subsequent assistant
    // message (e.g. session was cancelled mid-tool) should be
    // preserved — the manager never invents user/assistant
    // pairs out of tool messages.
    let mut msgs = vec![
        sys("sys"),
        user("do thing"),
        assistant("doing"),
        tool_result("call-1", "result"),
    ];
    let mut state = AgentState::with_window(1_000_000);

    TruncatingContextManager
        .prepare(&mut msgs, &mut state)
        .await;

    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[3].role, Role::Tool);
}

#[tokio::test]
async fn noop_leaves_messages_alone() {
    // Inputs sized so the estimator returns >0: 50-char system
    // prompt → 12 tokens + 0 overhead = 12.
    let mut msgs = vec![
        sys(&"S".repeat(50)),
        user(&"U".repeat(50)),
        assistant(&"A".repeat(50)),
    ];
    let before = msgs.clone();
    let mut state = AgentState::with_window(10);

    NoopContextManager.prepare(&mut msgs, &mut state).await;

    assert_eq!(msgs, before);
    assert!(!state.last_truncated);
    assert!(state.estimated_tokens > 0);
}

#[test]
fn estimate_tokens_matches_helper_for_text_only() {
    // The trait's default `estimate_tokens` delegates to the
    // shared helper; this test pins that delegation so a
    // refactor of the helper cannot silently change the
    // behaviour every manager inherits.
    let msgs = vec![sys("system prompt here")];
    let via_trait = NoopContextManager.estimate_tokens(&msgs);
    let via_helper =
        elysper_provider::token_counter::estimate_messages_token_count(&msgs);
    assert_eq!(via_trait, via_helper);
}

#[test]
fn agent_state_utilisation_handles_zero_window() {
    let s = AgentState::with_window(0);
    assert_eq!(s.context_utilization(), 0.0);
}

#[test]
fn agent_state_from_config_copies_window() {
    let cfg = ModelConfig {
        name: "test-model".to_string(),
        provider: "openai".to_string(),
        context_window: 128_000,
        max_output_tokens: 4096,
        supports_tools: true,
        supports_streaming: true,
        supports_reasoning: false,
    };
    let s = AgentState::from_config(&cfg);
    assert_eq!(s.context_window, 128_000);
    assert_eq!(s.estimated_tokens, 0);
    assert!(!s.last_truncated);
    // Steering fields start neutral for every run.
    assert_eq!(s.total_tokens, 0);
    assert_eq!(s.tool_call_count, 0);
    assert!(s.recent_tool_fingerprints.is_empty());
}

/// `record_tool_call` MUST bump the counter, append the
/// fingerprint, and cap the window at [`FINGERPRINT_WINDOW`]
/// (oldest entries evicted first).
#[test]
fn record_tool_call_caps_fingerprint_window() {
    let mut s = AgentState::with_window(1000);
    for i in 0..(FINGERPRINT_WINDOW + 4) {
        s.record_tool_call(format!("call-{i}"));
    }
    assert_eq!(s.tool_call_count, FINGERPRINT_WINDOW + 4);
    assert_eq!(s.recent_tool_fingerprints.len(), FINGERPRINT_WINDOW);
    assert_eq!(
        s.recent_tool_fingerprints.first().map(String::as_str),
        Some("call-4")
    );
}

/// `identical_tail_run` MUST count the trailing run of
/// identical fingerprints and report 0 on an empty window.
#[test]
fn identical_tail_run_counts_trailing_duplicates() {
    let mut s = AgentState::with_window(1000);
    assert_eq!(s.identical_tail_run(), 0);
    s.record_tool_call("a".to_string());
    s.record_tool_call("b".to_string());
    s.record_tool_call("b".to_string());
    s.record_tool_call("b".to_string());
    assert_eq!(s.identical_tail_run(), 3);
}

/// `add_token_usage` MUST accumulate totals across responses
/// and saturate instead of overflowing.
#[test]
fn add_token_usage_accumulates_and_saturates() {
    let mut s = AgentState::with_window(1000);
    s.add_token_usage(&TokenUsage {
        prompt_tokens: 100,
        completion_tokens: 50,
        total_tokens: 150,
        cached_prompt_tokens: None,
        cache_read_tokens: Some(30),
        cache_write_tokens: Some(10),
        reasoning_tokens: None,
    });
    s.add_token_usage(&TokenUsage {
        prompt_tokens: 200,
        completion_tokens: 10,
        total_tokens: 210,
        cached_prompt_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: Some(7),
    });
    assert_eq!(s.total_tokens, 360);
    // The meter carries the per-category split.
    assert_eq!(s.usage_meter.input_tokens, 300);
    assert_eq!(s.usage_meter.output_tokens, 60);
    assert_eq!(s.usage_meter.cache_read_tokens, 30);
    assert_eq!(s.usage_meter.cache_write_tokens, 10);
    assert_eq!(s.usage_meter.reasoning_tokens, 7);
    assert_eq!(s.usage_meter.calls, 2);
    assert!((s.usage_meter.cache_hit_ratio() - 0.1).abs() < 1e-6);
    s.add_token_usage(&TokenUsage {
        prompt_tokens: usize::MAX,
        completion_tokens: usize::MAX,
        total_tokens: usize::MAX,
        cached_prompt_tokens: None,
        cache_read_tokens: None,
        cache_write_tokens: None,
        reasoning_tokens: None,
    });
    assert_eq!(s.total_tokens, usize::MAX);
    assert_eq!(s.usage_meter.input_tokens, usize::MAX);
    assert_eq!(s.total_tokens, usize::MAX);
    assert_eq!(s.usage_meter.input_tokens, usize::MAX);
}

#[tokio::test]
async fn noop_prepare_arc_returns_input_arc_unchanged() {
    // R6-6 contract: when the manager does not need to mutate
    // the list, it must hand the input `Arc` back so the
    // provider-side cache policy's `Arc::ptr_eq` shortcut can
    // detect "no change since last request".
    let manager = NoopContextManager;
    let mut state = AgentState::with_window(1_000);
    let msgs: Vec<Message> = vec![sys("sys"), user("hi")];
    let arc_in = Arc::new(msgs);
    let arc_out = manager.prepare_arc(Arc::clone(&arc_in), &mut state).await;
    // Same allocation — `Arc::ptr_eq` would short-circuit on this.
    assert!(Arc::ptr_eq(&arc_in, &arc_out));
    assert!(!state.last_truncated);
}

#[tokio::test]
async fn truncating_prepare_arc_returns_input_when_no_truncation_needed() {
    let manager = TruncatingContextManager;
    let mut state = AgentState::with_window(1_000);
    // Tiny list — well under the window.
    let arc_in = Arc::new(vec![sys("sys"), user("hi")]);
    let arc_out = manager.prepare_arc(Arc::clone(&arc_in), &mut state).await;
    assert!(Arc::ptr_eq(&arc_in, &arc_out));
    assert!(!state.last_truncated);
}

#[tokio::test]
async fn truncating_prepare_arc_returns_new_arc_when_truncation_fires() {
    let manager = TruncatingContextManager;
    let mut state = AgentState::with_window(50);
    // Build a list that overflows the window so pairwise drop
    // has to fire.
    let mut msgs = vec![sys("sys")];
    for i in 0..20 {
        msgs.push(user(&format!(
            "question {i} with extra padding to push tokens up"
        )));
        msgs.push(assistant(&format!(
            "answer {i} with extra padding to push tokens up"
        )));
    }
    let arc_in = Arc::new(msgs);
    let arc_out = manager.prepare_arc(arc_in.clone(), &mut state).await;
    // Truncation happened: a fresh allocation must back the result.
    assert!(!Arc::ptr_eq(&arc_in, &arc_out));
    assert!(state.last_truncated);
    assert!(state.estimated_tokens <= state.context_window);
}

#[tokio::test]
async fn prepare_arc_copies_when_input_arc_is_shared() {
    // Default `prepare_arc` path: when the input Arc is shared
    // (e.g. cache policy still holds a ref), the manager must
    // materialise a fresh copy and apply `prepare` rather than
    // panicking on `Arc::try_unwrap` failure.
    let manager = TruncatingContextManager;
    let mut state = AgentState::with_window(1_000);
    let arc_a = Arc::new(vec![sys("sys"), user("hi")]);
    // Hold a second strong reference so `Arc::try_unwrap` fails.
    let _arc_b = Arc::clone(&arc_a);
    let arc_out = manager.prepare_arc(arc_a, &mut state).await;
    // Untouched because the list fits — the input was kept alive.
    assert_eq!(arc_out.len(), 2);
}

// -- the projection contract ----------------------------------------

/// A body shaped like a real transcript: the tool result and the
/// summary that follow a dropped user/assistant pair survive, so the
/// surviving set is *not* a contiguous range. A projection that
/// answered with a range would silently keep a different set.
fn projected_body(pairs: usize) -> Vec<Message> {
    let mut body = Vec::new();
    for i in 0..pairs {
        body.push(user(&format!("question {i} with padding")));
        body.push(assistant(&format!("answer {i} with padding")));
        body.push(tool_result(
            &format!("call-{i}"),
            &format!("result {i} with padding"),
        ));
        body.push(assistant(&format!("summary {i} with padding")));
    }
    body
}

#[tokio::test]
async fn the_projection_names_exactly_what_prepare_keeps() {
    // The contract: a caller that copies only the projected messages
    // sends the model exactly the list `prepare` would have left
    // behind. If the two ever disagree, the model reads a different
    // history than the one the manager decided on.
    let manager = TruncatingContextManager;
    let prefix = vec![sys("you are elysper, helpful")];
    let body = projected_body(12);

    for window in [10, 50, 200, 1_000, 100_000] {
        // What `prepare` leaves behind.
        let mut whole = prefix.clone();
        whole.extend(body.clone());
        let mut prepared_state = AgentState::with_window(window);
        manager.prepare(&mut whole, &mut prepared_state).await;

        // What the projection names.
        let mut projected_state = AgentState::with_window(window);
        let kept = manager
            .kept_indices(&prefix, &body, &mut projected_state)
            .await
            .expect("the truncating manager projects");

        let projected: Vec<Message> =
            kept.iter().map(|index| body[*index].clone()).collect();
        let mut expected = prefix.clone();
        expected.extend(projected);

        assert_eq!(
            expected, whole,
            "the projection and prepare disagree at window {window}"
        );
        assert_eq!(
            projected_state.estimated_tokens, prepared_state.estimated_tokens,
            "the two paths must leave the same estimate at window {window}"
        );
        assert_eq!(
            projected_state.last_truncated, prepared_state.last_truncated,
            "the two paths must agree on whether anything was dropped \
             at window {window}"
        );
    }
}

#[tokio::test]
async fn the_projection_keeps_the_messages_prepare_keeps_when_it_fits() {
    // A list that fits is not pruned, so the projection is every
    // message — and the caller that copies only those sends the
    // whole body.
    let manager = TruncatingContextManager;
    let prefix = vec![sys("sys")];
    let body = projected_body(3);

    let mut state = AgentState::with_window(1_000_000);
    let kept = manager
        .kept_indices(&prefix, &body, &mut state)
        .await
        .expect("the truncating manager projects");

    assert_eq!(kept, (0..body.len()).collect::<Vec<usize>>());
    assert!(!state.last_truncated);
}

#[tokio::test]
async fn the_projection_is_not_a_range() {
    // The pairwise drop leaves the tool result and the summary of a
    // dropped turn in place, so the survivors are scattered. A
    // projection that answered with a range would keep a different
    // set — this test is why the seam is indices and not a range.
    let manager = TruncatingContextManager;
    let prefix = vec![sys("sys")];
    let body = projected_body(6);

    let mut state = AgentState::with_window(60);
    let kept = manager
        .kept_indices(&prefix, &body, &mut state)
        .await
        .expect("the truncating manager projects");

    assert!(state.last_truncated, "the window must have forced a drop");
    assert!(
        kept.len() < body.len(),
        "the projection must have dropped something"
    );
    // The survivors are not the tail of the body: the first message
    // is gone while a later one is not.
    assert!(
        !kept.contains(&0),
        "the oldest user turn must be among the dropped"
    );
    assert!(
        kept.iter().any(|index| *index > 0),
        "a later message must have survived the drop"
    );
}
