//! Wire-tag contract: serde `type` tags for every public
//! events enum, plus `kind()` labels and the catch-all rejection
//! of unknown variant strings.

use super::{
    super::{AgentEvent, SessionEndReason, SystemEvent, WarningKind},
    helpers::{
        all_system_events,
        make_meta,
        make_reasoning,
        make_text,
        make_tool_use,
    },
};

#[test]
fn wire_tags_are_stable() {
    // AgentEvent outer tag + content slot.
    let ev = AgentEvent::text_delta("x");
    let json: serde_json::Value = serde_json::to_value(&ev).unwrap();
    assert!(json.get("type").is_some());
    assert!(json.get("data").is_some());

    // The nested Model(ContentPart) carries the inner part tag too.
    let nested = AgentEvent::Model(make_reasoning("Let me think"));
    let nested_json = serde_json::to_string(&nested).unwrap();
    assert!(nested_json.contains("\"type\":\"Model\""));
    assert!(nested_json.contains("\"type\":\"reasoning\""));

    // SystemEvent: all 8 snake_case tags.
    let expected_tags = [
        "session_started",
        "session_ended",
        "session_interrupted",
        "progress",
        "tool_progress",
        "warning",
        "usage",
        "policy_violation",
    ];
    let cases = all_system_events();
    assert_eq!(cases.len(), 8, "SystemEvent fixture must cover 8 variants");
    for (ev, expected_tag) in cases.iter().zip(expected_tags) {
        let json = serde_json::to_string(ev).unwrap();
        assert!(
            json.contains(&format!("\"type\":\"{expected_tag}\"")),
            "type tag for {ev:?} must be `{expected_tag}`: {json}"
        );
    }

    // WarningKind snake_case.
    for (kind, tag) in [
        (WarningKind::Loop, "\"loop\""),
        (WarningKind::TokenBudget, "\"token_budget\""),
        (WarningKind::ContextCompaction, "\"context_compaction\""),
        (WarningKind::Hook, "\"hook\""),
        (WarningKind::EditConflict, "\"edit_conflict\""),
        (WarningKind::Guard, "\"guard\""),
        (WarningKind::Attachments, "\"attachments\""),
    ] {
        assert_eq!(serde_json::to_string(&kind).unwrap(), tag);
    }

    // SessionEndReason: externally-tagged PascalCase.
    assert_eq!(
        serde_json::to_string(&SessionEndReason::Completed).unwrap(),
        "\"Completed\""
    );
    assert_eq!(
        serde_json::to_string(&SessionEndReason::Error(
            "rate_limit_exceeded".to_string()
        ))
        .unwrap(),
        r#"{"Error":"rate_limit_exceeded"}"#
    );
}

#[test]
fn unknown_variant_strings_are_rejected() {
    let sys: Result<SystemEvent, _> =
        serde_json::from_str(r#"{"type":"future_variant","session_id":"x"}"#);
    assert!(sys.is_err());
    let top: Result<AgentEvent, _> =
        serde_json::from_str(r#"{"type":"UnknownVariant","data":{}}"#);
    assert!(top.is_err());
    let reason: Result<SessionEndReason, _> =
        serde_json::from_str("\"NonexistentReason\"");
    assert!(reason.is_err());
}

#[test]
fn kind_returns_stable_label_for_every_variant() {
    assert_eq!(AgentEvent::Model(make_text("x")).kind(), "Model");
    assert_eq!(AgentEvent::Model(make_tool_use()).kind(), "Model");
    assert_eq!(
        AgentEvent::ModelDone(synthia_provider::SamplingResult::default())
            .kind(),
        "ModelDone"
    );
    assert_eq!(
        AgentEvent::System(SystemEvent::SessionStarted {
            session_id: "s".into()
        })
        .kind(),
        "System"
    );
    assert_eq!(
        AgentEvent::Agent(make_meta(), Box::new(AgentEvent::text_delta("i")))
            .kind(),
        "Agent"
    );

    let expected_kinds = [
        "SessionStarted",
        "SessionEnded",
        "SessionInterrupted",
        "Progress",
        "ToolProgress",
        "Warning",
        "Usage",
        "PolicyViolation",
    ];
    for (ev, expected) in all_system_events().into_iter().zip(expected_kinds) {
        assert_eq!(ev.kind(), expected, "kind() mismatch for {ev:?}");
    }
}

#[test]
fn agent_event_agent_carries_meta_and_inner() {
    let inner = AgentEvent::text_delta("inner-text");
    let meta = make_meta();
    let ev = AgentEvent::Agent(meta, Box::new(inner));
    let json: serde_json::Value = serde_json::to_value(&ev).unwrap();
    assert_eq!(json["type"], "Agent");
    // data is a tuple [meta, inner].
    let data = json["data"].as_array().expect("data must be array");
    assert_eq!(data.len(), 2);
    assert_eq!(data[0]["parent_session_id"], "parent-1");
    assert_eq!(data[0]["child_session_id"], "agent-1");
    assert_eq!(data[0]["parent_depth"], 1);
    assert_eq!(data[1]["type"], "Model");
}

#[test]
fn model_done_carries_sampling_result() {
    use super::helpers::sampling_result_for_test;
    let event = AgentEvent::ModelDone(sampling_result_for_test());
    let json = serde_json::to_string(&event).unwrap();
    assert!(json.contains("\"type\":\"ModelDone\""));
    assert!(json.contains("\"text\":\"hi\""));
}

#[test]
fn plugin_is_a_live_only_notification() {
    let event = SystemEvent::Plugin {
        kind: "acme.span".to_string(),
        payload: serde_json::json!({"span": "s1"}),
    };
    let json: serde_json::Value = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "plugin", "the wire tag is snake_case");
    assert_eq!(json["kind"], "acme.span", "the kind is the plugin's own");
    assert_eq!(json["payload"]["span"], "s1");
    assert_eq!(event.kind(), "Plugin");

    // Live only: the durable half of the same fact is the session
    // log's own `plugin` row, never this event.
    let wrapped = AgentEvent::System(event);
    assert!(
        !wrapped.is_durable(),
        "a plugin notification is a ping, not a record"
    );
    assert!(
        wrapped.size_hint() >= "acme.span".len() + "s1".len(),
        "the payload is priced into the buffer hint"
    );
}
