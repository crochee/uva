use serde_json::json;

use super::*;

/// Non-`data` fields a tag requires beyond `seq`/`ts`.
fn extra_fields(tag: &str) -> Value {
    match tag {
        "assistant_chunk" => json!({"chunk_seq": 7}),
        "compaction_start" | "compaction_summary" => {
            json!({"token": "t1"})
        }
        "compaction_end" => json!({"token": "t1", "outcome": "committed"}),
        "sandbox_mode" => json!({"mode": "workspace-write"}),
        "approval_asked" => json!({
            "id": "a1", "call_id": "c1", "tool": "shell",
            "arguments_hash": "sha256:x", "risk": "high",
            "agent": "default",
        }),
        "approval_decided" => json!({
            "id": "a1", "call_id": "c1", "status": "approved",
            "by": "resume",
        }),
        "approval_granted" => json!({
            "id": "a1", "tool": "shell", "scope": "session",
            "workspace_hash": "sha256:w",
        }),
        "tool_dispatch" => json!({
            "call_id": "c1", "tool": "shell",
            "arguments_hash": "sha256:x",
        }),
        "tool_result_transformed" => json!({
            "transformers": ["eraser"], "tool_name": "read",
            "call_id": "c1", "before": "sha256:a",
            "after": "sha256:b",
        }),
        "parked_prompt" => json!({
            "park_id": "park-1", "text": "queued",
        }),
        "park_consumed" => json!({"park_id": "park-1"}),
        "plugin" => json!({
            "kind": "acme.span", "payload": {"span": "s1"},
        }),
        "request_snapshot" => json!({
            "provider": "fake", "model": "fake-1",
            "params_hash": "p", "tools_hash": "t",
            "system_hash": "s", "sections": ["identity"],
            "policy_hash": "pol",
        }),
        _ => json!({}),
    }
}

fn event_json(tag: &str, seq: u64, with_surface_op: bool) -> Value {
    let mut v = json!({
        "seq": seq,
        "ts": "2026-09-10T00:00:00Z",
        "type": tag,
        "data": {"text": "hello"},
    });
    let obj = v.as_object_mut().unwrap();
    for (key, value) in extra_fields(tag).as_object().unwrap() {
        obj.insert(key.clone(), value.clone());
    }
    if with_surface_op {
        obj.insert("surface_op".to_string(), json!("append"));
    }
    v
}

#[test]
fn parses_user_message_with_surface_op() {
    let v = event_json("user_message", 0, true);
    let ev = SessionEvent::from_value(&v).expect("parses");
    assert!(ev.is_surface_eligible());
    assert_eq!(ev.seq(), 0);
    assert!(ev.surface_op().unwrap().is_append());
    assert_eq!(ev.type_tag(), "user_message");
}

#[test]
fn parses_assistant_message_with_replace_op() {
    let v = json!({
        "seq": 7,
        "ts": "2026-09-10T00:00:00Z",
        "type": "assistant_message",
        "data": {"text": "summary"},
        "surface_op": {
            "start": 1,
            "end": 5,
            "source_event_seqs": [3, 4],
        }
    });
    let ev = SessionEvent::from_value(&v).expect("parses");
    let op = ev.surface_op().expect("has surface_op");
    assert!(op.is_replace());
    let r = op.as_replace().unwrap();
    assert_eq!(r.start, 1);
    assert_eq!(r.end, 5);
    assert_eq!(r.source_event_seqs, &[3, 4]);
}

#[test]
fn parses_compaction_with_surface_op() {
    let v = json!({
        "seq": 12,
        "ts": "2026-09-10T00:00:00Z",
        "type": "compaction",
        "data": {"text": "summary"},
        "surface_op": {
            "start": 0,
            "end": 4,
            "source_event_seqs": [1, 2, 3, 4]
        }
    });
    let ev = SessionEvent::from_value(&v).expect("parses");
    assert!(ev.is_surface_eligible());
    assert!(ev.surface_op().unwrap().is_replace());
}

#[test]
fn log_only_events_carry_no_surface_op() {
    let log_only = [
        "assistant_chunk",
        "compaction_start",
        "compaction_summary",
        "compaction_end",
        "tool_call",
        "step",
        "turn",
        "iteration",
        "warning",
        "steering_guard",
        "steering_hint",
        "hook_block",
        "subagent_enter",
        "subagent_exit",
        "request_header",
        "approval_asked",
        "approval_decided",
        "approval_granted",
        "tool_dispatch",
        "request_snapshot",
        "usage",
        "sandbox_mode",
        "session_shutdown",
        "tool_result_transformed",
        "parked_prompt",
        "park_consumed",
        "plugin",
    ];
    for tag in log_only {
        let v = event_json(tag, 99, false);
        let ev = SessionEvent::from_value(&v)
            .unwrap_or_else(|| panic!("{tag} should parse"));
        assert!(!ev.is_surface_eligible(), "{tag} should not be surface");
        assert!(ev.surface_op().is_none(), "{tag} should carry no op");
    }
}

#[test]
fn a_plugin_payload_under_the_cap_is_kept_verbatim() {
    let payload = json!({"span": "s1", "attrs": {"attempt": 1}});
    let ev = SessionEvent::plugin(
        5,
        "2026-10-26T00:00:00Z",
        "acme.span",
        payload.clone(),
    );
    let SessionEvent::Plugin {
        kind,
        payload: kept,
        original_bytes,
        ..
    } = &ev
    else {
        panic!("a plugin row, got {ev:?}");
    };
    assert_eq!(kind, "acme.span");
    assert_eq!(kept, &payload, "under the cap the payload is untouched");
    assert_eq!(*original_bytes, None);
    assert_eq!(ev.type_tag(), "plugin");
    assert!(!ev.is_surface_eligible());
    assert!(ev.surface_op().is_none());
}

#[test]
fn a_plugin_payload_over_the_cap_is_cut_to_its_encoded_budget() {
    // Quote-dense filler: the kept text is stored as a JSON
    // string and escaped a second time when the row is written,
    // so a raw-length cap would let the row carry ~1.5× the
    // constant it promises.
    let filler = "\"".repeat(MAX_PLUGIN_PAYLOAD_BYTES);
    let ev = SessionEvent::plugin(
        6,
        "2026-10-26T00:00:01Z",
        "acme.blob",
        json!({"blob": filler}),
    );
    let SessionEvent::Plugin {
        payload,
        original_bytes,
        ..
    } = &ev
    else {
        panic!("a plugin row, got {ev:?}");
    };
    let kept = payload
        .as_str()
        .expect("a truncated payload is a JSON string");
    // What the row actually carries is the escaped literal.
    let stored = serde_json::to_string(payload).expect("serialises");
    assert!(
        stored.len() <= MAX_PLUGIN_PAYLOAD_BYTES,
        "the row carries at most the cap, got {}",
        stored.len()
    );
    assert!(
        original_bytes.is_some_and(|n| n > MAX_PLUGIN_PAYLOAD_BYTES as u64),
        "the pre-cap size is recorded: {original_bytes:?}"
    );
    // Valid UTF-8 is the log's contract: the cut backs off to a
    // boundary rather than splitting a char.
    assert!(std::str::from_utf8(kept.as_bytes()).is_ok());

    // The record survives the wire with its truncation marker.
    let written = serde_json::to_value(&ev).expect("the row serialises");
    assert_eq!(
        written["original_bytes"],
        json!(original_bytes.expect("recorded"))
    );
    let replayed = SessionEvent::from_value(&written).expect("replays");
    assert_eq!(replayed.seq(), 6);
    assert_eq!(replayed.type_tag(), "plugin");
}

#[test]
fn a_plugin_kind_is_capped_and_an_empty_one_is_named() {
    let long = "k".repeat(MAX_PLUGIN_KIND_BYTES * 4);
    let ev = SessionEvent::plugin(7, "t", long, Value::Null);
    let SessionEvent::Plugin { kind, .. } = &ev else {
        panic!("a plugin row, got {ev:?}");
    };
    let stored = serde_json::to_string(kind).expect("the kind serialises");
    assert!(
        stored.len() <= MAX_PLUGIN_KIND_BYTES,
        "a kind is a filter key, not a body: {} bytes",
        stored.len()
    );

    let empty = SessionEvent::plugin(8, "t", "", Value::Null);
    let SessionEvent::Plugin { kind, .. } = &empty else {
        panic!("a plugin row, got {empty:?}");
    };
    assert_eq!(
        kind, "unnamed",
        "an empty filter key is indistinguishable from a missing one"
    );
}

#[test]
fn bounding_a_row_twice_is_a_no_op() {
    // `append_copied` re-spills a stamped row that already went
    // through `enqueue`, so the bound runs over its own output.
    // If the second pass re-cut, a bounded row would lose bytes
    // every time it was re-spilled, and `original_bytes` would
    // drift off the size the plugin actually sent.
    let filler = "\"".repeat(MAX_PLUGIN_PAYLOAD_BYTES);
    let row = serde_json::to_value(SessionEvent::plugin(
        9,
        "2026-10-26T00:00:02Z",
        "k".repeat(MAX_PLUGIN_KIND_BYTES * 4),
        json!({"blob": filler}),
    ))
    .expect("the row serialises");

    let once = bound_plugin_row(row);
    let twice = bound_plugin_row(once.clone());
    assert_eq!(twice, once);
}

#[test]
fn rejects_unknown_legacy_shape() {
    let legacy = json!({"role": "user", "text": "hi"});
    assert!(SessionEvent::from_value(&legacy).is_none());
    let legacy2 = json!({"type": "UserInput", "data": {"text": "go"}});
    assert!(SessionEvent::from_value(&legacy2).is_none());
}

#[test]
fn round_trips_through_json() {
    let v = json!({
        "seq": 3,
        "ts": "2026-09-10T00:00:00Z",
        "type": "tool_result",
        "data": {"call_id": "c1", "content": "ok"},
        "surface_op": "append"
    });
    let ev = SessionEvent::from_value(&v).unwrap();
    let s = serde_json::to_string(&ev).unwrap();
    let back =
        SessionEvent::from_value(&serde_json::from_str::<Value>(&s).unwrap())
            .expect("round-trip");
    assert_eq!(back.seq(), 3);
    assert!(back.is_surface_eligible());
    assert!(back.surface_op().unwrap().is_append());
}

#[test]
fn a_tool_result_row_carries_its_failure_flag() {
    let failed = json!({
        "seq": 4,
        "ts": "2026-09-10T00:00:00Z",
        "type": "tool_result",
        "data": {"role": "tool", "tool_call_id": "c1", "content": "boom"},
        "is_error": true,
    });
    let ev = SessionEvent::from_value(&failed).expect("the row parses");
    assert!(
        matches!(ev, SessionEvent::ToolResult { is_error: true, .. }),
        "the flag survives the parse: {ev:?}"
    );
    let written = serde_json::to_value(&ev).expect("the row serialises");
    assert_eq!(written["is_error"], json!(true));
    // The flag is beside the message, not inside it: the payload
    // stays exactly what the loop put into the transcript.
    assert_eq!(written["data"]["role"], json!("tool"));
    assert_eq!(written["data"]["tool_call_id"], json!("c1"));
}

#[test]
fn a_tool_result_row_without_a_failure_keeps_todays_wire_shape() {
    let row = json!({
        "seq": 4,
        "ts": "2026-09-10T00:00:00Z",
        "type": "tool_result",
        "data": {"role": "tool", "tool_call_id": "c1", "content": "ok"},
    });
    let ev = SessionEvent::from_value(&row).expect("the row parses");
    assert!(
        matches!(
            ev,
            SessionEvent::ToolResult {
                is_error: false,
                ..
            }
        ),
        "a row without the flag is a tool that returned: {ev:?}"
    );
    let written = serde_json::to_value(&ev).expect("the row serialises");
    assert!(
        written.get("is_error").is_none(),
        "a tool that returned keeps the wire it had: {written}"
    );
}

/// A `user_message` row written before `source` existed has no
/// such key, and must keep deserialising: the field is an
/// annotation on top of the wire, not a new requirement.
#[test]
fn an_old_user_row_without_source_still_deserializes() {
    let row = json!({
        "type": "user_message",
        "seq": 1,
        "ts": "2026-10-25T00:00:00Z",
        "data": {"role": "user", "content": "hi"},
        "surface_op": "append",
    });
    let event: SessionEvent =
        serde_json::from_value(row).expect("the old row parses");
    match event {
        SessionEvent::UserMessage { source, .. } => {
            assert!(source.is_none(), "no source means the ordinary turn")
        }
        other => panic!("expected a user_message row, got {other:?}"),
    }
}

/// The steer row's source survives the wire, and an ordinary row
/// keeps the shape it always had — no `"source": null` on every
/// user turn.
#[test]
fn a_user_row_carries_its_source_only_when_it_has_one() {
    let steer = SessionEvent::UserMessage {
        seq: 3,
        ts: "2026-10-25T00:00:00Z".to_string(),
        data: json!({"role": "user", "content": "focus on the tests"}),
        source: Some("steer".to_string()),
        surface_op: None,
    };
    let written = serde_json::to_value(&steer).expect("serialises");
    assert_eq!(written["source"], json!("steer"));

    let ordinary = SessionEvent::UserMessage {
        seq: 4,
        ts: "2026-10-25T00:00:00Z".to_string(),
        data: json!({"role": "user", "content": "hi"}),
        source: None,
        surface_op: None,
    };
    let written = serde_json::to_value(&ordinary).expect("serialises");
    assert!(
        written.get("source").is_none(),
        "an ordinary turn keeps its shape: {written}"
    );
}

#[test]
fn surface_op_default_is_append() {
    assert!(SurfaceOp::default().is_append());
}

#[test]
fn seq_accessor_returns_payload_value() {
    let v = event_json("step", 42, false);
    let ev = SessionEvent::from_value(&v).unwrap();
    assert_eq!(ev.seq(), 42);
}

#[test]
fn append_helper_round_trips_through_string_form() {
    let op = SurfaceOp::append();
    let s = serde_json::to_string(&op).unwrap();
    assert_eq!(s, "\"append\"");
    let parsed: SurfaceOp = serde_json::from_str(&s).unwrap();
    assert!(parsed.is_append());
}

#[test]
fn compaction_event_requires_surface_op() {
    let v = json!({
        "seq": 1,
        "ts": "2026-09-10T00:00:00Z",
        "type": "compaction",
        "data": {"text": "summary"},
    });
    // Compaction always carries a surface op; missing one
    // MUST fail deserialization.
    assert!(SessionEvent::from_value(&v).is_none());
}

#[test]
fn known_session_event_types_matches_type_tag_arm_count() {
    // Every wire tag in `KNOWN_SESSION_EVENT_TYPES` must round-trip
    // through `empty_event_of` -> `type_tag`. The round trip must
    // be stable across the wire alphabet.
    for tag in KNOWN_SESSION_EVENT_TYPES {
        let event = empty_event_of(tag).expect(tag);
        assert_eq!(event.type_tag(), *tag);
    }
}

#[test]
fn known_session_event_types_round_trips_via_serde() {
    // Each skeleton must serialise + deserialise without losing
    // its type discriminant; downstream consumers may persist
    // the skeleton and expect the shape to round-trip.
    for tag in KNOWN_SESSION_EVENT_TYPES {
        let event = empty_event_of(tag).expect(tag);
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"].as_str(), Some(*tag));
        let restored: SessionEvent = serde_json::from_value(json).unwrap();
        assert_eq!(restored.type_tag(), *tag);
    }
}

#[test]
fn empty_event_of_returns_none_for_unknown_tag() {
    assert!(empty_event_of("not_a_real_event").is_none());
    assert!(empty_event_of("").is_none());
}

#[test]
fn known_session_event_types_has_one_entry_per_variant() {
    // The const is the public contract; every variant of
    // `SessionEvent` must appear exactly once. This catches
    // the case where a new variant is added but the const
    // is forgotten.
    let mut seen = std::collections::HashSet::new();
    for tag in KNOWN_SESSION_EVENT_TYPES {
        assert!(
            seen.insert(*tag),
            "duplicate tag in KNOWN_SESSION_EVENT_TYPES: {tag}"
        );
    }
    assert_eq!(
        seen.len(),
        31,
        "expected 31 typed event variants; add the new one to KNOWN_SESSION_EVENT_TYPES"
    );
}

#[test]
fn assistant_chunk_round_trips_and_omits_finish_reason_when_none() {
    let v = json!({
        "seq": 4,
        "ts": "2026-09-10T00:00:00Z",
        "type": "assistant_chunk",
        "chunk_seq": 2,
        "data": {"delta": "he"},
    });
    let ev = SessionEvent::from_value(&v).expect("parses");
    assert_eq!(ev.seq(), 4);
    assert_eq!(ev.type_tag(), "assistant_chunk");
    assert!(!ev.is_surface_eligible());
    let SessionEvent::AssistantChunk {
        chunk_seq,
        finish_reason,
        ..
    } = &ev
    else {
        panic!("wrong variant");
    };
    assert_eq!(*chunk_seq, 2);
    assert!(finish_reason.is_none());

    // `None` is omitted from the wire rather than written as
    // `null`, so a non-terminal chunk stays shape-stable.
    let s = serde_json::to_string(&ev).unwrap();
    assert!(!s.contains("finish_reason"), "got {s}");
    let back: SessionEvent = serde_json::from_str(&s).unwrap();
    assert_eq!(back.type_tag(), "assistant_chunk");

    // A terminal chunk carries the provider stop reason.
    let terminal = json!({
        "seq": 5,
        "ts": "2026-09-10T00:00:00Z",
        "type": "assistant_chunk",
        "chunk_seq": 3,
        "finish_reason": "end_turn",
        "data": {"delta": ""},
    });
    let ev = SessionEvent::from_value(&terminal).expect("parses");
    let SessionEvent::AssistantChunk { finish_reason, .. } = &ev else {
        panic!("wrong variant");
    };
    assert_eq!(finish_reason.as_deref(), Some("end_turn"));
    let s = serde_json::to_string(&ev).unwrap();
    assert!(s.contains("\"finish_reason\":\"end_turn\""), "got {s}");
}

#[test]
fn lifecycle_shutdown_round_trips() {
    let v = json!({
        "seq": 11,
        "ts": "2026-09-10T00:00:00Z",
        "type": "session_shutdown",
        "data": {"reason": "controller_close", "children": 2},
    });
    let ev = SessionEvent::from_value(&v).expect("parses");
    assert_eq!(ev.type_tag(), "session_shutdown");
    assert_eq!(ev.seq(), 11);
    assert!(!ev.is_surface_eligible());
    assert!(ev.surface_op().is_none());

    let s = serde_json::to_string(&ev).unwrap();
    let restored =
        SessionEvent::from_value(&serde_json::from_str::<Value>(&s).unwrap())
            .expect("round-trip");
    assert_eq!(restored.type_tag(), "session_shutdown");
    assert_eq!(restored.seq(), 11);
}

/// R31: the sandbox-mode override rides the log as one
/// log-only row. A row written by a newer build — a mode this
/// build does not know — must still parse, because the policy
/// fold ignores what it cannot interpret rather than failing
/// the log.
#[test]
fn sandbox_mode_row_parses_and_preserves_unknown_modes() {
    let ev = SessionEvent::from_value(&event_json("sandbox_mode", 9, false))
        .expect("parses");
    assert_eq!(ev.type_tag(), "sandbox_mode");
    assert_eq!(ev.seq(), 9);
    assert_eq!(ev.sandbox_mode(), Some("workspace-write"));
    assert!(!ev.is_surface_eligible());
    assert!(ev.surface_op().is_none());

    let unknown = json!({
        "seq": 10,
        "ts": "2026-09-12T00:00:00Z",
        "type": "sandbox_mode",
        "mode": "future-mode",
    });
    let ev = SessionEvent::from_value(&unknown).expect("parses");
    assert_eq!(ev.sandbox_mode(), Some("future-mode"));

    // A row that predates the variant never misreads as one.
    let legacy = SessionEvent::from_value(&event_json("warning", 1, false))
        .expect("parses");
    assert!(legacy.sandbox_mode().is_none());
}

#[test]
fn compaction_outcome_serialises_snake_case() {
    for (outcome, wire) in [
        (CompactionLifecycleOutcome::Committed, "committed"),
        (CompactionLifecycleOutcome::Failed, "failed"),
        (CompactionLifecycleOutcome::Interrupted, "interrupted"),
    ] {
        assert_eq!(
            serde_json::to_string(&outcome).unwrap(),
            format!("\"{wire}\"")
        );
        let back: CompactionLifecycleOutcome =
            serde_json::from_str(&format!("\"{wire}\"")).unwrap();
        assert_eq!(back, outcome);
        assert_eq!(outcome.as_str(), wire);
    }
    assert!(
        serde_json::from_str::<CompactionLifecycleOutcome>("\"bogus\"")
            .is_err()
    );
}

#[test]
fn surface_token_mints_ulids_and_serialises_transparently() {
    let minted = SurfaceToken::new();
    assert_eq!(minted.as_str().len(), 26, "ULID text is 26 chars");
    assert_eq!(minted.to_string(), minted.as_str());
    assert_eq!(
        serde_json::to_string(&minted).unwrap(),
        format!("\"{minted}\"")
    );

    let pinned = SurfaceToken::from_string("t1");
    assert_eq!(serde_json::to_string(&pinned).unwrap(), "\"t1\"");
    let back: SurfaceToken = serde_json::from_str("\"t1\"").unwrap();
    assert_eq!(back, pinned);
    assert_eq!(
        SurfaceToken::from_string(String::from("t1")),
        pinned,
        "Eq spans both `Into<String>` forms"
    );
    let mut set = std::collections::HashSet::new();
    assert!(set.insert(pinned.clone()));
    assert!(set.contains(&back));
    assert!(set.insert(SurfaceToken::default()));
    assert_eq!(set.len(), 2);
}

#[test]
fn pre_r30_usage_row_parses_without_typed_field() {
    // Old-session payload: no `usage` key at all. The typed
    // field MUST default to `None` and stay out of the
    // serialised form.
    let row = json!({
        "type": "usage",
        "seq": 4,
        "ts": "2026-09-10T00:00:00Z",
        "data": {
            "prompt_tokens": 900,
            "completion_tokens": 60,
            "total_tokens": 960,
            "cache_read_tokens": 128,
            "cache_write_tokens": 32
        }
    });
    let event = SessionEvent::from_value(&row).expect("old row parses");
    assert_eq!(
        event.provider_usage(),
        Some(UsageBuckets {
            input_tokens: 900,
            output_tokens: 60,
            cache_read_tokens: Some(128),
            cache_write_tokens: Some(32),
        })
    );
    let round = serde_json::to_value(&event).unwrap();
    assert!(round.get("usage").is_none(), "None field is skipped");
}

#[test]
fn typed_usage_field_wins_over_legacy_data() {
    let row = json!({
        "type": "usage",
        "seq": 5,
        "ts": "2026-09-10T00:00:00Z",
        "data": {"prompt_tokens": 1, "completion_tokens": 1},
        "usage": {
            "input_tokens": 700,
            "output_tokens": 40,
            "cache_read_tokens": 12
        }
    });
    let event = SessionEvent::from_value(&row).expect("new row parses");
    let buckets = event.provider_usage().expect("report");
    assert_eq!(buckets.input_tokens, 700);
    assert_eq!(buckets.output_tokens, 40);
    assert_eq!(buckets.cache_read_tokens, Some(12));
    assert_eq!(buckets.cache_write_tokens, None);
}

#[test]
fn usage_row_without_a_decodable_report_has_none_provider_usage() {
    // A usage row whose data carries no `prompt_tokens` is not
    // a provider report; readers must see `None` rather than a
    // fabricated zero sample.
    let row = json!({
        "type": "usage",
        "seq": 6,
        "ts": "2026-09-10T00:00:00Z",
        "data": {"note": "no buckets"}
    });
    let event = SessionEvent::from_value(&row).expect("parses");
    assert!(event.provider_usage().is_none());
    assert!(
        SessionEvent::UserMessage {
            seq: 0,
            ts: String::new(),
            data: json!({}),
            source: None,
            surface_op: None,
        }
        .provider_usage()
        .is_none()
    );
}

#[test]
fn typed_usage_field_round_trips_through_json() {
    let event = SessionEvent::Usage {
        seq: 9,
        ts: "2026-09-10T00:00:00Z".to_string(),
        data: json!({"prompt_tokens": 5}),
        usage: Some(UsageBuckets {
            input_tokens: 5,
            output_tokens: 1,
            cache_read_tokens: None,
            cache_write_tokens: Some(3),
        }),
    };
    let restored = round_trip_value(&event);
    assert_eq!(restored.provider_usage(), event.provider_usage());
    assert_eq!(restored.type_tag(), "usage");
}

fn round_trip_value(event: &SessionEvent) -> SessionEvent {
    let value = serde_json::to_value(event).unwrap();
    SessionEvent::from_value(&value).expect("round-trip")
}

/// The audit row: the log must be able to name the plugin that
/// rewrote a tool result, and to tell the two versions apart.
#[test]
fn a_transformed_result_row_names_its_plugin_and_its_two_versions() {
    let row = json!({
        "type": "tool_result_transformed",
        "seq": 11,
        "ts": "2026-10-26T00:00:00Z",
        "transformers": ["eraser", "a, b"],
        "tool_name": "read",
        "call_id": "c1",
        "before": "sha256:aaaa",
        "after": "sha256:bbbb",
    });
    let event = SessionEvent::from_value(&row).expect("the row parses");
    let SessionEvent::ToolResultTransformed {
        transformers,
        tool_name,
        call_id,
        before,
        after,
        ..
    } = &event
    else {
        panic!("the row is an audit row: {event:?}");
    };
    assert_eq!(transformers, &["eraser", "a, b"]);
    assert_eq!(tool_name, "read");
    assert_eq!(call_id, "c1");
    assert_ne!(before, after, "the two versions must be distinguishable");

    // Log-only: it never joins the model's history or the surface.
    assert!(!event.is_surface_eligible());
    assert!(event.surface_op().is_none());
    assert_eq!(event.type_tag(), "tool_result_transformed");
    assert_eq!(event.seq(), 11);
    assert_eq!(
        round_trip_value(&event).type_tag(),
        "tool_result_transformed"
    );
}
