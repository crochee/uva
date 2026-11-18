//! Tests for the session-status module.
//!
//! Everything here goes through the module's public face: the two
//! state vocabularies, the rule that turns facts into a state, and
//! the two label projections. Nothing reaches into the module's
//! internals.

use super::*;

/// The declared filter vocabulary is the wire contract: the listing
/// accepts exactly these nine labels, so a rename is a wire change.
#[test]
fn the_declared_filter_vocabulary_is_the_wire_contract() {
    assert_eq!(
        STATUS_FILTER_LABELS,
        [
            "unspecified",
            "submitted",
            "working",
            "completed",
            "failed",
            "canceled",
            "input_required",
            "rejected",
            "auth_required",
        ]
    );
}

#[test]
fn parse_status_filter_accepts_all_known_labels() {
    for label in STATUS_FILTER_LABELS {
        assert_eq!(parse_status_filter(label).unwrap(), label);
    }
}

#[test]
fn parse_status_filter_rejects_unknown_label() {
    let err = parse_status_filter("not_a_state").expect_err("unknown label");
    assert!(matches!(err, uva::core::Error::InvalidItem { .. }));
    assert!(err.to_string().contains("status filter 'not_a_state'"));
}

#[test]
fn parse_status_filter_is_case_sensitive() {
    let err = parse_status_filter("Completed").expect_err("uppercase");
    assert!(matches!(err, uva::core::Error::InvalidItem { .. }));
}

/// Every status the listing can report must be filterable on — a
/// produced status you cannot filter by is a status nobody can query.
#[test]
fn every_produced_status_is_filterable() {
    for status in [
        SessionStatus::Unspecified,
        SessionStatus::Working,
        SessionStatus::InputRequired,
        SessionStatus::Canceled,
        SessionStatus::Completed,
    ] {
        let label = status.as_wire();
        assert_eq!(
            parse_status_filter(label).unwrap(),
            label,
            "produced status {label} is not filterable"
        );
    }
}

/// The whole rule, in one table.
///
/// `controller` is the controller's in-memory state (`None` = this
/// process holds no controller for the session), `awaiting` is the
/// log's open-approval verdict, `has_log` is whether the log holds
/// any row at all.
#[test]
fn the_rule_is_exhaustive_over_every_fact_combination() {
    type Row = (
        Option<SessionState>,
        bool,
        bool,
        SessionState,
        SessionStatus,
    );
    let cases: [Row; 20] = [
        // No controller: the log is the only source.
        (
            None,
            false,
            false,
            SessionState::Idle,
            SessionStatus::Unspecified,
        ),
        (
            None,
            false,
            true,
            SessionState::Idle,
            SessionStatus::Completed,
        ),
        (
            None,
            true,
            false,
            SessionState::AwaitingApproval,
            SessionStatus::Unspecified,
        ),
        (
            None,
            true,
            true,
            SessionState::AwaitingApproval,
            SessionStatus::InputRequired,
        ),
        // Idle.
        (
            Some(SessionState::Idle),
            false,
            false,
            SessionState::Idle,
            SessionStatus::Unspecified,
        ),
        (
            Some(SessionState::Idle),
            false,
            true,
            SessionState::Idle,
            SessionStatus::Completed,
        ),
        (
            Some(SessionState::Idle),
            true,
            false,
            SessionState::AwaitingApproval,
            SessionStatus::Unspecified,
        ),
        (
            Some(SessionState::Idle),
            true,
            true,
            SessionState::AwaitingApproval,
            SessionStatus::InputRequired,
        ),
        // Running outranks the log: a run is executing right now.
        (
            Some(SessionState::Running),
            false,
            false,
            SessionState::Running,
            SessionStatus::Working,
        ),
        (
            Some(SessionState::Running),
            false,
            true,
            SessionState::Running,
            SessionStatus::Working,
        ),
        (
            Some(SessionState::Running),
            true,
            false,
            SessionState::Running,
            SessionStatus::Working,
        ),
        (
            Some(SessionState::Running),
            true,
            true,
            SessionState::Running,
            SessionStatus::Working,
        ),
        // Awaiting approval: kept while the log holds the open ask,
        // demoted to idle once the ask is answered.
        (
            Some(SessionState::AwaitingApproval),
            false,
            false,
            SessionState::Idle,
            SessionStatus::Unspecified,
        ),
        (
            Some(SessionState::AwaitingApproval),
            false,
            true,
            SessionState::Idle,
            SessionStatus::Completed,
        ),
        (
            Some(SessionState::AwaitingApproval),
            true,
            false,
            SessionState::AwaitingApproval,
            SessionStatus::Unspecified,
        ),
        (
            Some(SessionState::AwaitingApproval),
            true,
            true,
            SessionState::AwaitingApproval,
            SessionStatus::InputRequired,
        ),
        // Cancelled.
        (
            Some(SessionState::Cancelled),
            false,
            false,
            SessionState::Cancelled,
            SessionStatus::Unspecified,
        ),
        (
            Some(SessionState::Cancelled),
            false,
            true,
            SessionState::Cancelled,
            SessionStatus::Canceled,
        ),
        (
            Some(SessionState::Cancelled),
            true,
            false,
            SessionState::AwaitingApproval,
            SessionStatus::Unspecified,
        ),
        (
            Some(SessionState::Cancelled),
            true,
            true,
            SessionState::AwaitingApproval,
            SessionStatus::InputRequired,
        ),
    ];

    for (controller, awaiting, has_log, want_state, want_status) in cases {
        let facts = StatusFacts {
            controller,
            awaiting,
        };
        assert_eq!(
            controller_state(facts),
            want_state,
            "controller_state({controller:?}, awaiting={awaiting})"
        );
        assert_eq!(
            session_status(facts, has_log),
            want_status,
            "session_status({controller:?}, awaiting={awaiting}, has_log={has_log})"
        );
    }
}

/// The cell that had no test before this module existed: a
/// controller whose flag still says «awaiting approval» while the log
/// holds no open ask is **stale**, and a stale flag must not be
/// reported as a session that needs input.
#[test]
fn a_stale_awaiting_approval_flag_is_demoted() {
    let facts = StatusFacts {
        controller: Some(SessionState::AwaitingApproval),
        awaiting: false,
    };
    assert_eq!(controller_state(facts), SessionState::Idle);
    assert_eq!(
        session_status(facts, true),
        SessionStatus::Completed,
        "a session whose approvals were answered has finished"
    );
}

/// The log outranks the flag: an open ask means the session waits,
/// whatever the controller remembers.
#[test]
fn an_open_approval_outranks_the_flag() {
    let facts = StatusFacts {
        controller: Some(SessionState::Idle),
        awaiting: true,
    };
    assert_eq!(controller_state(facts), SessionState::AwaitingApproval);
    assert_eq!(session_status(facts, true), SessionStatus::InputRequired);
}

/// One vocabulary on the wire: the session status's five spellings,
/// and nothing else. The controller state has no wire spelling — it
/// is the controller's own lifecycle, and the wire carries the
/// refined status.
#[test]
fn the_wire_carries_one_session_vocabulary() {
    assert_eq!(SessionStatus::Unspecified.as_wire(), "unspecified");
    assert_eq!(SessionStatus::Working.as_wire(), "working");
    assert_eq!(SessionStatus::InputRequired.as_wire(), "input_required");
    assert_eq!(SessionStatus::Canceled.as_wire(), "canceled");
    assert_eq!(SessionStatus::Completed.as_wire(), "completed");
}

/// The enum's `snake_case` derivation and the hand-written
/// `as_wire` mirror must spell every status the same way — the wire
/// spelling is what the client reads, and the listing's filter
/// compares against it.
#[test]
fn the_wire_spelling_and_the_derived_one_agree() {
    for status in [
        SessionStatus::Unspecified,
        SessionStatus::Working,
        SessionStatus::InputRequired,
        SessionStatus::Canceled,
        SessionStatus::Completed,
    ] {
        let derived =
            serde_json::to_value(status).expect("a status serializes");
        assert_eq!(
            derived.as_str(),
            Some(status.as_wire()),
            "{status:?} serializes to {derived} but spells itself {}",
            status.as_wire()
        );
    }
}
