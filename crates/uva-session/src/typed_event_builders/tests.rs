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
    let end = turn_end(0, "max_iterations", None);
    assert_eq!(start.tag().as_wire(), "turn");
    assert_eq!(end.tag().as_wire(), "turn");
    // Both round-trip through serde.
    assert!(round_trip(&start).is_some());
    assert!(round_trip(&end).is_some());
}

/// The reason a turn stopped with belongs to the row's own
/// constructor: the row that goes on disk says what happened, so
/// whoever writes it never has to patch the payload afterwards.
#[test]
fn turn_end_carries_the_failure_message_in_its_payload() {
    let event = turn_end(2, "error", Some("the provider refused"));
    let value = serde_json::to_value(&event).unwrap();
    assert_eq!(value["data"]["reason"], "error");
    assert_eq!(value["data"]["message"], "the provider refused");
}

/// A row written today reads back as the same row: the payload the
/// constructor wrote is the payload the reader sees — the failure
/// message included.
///
/// The typed fields are compared with the ones the constructor
/// wrote, not with the ones the reader ends up with: a constructor
/// that wrote one view and left the other empty would be repaired
/// silently on the way back in, and the row would read fine.
#[test]
fn turn_end_with_a_failure_message_reads_back_as_the_same_row() {
    let event = turn_end(2, "error", Some("the provider refused"));
    let restored = round_trip(&event).expect("round-trips");
    let SessionEvent::Turn {
        data, kind, turn, ..
    } = &restored
    else {
        panic!("expected a turn row, got {restored:?}");
    };
    let SessionEvent::Turn {
        data: written,
        kind: written_kind,
        turn: written_turn,
        ..
    } = &event
    else {
        panic!("expected a turn row, got {event:?}");
    };
    assert_eq!(data["message"], "the provider refused");
    // The two views the reader gets are the two the constructor
    // wrote.
    assert_eq!(kind.as_deref(), written_kind.as_deref());
    assert_eq!(*turn, *written_turn);
    assert_eq!(kind.as_deref(), Some("end"));
    assert_eq!(*turn, Some(2));
    // The payload is the one the constructor wrote, byte for byte.
    assert_eq!(
        serde_json::to_string(data).unwrap(),
        serde_json::to_string(written).unwrap(),
        "the row reads back with the payload it was written with"
    );
}

/// The bytes on the wire do not move: the payload of a turn end
/// carries exactly the keys it carried before the constructor took
/// the failure message — and the key is there only when there is a
/// message to carry.
///
/// The order is the order the keys were written in, so the literal
/// below is the bytes an already-written log carries.
#[test]
fn turn_end_payload_bytes_are_the_written_shape() {
    let payload = |event: &SessionEvent| match event {
        SessionEvent::Turn { data, .. } => serde_json::to_string(data).unwrap(),
        other => panic!("expected a turn row, got {other:?}"),
    };
    let with = turn_end(0, "error", Some("boom"));
    let without = turn_end(0, "completed", None);
    assert_eq!(
        payload(&with),
        r#"{"kind":"end","turn":0,"reason":"error","message":"boom"}"#
    );
    assert_eq!(
        payload(&without),
        r#"{"kind":"end","turn":0,"reason":"completed"}"#
    );
}

/// The payload and the typed fields say the same thing: a reader
/// that asks the row and a reader that reads the payload see one
/// row, and the row needs no read-side repair to be readable.
#[test]
fn turn_end_payload_and_typed_fields_say_the_same_thing() {
    let event = turn_end(2, "error", Some("boom"));
    let SessionEvent::Turn {
        data, kind, turn, ..
    } = &event
    else {
        panic!("expected a turn row, got {event:?}");
    };
    assert_eq!(data["kind"].as_str(), kind.as_deref());
    assert_eq!(data["turn"].as_u64(), *turn);
    // The typed fields are already there — the read side has
    // nothing to fill in.
    assert_eq!(event.boundary_kind(), Some("end"));
    assert_eq!(event.boundary_turn(), Some(2));
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
    assert_eq!(structural_kind(&turn_end(0, "x", None)), Some("end"));
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
        ("turn_end", turn_end(0, "completed", None)),
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
            None,
        );
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "tool_result");
        assert_eq!(value["tool_call_id"], "call-1");
        assert_eq!(value["is_error"], true);
    }

    /// The interrupted row a crash-repair writes is the vocabulary's
    /// row too: the payload names the call, the tool, the recovery
    /// code and the `interrupted` mark, and the typed fields say the
    /// call failed.
    #[test]
    fn an_interrupted_tool_result_carries_the_recovery_code() {
        let event =
            tool_result_interrupted("c1", "read", "TOOL_OUTCOME_UNKNOWN");
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "tool_result");
        assert_eq!(value["data"]["call_id"], "c1");
        assert_eq!(value["data"]["tool_name"], "read");
        assert_eq!(value["data"]["error"], "TOOL_OUTCOME_UNKNOWN");
        assert_eq!(value["data"]["interrupted"], true);
        // The call never finished: the row says the tool failed.
        assert_eq!(value["is_error"], true);
        assert_eq!(value["tool_call_id"], "c1");
        // The row joins the surface as an append.
        assert_eq!(value["surface_op"], "append");
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

/// The compaction rows: one constructor per shape, one owner for
/// every key.
///
/// The six keys the compaction row carries used to live in the one
/// call site that wrote the row. They live in the constructor now:
/// a caller names what the row says, never how it says it.
mod compaction_tests {
    use uva_provider::TokenUsage;

    use super::*;

    /// The payload's keys, in the order they were written.
    fn payload_keys(data: &serde_json::Value) -> Vec<&str> {
        data.as_object()
            .expect("a payload")
            .keys()
            .map(String::as_str)
            .collect()
    }

    /// The six keys the compaction row carries, in the order the
    /// constructor writes them.
    const COMPACTION_KEYS: [&str; 6] = [
        "summary",
        "source_indices",
        "shadowed_token_count",
        "provider",
        "model",
        "usage",
    ];

    fn replace_op() -> SurfaceOp {
        SurfaceOp::Replace {
            start: 2,
            end: 5,
            source_event_seqs: vec![7, 8, 9],
        }
    }

    /// The pass opens with a row that names the span it is about to
    /// replace: the range it shadows and what that span cost.
    #[test]
    fn compaction_start_carries_the_span_the_pass_opens() {
        let event = compaction_start("token-1", "context_pressure", 2, 5, 321);
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "compaction_start");
        assert_eq!(value["token"], "token-1");
        assert_eq!(value["seq"], 0, "the writer stamps the ordinal");
        assert_eq!(value["ts"], "");
        assert_eq!(
            payload_keys(&value["data"]),
            vec!["reason", "start", "end", "shadowed_token_count"],
            "the payload names the span the pass opens"
        );
        assert_eq!(value["data"]["reason"], "context_pressure");
        assert_eq!(value["data"]["start"], 2);
        assert_eq!(value["data"]["end"], 5);
        assert_eq!(value["data"]["shadowed_token_count"], 321);
    }

    /// The summary row carries the text the pass produced.
    #[test]
    fn compaction_summary_carries_the_summary_text() {
        let event = compaction_summary("token-1", "the summary");
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "compaction_summary");
        assert_eq!(value["token"], "token-1");
        assert_eq!(payload_keys(&value["data"]), vec!["summary"]);
        assert_eq!(value["data"]["summary"], "the summary");
    }

    /// The six keys are written in one place: the constructor.
    /// Nobody downstream spells them again.
    #[test]
    fn compaction_row_writes_its_six_keys_in_one_place() {
        let event = compaction_row(
            "the summary",
            vec![2, 3, 4],
            321,
            Some("anthropic"),
            Some("claude-opus-4-7"),
            None,
            replace_op(),
        );
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["type"], "compaction");
        assert_eq!(
            payload_keys(&value["data"]),
            COMPACTION_KEYS,
            "the payload carries exactly the six keys it always carried"
        );
        assert_eq!(value["data"]["summary"], "the summary");
        assert_eq!(value["data"]["source_indices"], json!([2, 3, 4]));
        assert_eq!(value["data"]["shadowed_token_count"], 321);
        assert_eq!(value["data"]["provider"], "anthropic");
        assert_eq!(value["data"]["model"], "claude-opus-4-7");
        assert!(value["data"]["usage"].is_null());
        // The replacement the row claims is the row's own typed
        // field: a compaction row is a replacement, and the fold
        // refuses one that does not say what it replaces.
        assert_eq!(value["surface_op"]["start"], 2);
        assert_eq!(value["surface_op"]["end"], 5);
        assert_eq!(value["surface_op"]["source_event_seqs"], json!([7, 8, 9]));
    }

    /// The provider's usage report rides in the payload as it always
    /// did — the report itself, not a summary of it.
    #[test]
    fn compaction_row_carries_the_provider_usage_report() {
        let usage = TokenUsage {
            prompt_tokens: 10,
            completion_tokens: 5,
            total_tokens: 15,
            ..TokenUsage::default()
        };
        let event = compaction_row(
            "the summary",
            vec![2],
            321,
            None,
            None,
            Some(&usage),
            replace_op(),
        );
        let value = serde_json::to_value(&event).unwrap();
        assert_eq!(value["data"]["usage"]["prompt_tokens"], 10);
        assert_eq!(value["data"]["usage"]["completion_tokens"], 5);
        assert_eq!(value["data"]["usage"]["total_tokens"], 15);
        assert!(value["data"]["provider"].is_null());
        assert!(value["data"]["model"].is_null());
    }

    /// A compaction row written today reads back with the same six
    /// keys: the payload the constructor wrote is the payload the
    /// reader sees.
    #[test]
    fn compaction_row_reads_back_with_the_same_six_keys() {
        let event = compaction_row(
            "the summary",
            vec![2, 3, 4],
            321,
            Some("anthropic"),
            Some("claude-opus-4-7"),
            None,
            replace_op(),
        );
        let restored = round_trip(&event).expect("round-trips");
        let SessionEvent::Compaction {
            data, surface_op, ..
        } = &restored
        else {
            panic!("expected a compaction row, got {restored:?}");
        };
        let SessionEvent::Compaction { data: written, .. } = &event else {
            panic!("expected a compaction row, got {event:?}");
        };
        assert_eq!(
            payload_keys(data),
            COMPACTION_KEYS,
            "the row reads back with the keys it was written with"
        );
        assert_eq!(
            serde_json::to_string(data).unwrap(),
            serde_json::to_string(written).unwrap(),
            "the payload survives the log byte for byte"
        );
        assert_eq!(surface_op, &replace_op());
    }

    /// The end row has two shapes, and each says what happened: a
    /// committed pass names what it shadowed, a failed one names the
    /// failure.
    #[test]
    fn compaction_end_committed_and_failed_carry_their_own_shapes() {
        let committed =
            compaction_end_committed("token-1", 3, 321, "context_pressure");
        let value = serde_json::to_value(&committed).unwrap();
        assert_eq!(value["type"], "compaction_end");
        assert_eq!(value["outcome"], "committed");
        assert_eq!(value["token"], "token-1");
        assert_eq!(
            payload_keys(&value["data"]),
            vec!["shadowed_messages", "shadowed_tokens", "reason"]
        );
        assert_eq!(value["data"]["shadowed_messages"], 3);
        assert_eq!(value["data"]["shadowed_tokens"], 321);
        assert_eq!(value["data"]["reason"], "context_pressure");

        let failed = compaction_end_failed(
            "token-1",
            CompactionLifecycleOutcome::Failed,
            "the summariser refused",
            "context_pressure",
        );
        let value = serde_json::to_value(&failed).unwrap();
        assert_eq!(value["outcome"], "failed");
        assert_eq!(payload_keys(&value["data"]), vec!["error", "reason"]);
        assert_eq!(value["data"]["error"], "the summariser refused");
        assert_eq!(value["data"]["reason"], "context_pressure");

        let interrupted = compaction_end_failed(
            "token-1",
            CompactionLifecycleOutcome::Interrupted,
            "cancelled",
            "context_pressure",
        );
        let value = serde_json::to_value(&interrupted).unwrap();
        assert_eq!(
            value["outcome"], "interrupted",
            "a cancelled pass ends interrupted"
        );
    }

    /// The bytes on the wire do not move: every payload carries
    /// exactly the keys it carried before the constructors took them,
    /// in the order they were written in.
    #[test]
    fn the_compaction_payload_bytes_are_the_written_shape() {
        let payload = |event: &SessionEvent| match event {
            SessionEvent::CompactionStart { data, .. }
            | SessionEvent::CompactionSummary { data, .. }
            | SessionEvent::Compaction { data, .. }
            | SessionEvent::CompactionEnd { data, .. } => {
                serde_json::to_string(data).unwrap()
            }
            other => panic!("expected a compaction row, got {other:?}"),
        };
        assert_eq!(
            payload(&compaction_start(
                "token-1",
                "context_pressure",
                2,
                5,
                321
            )),
            r#"{"reason":"context_pressure","start":2,"end":5,"shadowed_token_count":321}"#
        );
        assert_eq!(
            payload(&compaction_summary("token-1", "the summary")),
            r#"{"summary":"the summary"}"#
        );
        assert_eq!(
            payload(&compaction_row(
                "the summary",
                vec![2, 3],
                321,
                Some("anthropic"),
                Some("claude-opus-4-7"),
                None,
                replace_op(),
            )),
            r#"{"summary":"the summary","source_indices":[2,3],"shadowed_token_count":321,"provider":"anthropic","model":"claude-opus-4-7","usage":null}"#
        );
        assert_eq!(
            payload(&compaction_end_committed(
                "token-1",
                3,
                321,
                "context_pressure"
            )),
            r#"{"shadowed_messages":3,"shadowed_tokens":321,"reason":"context_pressure"}"#
        );
        assert_eq!(
            payload(&compaction_end_failed(
                "token-1",
                CompactionLifecycleOutcome::Failed,
                "the summariser refused",
                "context_pressure",
            )),
            r#"{"error":"the summariser refused","reason":"context_pressure"}"#
        );
    }
}

/// One owner per typed tag: the constructor writes the row's payload
/// bag and its typed fields together, and the row that reaches the log
/// says the same thing twice.
///
/// The assertions read the row **the constructor wrote** — the wire
/// form of the constructed value, before any reader touches it. A
/// constructor that wrote one view and left the other empty is
/// repaired silently on the way back in (`normalize` fills the typed
/// fields from the payload), so a test that asserted on the read-back
/// row would keep that bug green: the row reads fine and the drift
/// ships. The round trip is asserted separately, and byte for byte.
///
/// The tags split three ways:
///
/// - **A — a payload bag and typed fields**: the constructor owns
///   both, and the two must agree. `assistant_chunk` and `tool_call`
///   have no constructor at all — production never writes them — so
///   the vocabulary's skeleton is the only shape they have, and they
///   are covered by the round trip alone.
/// - **B — typed fields, no payload bag**: the payload half does not
///   exist for them, so the property is that every typed field is
///   there. The plugin row is here too: its payload is the plugin's
///   own free shape (ADR-0009), and the property is that the
///   caller's payload is the row's payload.
/// - **C — a payload bag, no typed fields**: the free-format rows.
///   ADR-0009 keeps them free: the payload *is* the shape, and there
///   is no typed field to agree with — so they are excluded here and
///   pinned by `the_free_format_rows_keep_their_shape` instead.
mod one_owner_per_tag {
    use std::collections::HashSet;

    use super::*;
    use crate::events::RowTag;

    /// What a row's payload is.
    enum Payload {
        /// The row has no payload bag: every fact it carries is a
        /// typed field.
        NoBag,
        /// The payload bag the constructor owns: the exact key set,
        /// in the order the constructor writes them.
        Keys {
            /// The wire key the payload lives under.
            key: &'static str,
            /// The keys, in the order the constructor writes them.
            keys: &'static [&'static str],
        },
        /// The payload the caller handed over, passed through
        /// untouched.
        Caller {
            /// The wire key the payload lives under.
            key: &'static str,
            /// The caller's payload.
            value: Value,
        },
    }

    /// A row the constructor wrote, with what the row must say.
    struct Case {
        /// The constructor's name — the owner of this shape.
        owner: &'static str,
        /// The row the constructor wrote.
        event: SessionEvent,
        /// What the row's payload is.
        payload: Payload,
        /// The typed fields the row carries on the wire: the field's
        /// name and the value the constructor wrote.
        typed: Vec<(&'static str, Value)>,
        /// The facts the payload and the typed fields must agree on:
        /// the typed field's path in the wire form and the payload
        /// key that carries the same fact.
        twins: Vec<(&'static str, &'static str)>,
    }

    impl Case {
        fn new(owner: &'static str, event: SessionEvent) -> Self {
            Self {
                owner,
                event,
                payload: Payload::NoBag,
                typed: Vec::new(),
                twins: Vec::new(),
            }
        }

        fn payload(mut self, payload: Payload) -> Self {
            self.payload = payload;
            self
        }

        fn typed<const N: usize>(
            mut self,
            typed: [(&'static str, Value); N],
        ) -> Self {
            self.typed = typed.to_vec();
            self
        }

        fn twins<const N: usize>(
            mut self,
            twins: [(&'static str, &'static str); N],
        ) -> Self {
            self.twins = twins.to_vec();
            self
        }
    }

    /// The row as the constructor wrote it: the wire form, before a
    /// reader touches it.
    fn written(event: &SessionEvent) -> Value {
        serde_json::to_value(event).expect("the row serialises")
    }

    /// The payload bag's keys, in the order the constructor wrote
    /// them.
    fn payload_keys<'a>(wire: &'a Value, key: &str) -> Vec<&'a str> {
        wire.get(key)
            .and_then(Value::as_object)
            .map(|bag| bag.keys().map(String::as_str).collect())
            .unwrap_or_default()
    }

    /// The row's typed fields on the wire — everything but the shell
    /// (`type` / `seq` / `ts`) and the payload bag.
    fn typed_keys(wire: &Value) -> Vec<&str> {
        let mut keys: Vec<&str> = wire
            .as_object()
            .expect("a row is an object")
            .keys()
            .map(String::as_str)
            .filter(|key| !matches!(*key, "type" | "seq" | "ts" | "data"))
            .collect();
        keys.sort_unstable();
        keys
    }

    /// The value at a dotted path in the wire form.
    fn at<'a>(wire: &'a Value, path: &str) -> Option<&'a Value> {
        path.split('.')
            .try_fold(wire, |value, part| value.get(part))
    }

    /// The compaction row's replacement — the fold refuses a
    /// compaction row that does not say what it replaces.
    fn replace_op() -> SurfaceOp {
        SurfaceOp::Replace {
            start: 2,
            end: 5,
            source_event_seqs: vec![7, 8, 9],
        }
    }

    /// Every row a constructor writes, with what the row must say.
    ///
    /// One case per constructor, not per tag: a tag may have several
    /// shapes (`turn` has a start, an end, a suspension and a
    /// suspended end), and each shape is the constructor's own.
    fn cases() -> Vec<Case> {
        vec![
            // ---- A: a payload bag and typed fields -----------------
            Case::new(
                "user_message",
                user_message(
                    json!({"role": "user", "content": "hi"}),
                    Some("steer"),
                ),
            )
            .payload(Payload::Caller {
                key: "data",
                value: json!({"role": "user", "content": "hi"}),
            })
            .typed([("source", json!("steer"))]),
            Case::new(
                "turn_prompt_row",
                turn_prompt_row(
                    json!({"role": "user", "content": "hi"}),
                    Vec::new(),
                    Some(SurfaceOp::append()),
                ),
            )
            .payload(Payload::Caller {
                key: "data",
                value: json!({"role": "user", "content": "hi"}),
            })
            .typed([("surface_op", json!("append"))]),
            Case::new(
                "assistant_message",
                assistant_message(
                    json!({"role": "assistant", "content": "ok"}),
                ),
            )
            .payload(Payload::Caller {
                key: "data",
                value: json!({"role": "assistant", "content": "ok"}),
            }),
            Case::new(
                "tool_result",
                tool_result(
                    json!({"role": "tool", "content": "ok"}),
                    Some("call-1".to_string()),
                    true,
                    None,
                ),
            )
            .payload(Payload::Caller {
                key: "data",
                value: json!({"role": "tool", "content": "ok"}),
            })
            .typed([
                ("tool_call_id", json!("call-1")),
                ("is_error", json!(true)),
            ]),
            // The interrupted row is the repair's own shape: ADR-0009
            // keeps it — one tag, two payloads — and the typed field
            // spells the call id `tool_call_id` where the payload
            // spells it `call_id`. The two carry the same fact.
            Case::new(
                "tool_result_interrupted",
                tool_result_interrupted("c1", "read", "TOOL_OUTCOME_UNKNOWN"),
            )
            .payload(Payload::Keys {
                key: "data",
                keys: &["call_id", "tool_name", "error", "interrupted"],
            })
            .typed([
                ("tool_call_id", json!("c1")),
                ("is_error", json!(true)),
                ("surface_op", json!("append")),
            ])
            .twins([("tool_call_id", "call_id")]),
            Case::new("tool_output", tool_output("c1", "shell", "half"))
                .payload(Payload::Keys {
                    key: "data",
                    keys: &["output"],
                })
                .typed([("call_id", json!("c1")), ("tool", json!("shell"))]),
            Case::new("step_start", step_start(2, 4))
                .payload(Payload::Keys {
                    key: "data",
                    keys: &["kind", "turn", "step"],
                })
                .typed([
                    ("kind", json!("start")),
                    ("turn", json!(2)),
                    ("step", json!(4)),
                ])
                .twins([("kind", "kind"), ("turn", "turn"), ("step", "step")]),
            Case::new("step_end", step_end(0, 1, "tool_call"))
                .payload(Payload::Keys {
                    key: "data",
                    keys: &["kind", "turn", "step", "action"],
                })
                .typed([
                    ("kind", json!("end")),
                    ("turn", json!(0)),
                    ("step", json!(1)),
                ])
                .twins([("kind", "kind"), ("turn", "turn"), ("step", "step")]),
            Case::new("step_end_suspended", step_end_suspended())
                .payload(Payload::Keys {
                    key: "data",
                    keys: &["kind", "status"],
                })
                .typed([("kind", json!("end"))])
                .twins([("kind", "kind")]),
            Case::new("turn_start", turn_start(2))
                .payload(Payload::Keys {
                    key: "data",
                    keys: &["kind", "turn"],
                })
                .typed([("kind", json!("start")), ("turn", json!(2))])
                .twins([("kind", "kind"), ("turn", "turn")]),
            Case::new("turn_end", turn_end(2, "error", Some("boom")))
                .payload(Payload::Keys {
                    key: "data",
                    keys: &["kind", "turn", "reason", "message"],
                })
                .typed([("kind", json!("end")), ("turn", json!(2))])
                .twins([("kind", "kind"), ("turn", "turn")]),
            Case::new("turn_suspended", turn_suspended(vec!["c1".to_string()]))
                .payload(Payload::Keys {
                    key: "data",
                    keys: &["kind", "status", "pending"],
                })
                .typed([("kind", json!("suspended"))])
                .twins([("kind", "kind")]),
            Case::new("turn_end_suspended", turn_end_suspended())
                .payload(Payload::Keys {
                    key: "data",
                    keys: &["kind", "reason"],
                })
                .typed([("kind", json!("end"))])
                .twins([("kind", "kind")]),
            Case::new(
                "subagent_enter",
                subagent_enter("child-1", "parent-1", 2, "researcher"),
            )
            .payload(Payload::Keys {
                key: "data",
                keys: &[
                    "child_session_id",
                    "parent_session_id",
                    "depth",
                    "agent_name",
                ],
            })
            .typed([
                ("child_session_id", json!("child-1")),
                ("parent_session_id", json!("parent-1")),
                ("agent_name", json!("researcher")),
                ("depth", json!(2)),
            ])
            .twins([
                ("child_session_id", "child_session_id"),
                ("parent_session_id", "parent_session_id"),
                ("agent_name", "agent_name"),
                ("depth", "depth"),
            ]),
            Case::new(
                "subagent_exit",
                subagent_exit("child-1", 2, "completed"),
            )
            .payload(Payload::Keys {
                key: "data",
                keys: &["child_session_id", "depth", "status"],
            })
            .typed([
                ("child_session_id", json!("child-1")),
                ("depth", json!(2)),
                ("status", json!("completed")),
            ])
            .twins([
                ("child_session_id", "child_session_id"),
                ("depth", "depth"),
                ("status", "status"),
            ]),
            Case::new(
                "request_header",
                request_header(
                    "anthropic",
                    "claude-opus-4-7",
                    "deadbeef",
                    "initial",
                ),
            )
            .payload(Payload::Keys {
                key: "data",
                keys: &["reason", "provider", "model", "tools_hash"],
            })
            .typed([
                ("provider", json!("anthropic")),
                ("model", json!("claude-opus-4-7")),
            ])
            .twins([("provider", "provider"), ("model", "model")]),
            // The usage row carries the provider report twice: the
            // typed buckets are the disjoint report, and the payload
            // is the legacy envelope. The two spell the same counts
            // under different names.
            Case::new(
                "usage",
                usage(100, 50, 150, Some(7), Some(80), Some(20)),
            )
            .payload(Payload::Keys {
                key: "data",
                keys: &[
                    "prompt_tokens",
                    "completion_tokens",
                    "total_tokens",
                    "reasoning_tokens",
                    "cache_read_tokens",
                    "cache_write_tokens",
                ],
            })
            .typed([(
                "usage",
                json!({
                    "input_tokens": 100,
                    "output_tokens": 50,
                    "cache_read_tokens": 80,
                    "cache_write_tokens": 20,
                }),
            )])
            .twins([
                ("usage.input_tokens", "prompt_tokens"),
                ("usage.output_tokens", "completion_tokens"),
                ("usage.cache_read_tokens", "cache_read_tokens"),
                ("usage.cache_write_tokens", "cache_write_tokens"),
            ]),
            Case::new(
                "compaction_row",
                compaction_row(
                    "the summary",
                    vec![2, 3, 4],
                    321,
                    Some("anthropic"),
                    Some("claude-opus-4-7"),
                    None,
                    replace_op(),
                ),
            )
            .payload(Payload::Keys {
                key: "data",
                keys: &[
                    "summary",
                    "source_indices",
                    "shadowed_token_count",
                    "provider",
                    "model",
                    "usage",
                ],
            })
            .typed([(
                "surface_op",
                json!({"start": 2, "end": 5, "source_event_seqs": [7, 8, 9]}),
            )]),
            Case::new(
                "compaction_start",
                compaction_start("token-1", "context_pressure", 2, 5, 321),
            )
            .payload(Payload::Keys {
                key: "data",
                keys: &["reason", "start", "end", "shadowed_token_count"],
            })
            .typed([("token", json!("token-1"))]),
            Case::new(
                "compaction_summary",
                compaction_summary("token-1", "the summary"),
            )
            .payload(Payload::Keys {
                key: "data",
                keys: &["summary"],
            })
            .typed([("token", json!("token-1"))]),
            Case::new(
                "compaction_end_committed",
                compaction_end_committed("token-1", 3, 321, "context_pressure"),
            )
            .payload(Payload::Keys {
                key: "data",
                keys: &["shadowed_messages", "shadowed_tokens", "reason"],
            })
            .typed([
                ("token", json!("token-1")),
                ("outcome", json!("committed")),
            ]),
            Case::new(
                "compaction_end_failed",
                compaction_end_failed(
                    "token-1",
                    CompactionLifecycleOutcome::Failed,
                    "the summariser refused",
                    "context_pressure",
                ),
            )
            .payload(Payload::Keys {
                key: "data",
                keys: &["error", "reason"],
            })
            .typed([("token", json!("token-1")), ("outcome", json!("failed"))]),
            // ---- B: typed fields, no payload bag ------------------
            Case::new(
                "approval_asked",
                approval_asked(
                    "req-1",
                    "call-1",
                    "shell",
                    "hash-1",
                    "high",
                    "researcher",
                ),
            )
            .typed([
                ("id", json!("req-1")),
                ("call_id", json!("call-1")),
                ("tool", json!("shell")),
                ("arguments_hash", json!("hash-1")),
                ("risk", json!("high")),
                ("agent", json!("researcher")),
            ]),
            Case::new(
                "approval_decided",
                approval_decided(
                    "req-1",
                    "call-1",
                    "denied",
                    "operator",
                    Some(json!({"reason": "no"})),
                ),
            )
            .typed([
                ("id", json!("req-1")),
                ("call_id", json!("call-1")),
                ("status", json!("denied")),
                ("by", json!("operator")),
                ("payload", json!({"reason": "no"})),
            ]),
            Case::new(
                "approval_granted",
                approval_granted("req-1", "shell", "session", "ws-hash"),
            )
            .typed([
                ("id", json!("req-1")),
                ("tool", json!("shell")),
                ("scope", json!("session")),
                ("workspace_hash", json!("ws-hash")),
            ]),
            Case::new(
                "tool_dispatch",
                tool_dispatch("call-1", "shell", "hash-1"),
            )
            .typed([
                ("call_id", json!("call-1")),
                ("tool", json!("shell")),
                ("arguments_hash", json!("hash-1")),
            ]),
            Case::new(
                "request_snapshot",
                request_snapshot(
                    "anthropic",
                    "claude-opus-4-7",
                    "params",
                    "tools",
                    "system",
                    vec!["a".to_string(), "b".to_string()],
                    "policy",
                ),
            )
            .typed([
                ("provider", json!("anthropic")),
                ("model", json!("claude-opus-4-7")),
                ("params_hash", json!("params")),
                ("tools_hash", json!("tools")),
                ("system_hash", json!("system")),
                ("sections", json!(["a", "b"])),
                ("policy_hash", json!("policy")),
            ]),
            Case::new("sandbox_mode", sandbox_mode("read-only"))
                .typed([("mode", json!("read-only"))]),
            Case::new(
                "tool_result_transformed",
                tool_result_transformed(
                    vec!["redact".to_string()],
                    "shell",
                    "call-1",
                    "before".to_string(),
                    "after".to_string(),
                ),
            )
            .typed([
                ("transformers", json!(["redact"])),
                ("tool_name", json!("shell")),
                ("call_id", json!("call-1")),
                ("before", json!("before")),
                ("after", json!("after")),
            ]),
            Case::new(
                "parked_prompt",
                parked_prompt(
                    "park-1".to_string(),
                    "queued text".to_string(),
                    Vec::new(),
                    None,
                    Some("anthropic".to_string()),
                    Some("claude-opus-4-7".to_string()),
                    true,
                ),
            )
            .typed([
                ("park_id", json!("park-1")),
                ("text", json!("queued text")),
                ("provider", json!("anthropic")),
                ("model", json!("claude-opus-4-7")),
                ("rerun", json!(true)),
            ]),
            Case::new("park_consumed", park_consumed("park-1".to_string()))
                .typed([("park_id", json!("park-1"))]),
            Case::new(
                "goal",
                goal("active", "ship the tool family", Some(50_000)),
            )
            .typed([
                ("state", json!("active")),
                ("objective", json!("ship the tool family")),
                ("budget_tokens", json!(50_000)),
            ]),
            Case::new("feedback", feedback("42".to_string(), true)).typed([
                ("message_id", json!("42")),
                ("thumbs_up", json!(true)),
            ]),
            // The plugin row's payload is the plugin's own shape
            // (ADR-0009): the constructor passes it through, so the
            // property is that the caller's payload is the row's
            // payload.
            Case::new(
                "plugin_row",
                plugin_row(0, "", "acme.span", json!({"span": "s1"})),
            )
            .payload(Payload::Caller {
                key: "payload",
                value: json!({"span": "s1"}),
            })
            .typed([
                ("kind", json!("acme.span")),
                ("payload", json!({"span": "s1"})),
            ]),
        ]
    }

    /// The free-format rows: a payload bag and no typed field at all.
    ///
    /// ADR-0009 keeps them free — the payload *is* the shape — so
    /// they are excluded from the property above: there is no typed
    /// field for the payload to agree with. Their shape is pinned
    /// here instead, and the round trip covers them like any other
    /// row.
    fn free_format_rows() -> Vec<(&'static str, SessionEvent, Vec<&'static str>)>
    {
        vec![
            (
                "warning",
                warning("config_drift", "envelope changed"),
                vec!["kind", "message"],
            ),
            (
                "hook_block",
                hook_block("guard", "denied", "shell", "call-1"),
                vec!["hook", "reason", "tool", "call_id"],
            ),
            (
                "iteration_start",
                iteration_start(7),
                vec!["kind", "iteration"],
            ),
            (
                "iteration_end",
                iteration_end(7, "completed"),
                vec!["kind", "iteration", "action"],
            ),
            (
                "session_shutdown",
                session_shutdown(SessionEndReason::Cancelled),
                vec!["reason"],
            ),
        ]
    }

    /// The constructor writes exactly the payload keys it owns: one
    /// key less and the row says less than it means, one key more and
    /// a second owner has appeared.
    #[test]
    fn every_constructor_writes_exactly_the_keys_it_owns() {
        for case in cases() {
            let wire = written(&case.event);
            match case.payload {
                Payload::NoBag => {
                    assert!(
                        wire.get("data").is_none(),
                        "{}: the row has no payload bag",
                        case.owner
                    );
                }
                Payload::Keys { key, keys } => {
                    assert_eq!(
                        payload_keys(&wire, key),
                        keys,
                        "{}: the payload carries exactly the keys it owns",
                        case.owner
                    );
                }
                Payload::Caller { key, value } => {
                    assert_eq!(
                        wire.get(key),
                        Some(&value),
                        "{}: the payload is the caller's own",
                        case.owner
                    );
                }
            }
        }
    }

    /// The constructor writes every typed field the row has: the
    /// field set on the wire is exactly the fields it wrote, and each
    /// one carries the value it was given.
    #[test]
    fn every_constructor_writes_every_typed_field() {
        for case in cases() {
            let wire = written(&case.event);
            let mut expected: Vec<&str> =
                case.typed.iter().map(|(name, _)| *name).collect();
            expected.sort_unstable();
            assert_eq!(
                typed_keys(&wire),
                expected,
                "{}: the row carries exactly the typed fields it wrote",
                case.owner
            );
            for (name, value) in &case.typed {
                assert_eq!(
                    wire.get(name),
                    Some(value),
                    "{}: the typed field {name}",
                    case.owner
                );
            }
        }
    }

    /// The two views of the row say the same thing: the typed field
    /// and the payload key that carries the same fact carry the same
    /// value. A constructor that wrote one view and contradicted the
    /// other fails here.
    #[test]
    fn every_constructor_agrees_with_its_own_payload() {
        for case in cases() {
            let wire = written(&case.event);
            for (field, key) in &case.twins {
                assert_eq!(
                    at(&wire, field),
                    wire.get("data").and_then(|bag| bag.get(key)),
                    "{}: the typed field {field} and the payload key {key} must agree",
                    case.owner
                );
            }
        }
    }

    /// The row that goes on disk is the row that comes back: the
    /// payload survives the log byte for byte, and no reader has to
    /// repair it on the way in.
    #[test]
    fn every_constructor_reads_back_as_the_row_it_wrote() {
        for case in cases() {
            let restored = round_trip(&case.event)
                .unwrap_or_else(|| panic!("{} round-trips", case.owner));
            assert_eq!(
                serde_json::to_string(&restored).expect("serialises"),
                serde_json::to_string(&case.event).expect("serialises"),
                "{}: the row reads back as the row that was written",
                case.owner
            );
        }
    }

    /// Every tag — the ones a constructor writes and the ones nobody
    /// writes — reads back as the row it was.
    ///
    /// Four tags have no constructor: production never writes
    /// `assistant_chunk`, `tool_call`, `steering_guard` or
    /// `steering_hint`. The vocabulary's skeleton is the only shape
    /// they have, and it round-trips like any other row.
    #[test]
    fn every_tag_reads_back_as_the_row_it_was() {
        let mut covered: HashSet<RowTag> = HashSet::new();
        for case in cases() {
            covered.insert(case.event.tag());
        }
        for (_, row, _) in free_format_rows() {
            covered.insert(row.tag());
        }
        let mut without: Vec<RowTag> = RowTag::ALL
            .iter()
            .copied()
            .filter(|tag| !covered.contains(tag))
            .collect();
        without.sort_by_key(|tag| tag.as_wire());
        assert_eq!(
            without,
            vec![
                RowTag::AssistantChunk,
                RowTag::SteeringGuard,
                RowTag::SteeringHint,
                RowTag::ToolCall,
            ],
            "these four tags have no constructor: production never writes them"
        );

        for case in cases() {
            let restored = round_trip(&case.event).expect("round-trips");
            assert_eq!(restored.tag(), case.event.tag(), "{}", case.owner);
        }
        for (_, row, _) in free_format_rows() {
            let restored = round_trip(&row).expect("round-trips");
            assert_eq!(restored.tag(), row.tag());
        }
        for tag in without {
            let row = tag.empty_event();
            let restored = round_trip(&row)
                .unwrap_or_else(|| panic!("{} round-trips", tag.as_wire()));
            assert_eq!(restored.tag(), tag);
            assert_eq!(
                serde_json::to_string(&restored).expect("serialises"),
                serde_json::to_string(&row).expect("serialises"),
                "{}: the row reads back as the row that was written",
                tag.as_wire()
            );
        }
    }

    /// The free-format rows keep today's shape: the payload bag is
    /// the shape, and no typed field says otherwise.
    #[test]
    fn the_free_format_rows_keep_their_shape() {
        for (owner, row, keys) in free_format_rows() {
            let wire = written(&row);
            assert_eq!(
                payload_keys(&wire, "data"),
                keys,
                "{owner}: the payload carries exactly the keys it owns"
            );
            assert!(
                typed_keys(&wire).is_empty(),
                "{owner}: a free-format row has no typed field"
            );
            let restored = round_trip(&row)
                .unwrap_or_else(|| panic!("{owner} round-trips"));
            assert_eq!(
                serde_json::to_string(&restored).expect("serialises"),
                serde_json::to_string(&row).expect("serialises"),
                "{owner}: the row reads back as the row that was written"
            );
        }
    }
}
