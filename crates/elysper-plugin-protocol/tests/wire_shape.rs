use elysper_plugin_protocol::{
    PluginCapabilities,
    PluginDecision,
    PluginError,
    PluginEvent,
    PluginPublish,
    ToolSpec,
};
use serde_json::json;

#[test]
fn capabilities_round_trip() {
    let caps = PluginCapabilities {
        tools: true,
        observe: false,
        transform: true,
        gate: true,
        publish: false,
    };
    let json = serde_json::to_string(&caps).unwrap();
    assert_eq!(
        serde_json::from_str::<PluginCapabilities>(&json).unwrap(),
        caps
    );
}

#[test]
fn event_shape_is_tagged() {
    let event = PluginEvent::BeforeTool {
        tool_name: "read".into(),
        call_id: "c1".into(),
        arguments: serde_json::json!({"path": "a.txt"}),
    };
    let json = serde_json::to_value(&event).unwrap();
    assert_eq!(json["type"], "before_tool");
}

#[test]
fn decision_shape_is_tagged() {
    let json = serde_json::to_value(PluginDecision::Block {
        reason: "no".into(),
    })
    .unwrap();
    assert_eq!(json["type"], "block");
    assert_eq!(json["reason"], "no");
}

#[test]
fn error_codes_are_stable() {
    assert_eq!(
        serde_json::to_value(PluginError::Timeout).unwrap(),
        serde_json::json!("timeout")
    );
}

// The tests below pin the *entire* wire shape. Every wire name is the
// Rust identifier, so a rename with a green suite is exactly the ABI
// break this crate exists to prevent.

#[test]
fn capabilities_wire_shape() {
    let caps = PluginCapabilities {
        tools: true,
        observe: true,
        transform: false,
        gate: true,
        publish: true,
    };
    assert_eq!(
        serde_json::to_value(caps).unwrap(),
        json!({
            "tools": true,
            "observe": true,
            "transform": false,
            "gate": true,
            "publish": true,
        })
    );
}

#[test]
fn capabilities_absent_keys_mean_false() {
    let caps: PluginCapabilities =
        serde_json::from_value(json!({"tools": true})).unwrap();
    assert_eq!(
        caps,
        PluginCapabilities {
            tools: true,
            observe: false,
            transform: false,
            gate: false,
            publish: false,
        }
    );
}

#[test]
fn tool_spec_wire_shape() {
    let spec = ToolSpec {
        name: "read".to_string(),
        description: "read a file".to_string(),
        input_schema: json!({"type": "object"}),
    };
    assert_eq!(
        serde_json::to_value(&spec).unwrap(),
        json!({
            "name": "read",
            "description": "read a file",
            "input_schema": {"type": "object"},
        })
    );
}

#[test]
fn publish_wire_shape() {
    let publish = PluginPublish {
        kind: "custom".to_string(),
        payload: json!({"x": 1}),
    };
    assert_eq!(
        serde_json::to_value(&publish).unwrap(),
        json!({
            "kind": "custom",
            "payload": {"x": 1},
        })
    );
    // A missing payload is not a publish: the params must name both
    // fields, so a malformed notification fails to deserialise and
    // the host drops it.
    assert!(
        serde_json::from_value::<PluginPublish>(json!({
            "kind": "custom",
        }))
        .is_err()
    );
}

#[test]
fn event_wire_shapes() {
    let cases = [
        (
            PluginEvent::BeforeTool {
                tool_name: "read".into(),
                call_id: "c1".into(),
                arguments: json!({"path": "a.txt"}),
            },
            json!({
                "type": "before_tool",
                "tool_name": "read",
                "call_id": "c1",
                "arguments": {"path": "a.txt"},
            }),
        ),
        (
            PluginEvent::AfterTool {
                tool_name: "read".into(),
                call_id: "c1".into(),
                is_error: true,
            },
            json!({
                "type": "after_tool",
                "tool_name": "read",
                "call_id": "c1",
                "is_error": true,
            }),
        ),
        (
            PluginEvent::BeforeStep { iteration: 2 },
            json!({"type": "before_step", "iteration": 2}),
        ),
        (
            PluginEvent::AfterStep { iteration: 3 },
            json!({"type": "after_step", "iteration": 3}),
        ),
        (
            PluginEvent::TurnStart {
                agent_name: "planner".into(),
            },
            json!({"type": "turn_start", "agent_name": "planner"}),
        ),
        (
            PluginEvent::TurnEnd {
                agent_name: "planner".into(),
                reason: "done".into(),
            },
            json!({
                "type": "turn_end",
                "agent_name": "planner",
                "reason": "done",
            }),
        ),
    ];
    for (event, expected) in cases {
        assert_eq!(serde_json::to_value(&event).unwrap(), expected);
        assert_eq!(
            serde_json::from_value::<PluginEvent>(expected).unwrap(),
            event
        );
    }
}

#[test]
fn decision_wire_shapes() {
    let cases = [
        (PluginDecision::Allow, json!({"type": "allow"})),
        (
            PluginDecision::Block {
                reason: "no".into(),
            },
            json!({"type": "block", "reason": "no"}),
        ),
        (
            PluginDecision::Ask {
                reason: "approve?".into(),
            },
            json!({"type": "ask", "reason": "approve?"}),
        ),
        (
            PluginDecision::Terminate {
                reason: "stop".into(),
            },
            json!({"type": "terminate", "reason": "stop"}),
        ),
    ];
    for (decision, expected) in cases {
        assert_eq!(serde_json::to_value(&decision).unwrap(), expected);
        assert_eq!(
            serde_json::from_value::<PluginDecision>(expected).unwrap(),
            decision
        );
    }
}

#[test]
fn error_wire_shapes() {
    let cases = [
        (PluginError::Unavailable, json!("unavailable")),
        (PluginError::Timeout, json!("timeout")),
        (PluginError::Denied, json!("denied")),
        (PluginError::BadRequest, json!("bad_request")),
        (PluginError::Internal, json!("internal")),
    ];
    for (error, expected) in cases {
        assert_eq!(serde_json::to_value(error).unwrap(), expected);
        assert_eq!(
            serde_json::from_value::<PluginError>(expected).unwrap(),
            error
        );
    }
}
