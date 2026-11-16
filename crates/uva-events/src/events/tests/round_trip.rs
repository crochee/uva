//! Serde round-trips and pairwise variant distinctness — the
//! replay contracts (sqlite, file, kafka) read through these
//! invariants.

use super::{
    super::{AgentEvent, SessionEndReason, SystemEvent, WarningKind},
    helpers::{all_system_events, make_reasoning},
};

#[test]
fn round_trips_through_json() {
    use super::helpers::sampling_result_for_test;

    // All 8 SystemEvent variants.
    for ev in all_system_events() {
        let json = serde_json::to_string(&ev).unwrap();
        let parsed: SystemEvent =
            serde_json::from_str(&json).expect("round-trip");
        assert_eq!(parsed, ev, "round-trip lost {ev:?}");
    }

    // AgentEvent top-level shapes, including the two newest
    // System variants riding the System arm.
    let top_level = [
        AgentEvent::text_delta("hi"),
        AgentEvent::Model(make_reasoning("think")),
        AgentEvent::ModelDone(sampling_result_for_test()),
        AgentEvent::System(SystemEvent::SessionStarted {
            session_id: "s".into(),
        }),
        AgentEvent::System(SystemEvent::SessionEnded {
            reason: SessionEndReason::Completed,
        }),
    ];
    for ev in top_level {
        let json = serde_json::to_string(&ev).unwrap();
        let back: AgentEvent = serde_json::from_str(&json).unwrap();
        let json2 = serde_json::to_string(&back).unwrap();
        assert_eq!(json, json2, "round-trip failed for {ev:?}");
    }

    // WarningKind (all 6).
    for kind in [
        WarningKind::Loop,
        WarningKind::ContextCompaction,
        WarningKind::Hook,
        WarningKind::Guard,
        WarningKind::StructuredOutput,
        WarningKind::Attachments,
    ] {
        let json = serde_json::to_string(&kind).unwrap();
        let parsed: WarningKind = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, kind);
    }

    // SessionEndReason (all 4).
    for reason in [
        SessionEndReason::Completed,
        SessionEndReason::Cancelled,
        SessionEndReason::Error("provider_429".to_string()),
        SessionEndReason::MaxIterations,
    ] {
        let json = serde_json::to_string(&reason).unwrap();
        let parsed: SessionEndReason = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, reason);
    }
}

#[test]
fn system_event_variants_are_distinct() {
    let all = all_system_events();
    assert_eq!(all.len(), 12);
    for (i, a) in all.iter().enumerate() {
        for b in &all[(i + 1)..] {
            assert_ne!(a, b, "{a:?} must differ from {b:?}");
        }
    }
}

#[test]
fn session_end_reason_variants_are_distinct() {
    let all = [
        SessionEndReason::Completed,
        SessionEndReason::Cancelled,
        SessionEndReason::Error("x".to_string()),
        SessionEndReason::MaxIterations,
    ];
    for (i, a) in all.iter().enumerate() {
        for b in &all[(i + 1)..] {
            assert_ne!(a, b, "{a:?} must differ from {b:?}");
        }
    }
    assert_ne!(
        SessionEndReason::Error("a".to_string()),
        SessionEndReason::Error("b".to_string())
    );
}

#[test]
fn size_hint_bounds_the_serde_json_len() {
    for text in ["the quick brown fox", &"x".repeat(4_096), ""] {
        let event = AgentEvent::text_delta(text);
        let wire = serde_json::to_vec(&event).unwrap().len();
        assert!(wire > 0 || text.is_empty());
        let hint = event.size_hint();
        assert!(
            hint >= text.len(),
            "hint must cover the payload text: {hint} < {}",
            text.len()
        );
        assert!(
            hint < 4 * wire.max(1),
            "hint must track the wire length: {hint} vs {wire}"
        );
    }
}
