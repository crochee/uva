use serde_json::{Value, json};
use uva_provider::{
    Content,
    ContentPart,
    ReasoningContent,
    Role as ProviderRole,
    TextContent,
    ToolUse,
};

use super::*;
use crate::{
    tests::{FIXTURE_TS, message_data, message_role},
    transport::{RequestKind, RpcFailure, TransportEvent},
};

fn text_row(kind: &str, seq: u64, text: &str) -> Value {
    json!({
        "type": kind,
        "seq": seq,
        "ts": FIXTURE_TS,
        "data": message_data(message_role(kind), Content::text(text)),
    })
}

/// An assistant row whose only content is reasoning: it renders no
/// line, so the turn it belongs to names no answer of its own.
fn reasoning_row(seq: u64, text: &str) -> Value {
    json!({
        "type": "assistant_message",
        "seq": seq,
        "ts": FIXTURE_TS,
        "data": message_data(
            ProviderRole::Assistant,
            Content::parts(vec![ContentPart::Reasoning(ReasoningContent {
                text: text.to_string(),
                signature: None,
            })]),
        ),
    })
}

/// A row whose content holds two text parts: the pieces of one
/// message, not one glued word.
fn multi_text_row(kind: &str, seq: u64, first: &str, second: &str) -> Value {
    json!({
        "type": kind,
        "seq": seq,
        "ts": FIXTURE_TS,
        "data": message_data(
            message_role(kind),
            Content::parts(vec![
                ContentPart::Text(TextContent {
                    text: first.to_string(),
                    cache_control: None,
                }),
                ContentPart::Text(TextContent {
                    text: second.to_string(),
                    cache_control: None,
                }),
            ]),
        ),
    })
}

fn turn_end() -> Value {
    json!({
        "type": "turn",
        "seq": 9,
        "ts": FIXTURE_TS,
        "data": { "kind": "end", "turn": 0, "reason": "completed" },
    })
}

fn baseline(rows: Vec<Value>) -> Value {
    json!({
        "header": {
            "session_id": "s1",
            "user_id": "u1",
            "agent_name": "default",
            "state": "idle",
        },
        "rows": rows,
        "cursor": 9,
        "live": {
            "start_seq": 0,
            "live_seq": 0,
            "running": false,
            "text": "",
            "reasoning": "",
            "tool_calls": [],
        },
        "context": { "used": 0 },
        "pending_interrupts": [],
        "request_snapshot_hash": "",
    })
}

fn follow(rows: Vec<Value>) -> TransportEvent {
    TransportEvent::Response {
        id: json!(1),
        request: Some(RequestKind::Follow),
        result: Ok(baseline(rows)),
    }
}

fn ready() -> TransportEvent {
    TransportEvent::Ready {
        version: "0.1.0".to_string(),
        workspace_root: "/work".to_string(),
    }
}

fn approval_notification() -> TransportEvent {
    TransportEvent::Notification {
        method: "approval/request".to_string(),
        params: json!({
            "session_id": "s1",
            "approval_id": "a1",
            "call_id": "c1",
            "tool": "shell",
            "risk": "high",
            "arguments_hash": "sha256:args",
            "agent": "default",
        }),
    }
}

#[test]
fn a_completed_turn_row_returns_the_projection_to_idle() {
    let mut app = App::new();
    app.apply(follow(vec![
        text_row("user_message", 1, "hello"),
        text_row("assistant_message", 2, "hi"),
    ]));
    assert_eq!(app.messages().len(), 2);
    assert_eq!(app.turn(), TurnState::Idle);

    app.apply(TransportEvent::Notification {
        method: "session/event".to_string(),
        params: json!({
            "session_id": "s1",
            "kind": "live",
            "live": {
                "start_seq": 8,
                "live_seq": 1,
                "running": true,
                "text": "Sunny",
                "reasoning": "",
                "tool_calls": [],
            },
        }),
    });
    assert_eq!(app.live_text(), "Sunny");
    assert_eq!(app.turn(), TurnState::Running);

    app.apply(TransportEvent::Notification {
        method: "session/event".to_string(),
        params: json!({
            "session_id": "s1",
            "kind": "row",
            "seq": 3,
            "row": turn_end(),
        }),
    });
    assert_eq!(app.turn(), TurnState::Idle);
    assert!(app.live_text().is_empty());
}

#[test]
fn the_assistant_row_replaces_the_live_copy() {
    let mut app = App::new();
    app.apply(follow(Vec::new()));
    app.apply(TransportEvent::Notification {
        method: "session/event".to_string(),
        params: json!({
            "kind": "live",
            "live": { "start_seq": 3, "live_seq": 1, "running": true, "text": "partial" },
        }),
    });
    assert_eq!(app.live_text(), "partial");
    app.apply(TransportEvent::Notification {
        method: "session/event".to_string(),
        params: json!({
            "kind": "row",
            "seq": 4,
            "row": text_row("assistant_message", 4, "partial"),
        }),
    });
    assert!(app.live_text().is_empty());
    assert_eq!(app.messages().last().expect("a line").text, "partial");
}

/// A row older than the projection's step does not retire it.
///
/// The projection began at a log position, and only a row past that
/// position carries the step's content. A row from an earlier step
/// arriving late says nothing about this one — so the answer keeps
/// streaming.
#[test]
fn an_older_row_does_not_retire_the_projection() {
    let mut app = App::new();
    app.apply(follow(Vec::new()));
    app.apply(TransportEvent::Notification {
        method: "session/event".to_string(),
        params: json!({
            "kind": "live",
            "live": { "start_seq": 12, "live_seq": 1, "running": true, "text": "streaming" },
        }),
    });
    app.apply(TransportEvent::Notification {
        method: "session/event".to_string(),
        params: json!({
            "kind": "row",
            "seq": 11,
            "row": text_row("assistant_message", 11, "an earlier step"),
        }),
    });
    assert_eq!(
        app.live_text(),
        "streaming",
        "the projection is still the step's live text"
    );
}

/// A frame at or below the counter the client accepted predates the
/// window: it is discarded.
#[test]
fn a_frame_at_or_below_the_accepted_counter_is_ignored() {
    let mut app = App::new();
    app.apply(follow(Vec::new()));
    app.apply(TransportEvent::Notification {
        method: "session/event".to_string(),
        params: json!({
            "kind": "live",
            "live": { "start_seq": 3, "live_seq": 5, "running": true, "text": "newer" },
        }),
    });
    app.apply(TransportEvent::Notification {
        method: "session/event".to_string(),
        params: json!({
            "kind": "live",
            "live": { "start_seq": 3, "live_seq": 4, "running": true, "text": "older" },
        }),
    });
    assert_eq!(app.live_text(), "newer", "the older frame is discarded");
}

/// A frame for a projection the durable rows already superseded must
/// not bring it back.
#[test]
fn a_frame_for_a_superseded_projection_is_ignored() {
    let mut app = App::new();
    app.apply(follow(Vec::new()));
    app.apply(TransportEvent::Notification {
        method: "session/event".to_string(),
        params: json!({
            "kind": "live",
            "live": { "start_seq": 3, "live_seq": 1, "running": true, "text": "streaming" },
        }),
    });
    app.apply(TransportEvent::Notification {
        method: "session/event".to_string(),
        params: json!({
            "kind": "row",
            "seq": 4,
            "row": text_row("assistant_message", 4, "the answer"),
        }),
    });
    app.apply(TransportEvent::Notification {
        method: "session/event".to_string(),
        params: json!({
            "kind": "live",
            "live": { "start_seq": 3, "live_seq": 2, "running": true, "text": "streaming" },
        }),
    });
    assert!(
        app.live_text().is_empty(),
        "the superseded projection does not come back"
    );
}

/// A row with two text parts renders them as the pieces they are: the
/// parts join with a space, so the line reads as words instead of one
/// glued word.
#[test]
fn a_row_with_two_text_parts_renders_them_separated() {
    let mut app = App::new();
    app.apply(follow(vec![
        multi_text_row("user_message", 4, "part one", "part two"),
        multi_text_row("assistant_message", 5, "part one", "part two"),
    ]));

    let lines = app.messages();
    assert_eq!(lines[0].text, "part one part two");
    assert_eq!(lines[1].text, "part one part two");
}

#[test]
fn an_approval_request_opens_the_prompt_and_serializes_the_answer() {
    let mut app = App::new();
    app.apply(follow(Vec::new()));
    app.apply(approval_notification());

    assert_eq!(app.turn(), TurnState::AwaitingApproval);
    let prompt = app.approval().expect("a prompt");
    assert_eq!(prompt.approval_id, "a1");
    assert_eq!(prompt.tool, "shell");
    assert_eq!(prompt.risk, "high");

    let action = app
        .answer_approval(Decision::Allow)
        .expect("an answer action");
    let request = action.request().expect("the answer is a wire request");
    assert_eq!(request.method(), "approval/respond");
    assert_eq!(
        request.params,
        json!({
            "session_id": "s1",
            "approval_id": "a1",
            "decision": "allow",
        })
    );
    // A second press is ignored while the decision is in flight.
    assert!(app.answer_approval(Decision::Deny).is_none());
}

#[test]
fn an_interrupts_event_opens_and_closes_the_prompt() {
    let mut app = App::new();
    app.apply(follow(Vec::new()));
    app.apply(TransportEvent::Notification {
        method: "session/event".to_string(),
        params: json!({
            "kind": "interrupts",
            "pending_interrupts": [{
                "id": "a2",
                "call_id": "c2",
                "tool": "write",
                "arguments_hash": "sha256:x",
                "risk": "medium",
                "agent": "default",
            }],
        }),
    });
    assert_eq!(app.approval().expect("a prompt").approval_id, "a2");
    assert_eq!(app.turn(), TurnState::AwaitingApproval);

    app.apply(TransportEvent::Notification {
        method: "session/event".to_string(),
        params: json!({ "kind": "interrupts", "pending_interrupts": [] }),
    });
    assert!(app.approval().is_none());
    assert_eq!(app.turn(), TurnState::Running);
}

#[test]
fn a_follow_baseline_loads_the_pending_interrupt() {
    let mut app = App::new();
    let mut snapshot = baseline(vec![text_row("user_message", 1, "hello")]);
    snapshot["pending_interrupts"] = json!([{
        "id": "a3",
        "call_id": "c3",
        "tool": "shell",
        "arguments_hash": "sha256:y",
        "risk": "high",
        "agent": "default",
    }]);
    snapshot["header"]["state"] = json!("awaiting_approval");
    app.apply(TransportEvent::Response {
        id: json!(1),
        request: Some(RequestKind::Follow),
        result: Ok(snapshot),
    });
    assert_eq!(app.approval().expect("a prompt").approval_id, "a3");
    assert_eq!(app.turn(), TurnState::AwaitingApproval);
}

#[test]
fn a_disconnect_marks_offline_and_the_next_ready_clears_it() {
    let mut app = App::new();
    app.apply(ready());
    app.apply(follow(vec![text_row("user_message", 1, "hello")]));
    app.apply(TransportEvent::Disconnected {
        reason: "the core exited".to_string(),
    });
    assert!(matches!(app.connection(), ConnectionState::Failed(_)));
    assert_eq!(app.turn(), TurnState::Idle);
    assert_eq!(app.messages().len(), 1, "the transcript survives");

    // A session exists, so the restart does not create a new one; the
    // transport re-follows it, and the new core generation is asked
    // for the UI tree.
    assert_eq!(app.apply(ready()), vec![Action::FetchUiSchema]);
    assert!(matches!(app.connection(), ConnectionState::Ready { .. }));
}

#[test]
fn submit_without_a_session_creates_then_sends_after_the_baseline() {
    let mut app = App::new();
    app.insert_str("hello");
    assert_eq!(app.submit(), vec![Action::CreateSession]);

    let actions = app.apply(TransportEvent::Response {
        id: json!(1),
        request: Some(RequestKind::CreateSession),
        result: Ok(json!({ "session_id": "s1", "agent_name": null })),
    });
    assert_eq!(
        actions,
        vec![Action::Follow {
            session_id: "s1".to_string(),
        }]
    );

    let actions = app.apply(follow(Vec::new()));
    assert_eq!(
        actions,
        vec![Action::SendMessage {
            session_id: "s1".to_string(),
            text: "hello".to_string(),
        }]
    );
    assert_eq!(app.messages().last().expect("the prompt").text, "hello");
    assert_eq!(app.turn(), TurnState::Running);
}

#[test]
fn a_failed_send_returns_to_idle_with_the_error() {
    let mut app = App::new();
    app.apply(follow(Vec::new()));
    app.insert_str("hello");
    assert_eq!(app.submit().len(), 1);
    app.apply(TransportEvent::Response {
        id: json!(2),
        request: Some(RequestKind::SendMessage),
        result: Err(RpcFailure {
            code: -32000,
            message: "provider unavailable".to_string(),
            data: Some(json!({ "code": "provider_error" })),
        }),
    });
    assert_eq!(app.turn(), TurnState::Idle);
    assert!(app.status().contains("provider unavailable"));
}

#[test]
fn gateway_lifecycle_events_return_the_projection_to_starting() {
    let mut app = App::new();
    app.apply(ready());
    assert!(matches!(app.connection(), ConnectionState::Ready { .. }));

    app.apply(TransportEvent::GatewayStarting {
        reason: "the gateway process is launching".to_string(),
    });
    assert_eq!(*app.connection(), ConnectionState::Starting);
    assert_eq!(app.status(), "the gateway process is launching");

    app.apply(TransportEvent::GatewayRestarting { attempt: 2 });
    assert_eq!(*app.connection(), ConnectionState::Starting);
    assert_eq!(app.status(), "gateway restarting, attempt 2");

    // A core `ready` restores the projection; the session survives.
    app.apply(ready());
    assert!(matches!(app.connection(), ConnectionState::Ready { .. }));
}

#[test]
fn the_first_bare_baseline_lists_sessions_for_the_sidebar() {
    let mut app = App::new();
    app.apply(ready());
    app.apply(TransportEvent::Response {
        id: json!(1),
        request: Some(RequestKind::CreateSession),
        result: Ok(json!({ "session_id": "s1", "agent_name": null })),
    });

    // A baseline with no parked prompt also asks for the listing.
    let actions = app.apply(follow(Vec::new()));
    assert_eq!(actions, vec![Action::ListSessions]);

    // The listing folds into one group named after the workspace.
    app.apply(TransportEvent::Response {
        id: json!(2),
        request: Some(RequestKind::ListSessions),
        result: Ok(json!({
            "data": [
                { "id": "s1", "status": "completed" },
                { "id": "s0", "status": "working" },
            ],
        })),
    });
    let tree = app.workspaces();
    assert_eq!(tree.len(), 1);
    assert_eq!(tree[0].name, "work");
    assert_eq!(tree[0].sessions.len(), 2);
    assert_eq!(tree[0].sessions[0].id, "s1");
    assert_eq!(tree[0].sessions[0].status, "completed");

    // A later baseline (a reconnect) does not list again.
    let actions = app.apply(follow(Vec::new()));
    assert!(actions.is_empty());

    // The manual refresh always can.
    assert_eq!(app.refresh_sidebar(), vec![Action::ListSessions]);
}

#[test]
fn a_pending_prompt_owns_the_first_baseline_and_the_listing_waits() {
    let mut app = App::new();
    app.insert_str("hello");
    assert_eq!(app.submit(), vec![Action::CreateSession]);
    app.apply(TransportEvent::Response {
        id: json!(1),
        request: Some(RequestKind::CreateSession),
        result: Ok(json!({ "session_id": "s1", "agent_name": null })),
    });
    let actions = app.apply(follow(Vec::new()));
    assert_eq!(
        actions,
        vec![Action::SendMessage {
            session_id: "s1".to_string(),
            text: "hello".to_string(),
        }]
    );
}

// --- Task 5: the Vim keys, driven through the reducer -------------

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ch(character: char) -> KeyEvent {
    key(KeyCode::Char(character))
}

fn ctrl(character: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(character), KeyModifiers::CONTROL)
}

fn sidebar_app(ids: &[&str]) -> App {
    let mut app = App::new();
    app.set_workspaces(vec![crate::app::WorkspaceNode {
        name: "work".to_string(),
        sessions: ids
            .iter()
            .map(|&id| crate::app::SessionEntry {
                id: id.to_string(),
                status: String::new(),
            })
            .collect(),
    }]);
    app
}

#[test]
fn list_keys_move_the_sidebar_cursor() {
    let ids = [
        "s0", "s1", "s2", "s3", "s4", "s5", "s6", "s7", "s8", "s9", "s10",
        "s11",
    ];
    let mut app = sidebar_app(&ids);
    // Esc leaves the composer; the sidebar owns the list keys.
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(ch('j'));
    app.handle_key(ch('j'));
    app.handle_key(ch('j'));
    assert_eq!(app.sidebar().cursor, 3);

    app.handle_key(ch('g'));
    assert_eq!(app.sidebar().cursor, 3, "g waits for its second g");
    app.handle_key(ch('g'));
    assert_eq!(app.sidebar().cursor, 0);

    app.handle_key(ch('G'));
    assert_eq!(app.sidebar().cursor, 11);
    app.handle_key(ctrl('d'));
    assert_eq!(app.sidebar().cursor, 11, "Ctrl+d clamps at the bottom");
    app.handle_key(ch('k'));
    app.handle_key(ctrl('u'));
    assert_eq!(app.sidebar().cursor, 0, "Ctrl+u clamps at the top");
}

#[test]
fn slash_searches_matches_cycle_and_esc_cancels() {
    let ids = ["s0", "s1", "s2", "s10", "s11"];
    let mut app = sidebar_app(&ids);
    // Esc leaves the composer; the sidebar owns the list keys.
    app.handle_key(key(KeyCode::Esc));

    app.handle_key(ch('/'));
    assert_eq!(app.sidebar().search.as_deref(), Some(""));
    for character in "s1".chars() {
        app.handle_key(ch(character));
    }
    assert_eq!(app.sidebar().search.as_deref(), Some("s1"));

    // Enter accepts the query and jumps to the next match.
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.sidebar().cursor, 1);
    assert_eq!(app.sidebar().search.as_deref(), Some("s1"));

    app.handle_key(ch('n'));
    assert_eq!(app.sidebar().cursor, 3);
    app.handle_key(ch('n'));
    assert_eq!(app.sidebar().cursor, 4);
    app.handle_key(ch('n'));
    assert_eq!(app.sidebar().cursor, 1, "n wraps");
    app.handle_key(ch('N'));
    assert_eq!(app.sidebar().cursor, 4, "N wraps back");

    app.handle_key(key(KeyCode::Esc));
    assert!(app.sidebar().search.is_none());
}

#[test]
fn ctrl_b_toggles_the_sidebar_rail() {
    let mut app = App::new();
    assert!(!app.sidebar().collapsed);
    app.handle_key(ctrl('b'));
    assert!(app.sidebar().collapsed);
    app.handle_key(ctrl('b'));
    assert!(!app.sidebar().collapsed);
}

#[test]
fn y_copies_the_selected_id() {
    let mut app = sidebar_app(&["s0", "s1"]);
    // Esc leaves the composer; the sidebar owns the list keys.
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(ch('j'));
    assert_eq!(
        app.handle_key(ch('y')),
        vec![Action::CopyId {
            id: "s1".to_string(),
        }]
    );
}

#[test]
fn x_and_dd_delete_the_selected_session() {
    let mut app = sidebar_app(&["s0", "s1"]);
    // Esc leaves the composer; the sidebar owns the list keys.
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(ch('j'));
    assert_eq!(
        app.handle_key(ch('x')),
        vec![Action::DeleteSession {
            id: "s1".to_string(),
        }]
    );

    // dd: the first d arms, the second deletes.
    app.handle_key(ch('d'));
    assert!(app.handle_key(ch('d')).len() == 1);

    // A different key after d disarms instead — the disarming key is
    // swallowed, so the cursor needs its own press to move.
    app.handle_key(ch('d'));
    assert!(app.handle_key(ch('j')).is_empty());
    app.handle_key(ch('k'));
    assert_eq!(
        app.handle_key(ch('x')),
        vec![Action::DeleteSession {
            id: "s0".to_string(),
        }]
    );
}

#[test]
fn enter_opens_the_selected_session() {
    let mut app = App::new();
    // Esc leaves the composer; the sidebar owns the list keys.
    app.handle_key(key(KeyCode::Esc));
    app.apply(follow(vec![text_row("user_message", 1, "hello")]));
    app.set_workspaces(vec![crate::app::WorkspaceNode {
        name: "work".to_string(),
        sessions: vec![
            crate::app::SessionEntry {
                id: "s1".to_string(),
                status: String::new(),
            },
            crate::app::SessionEntry {
                id: "s2".to_string(),
                status: String::new(),
            },
        ],
    }]);
    app.handle_key(ch('j'));
    assert_eq!(
        app.handle_key(key(KeyCode::Enter)),
        vec![
            Action::Unfollow {
                session_id: "s1".to_string(),
            },
            Action::Follow {
                session_id: "s2".to_string(),
            },
        ]
    );
    assert_eq!(app.status(), "opening session s2");

    // Enter on the followed session re-opens nothing. The follow
    // baseline lands with the newly opened session's id.
    let mut event = follow(vec![text_row("user_message", 1, "hi")]);
    if let TransportEvent::Response {
        result: Ok(value), ..
    } = &mut event
    {
        value["header"]["session_id"] = json!("s2");
    }
    app.apply(event);
    app.set_workspaces(vec![crate::app::WorkspaceNode {
        name: "work".to_string(),
        sessions: vec![crate::app::SessionEntry {
            id: "s2".to_string(),
            status: String::new(),
        }],
    }]);
    assert!(app.handle_key(key(KeyCode::Enter)).is_empty());
}

#[test]
fn approval_answers_win_over_the_sidebar_keys() {
    let mut app = sidebar_app(&["s0", "s1"]);
    app.apply(follow(Vec::new()));
    app.apply(approval_notification());
    assert!(app.approval().is_some());

    let actions = app.handle_key(ch('y'));
    assert!(
        matches!(actions.as_slice(), [Action::RespondApproval { .. }]),
        "{actions:?}"
    );
    // A second press while the decision is in flight is swallowed.
    assert!(app.handle_key(ch('y')).is_empty());
}

#[test]
fn esc_leaves_the_composer_and_i_returns_to_it() {
    let mut app = App::new();
    assert_eq!(app.pane(), crate::app::Pane::Composer);
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.pane(), crate::app::Pane::Sidebar);

    // In the sidebar, j is a motion, not a character.
    app.handle_key(ch('j'));
    assert!(app.composer().is_empty());

    app.handle_key(ch('i'));
    assert_eq!(app.pane(), crate::app::Pane::Composer);
    app.handle_key(ch('j'));
    assert_eq!(app.composer(), "j");
}

#[test]
fn ctrl_c_quits_from_any_pane() {
    let mut app = sidebar_app(&["s0"]);
    app.handle_key(ctrl('c'));
    assert!(app.is_quit());

    let mut typing = App::new();
    typing.handle_key(ctrl('c'));
    assert!(typing.is_quit());
}

#[test]
fn tab_switches_to_the_settings_sections() {
    let mut app = App::new();
    // Esc leaves the composer; the sidebar owns the list keys.
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.sidebar().tab, crate::app::SidebarTab::Settings);
    assert_eq!(app.sidebar().cursor, 0);

    app.handle_key(ch('j'));
    assert_eq!(app.sidebar().cursor, 1);
    app.handle_key(ch('G'));
    assert_eq!(
        app.sidebar().cursor,
        crate::app::SETTINGS_SECTIONS.len() - 1
    );

    // The settings list has no ids to copy or sessions to open.
    assert!(app.handle_key(ch('y')).is_empty());
    assert!(app.handle_key(key(KeyCode::Enter)).is_empty());

    app.handle_key(key(KeyCode::Tab));
    assert_eq!(app.sidebar().tab, crate::app::SidebarTab::Sessions);
}

#[test]
fn colon_ctrl_k_and_question_open_the_overlays() {
    let mut app = App::new();
    // Esc leaves the composer; the sidebar owns the list keys.
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(ch(':'));
    assert_eq!(app.overlay(), Some(crate::app::Overlay::CommandPalette));
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.overlay(), None);

    app.handle_key(ch('?'));
    assert_eq!(app.overlay(), Some(crate::app::Overlay::Help));
    // Any overlay swallows keys until Esc (or Enter) closes it.
    app.handle_key(ch('j'));
    assert_eq!(app.overlay(), Some(crate::app::Overlay::Help));
    app.handle_key(key(KeyCode::Enter));
    assert_eq!(app.overlay(), None);

    app.handle_key(ctrl('k'));
    assert_eq!(app.overlay(), Some(crate::app::Overlay::CommandPalette));
}

#[test]
fn r_refreshes_the_sidebar_listing() {
    let mut app = App::new();
    app.handle_key(key(KeyCode::Esc));
    assert_eq!(app.handle_key(ch('r')), vec![Action::ListSessions]);
}

#[test]
fn a_successful_delete_refreshes_and_prunes_a_followed_session() {
    let mut app = App::new();
    // Esc leaves the composer; the sidebar owns the list keys.
    app.handle_key(key(KeyCode::Esc));
    app.apply(follow(vec![text_row("user_message", 1, "hello")]));
    app.set_workspaces(vec![crate::app::WorkspaceNode {
        name: "work".to_string(),
        sessions: vec![crate::app::SessionEntry {
            id: "s1".to_string(),
            status: String::new(),
        }],
    }]);
    let actions = app.handle_key(ch('x'));
    assert_eq!(
        actions,
        vec![Action::DeleteSession {
            id: "s1".to_string(),
        }]
    );

    let actions = app.apply(TransportEvent::Response {
        id: json!(1),
        request: Some(RequestKind::DeleteSession),
        result: Ok(Value::Null),
    });
    assert_eq!(actions, vec![Action::ListSessions]);

    app.apply(TransportEvent::Response {
        id: json!(2),
        request: Some(RequestKind::ListSessions),
        result: Ok(json!({ "data": [] })),
    });
    assert_eq!(app.session_id(), None, "the deleted session is gone");
    assert!(app.messages().is_empty());
}

// --- Task 5: the directory browser through the reducer -------------

#[test]
fn o_opens_the_catalog_and_enter_follows_the_row() {
    let mut app = sidebar_app(&["s1", "s2"]);
    // Esc leaves the composer; the sidebar owns the keys.
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(ch('o'));
    assert_eq!(app.pane(), crate::app::Pane::Catalog);

    let requests = app.take_catalog_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, RequestKind::Catalog("sessions/list"));
    assert_eq!(requests[0].params, json!({ "limit": 50 }));

    // The listing lands the way `main` folds it.
    app.catalog_mut().apply_response(
        "sessions/list",
        &Ok(json!({ "data": [{ "id": "s1" }, { "id": "s2" }] })),
    );
    app.handle_key(ch('j'));
    let actions = app.handle_key(key(KeyCode::Enter));
    assert_eq!(
        actions,
        vec![Action::Follow {
            session_id: "s2".to_string(),
        }]
    );
    assert_eq!(app.pane(), crate::app::Pane::Composer);
}

#[test]
fn the_follow_baseline_records_the_resume_material() {
    let mut app = App::new();
    let mut event = follow(vec![text_row("user_message", 1, "hello")]);
    if let TransportEvent::Response {
        result: Ok(value), ..
    } = &mut event
    {
        value["cursor"] = json!(9);
        value["pending_interrupts"] = json!([
            { "id": "a1", "call_id": "call-1", "tool": "shell", "risk": "high", "agent": "agent" },
        ]);
        value["request_snapshot_hash"] = json!("abc123");
    }
    app.apply(event);

    let material = app.resume_material().expect("a followed session");
    assert_eq!(material.session_id, "s1");
    assert_eq!(material.last_seq, 9);
    assert_eq!(material.pending, vec!["call-1".to_string()]);
    assert_eq!(material.request_snapshot_hash, "abc123");
}

/// The feedback target is the last assistant row's **ordinal** — the
/// `seq` the log's writer stamped on it.
///
/// A count of the assistant lines would name nothing: the count is a
/// fact about this view, not about the log, and two sessions both
/// count "1". The server resolves the target against the log's rows,
/// so the target has to be the row's identity there.
#[test]
fn the_feedback_target_is_the_last_assistant_rows_ordinal() {
    let mut app = App::new();
    app.apply(follow(vec![
        text_row("user_message", 4, "hello"),
        text_row("assistant_message", 5, "first answer"),
        text_row("assistant_message", 9, "second answer"),
    ]));

    let material = app.resume_material().expect("a followed session");
    assert_eq!(
        material.last_message_id.as_deref(),
        Some("9"),
        "the target is the last assistant row's ordinal, not a count \
         of the assistant lines"
    );
}

/// A turn whose only assistant row carries reasoning — the model hit
/// its token cap mid-thinking — renders no transcript line, so the
/// turn has no answer to rate. The earlier turn's answer belongs to
/// a different turn: the target must not reach back into it. The
/// web's projection names no row in the same case.
#[test]
fn a_turn_with_only_a_reasoning_row_offers_no_target() {
    let mut app = App::new();
    app.apply(follow(vec![
        text_row("user_message", 4, "hello"),
        text_row("assistant_message", 5, "the answer"),
        text_row("user_message", 6, "again"),
        reasoning_row(9, "thinking about it"),
    ]));

    let material = app.resume_material().expect("a followed session");
    assert_eq!(
        material.last_message_id, None,
        "the current turn wrote no answer, so it names no row to rate"
    );
}

/// A reasoning-only row in the turn that already answered keeps that
/// turn's answer as the target: the row renders no line of its own,
/// and the turn still has an answer. The web's projection names the
/// same row.
#[test]
fn a_reasoning_only_row_in_the_same_turn_keeps_the_answers_ordinal() {
    let mut app = App::new();
    app.apply(follow(vec![
        text_row("user_message", 4, "hello"),
        text_row("assistant_message", 5, "the answer"),
        reasoning_row(9, "thinking about it"),
    ]));

    let material = app.resume_material().expect("a followed session");
    assert_eq!(
        material.last_message_id.as_deref(),
        Some("5"),
        "a reasoning-only row renders no line, so the answer before \
         it stays the target"
    );
}

/// The target is the **current turn's** answer: an earlier turn's
/// answer is a different turn's row, so it is never named once the
/// operator has asked again.
#[test]
fn the_feedback_target_is_the_current_turns_answer() {
    let mut app = App::new();
    app.apply(follow(vec![
        text_row("user_message", 4, "hello"),
        text_row("assistant_message", 5, "the first answer"),
        text_row("user_message", 6, "again"),
        text_row("assistant_message", 9, "the second answer"),
    ]));

    let material = app.resume_material().expect("a followed session");
    assert_eq!(
        material.last_message_id.as_deref(),
        Some("9"),
        "the current turn's answer is the target, not the earlier \
         turn's"
    );
}

/// An old log's rows carry no ordinal, so the transcript's last
/// assistant line has none to offer — and a rating needs a row to
/// name.
#[test]
fn a_line_without_an_ordinal_offers_no_feedback_target() {
    let mut app = App::new();
    let mut row = text_row("assistant_message", 5, "the answer");
    row.as_object_mut().expect("a row object").remove("seq");
    app.apply(follow(vec![row]));

    let material = app.resume_material().expect("a followed session");
    assert_eq!(
        material.last_message_id, None,
        "a row with no ordinal names nothing to rate"
    );
}

/// The tool call and the feedback target must both survive a turn
/// that called a tool.
///
/// The tool call renders as its own `Role::Assistant` line, so it is
/// the transcript's *last* assistant line — the one a rating names.
/// If that line carried no ordinal, the turn's tool call would
/// silently cost the reader their rating target.
#[test]
fn a_tool_calling_turn_still_offers_its_ordinal() {
    let mut app = App::new();
    app.apply(follow(vec![
        text_row("user_message", 4, "hello"),
        json!({
            "type": "assistant_message",
            "seq": 5,
            "ts": FIXTURE_TS,
            "data": message_data(
                ProviderRole::Assistant,
                Content::parts(vec![
                    ContentPart::Text(TextContent {
                        text: "let me look".to_string(),
                        cache_control: None,
                    }),
                    ContentPart::ToolUse(ToolUse {
                        id: "call_1".to_string(),
                        name: "shell".to_string(),
                        input: json!({ "command": "ls" }),
                    }),
                ]),
            ),
        }),
    ]));

    let lines = app.messages();
    let call = lines
        .iter()
        .find(|line| line.text == "tool call: shell {\"command\":\"ls\"}")
        .expect("the tool call line");
    assert_eq!(call.role, Role::Assistant);

    let material = app.resume_material().expect("a followed session");
    assert_eq!(
        material.last_message_id.as_deref(),
        Some("5"),
        "the tool call line is the last assistant line, so it must \
         carry the row's ordinal"
    );
}

#[test]
fn approval_answers_win_over_the_catalog_y() {
    let mut app = sidebar_app(&["s1"]);
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(ch('o'));
    app.apply(approval_notification());

    // `y` in the catalog pane is normally "copy id"; with a prompt
    // open it must answer the prompt, in the existing spelling.
    let actions = app.handle_key(ch('y'));
    assert!(matches!(
        actions.as_slice(),
        [Action::RespondApproval { .. }]
    ));
}

#[test]
fn a_write_response_republishes_the_listing_through_the_loop() {
    // The binary's path: fold the write's answer, ask for a refresh,
    // queue it, and drain it for the transport.
    let mut app = App::new();
    let spec = crate::domains::DomainSpec::by_list_method("sessions/list")
        .expect("sessions");
    let _ = app.catalog_mut().open(spec);
    let _ = app.take_catalog_requests();

    app.catalog_mut()
        .apply_response("sessions/delete", &Ok(json!({ "deleted": true })));
    let refresh = app.catalog_mut().refresh();
    app.enqueue_catalog_requests(refresh);
    let requests = app.take_catalog_requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, RequestKind::Catalog("sessions/list"));
    assert_eq!(requests[0].params, json!({ "limit": 50 }));

    // A failed send clears the loading state instead of spinning.
    app.catalog_mut().note_send_failure("still connecting");
    app.catalog_mut()
        .apply_response("sessions/list", &Ok(json!({ "data": [] })));
    assert!(
        app.catalog().status.contains("行"),
        "{}",
        app.catalog().status
    );
}

#[test]
fn catalog_responses_are_routed_past_the_reducer() {
    let mut app = App::new();
    app.insert_str("hi");
    let _ = app.submit();
    app.apply(TransportEvent::Ready {
        version: "0.1.0".to_string(),
        workspace_root: "/work".to_string(),
    });
    assert_eq!(app.status(), "connected");

    // `main` folds Catalog responses into `catalog_mut` and never
    // calls `App::apply`; the status line proves it.
    app.catalog_mut().apply_response(
        "sessions/list",
        &Ok(json!({ "data": [{ "id": "s1" }] })),
    );
    assert_eq!(app.status(), "connected");
}

#[test]
fn s_opens_the_search_prompt_from_the_sidebar() {
    let mut app = sidebar_app(&["s1"]);
    app.handle_key(key(KeyCode::Esc));
    app.handle_key(ch('s'));
    assert_eq!(app.pane(), crate::app::Pane::Catalog);
    assert_eq!(
        app.catalog().domain.map(|spec| spec.list_method),
        Some("search/all")
    );
    assert_eq!(app.catalog().query.as_deref(), Some(""));
    assert!(
        app.take_catalog_requests().is_empty(),
        "the prompt sends nothing before the query"
    );
}

// -- the ui tree -----------------------------------------------------

fn ui_schema() -> Value {
    json!({
        "slots": [
            { "id": "chat.sidebar", "cardinality": "many", "owner": "ui.shell" },
        ],
        "schemas": [{ "version": 1, "components": [
            { "slot": "chat.sidebar", "kind": "panel", "props": { "title": "Notes" } },
        ] }],
    })
}

#[test]
fn ready_fetches_the_ui_schema() {
    let mut app = App::new();
    let actions = app.apply(ready());
    assert_eq!(
        actions.last(),
        Some(&Action::FetchUiSchema),
        "every core generation asks for the tree once: {actions:?}"
    );
}

#[test]
fn a_ui_schema_response_replaces_the_tree() {
    let mut app = App::new();
    app.apply(TransportEvent::Response {
        id: json!(2),
        request: Some(RequestKind::UiSchema),
        result: Ok(ui_schema()),
    });
    let components = app.ui().components_in("chat.sidebar");
    assert_eq!(components.len(), 1);
    assert_eq!(components[0].props["title"], json!("Notes"));
}

#[test]
fn an_unreadable_ui_schema_only_notes_the_status() {
    let mut app = App::new();
    app.apply(TransportEvent::Response {
        id: json!(3),
        request: Some(RequestKind::UiSchema),
        result: Ok(json!("not-an-object")),
    });
    assert!(app.ui().components_in("chat.sidebar").is_empty());
    assert!(
        app.status().contains("the ui schema was unreadable"),
        "unexpected status: {}",
        app.status()
    );
}
