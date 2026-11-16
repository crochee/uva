//! The handle table: what the control surface keeps for every child of
//! a session, and how a message, an interrupt and a settled round move
//! it.

use std::{sync::Arc, time::Duration};

use uva_agent::ChannelInbox;
use uva_core::{AtomicCancelToken, CancelToken};

use super::{
    super::{
        registry::{
            ChildHandle,
            ColdWake,
            ControlError,
            ControlRegistry,
            MessageTarget,
        },
        state::{ChildState, ChildStatus, PARK_TTL, RoundOutcome},
    },
    support::{EchoAgent, TestClock, running_handle},
};

/// A token a fresh round would be cancelled by.
fn fresh_token() -> Arc<dyn CancelToken> {
    Arc::new(AtomicCancelToken::new())
}

/// A registry with one running child of `parent`.
///
/// The inbox the child's steer channel belongs to is kept alive: a
/// steer only lands while the round that drains it is alive.
fn registry_with_child(
    parent: &str,
) -> (
    ControlRegistry,
    Arc<AtomicCancelToken>,
    Arc<uva_agent::ChannelInbox>,
) {
    let registry = ControlRegistry::new();
    let (handle, _steer, token, inbox) = running_handle("scout");
    registry.register(parent, handle);
    (registry, token, inbox)
}

#[test]
fn a_registered_child_shows_up_in_the_roster() {
    let (registry, _token, _inbox) = registry_with_child("parent");
    let roster = registry.list("parent");
    assert_eq!(roster.len(), 1);
    assert_eq!(roster[0].id, "child-1");
    assert_eq!(roster[0].name, "scout");
    assert_eq!(roster[0].status, ChildStatus::Running);
    assert_eq!(roster[0].activity, "starting");
}

#[test]
fn the_roster_is_scoped_to_its_own_session() {
    let (registry, _token, _inbox) = registry_with_child("parent");
    assert!(registry.list("other-session").is_empty());
}

#[test]
fn a_message_to_a_running_child_steers_it() {
    let (registry, _token, _inbox) = registry_with_child("parent");
    let target = registry
        .message_target("parent", "child-1", fresh_token())
        .expect("a running child can be messaged");
    let MessageTarget::Steer(steer) = target else {
        panic!("a running child is steered, not followed up");
    };
    // The steer channel is the running round's own: the message lands
    // in the child's inbox.
    steer
        .steer
        .unbounded_send(uva_provider::Message::user("focus"))
        .expect("the child is still draining its inbox");
    let roster = registry.list("parent");
    assert_eq!(
        roster[0].status,
        ChildStatus::Running,
        "a steer keeps the round"
    );
    assert_eq!(roster[0].activity, "steered");
}

#[test]
fn a_message_to_an_idle_child_asks_for_another_round() {
    let (registry, _token, _inbox) = registry_with_child("parent");
    registry.settle("parent", "child-1", RoundOutcome::Completed);
    assert_eq!(registry.list("parent")[0].status, ChildStatus::Idle);

    let target = registry
        .message_target("parent", "child-1", fresh_token())
        .expect("an idle child can be messaged");
    let MessageTarget::FollowUp(follow_up) = target else {
        panic!("an idle child is asked for another round");
    };
    assert_eq!(follow_up.depth, 1, "the round runs at the child's depth");
    assert_eq!(
        registry.list("parent")[0].status,
        ChildStatus::Running,
        "the follow-up round starts"
    );

    // The follow-up hands over a fresh steer channel: the previous
    // round's receiver died with it, so the old sender would fail.
    follow_up
        .steer
        .unbounded_send(uva_provider::Message::user("more"))
        .expect("the new round drains its own inbox");
}

#[test]
fn a_message_to_an_unknown_child_is_refused() {
    let registry = ControlRegistry::new();
    let error = registry
        .message_target("parent", "nobody", fresh_token())
        .expect_err("there is no such child");
    assert_eq!(error, ControlError::UnknownChild("nobody".to_string()));
    assert!(error.to_string().contains("nobody"), "{error}");
}

#[test]
fn a_message_to_an_aborted_child_is_refused() {
    let (registry, _token, _inbox) = registry_with_child("parent");
    registry
        .kill("parent", "child-1")
        .expect("a live child is killed");

    let error = registry
        .message_target("parent", "child-1", fresh_token())
        .expect_err("an aborted child is not messaged");
    assert!(
        matches!(error, ControlError::Rejected(_)),
        "{error:?} is a refusal, not a missing child"
    );
    assert!(error.to_string().contains("terminal"), "{error}");
}

#[test]
fn an_interrupt_stops_the_round_and_leaves_the_child_usable() {
    let (registry, token, _inbox) = registry_with_child("parent");
    let fired = registry
        .interrupt("parent", "child-1")
        .expect("a running child can be interrupted");
    assert!(!token.is_cancelled(), "the token is fired by the caller");
    fired.cancel();
    assert!(token.is_cancelled(), "the child's own token fired");

    let roster = registry.list("parent");
    assert_eq!(
        roster[0].status,
        ChildStatus::Idle,
        "an interrupt is not a kill"
    );
    assert_eq!(roster[0].activity, "interrupted");

    // The child can still be asked for another round.
    let target = registry
        .message_target("parent", "child-1", fresh_token())
        .expect("an interrupted child is still usable");
    assert!(matches!(target, MessageTarget::FollowUp(_)));
}

#[test]
fn an_interrupt_of_an_idle_child_is_refused() {
    let (registry, _token, _inbox) = registry_with_child("parent");
    registry.settle("parent", "child-1", RoundOutcome::Completed);
    let error = match registry.interrupt("parent", "child-1") {
        Ok(_) => panic!("an idle child is not interrupted"),
        Err(error) => error,
    };
    assert!(matches!(error, ControlError::Rejected(_)), "{error:?}");
}

#[test]
fn settling_a_round_moves_the_child_to_idle() {
    let (registry, _token, _inbox) = registry_with_child("parent");
    registry.settle("parent", "child-1", RoundOutcome::Completed);
    let roster = registry.list("parent");
    assert_eq!(roster[0].status, ChildStatus::Idle);
    assert_eq!(roster[0].activity, "finished");
}

#[test]
fn settling_after_an_interrupt_keeps_the_interrupted_state() {
    let (registry, _token, _inbox) = registry_with_child("parent");
    registry
        .interrupt("parent", "child-1")
        .expect("a running child can be interrupted");
    registry.settle("parent", "child-1", RoundOutcome::Cancelled);
    let roster = registry.list("parent");
    assert_eq!(roster[0].status, ChildStatus::Idle);
    assert_eq!(
        roster[0].activity, "interrupted",
        "the interrupt is what happened, not a finish"
    );
}

/// A child handle under `parent` whose id is `id`.
fn handle_for(
    id: &str,
    name: &str,
) -> (
    ChildHandle,
    futures::channel::mpsc::UnboundedSender<uva_provider::Message>,
    Arc<AtomicCancelToken>,
    Arc<uva_agent::ChannelInbox>,
) {
    let (inbox, steer) = ChannelInbox::channel();
    let cancel = Arc::new(AtomicCancelToken::new());
    let handle = ChildHandle::new(
        ChildState::spawn(id, name),
        Arc::clone(&cancel) as Arc<dyn CancelToken>,
    )
    .with_steer(steer.clone())
    .with_peer(EchoAgent::shared(name, "done"))
    .with_depth(1);
    (handle, steer, cancel, inbox)
}

// ------------------------------------------------------------------
// The memory lifecycle: ageing, parking, waking, terminating.
// ------------------------------------------------------------------

/// An idle child parks once the TTL has passed, and parking is what
/// releases its memory: the row stays, the handles are gone.
#[test]
fn an_idle_child_parks_after_the_ttl_and_releases_its_handles() {
    let clock = TestClock::new();
    let registry = ControlRegistry::with_clock(clock.shared());
    let (handle, _steer, _token, _inbox) = handle_for("child-1", "scout");
    registry.register("parent", handle);
    registry.settle("parent", "child-1", RoundOutcome::Completed);

    assert_eq!(registry.list("parent")[0].status, ChildStatus::Idle);
    assert_eq!(
        registry.has_handles("parent", "child-1"),
        Some(true),
        "an idle child is still in memory"
    );

    // One second short of the TTL: nothing parks.
    clock.advance(PARK_TTL - Duration::from_secs(1));
    assert!(registry.tick().is_empty());
    assert_eq!(registry.list("parent")[0].status, ChildStatus::Idle);
    assert_eq!(registry.has_handles("parent", "child-1"), Some(true));

    // The TTL itself is enough.
    clock.advance(Duration::from_secs(1));
    assert_eq!(
        registry.tick(),
        vec![("parent".to_string(), "child-1".to_string())]
    );
    let row = &registry.list("parent")[0];
    assert_eq!(row.status, ChildStatus::Parked);
    assert_eq!(row.activity, "parked");
    assert_eq!(row.name, "scout", "parking keeps the row's identity");
    assert_eq!(
        registry.has_handles("parent", "child-1"),
        Some(false),
        "parking released the steer channel, token, writer and peer"
    );
}

/// A running child never ages: only an idle one parks.
#[test]
fn a_running_child_never_parks() {
    let clock = TestClock::new();
    let registry = ControlRegistry::with_clock(clock.shared());
    let (handle, _steer, _token, _inbox) = handle_for("child-1", "scout");
    registry.register("parent", handle);

    clock.advance(PARK_TTL * 10);
    assert!(registry.tick().is_empty());
    assert_eq!(registry.list("parent")[0].status, ChildStatus::Running);
    assert_eq!(registry.has_handles("parent", "child-1"), Some(true));
}

/// The session that owns the table — a row whose id is its own bucket
/// — is the main session: it has no lifecycle and never parks.
#[test]
fn the_main_session_is_never_parked() {
    let clock = TestClock::new();
    let registry = ControlRegistry::with_clock(clock.shared());
    let (handle, _steer, _token, _inbox) = handle_for("root", "parent");
    registry.register("root", handle);
    registry.settle("root", "root", RoundOutcome::Completed);
    assert_eq!(registry.list("root")[0].status, ChildStatus::Idle);

    clock.advance(PARK_TTL * 3);
    assert!(registry.tick().is_empty(), "the main session does not age");
    assert_eq!(registry.list("root")[0].status, ChildStatus::Idle);
    assert_eq!(registry.has_handles("root", "root"), Some(true));
}

/// A parked child is woken from its log, and the wake takes over the
/// row: the new round gets handles of its own and the child is
/// running again.
#[test]
fn a_parked_child_wakes_and_the_wake_takes_over_the_row() {
    let clock = TestClock::new();
    let registry = ControlRegistry::with_clock(clock.shared());
    let (handle, _steer, _token, _inbox) = handle_for("child-1", "scout");
    registry.register("parent", handle);
    registry.settle("parent", "child-1", RoundOutcome::Completed);
    clock.advance(PARK_TTL);
    assert_eq!(registry.tick().len(), 1);

    // A parked child is a message that has to be woken, not followed
    // up: its peer and writer are gone.
    let target = registry
        .message_target("parent", "child-1", fresh_token())
        .expect("a parked child is messaged");
    assert!(
        matches!(target, MessageTarget::ColdWake),
        "the log has to be read again"
    );
    assert_eq!(
        registry.list("parent")[0].status,
        ChildStatus::Parked,
        "the row stays parked until the wake resolved the log"
    );

    let target = registry
        .cold_wake(
            "parent",
            "child-1",
            ColdWake {
                name: "scout".to_string(),
                peer: EchoAgent::shared("scout", "done"),
                writer: None,
                cancel: fresh_token(),
                depth: 1,
            },
        )
        .expect("the wake takes over the parked row");

    let row = &registry.list("parent")[0];
    assert_eq!(row.status, ChildStatus::Running);
    assert_eq!(row.activity, "cold wake · new round");
    assert_eq!(
        registry.has_handles("parent", "child-1"),
        Some(true),
        "the woken round has a live steer channel and token"
    );

    // The woken round drains its own inbox: the parked row's channel
    // is gone, so this one has to be the new round's.
    target
        .steer
        .unbounded_send(uva_provider::Message::user("more"))
        .expect("the new round drains its own inbox");
}

/// A parked child cannot be interrupted: the refusal comes from its
/// state (with the state's wording), not from a missing handle.
#[test]
fn an_interrupt_of_a_parked_child_is_refused_by_its_state() {
    let clock = TestClock::new();
    let registry = ControlRegistry::with_clock(clock.shared());
    let (handle, _steer, _token, _inbox) = handle_for("child-1", "scout");
    registry.register("parent", handle);
    registry.settle("parent", "child-1", RoundOutcome::Completed);
    clock.advance(PARK_TTL);
    assert_eq!(registry.tick().len(), 1);

    let error = match registry.interrupt("parent", "child-1") {
        Ok(_) => panic!("a parked child has no round to interrupt"),
        Err(error) => error,
    };
    assert!(matches!(error, ControlError::Rejected(_)), "{error:?}");
    assert!(error.to_string().contains("parked"), "{error}");
}

/// A killed child is terminal: no ageing, no message, and its handles
/// are released with the move.
#[test]
fn a_killed_child_is_terminal_and_holds_no_handles() {
    let clock = TestClock::new();
    let registry = ControlRegistry::with_clock(clock.shared());
    let (handle, _steer, _token, _inbox) = handle_for("child-1", "scout");
    registry.register("parent", handle);

    registry
        .kill("parent", "child-1")
        .expect("a live child is killed");
    let row = &registry.list("parent")[0];
    assert_eq!(row.status, ChildStatus::Aborted);
    assert_eq!(
        registry.has_handles("parent", "child-1"),
        Some(false),
        "a terminal child will never need a round again"
    );

    clock.advance(PARK_TTL * 2);
    assert!(registry.tick().is_empty(), "aborted is terminal");
    assert_eq!(registry.list("parent")[0].status, ChildStatus::Aborted);

    let error = registry
        .message_target("parent", "child-1", fresh_token())
        .expect_err("an aborted child is not messaged");
    assert!(error.to_string().contains("terminal"), "{error}");
}

/// A child read back from the sessions is parked — the handles are
/// gone by definition — and the live row always wins over the log's
/// copy.
#[test]
fn a_child_recovered_from_the_log_is_parked_without_handles() {
    let registry = ControlRegistry::new();
    assert!(registry.adopt("parent", "child-9", "scout"));
    assert!(
        !registry.adopt("parent", "child-9", "scout"),
        "a child is adopted once"
    );

    let row = &registry.list("parent")[0];
    assert_eq!(row.id, "child-9");
    assert_eq!(row.name, "scout");
    assert_eq!(row.status, ChildStatus::Parked);
    assert_eq!(registry.has_handles("parent", "child-9"), Some(false));

    // A process that holds the child keeps holding it: the log's copy
    // never replaces a live row.
    let (handle, _steer, _token, _inbox) = handle_for("child-9", "scout");
    registry.register("parent", handle);
    assert!(!registry.adopt("parent", "child-9", "scout"));
    assert_eq!(registry.list("parent")[0].status, ChildStatus::Running);
    assert_eq!(registry.has_handles("parent", "child-9"), Some(true));
}
