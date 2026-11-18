use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use serde_json::json;
use uva_provider::Content;

use super::*;
use crate::{
    app::App,
    tests::{FIXTURE_TS, message_data, message_role},
    transport::{RequestKind, TransportEvent},
};

fn text_row(kind: &str, seq: u64, text: &str) -> serde_json::Value {
    json!({
        "type": kind,
        "seq": seq,
        "ts": FIXTURE_TS,
        "data": message_data(message_role(kind), Content::text(text)),
    })
}

fn followed(app: &mut App) {
    app.apply(TransportEvent::Response {
        id: json!(1),
        request: Some(RequestKind::Follow),
        result: Ok(json!({
            "header": {
                "session_id": "s1",
                "user_id": "u1",
                "agent_name": "default",
                "state": "idle",
            },
            "rows": [
                text_row("user_message", 1, "what is the answer?"),
                text_row("assistant_message", 2, "The answer is 42."),
            ],
            "cursor": 2,
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
        })),
    });
}

fn screen_text(buffer: &Buffer) -> String {
    let mut text = String::new();
    for y in 0..buffer.area.height {
        for x in 0..buffer.area.width {
            text.push_str(buffer[(x, y)].symbol());
        }
        text.push('\n');
    }
    text
}

fn render_to_buffer(app: &App, width: u16, height: u16) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| draw(frame, app))
        .expect("the draw succeeds");
    screen_text(terminal.backend().buffer())
}

/// The screen text with every space removed. A wide (CJK) glyph
/// occupies two cells — the symbol plus a blank — so a multi-glyph
/// phrase never appears contiguous in the raw screen text;
/// squeezing it makes `contains` work for both widths.
fn squeezed(screen: &str) -> String {
    screen.chars().filter(|c| !c.is_whitespace()).collect()
}

#[test]
fn the_transcript_and_composer_are_drawn() {
    let mut app = App::new();
    followed(&mut app);
    app.insert_str("next question");
    let screen = render_to_buffer(&app, 80, 24);
    assert!(screen.contains("what is the answer?"), "{screen}");
    assert!(screen.contains("The answer is 42."), "{screen}");
    assert!(screen.contains("next question"), "{screen}");
    assert!(screen.contains("Conversation"), "{screen}");
    assert!(screen.contains("session s1"), "{screen}");
}

#[test]
fn the_approval_prompt_is_drawn_over_the_transcript() {
    let mut app = App::new();
    followed(&mut app);
    app.apply(TransportEvent::Notification {
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
    });
    let screen = render_to_buffer(&app, 80, 24);
    assert!(screen.contains("Approval required"), "{screen}");
    assert!(screen.contains("tool: shell"), "{screen}");
    assert!(screen.contains("approval_id: a1"), "{screen}");
    assert!(screen.contains("[y] allow"), "{screen}");
    assert!(screen.contains("[n] deny"), "{screen}");
    assert!(screen.contains("what is the answer?"), "{screen}");
}

#[test]
fn an_empty_projection_still_draws_the_composer_hint() {
    let app = App::new();
    let screen = render_to_buffer(&app, 40, 12);
    assert!(screen.contains("type a message"), "{screen}");
    assert!(screen.contains("starting"), "{screen}");
}

fn workspace(name: &str, ids: &[&str]) -> crate::app::WorkspaceNode {
    crate::app::WorkspaceNode {
        name: name.to_string(),
        sessions: ids
            .iter()
            .map(|&id| crate::app::SessionEntry {
                id: id.to_string(),
                status: String::new(),
            })
            .collect(),
    }
}

/// Render and hand back the buffer, so position-sensitive assertions
/// can read cells.
fn render_to_cells(app: &App, width: u16, height: u16) -> Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal
        .draw(|frame| draw(frame, app))
        .expect("the draw succeeds");
    terminal.backend().buffer().clone()
}

#[test]
fn the_sidebar_lists_workspace_groups_and_sessions() {
    let mut app = App::new();
    app.set_workspaces(vec![
        workspace("alpha", &["s1", "s2"]),
        workspace("beta", &["s3", "s4"]),
    ]);
    let screen = render_to_buffer(&app, 80, 24);
    assert!(screen.contains("alpha"), "{screen}");
    assert!(screen.contains("beta"), "{screen}");
    for id in ["s1", "s2", "s3", "s4"] {
        assert!(screen.contains(id), "{screen}");
    }
}

#[test]
fn collapsing_the_sidebar_leaves_only_the_icon_rail() {
    let mut app = App::new();
    app.set_workspaces(vec![workspace("alpha", &["s1", "s2"])]);
    app.toggle_sidebar();
    let buffer = render_to_cells(&app, 80, 24);
    let screen = screen_text(&buffer);
    assert!(!screen.contains("s1"), "{screen}");
    assert!(!screen.contains("alpha"), "{screen}");
    // The main area (the conversation block) starts right after the
    // three-column rail.
    assert_eq!(buffer[(3, 1)].symbol(), "┌", "{screen}");

    app.toggle_sidebar();
    let buffer = render_to_cells(&app, 80, 24);
    assert_eq!(buffer[(24, 1)].symbol(), "┌");
    assert!(screen_text(&buffer).contains("s1"));
}

#[test]
fn user_lines_render_as_right_aligned_bubbles() {
    let mut app = App::new();
    followed(&mut app);
    let buffer = render_to_cells(&app, 80, 24);
    // The conversation block spans x = 24..80 (bordered), so a
    // right-aligned bubble's last glyph sits at x = 78.
    let mut found = None;
    for y in 0..buffer.area.height {
        let row: String = (0..buffer.area.width)
            .map(|x| buffer[(x, y)].symbol())
            .collect();
        if row.contains("what is the answer?")
            && let Some(at) = row.find('?')
        {
            // `find` answers a byte index; the border glyphs are
            // multi-byte, so count characters instead.
            let column =
                u16::try_from(row[..at].chars().count()).expect("a column");
            found = Some((column, y, row));
            break;
        }
    }
    let (at, y, row) = found.expect("the user line is drawn");
    assert_eq!(at, 78, "the bubble hugs the right edge: {row}");
    // The bubble's text carries a background colour (the "bubble").
    assert_ne!(
        buffer[(at, y)].bg,
        ratatui::style::Color::Reset,
        "the bubble is highlighted"
    );
}

#[test]
fn thinking_expands_while_streaming_and_collapses_when_done() {
    let mut app = App::new();
    followed(&mut app);
    app.apply(TransportEvent::Notification {
        method: "session/event".to_string(),
        params: json!({
            "kind": "live",
            "live": {
                "running": true,
                "text": "",
                "reasoning": "consider the question\ncheck the weather",
            },
        }),
    });
    let screen = render_to_buffer(&app, 80, 24);
    assert!(squeezed(&screen).contains("思考中"), "{screen}");
    assert!(screen.contains("consider the question"), "{screen}");

    app.apply(TransportEvent::Notification {
        method: "session/event".to_string(),
        params: json!({
            "kind": "row",
            "seq": 5,
            "row": {
                "type": "turn",
                "seq": 5,
                "ts": FIXTURE_TS,
                "data": { "kind": "end", "turn": 0, "reason": "completed" },
            },
        }),
    });
    let screen = render_to_buffer(&app, 80, 24);
    assert!(squeezed(&screen).contains("已思考>"), "{screen}");
    assert!(!screen.contains("consider the question"), "{screen}");
}

#[test]
fn todo_progress_renders_and_takes_no_space_when_empty() {
    let mut app = App::new();
    followed(&mut app);
    app.set_todo(crate::app::TodoState::new(vec![
        crate::app::TodoItem {
            content: "scan the routes".to_string(),
            status: crate::app::TodoStatus::Done,
        },
        crate::app::TodoItem {
            content: "write the views".to_string(),
            status: crate::app::TodoStatus::Active,
        },
        crate::app::TodoItem {
            content: "wire the keys".to_string(),
            status: crate::app::TodoStatus::Pending,
        },
    ]));
    let screen = render_to_buffer(&app, 80, 24);
    assert!(squeezed(&screen).contains("待办1/3"), "{screen}");
    assert!(screen.contains("✔ scan the routes"), "{screen}");
    assert!(screen.contains("▸ write the views"), "{screen}");
    assert!(screen.contains("· wire the keys"), "{screen}");

    let mut bare = App::new();
    followed(&mut bare);
    let screen = render_to_buffer(&bare, 80, 24);
    assert!(!squeezed(&screen).contains("待办"), "{screen}");
    assert!(!screen.contains("✔"), "{screen}");
}

#[test]
fn the_composer_wears_a_bash_prompt_and_a_footer() {
    let mut app = App::new();
    app.apply(TransportEvent::Ready {
        version: "0.1.0".to_string(),
        workspace_root: "/work".to_string(),
    });
    let screen = render_to_buffer(&app, 80, 24);
    assert!(screen.contains("❯ /work"), "{screen}");
    assert!(squeezed(&screen).contains("+/工作区/模型/发送"), "{screen}");
}

#[test]
fn the_settings_tab_lists_the_sections() {
    let mut app = App::new();
    app.set_sidebar_tab(crate::app::SidebarTab::Settings);
    let screen = render_to_buffer(&app, 80, 24);
    let squeezed = squeezed(&screen);
    for section in crate::app::SETTINGS_SECTIONS {
        assert!(squeezed.contains(section), "{section} missing: {screen}");
    }
}

// -- the deployment's UI tree ----------------------------------------

fn ui_tree(value: serde_json::Value) -> TransportEvent {
    TransportEvent::Response {
        id: json!(9),
        request: Some(RequestKind::UiSchema),
        result: Ok(value),
    }
}

fn slot_component(
    slot: &str,
    kind: &str,
    props: serde_json::Value,
) -> serde_json::Value {
    json!({ "slot": slot, "kind": kind, "props": props })
}

#[test]
fn an_empty_ui_tree_leaves_the_layout_untouched() {
    let mut app = App::new();
    followed(&mut app);
    let buffer = render_to_cells(&app, 80, 24);
    let screen = screen_text(&buffer);
    assert!(screen.contains("what is the answer?"), "{screen}");
    // No right column: the conversation block spans the main column.
    assert_eq!(buffer[(24, 1)].symbol(), "┌", "{screen}");
    assert_eq!(buffer[(79, 1)].symbol(), "┐", "{screen}");
}

#[test]
fn a_chat_sidebar_slot_renders_a_right_column() {
    let mut app = App::new();
    followed(&mut app);
    app.apply(ui_tree(json!({
        "slots": [{ "id": "chat.sidebar", "cardinality": "many", "owner": "ui.shell" }],
        "schemas": [{ "version": 1, "components": [
            slot_component("chat.sidebar", "panel", json!({ "title": "Notes" })),
        ] }],
    })));

    let buffer = render_to_cells(&app, 80, 24);
    let screen = screen_text(&buffer);
    assert!(screen.contains("Notes"), "{screen}");
    assert!(screen.contains("chat.sidebar"), "{screen}");
    // The conversation block no longer spans the whole main column.
    let row: Vec<&str> = (0..buffer.area.width)
        .map(|x| buffer[(x, 1)].symbol())
        .collect();
    let start = row
        .iter()
        .position(|symbol| *symbol == "┌")
        .expect("the conversation border");
    let end = row
        .iter()
        .enumerate()
        .skip(start + 1)
        .find(|(_, symbol)| **symbol == "┐")
        .map(|(x, _)| x)
        .expect("the conversation border closes");
    assert!(end < 79, "the conversation is narrower: {screen}");
    assert_eq!(row[end + 1], "┌", "the slot column starts beside it");
}

#[test]
fn an_unknown_kind_renders_a_placeholder_line() {
    let mut app = App::new();
    followed(&mut app);
    app.apply(ui_tree(json!({
        "schemas": [{ "version": 1, "components": [
            slot_component("chat.sidebar", "weather-card", json!({ "city": "Berlin" })),
        ] }],
    })));
    let screen = render_to_buffer(&app, 80, 24);
    assert!(screen.contains("? weather-card"), "{screen}");
}

#[test]
fn session_extensions_render_below_the_transcript() {
    let mut app = App::new();
    followed(&mut app);
    app.apply(ui_tree(json!({
        "schemas": [{ "version": 1, "components": [
            slot_component(
                "session.detail.extensions",
                "key-value",
                json!({ "title": "Facts", "entries": { "build": "knife8" } }),
            ),
            slot_component(
                "debug.dump",
                "list",
                json!({ "title": "Dump", "items": ["one", "two"] }),
            ),
        ] }],
    })));

    let screen = render_to_buffer(&app, 80, 24);
    assert!(screen.contains("session.detail.extensions"), "{screen}");
    assert!(screen.contains("build: knife8"), "{screen}");
    assert!(screen.contains("unplaced: debug.dump"), "{screen}");
    assert!(screen.contains("Facts"), "{screen}");
    // Below the transcript: the extensions block's title sits further
    // down than the conversation's.
    let conversation = screen
        .lines()
        .position(|line| line.contains("Conversation"))
        .expect("the conversation is drawn");
    let extensions = screen
        .lines()
        .position(|line| line.contains("session.detail.extensions"))
        .expect("the extensions block is drawn");
    assert!(extensions > conversation, "{screen}");
}
