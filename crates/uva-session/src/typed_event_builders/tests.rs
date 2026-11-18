//! The typed builders' own tests: each constructor's wire shape, the
//! round-trip contract every builder owes, and the row-seam builders
//! that route a payload to its row.

use super::*;

#[test]
fn step_start_carries_turn_and_step_ordinals() {
    let event = step_start(2, 4);
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], "step");
    assert_eq!(value["data"]["kind"], "start");
    assert_eq!(value["data"]["turn"], 2);
    assert_eq!(value["data"]["step"], 4);
    // seq is 0 — the controller stamps it.
    assert_eq!(value["seq"], 0);
    assert_eq!(value["ts"], "");
}

#[test]
fn step_end_carries_action() {
    let event = step_end(0, 1, "tool_call");
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["data"]["kind"], "end");
    assert_eq!(value["data"]["action"], "tool_call");
}

#[test]
fn turn_start_end_round_trip_via_known_event_types() {
    let start = turn_start(0);
    let end = turn_end(0, "max_iterations");
    assert_eq!(start.tag().as_wire(), "turn");
    assert_eq!(end.tag().as_wire(), "turn");
    // Both round-trip through serde.
    assert!(round_trip(&start).is_some());
    assert!(round_trip(&end).is_some());
}
#[test]
fn iteration_start_end_paired() {
    let start = iteration_start(7);
    let end = iteration_end(7, "completed");
    let value = serde_json::to_value(&end).unwrap();
    assert_eq!(value["data"]["kind"], "end");
    assert_eq!(value["data"]["iteration"], 7);
    assert_eq!(value["data"]["action"], "completed");
    // Pin `start` is constructed without warnings.
    let _ = start.tag().as_wire();
}

#[test]
fn subagent_enter_carries_parent_and_depth() {
    let event = subagent_enter("child-1", "parent-1", 2, "researcher");
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], "subagent_enter");
    assert_eq!(value["data"]["child_session_id"], "child-1");
    assert_eq!(value["data"]["parent_session_id"], "parent-1");
    assert_eq!(value["data"]["depth"], 2);
    assert_eq!(value["data"]["agent_name"], "researcher");
}

#[test]
fn subagent_exit_carries_status() {
    let event = subagent_exit("child-1", 2, "completed");
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], "subagent_exit");
    assert_eq!(value["data"]["status"], "completed");
}

#[test]
fn request_header_round_trips_with_reason() {
    let event =
        request_header("anthropic", "claude-opus-4-7", "deadbeef", "initial");
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], "request_header");
    assert_eq!(value["data"]["provider"], "anthropic");
    assert_eq!(value["data"]["model"], "claude-opus-4-7");
    assert_eq!(value["data"]["tools_hash"], "deadbeef");
    assert_eq!(value["data"]["reason"], "initial");
    // Round-trip via from_value.
    let restored = round_trip(&event).expect("parses");
    assert_eq!(restored.tag().as_wire(), "request_header");
}

#[test]
fn usage_omits_none_optionals_from_data() {
    // The Usage event must not include `"reasoning_tokens":
    // null` in the serialised form — providers that do not
    // surface reasoning tokens must produce a clean
    // envelope.
    let event = usage(100, 50, 150, None, Some(80), Some(20));
    let value = serde_json::to_value(&event).unwrap();
    assert!(value["data"].get("prompt_tokens").is_some());
    assert!(value["data"].get("completion_tokens").is_some());
    assert!(value["data"].get("total_tokens").is_some());
    assert!(value["data"].get("cache_read_tokens").is_some());
    assert!(value["data"].get("cache_write_tokens").is_some());
    // `reasoning_tokens` is None → not in the payload.
    assert!(value["data"].get("reasoning_tokens").is_none());
}

#[test]
fn usage_includes_reasoning_when_provided() {
    let event = usage(100, 50, 150, Some(7), None, None);
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["data"]["reasoning_tokens"], 7);
    // Optionals not provided must not appear.
    assert!(value["data"].get("cache_read_tokens").is_none());
    assert!(value["data"].get("cache_write_tokens").is_none());
}

#[test]
fn usage_stamps_typed_buckets_matching_legacy_payload() {
    let event = usage(100, 50, 150, Some(7), Some(80), Some(20));
    let value = serde_json::to_value(&event).unwrap();
    // The typed field is the current wire form.
    assert_eq!(value["usage"]["input_tokens"], 100);
    assert_eq!(value["usage"]["output_tokens"], 50);
    assert_eq!(value["usage"]["cache_read_tokens"], 80);
    assert_eq!(value["usage"]["cache_write_tokens"], 20);
    // And it decodes back to the same disjoint report.
    let buckets = event.provider_usage().expect("typed report");
    assert_eq!(buckets.input_tokens, 100);
    assert_eq!(buckets.output_tokens, 50);
    assert_eq!(buckets.cache_read_tokens, Some(80));
    assert_eq!(buckets.cache_write_tokens, Some(20));
    assert_eq!(buckets.prompt_side_tokens(), 200);
}

#[test]
fn sandbox_mode_carries_the_policy_wire_name() {
    let event = sandbox_mode("read-only");
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["type"], "sandbox_mode");
    assert_eq!(value["mode"], "read-only");
    assert_eq!(value["seq"], 0);
    assert_eq!(value["ts"], "");
    let restored = round_trip(&event).expect("round-trips");
    assert_eq!(restored.sandbox_mode(), Some("read-only"));
    // A mode this build does not know survives the log rather
    // than failing it: the fold, not the wire, rejects it.
    let unknown =
        round_trip(&sandbox_mode("future-mode")).expect("unknown mode parses");
    assert_eq!(unknown.sandbox_mode(), Some("future-mode"));
}

#[test]
fn structural_kind_extracts_kind_tag() {
    assert_eq!(structural_kind(&step_start(0, 0)), Some("start"));
    assert_eq!(structural_kind(&step_end(0, 0, "x")), Some("end"));
    assert_eq!(structural_kind(&turn_start(0)), Some("start"));
    assert_eq!(structural_kind(&turn_end(0, "x")), Some("end"));
    assert_eq!(structural_kind(&iteration_start(0)), Some("start"));
    // Non-structural events return None.
    let non_structural = usage(1, 1, 2, None, None, None);
    assert_eq!(structural_kind(&non_structural), None);
}

#[test]
fn user_message_carries_its_source_only_when_it_has_one() {
    let steer = user_message(
        json!({"role": "user", "content": "focus"}),
        Some("steer"),
    );
    let value = serde_json::to_value(&steer).unwrap();
    assert_eq!(value["type"], "user_message");
    assert_eq!(value["source"], "steer");
    assert_eq!(value["seq"], 0, "the writer stamps the ordinal");
    let restored = round_trip(&steer).expect("round-trips");
    let source = match &restored {
        SessionEvent::UserMessage { source, .. } => source.clone(),
        other => panic!("expected a user_message row, got {other:?}"),
    };
    assert_eq!(
        source.as_deref(),
        Some("steer"),
        "the source survives the log"
    );

    let ordinary = user_message(json!({"role": "user", "content": "hi"}), None);
    let value = serde_json::to_value(&ordinary).unwrap();
    assert!(
        value.get("source").is_none(),
        "an ordinary turn keeps its shape: {value}"
    );
}

#[test]
fn all_builders_round_trip_through_serde() {
    // One test per builder — pins the durability contract
    // for every event the loop might emit.
    let cases: Vec<(&str, SessionEvent)> = vec![
        ("step_start", step_start(0, 0)),
        ("step_end", step_end(0, 0, "completed")),
        ("turn_start", turn_start(0)),
        ("turn_end", turn_end(0, "completed")),
        ("iteration_start", iteration_start(0)),
        ("iteration_end", iteration_end(0, "completed")),
        ("subagent_enter", subagent_enter("c", "p", 1, "a")),
        ("subagent_exit", subagent_exit("c", 1, "completed")),
        ("request_header", request_header("p", "m", "h", "initial")),
        ("usage", usage(1, 1, 2, None, None, None)),
        ("sandbox_mode", sandbox_mode("workspace-write")),
        ("tool_output", tool_output("c1", "shell", "half")),
    ];
    for (name, event) in cases {
        let restored = round_trip(&event)
            .unwrap_or_else(|| panic!("{name} must round-trip"));
        assert_eq!(restored.tag().as_wire(), event.tag().as_wire(), "{name}");
    }
}

mod row_seam_tests {
    use super::*;

    #[test]
    fn warning_carries_kind_and_message() {
        let event = warning("config_drift", "envelope changed");
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "warning");
        assert_eq!(value["data"]["kind"], "config_drift");
        assert_eq!(value["data"]["message"], "envelope changed");
        assert_eq!(value["seq"], 0);
        assert_eq!(value["ts"], "");
    }

    #[test]
    fn tool_result_carries_call_id_and_flag() {
        let event = tool_result(
            serde_json::json!({"role": "tool", "content": "ok"}),
            Some("call-1".to_string()),
            true,
        );
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "tool_result");
        assert_eq!(value["tool_call_id"], "call-1");
        assert_eq!(value["is_error"], true);
    }

    #[test]
    fn message_row_routes_each_role_to_its_tag() {
        let user = message_row(&uva_provider::Message::user("hi"))
            .unwrap()
            .tag()
            .as_wire();
        let assistant = message_row(&uva_provider::Message::assistant("ok"))
            .unwrap()
            .tag()
            .as_wire();
        let tool =
            message_row(&uva_provider::Message::tool("out", "call-1")).unwrap();
        assert_eq!(user, "user_message");
        assert_eq!(assistant, "assistant_message");
        assert_eq!(tool.tag().as_wire(), "tool_result");
        let value = serde_json::to_value(&tool).unwrap();
        assert_eq!(value["tool_call_id"], "call-1");
        // `false` is skipped on the wire — the row's default is no
        // flag at all.
        assert!(value.get("is_error").is_none());
    }

    #[test]
    fn turn_prompt_row_carries_attachments_and_op() {
        let event = turn_prompt_row(
            serde_json::json!({"role": "user", "content": "again"}),
            Vec::new(),
            Some(SurfaceOp::append()),
        );
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "user_message");
        assert!(value.get("surface_op").is_some());

        let plain = turn_prompt_row(
            serde_json::json!({"role": "user", "content": "first"}),
            Vec::new(),
            None,
        );
        let value = serde_json::to_value(&plain).unwrap();
        assert!(value.get("surface_op").is_none());
    }

    #[test]
    fn park_rows_and_feedback_carry_their_ids() {
        let parked = parked_prompt(
            "park-1".to_string(),
            "queued text".to_string(),
            Vec::new(),
            None,
            Some("anthropic".to_string()),
            Some("claude".to_string()),
            true,
        );
        let value = serde_json::to_value(&parked).unwrap();
        assert_eq!(value["type"], "parked_prompt");
        assert_eq!(value["park_id"], "park-1");
        assert_eq!(value["rerun"], true);

        let consumed = park_consumed("park-1".to_string());
        let value = serde_json::to_value(&consumed).unwrap();
        assert_eq!(value["type"], "park_consumed");
        assert_eq!(value["park_id"], "park-1");

        let rating = feedback("42".to_string(), true);
        let value = serde_json::to_value(&rating).unwrap();
        assert_eq!(value["type"], "feedback");
        assert_eq!(value["message_id"], "42");
        assert_eq!(value["thumbs_up"], true);
    }

    #[test]
    fn session_shutdown_names_its_reason_and_never_stamps_ts() {
        let event = session_shutdown(SessionEndReason::Cancelled);
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "session_shutdown");
        assert_eq!(value["data"]["reason"], "cancelled");
        assert_eq!(value["ts"], "");
    }
}
