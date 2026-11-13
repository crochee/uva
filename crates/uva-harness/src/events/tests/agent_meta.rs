//! `AgentMeta` constructors, equality, and Debug.

use super::super::AgentMeta;

#[test]
fn agent_meta_constructor_and_equality() {
    let m = AgentMeta::new("p", "c", 2);
    assert_eq!(m.parent_session_id, "p");
    assert_eq!(m.child_session_id, "c");
    assert_eq!(m.parent_depth, 2);
    let m1 = AgentMeta::new(String::from("p"), String::from("c"), 0);
    let m2 = AgentMeta::new("p", "c", 0);
    assert_eq!(m1.parent_session_id, m2.parent_session_id);

    let mut other = m.clone();
    other.parent_depth = 3;
    assert_ne!(m, other);
    let mut other = m.clone();
    other.child_session_id = "other".into();
    assert_ne!(m, other);
}

#[test]
fn constructors_build_documented_shapes() {
    use uva_provider::ContentPart;

    use super::super::{AgentEvent, SystemEvent, WarningKind};

    match AgentEvent::text_delta("hi") {
        AgentEvent::Model(ContentPart::Text(t)) => {
            assert_eq!(t.text, "hi");
            assert!(t.cache_control.is_none());
        }
        _ => panic!("expected Model(Text)"),
    }
    let progress = AgentEvent::System(SystemEvent::Progress {
        message: "step".to_string(),
        step: 3,
        total: 10,
    });
    let progress_json = serde_json::to_string(&progress).unwrap();
    assert!(progress_json.contains("\"type\":\"progress\""));
    assert!(progress_json.contains("\"step\":3"));
    let warning_event = AgentEvent::System(SystemEvent::Warning {
        kind: WarningKind::Hook,
        message: "oops".to_string(),
        iteration: None,
    });
    let warning_json = serde_json::to_string(&warning_event).unwrap();
    assert!(warning_json.contains("\"kind\":\"hook\""));
    assert!(warning_json.contains("\"message\":\"oops\""));
    assert!(warning_json.contains("\"iteration\":null"));
    let warning = AgentEvent::System(SystemEvent::Warning {
        kind: WarningKind::Loop,
        message: "loop detected".to_string(),
        iteration: None,
    });
    assert!(
        serde_json::to_string(&warning)
            .unwrap()
            .contains("\"kind\":\"loop\"")
    );
    match AgentEvent::usage(100, 50, Some(20), None) {
        AgentEvent::System(SystemEvent::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
        }) => {
            assert_eq!(input_tokens, 100);
            assert_eq!(output_tokens, 50);
            assert_eq!(cache_read_tokens, Some(20));
            assert_eq!(cache_creation_tokens, None);
        }
        _ => panic!("expected System(Usage)"),
    }
}
