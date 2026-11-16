//! The lifecycle state machine, tested as the transition table the
//! prototype's reducer defines: every state, every operation, and the
//! verdict each pair produces.
//!
//! The table is the contract. A transition the prototype refused must
//! be refused here too — with an error the model can read, never a
//! silent no-op and never a hang.

use std::time::Duration;

use super::super::state::{
    ChildState,
    ChildStatus,
    MessageOutcome,
    PARK_TTL,
    RoundOutcome,
    TransitionError,
};

/// Every state the machine knows.
const STATES: [ChildStatus; 4] = [
    ChildStatus::Running,
    ChildStatus::Idle,
    ChildStatus::Parked,
    ChildStatus::Aborted,
];

/// A child in `status`, with the rest of its row filled in.
fn child(status: ChildStatus) -> ChildState {
    let mut state = ChildState::spawn("child-1", "scout");
    match status {
        ChildStatus::Running => {}
        ChildStatus::Idle => {
            state
                .finish(RoundOutcome::Completed)
                .expect("a running child finishes");
        }
        ChildStatus::Parked => {
            state
                .finish(RoundOutcome::Completed)
                .expect("a running child finishes");
            state.age(PARK_TTL);
        }
        ChildStatus::Aborted => {
            state.kill().expect("a live child is killed");
        }
    }
    assert_eq!(state.status, status, "the fixture reaches {status:?}");
    state
}

/// The whole table: state × operation → the state the child is in
/// afterwards, or the error the operation answers with.
#[test]
fn the_whole_transition_table_matches_the_prototype() {
    // (operation, running, idle, parked, aborted) — each cell is the
    // status the child ends in, or `None` when the operation is
    // refused.
    let table: [(&str, [Option<ChildStatus>; 4]); 6] = [
        // `step` only advances a running child.
        ("step", [Some(ChildStatus::Running), None, None, None]),
        // `finish` ends a running child's round.
        ("finish", [Some(ChildStatus::Idle), None, None, None]),
        // `message` steers a running child, follows up an idle one,
        // wakes a parked one and is refused on an aborted one.
        (
            "message",
            [
                Some(ChildStatus::Running),
                Some(ChildStatus::Running),
                Some(ChildStatus::Running),
                None,
            ],
        ),
        // `interrupt` stops the round and leaves the child usable.
        ("interrupt", [Some(ChildStatus::Idle), None, None, None]),
        // `kill` is the hard termination — legal from every live state.
        (
            "kill",
            [
                Some(ChildStatus::Aborted),
                Some(ChildStatus::Aborted),
                Some(ChildStatus::Aborted),
                None,
            ],
        ),
        // `age` only ever ages an idle child.
        ("age", [None, Some(ChildStatus::Idle), None, None]),
    ];

    for (operation, expected) in table {
        for (index, status) in STATES.iter().copied().enumerate() {
            let mut state = child(status);
            let outcome = match operation {
                "step" => state.step("working").map(|()| state.status),
                "finish" => {
                    state.finish(RoundOutcome::Completed).map(|()| state.status)
                }
                "message" => state.message().map(|_| {
                    assert_eq!(
                        state.status,
                        expected[index].expect("the table says the move"),
                        "{operation} from {status:?}"
                    );
                    state.status
                }),
                "interrupt" => state.interrupt().map(|()| state.status),
                "kill" => state.kill().map(|()| state.status),
                "age" => {
                    // Ageing by zero never reaches the TTL, so an idle
                    // child stays idle and every other state is a
                    // no-op rather than a refusal.
                    state.age(Duration::ZERO);
                    Ok(state.status)
                }
                other => panic!("unknown operation {other}"),
            };
            match expected[index] {
                Some(want) => {
                    assert_eq!(
                        outcome.expect("{operation} from {status:?} is legal"),
                        want,
                        "{operation} from {status:?}"
                    );
                }
                None => {
                    if operation == "age" {
                        // Ageing is never a refusal: it is a no-op
                        // outside the idle state.
                        assert!(outcome.is_ok(), "{operation} from {status:?}");
                        continue;
                    }
                    let error = outcome.expect_err(
                        "{operation} from {status:?} must be refused, not applied",
                    );
                    assert!(
                        matches!(
                            error,
                            TransitionError::NotRunning { .. }
                                | TransitionError::Aborted { .. }
                        ),
                        "{operation} from {status:?} answers {error:?}"
                    );
                }
            }
        }
    }
}

/// A message decides by the target's state — the whole point of the
/// control surface.
#[test]
fn a_message_takes_the_branch_the_target_state_names() {
    let mut running = child(ChildStatus::Running);
    assert_eq!(running.message().expect("steer"), MessageOutcome::Steered);
    assert_eq!(
        running.status,
        ChildStatus::Running,
        "a steer keeps the round"
    );
    assert_eq!(running.activity, "steered");

    let mut idle = child(ChildStatus::Idle);
    assert_eq!(
        idle.message().expect("follow up"),
        MessageOutcome::FollowedUp
    );
    assert_eq!(
        idle.status,
        ChildStatus::Running,
        "a follow-up starts a round"
    );

    let mut parked = child(ChildStatus::Parked);
    assert_eq!(parked.message().expect("wake"), MessageOutcome::Woken);
    assert_eq!(
        parked.status,
        ChildStatus::Running,
        "a cold wake starts a round"
    );
    assert!(parked.revived, "a woken child says so");
}

/// An aborted child is terminal: no message, no interrupt, no second
/// kill — and the refusal names the reason.
#[test]
fn an_aborted_child_is_terminal() {
    let mut state = child(ChildStatus::Aborted);
    let error = state
        .message()
        .expect_err("an aborted child is not messaged");
    assert!(
        matches!(error, TransitionError::Aborted { .. }),
        "{error:?}"
    );
    assert!(error.to_string().contains("terminal"), "{error}");

    let error = state
        .interrupt()
        .expect_err("an aborted child is not interrupted");
    assert!(
        matches!(error, TransitionError::NotRunning { .. }),
        "{error:?}"
    );

    let error = state
        .kill()
        .expect_err("an aborted child is not killed twice");
    assert!(
        matches!(error, TransitionError::Aborted { .. }),
        "{error:?}"
    );
}

/// The refusal of a non-running child says which state it is in, so
/// the model can tell "not running" from "terminal".
#[test]
fn a_refusal_names_the_state_it_refused() {
    let mut idle = child(ChildStatus::Idle);
    let error = idle
        .interrupt()
        .expect_err("an idle child is not interrupted");
    assert_eq!(
        error,
        TransitionError::NotRunning {
            action: "be interrupted",
            status: ChildStatus::Idle,
        }
    );
    assert!(error.to_string().contains("idle"), "{error}");
}

/// Ageing is the TTL's business: an idle child parks once it has been
/// idle for five minutes, and nothing else ages.
#[test]
fn only_an_idle_child_ages_into_parked() {
    let mut idle = child(ChildStatus::Idle);
    assert!(!idle.age(PARK_TTL - Duration::from_secs(1)));
    assert_eq!(idle.status, ChildStatus::Idle);
    assert!(idle.age(Duration::from_secs(1)));
    assert_eq!(idle.status, ChildStatus::Parked);
    assert_eq!(idle.activity, "parked");

    for status in [
        ChildStatus::Running,
        ChildStatus::Parked,
        ChildStatus::Aborted,
    ] {
        let mut state = child(status);
        assert!(!state.age(PARK_TTL * 2), "{status:?} does not age");
        assert_eq!(state.status, status);
    }
}

/// A finished round says how it ended — the roster's activity line is
/// the model's only view of what happened.
#[test]
fn a_finished_round_reports_its_outcome() {
    let mut state = ChildState::spawn("child-1", "scout");
    state.finish(RoundOutcome::Completed).expect("finish");
    assert_eq!(state.activity, "finished");
    assert_eq!(state.status, ChildStatus::Idle);

    let mut state = ChildState::spawn("child-1", "scout");
    state.finish(RoundOutcome::Cancelled).expect("finish");
    assert_eq!(state.activity, "cancelled");

    let mut state = ChildState::spawn("child-1", "scout");
    state.finish(RoundOutcome::Failed).expect("finish");
    assert_eq!(state.activity, "failed");
}

/// The state names the model sees are the four the design fixes.
#[test]
fn the_state_names_are_the_four_the_design_fixes() {
    assert_eq!(ChildStatus::Running.as_str(), "running");
    assert_eq!(ChildStatus::Idle.as_str(), "idle");
    assert_eq!(ChildStatus::Parked.as_str(), "parked");
    assert_eq!(ChildStatus::Aborted.as_str(), "aborted");
}
