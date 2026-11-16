//! View tests: the generic list, the generic detail and the
//! directory state machine — all asserted on a `TestBackend` buffer
//! or on pure state, never a tty.

use crossterm::event::{KeyCode, KeyModifiers};
use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
use serde_json::{Value, json};

use super::{
    detail::{Confirm, DetailView, draw_confirm, draw_detail, draw_ops_footer},
    list::{ListView, draw_list},
};
use crate::domains::{DomainSpec, ListPage};

fn key(code: KeyCode) -> (KeyCode, KeyModifiers) {
    (code, KeyModifiers::NONE)
}

fn ctrl(character: char) -> (KeyCode, KeyModifiers) {
    (KeyCode::Char(character), KeyModifiers::CONTROL)
}

fn sessions() -> &'static DomainSpec {
    DomainSpec::by_list_method("sessions/list").expect("the sessions domain")
}

fn session_rows(n: usize) -> Vec<Value> {
    (0..n)
        .map(|index| {
            json!({
                "id": format!("s{index}"),
                "status": "idle",
                "created_at": "2026-11-09T10:00:00Z",
                "agent_name": "agent",
            })
        })
        .collect()
}

fn view_with(rows: Vec<Value>) -> ListView {
    let mut view = ListView::new();
    view.set_page(
        ListPage {
            rows,
            next_cursor: None,
            total: None,
        },
        false,
    );
    view
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

fn squeezed(screen: &str) -> String {
    screen.chars().filter(|c| !c.is_whitespace()).collect()
}

fn render_with(
    width: u16,
    height: u16,
    paint: impl FnOnce(&mut ratatui::Frame<'_>),
) -> String {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("a test terminal");
    terminal.draw(paint).expect("the draw succeeds");
    screen_text(terminal.backend().buffer())
}

#[test]
fn the_list_draws_headers_rows_and_the_cursor_marker() {
    let view = view_with(session_rows(3));
    let screen = render_with(80, 24, |frame| {
        let area = frame.area();
        draw_list(frame, area, &view, sessions());
    });
    let squeezed = squeezed(&screen);
    assert!(squeezed.contains("会话"), "header missing:\n{screen}");
    assert!(squeezed.contains("状态"), "header missing:\n{screen}");
    assert!(squeezed.contains("s0"), "row missing:\n{screen}");
    assert!(squeezed.contains("s2"), "row missing:\n{screen}");
    assert!(squeezed.contains(">s0"), "cursor marker missing:\n{screen}");
}

#[test]
fn d30_keys_move_the_cursor() {
    let mut view = view_with(session_rows(12));
    let spec = sessions();
    let viewport = 4;
    let id = |view: &ListView| view.focused_id(spec);

    assert!(view.key(key(KeyCode::Char('j')).0, KeyModifiers::NONE, viewport));
    assert_eq!(id(&view).as_deref(), Some("s1"));

    view.key(KeyCode::Char('g'), KeyModifiers::NONE, viewport);
    view.key(KeyCode::Char('g'), KeyModifiers::NONE, viewport);
    assert_eq!(id(&view).as_deref(), Some("s0"), "gg jumps to the top");

    view.key(KeyCode::Char('G'), KeyModifiers::NONE, viewport);
    assert_eq!(id(&view).as_deref(), Some("s11"), "G jumps to the end");

    view.key(KeyCode::Char('g'), KeyModifiers::NONE, viewport);
    view.key(KeyCode::Char('g'), KeyModifiers::NONE, viewport);
    view.key(ctrl('d').0, KeyModifiers::CONTROL, viewport);
    assert_eq!(
        id(&view).as_deref(),
        Some("s2"),
        "Ctrl+d moves half the viewport (2 rows)"
    );
    view.key(ctrl('u').0, KeyModifiers::CONTROL, viewport);
    assert_eq!(id(&view).as_deref(), Some("s0"));

    view.key(KeyCode::Char('k'), KeyModifiers::NONE, viewport);
    assert_eq!(id(&view).as_deref(), Some("s0"), "k clamps at the top");
    // A stray key clears the pending operator instead of firing it.
    view.key(KeyCode::Char('g'), KeyModifiers::NONE, viewport);
    view.key(KeyCode::Char('x'), KeyModifiers::NONE, viewport);
    assert_eq!(id(&view).as_deref(), Some("s0"));
}

#[test]
fn slash_filters_rows_and_n_cycles_matches() {
    let rows = vec![
        json!({ "id": "s1" }),
        json!({ "id": "s2" }),
        json!({ "id": "t1" }),
    ];
    let mut view = view_with(rows.clone());
    let spec = sessions();
    let viewport = 10;
    assert_eq!(view.visible(), vec![0, 1, 2]);

    view.key(KeyCode::Char('/'), KeyModifiers::NONE, viewport);
    view.key(KeyCode::Char('s'), KeyModifiers::NONE, viewport);
    assert_eq!(view.visible(), vec![0, 1], "filter narrows to matches");
    assert_eq!(view.matches, vec![0, 1]);

    view.key(KeyCode::Enter, KeyModifiers::NONE, viewport);
    assert_eq!(view.focused_id(spec).as_deref(), Some("s1"));
    view.key(KeyCode::Char('n'), KeyModifiers::NONE, viewport);
    assert_eq!(view.focused_id(spec).as_deref(), Some("s2"));
    assert_eq!(view.match_at, 1);
    view.key(KeyCode::Char('N'), KeyModifiers::NONE, viewport);
    assert_eq!(view.match_at, 0);
    assert_eq!(view.focused_id(spec).as_deref(), Some("s1"));

    // Editing a filter again and cancelling clears it: Esc while the
    // query is typing cancels the whole filter; afterwards Esc belongs
    // to the owning view.
    view.key(KeyCode::Char('/'), KeyModifiers::NONE, viewport);
    view.key(KeyCode::Esc, KeyModifiers::NONE, viewport);
    assert!(view.filter.is_none());
    assert_eq!(view.visible(), vec![0, 1, 2]);
}

#[test]
fn dd_is_reported_to_the_owner() {
    let mut view = view_with(session_rows(2));
    assert!(view.key(KeyCode::Char('d'), KeyModifiers::NONE, 10));
    assert!(!view.take_double_d(), "the first d only arms");
    assert!(view.key(KeyCode::Char('d'), KeyModifiers::NONE, 10));
    assert!(view.take_double_d(), "the second d completes dd");
    assert!(!view.take_double_d(), "the signal fires once");
}

#[test]
fn narrow_terminals_shrink_the_rightmost_column() {
    let view = view_with(session_rows(3));
    let screen = render_with(60, 24, |frame| {
        let area = frame.area();
        draw_list(frame, area, &view, sessions());
    });
    assert!(
        squeezed(&screen).contains("s0"),
        "the first column survives a narrow terminal:\n{screen}"
    );
}

#[test]
fn a_next_cursor_is_reported_as_a_hint() {
    let mut view = ListView::new();
    view.set_page(
        ListPage {
            rows: session_rows(2),
            next_cursor: Some("Y3Vyc29y".to_string()),
            total: Some(12),
        },
        false,
    );
    assert!(view.next_cursor.is_some());
    assert_eq!(view.total, Some(12));
    let screen = render_with(80, 12, |frame| {
        let area = frame.area();
        draw_list(frame, area, &view, sessions());
    });
    assert!(
        squeezed(&screen).contains("共12"),
        "total missing:\n{screen}"
    );
}

#[test]
fn a_nested_object_flattens_to_pointer_rows() {
    let view = DetailView::from_value(
        "s1",
        &json!({
            "id": "s1",
            "header": { "state": "running", "agent_name": "agent" },
            "usage": { "input_tokens": 120, "output_tokens": 30 },
        }),
    );
    assert_eq!(view.id, "s1");
    let rows: Vec<(&str, &str)> = view
        .rows
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    assert!(rows.contains(&("id", "s1")));
    assert!(rows.contains(&("header/state", "running")));
    assert!(rows.contains(&("usage/input_tokens", "120")));
    assert!(
        !rows.iter().any(|(path, _)| path.contains("header/state/")),
        "nested objects keep their path segments"
    );
}

#[test]
fn arrays_flatten_by_index_and_stop_at_the_depth_cap() {
    let view = DetailView::from_value(
        "s1",
        &json!({
            "cases": [
                { "name": "first", "input": { "deep": { "deeper": { "too_deep": true } } } },
            ],
        }),
    );
    let rows: Vec<(&str, &str)> = view
        .rows
        .iter()
        .map(|(path, text)| (path.as_str(), text.as_str()))
        .collect();
    assert!(rows.contains(&("cases/0/name", "first")));
    // Three path segments are the floor: a container reached at the
    // cap collapses to its size instead of listing every leaf.
    assert!(
        rows.contains(&("cases/0/input", "{1 项}")),
        "the depth cap renders a summary: {:?}",
        view.rows
    );
    assert!(
        view.rows
            .iter()
            .all(|(path, _)| path.split('/').count() <= 3),
        "no row goes deeper than three segments"
    );
}

#[test]
fn deep_arrays_are_capped_with_an_ellipsis_row() {
    let cases: Vec<Value> =
        (0..300).map(|index| json!({ "n": index })).collect();
    let view = DetailView::from_value("s1", &json!({ "cases": cases }));
    assert!(view.rows.len() <= 201, "rows are bounded");
    let (_, last) = view.rows.last().expect("at least the ellipsis row");
    assert!(last.contains("省略"), "ellipsis row: {last}");
    assert_eq!(view.rows[0].0, "cases/0/n");
}

#[test]
fn the_confirm_modal_names_the_op_method_and_target() {
    let confirm = Confirm {
        op_label: "删除",
        method: "sessions/delete",
        target: "s1".to_string(),
    };
    let screen = render_with(80, 24, |frame| {
        let area = frame.area();
        draw_confirm(frame, area, &confirm);
    });
    let squeezed = squeezed(&screen);
    for needle in ["删除", "sessions/delete", "s1", "[y]", "[Esc]"] {
        assert!(squeezed.contains(needle), "missing {needle}:\n{screen}");
    }
}

#[test]
fn the_detail_and_the_ops_footer_draw() {
    let view =
        DetailView::from_value("s1", &json!({ "id": "s1", "status": "idle" }));
    let screen = render_with(100, 20, |frame| {
        let area = frame.area();
        let rows = ratatui::layout::Layout::vertical([
            ratatui::layout::Constraint::Min(0),
            ratatui::layout::Constraint::Length(1),
        ])
        .split(area);
        draw_detail(frame, rows[0], &view, sessions());
        draw_ops_footer(frame, rows[1], sessions());
    });
    let squeezed = squeezed(&screen);
    for needle in ["id:s1", "status:idle", "删除", "resume", "Esc返回"] {
        assert!(squeezed.contains(needle), "missing {needle}:\n{screen}");
    }
}

// --- Task 5: the directory state machine ---------------------------

use crate::{
    app::{App, WorkspaceNode},
    domains::DOMAINS,
    transport::{RequestKind, TransportEvent},
    views::catalog::{Catalog, ResumeMaterial},
};

fn opened(spec: &'static DomainSpec, rows: Value) -> Catalog {
    let mut catalog = Catalog::new();
    let _ = catalog.open(spec);
    catalog.apply_response(spec.list_method, &Ok(rows));
    catalog
}

fn type_text(catalog: &mut Catalog, text: &str) {
    for character in text.chars() {
        catalog.key(KeyCode::Char(character), KeyModifiers::NONE, 10, None);
    }
}

#[test]
fn opening_a_domain_asks_for_the_first_page() {
    let mut catalog = Catalog::new();
    let requests = catalog.open(&DOMAINS[0]);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, RequestKind::Catalog("sessions/list"));
    assert_eq!(requests[0].params, json!({ "limit": 50 }));
}

#[test]
fn a_list_response_renders_rows_and_a_get_opens_detail() {
    let agents =
        DomainSpec::by_list_method("agents/list").expect("the agents domain");
    let mut catalog = Catalog::new();
    let requests = catalog.open(agents);
    assert_eq!(requests[0].kind, RequestKind::Catalog("agents/list"));

    catalog.apply_response(
        "agents/list",
        &Ok(json!({
            "data": [
                { "name": "a1", "kind": "builtin", "version": "1" },
            ],
            "next_cursor": null,
            "total": 1,
        })),
    );
    assert_eq!(catalog.list.rows.len(), 1);
    assert_eq!(catalog.list.focused_id(agents).as_deref(), Some("a1"));

    let requests = catalog.key(KeyCode::Enter, KeyModifiers::NONE, 10, None);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, RequestKind::Catalog("agents/get"));
    assert_eq!(requests[0].params, json!({ "name": "a1" }));

    catalog.apply_response(
        "agents/get",
        &Ok(json!({ "name": "a1", "kind": "builtin" })),
    );
    let detail = catalog.detail.as_ref().expect("the detail opens");
    assert_eq!(detail.id, "a1");
    assert!(detail.rows.iter().any(|(path, _)| path == "name"));
}

#[test]
fn delete_requires_confirmation_before_sending() {
    let mut catalog = opened(
        sessions(),
        json!({ "data": [{ "id": "s1" }, { "id": "s2" }] }),
    );
    catalog.key(KeyCode::Char('j'), KeyModifiers::NONE, 10, None);

    let requests =
        catalog.key(KeyCode::Char('x'), KeyModifiers::NONE, 10, None);
    assert!(requests.is_empty(), "x must not send before the y");
    let confirm = catalog.confirm.as_ref().expect("the modal opens");
    assert_eq!(confirm.op_label, "删除");
    assert_eq!(confirm.method, "sessions/delete");
    assert_eq!(confirm.target, "s2");

    let requests =
        catalog.key(KeyCode::Char('y'), KeyModifiers::NONE, 10, None);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, RequestKind::Catalog("sessions/delete"));
    assert_eq!(requests[0].params, json!({ "id": "s2" }));
    assert!(catalog.confirm.is_none());

    // The write's answer asks for the first page again.
    catalog.apply_response("sessions/delete", &Ok(json!({ "deleted": true })));
    let requests = catalog.refresh();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, RequestKind::Catalog("sessions/list"));
    assert_eq!(requests[0].params, json!({ "limit": 50 }));
}

#[test]
fn cancelling_the_confirm_sends_nothing() {
    let mut catalog = opened(sessions(), json!({ "data": [{ "id": "s1" }] }));
    catalog.key(KeyCode::Char('x'), KeyModifiers::NONE, 10, None);
    let requests = catalog.key(KeyCode::Esc, KeyModifiers::NONE, 10, None);
    assert!(requests.is_empty());
    assert!(catalog.confirm.is_none());
    assert_eq!(catalog.status, "已取消");
}

#[test]
fn resume_is_disabled_without_a_token() {
    let mut catalog = opened(sessions(), json!({ "data": [{ "id": "s1" }] }));
    catalog.open_palette();
    type_text(&mut catalog, "resume");
    let requests = catalog.key(KeyCode::Enter, KeyModifiers::NONE, 10, None);
    assert!(requests.is_empty(), "no baseline, no request");
    assert!(catalog.confirm.is_none(), "no confirm without a token");
    assert!(catalog.status.contains("不可用"), "{}", catalog.status);

    // With the follow baseline's three token parts and an open
    // interrupt, the same menu entry confirms and sends the full body.
    let material = ResumeMaterial {
        session_id: "s1".to_string(),
        last_seq: 7,
        pending: vec!["call-1".to_string()],
        request_snapshot_hash: "hash".to_string(),
        last_message_id: None,
    };
    catalog.open_palette();
    type_text(&mut catalog, "resume");
    let requests =
        catalog.key(KeyCode::Enter, KeyModifiers::NONE, 10, Some(&material));
    assert!(requests.is_empty(), "the confirm comes first");
    assert!(catalog.confirm.is_some());

    let requests = catalog.key(
        KeyCode::Char('y'),
        KeyModifiers::NONE,
        10,
        Some(&material),
    );
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, RequestKind::Catalog("sessions/resume"));
    let params = &requests[0].params;
    assert_eq!(params["session_id"], "s1");
    assert_eq!(params["resume_token"]["last_seq"], 7);
    assert_eq!(params["resume_token"]["pending"], json!(["call-1"]));
    assert_eq!(params["resume_token"]["request_snapshot_hash"], "hash");
    assert_eq!(
        params["interrupts"],
        json!([{ "id": "call-1", "status": "resolved" }])
    );
    assert_eq!(params["allow_drift"], false);
}

#[test]
fn catalog_responses_never_reach_the_reducer() {
    // What `main` does: a directory answer is folded into the catalog
    // and the reducer never sees it. `App::apply` is the reducer's
    // door, so the proof is that the status line keeps its connection
    // wording after the directory answer lands.
    let mut app = App::new();
    app.insert_str("hi");
    let _ = app.submit(); // `creating` is set; Ready keeps "connected"
    app.apply(TransportEvent::Ready {
        version: "0.1.0".to_string(),
        workspace_root: "/work".to_string(),
    });
    assert_eq!(app.status(), "connected");

    let spec = crate::domains::DomainSpec::by_list_method("sessions/list")
        .expect("sessions");
    let _ = app.catalog_mut().open(spec);
    app.catalog_mut().apply_response(
        "sessions/list",
        &Ok(json!({ "data": [{ "id": "s1" }], "next_cursor": null })),
    );
    assert_eq!(app.status(), "connected");
    assert_eq!(app.catalog().list.rows.len(), 1);
}

#[test]
fn the_sidebar_lists_the_domains_and_the_workspace_tree_shows_sessions() {
    let mut app = App::new();
    app.set_workspaces(vec![WorkspaceNode {
        name: "work".to_string(),
        sessions: vec![crate::app::SessionEntry {
            id: "s1".to_string(),
            status: String::new(),
        }],
    }]);
    let screen = render_with(80, 24, |frame| {
        crate::render::draw(frame, &app);
    });
    let squeezed = squeezed(&screen);
    assert!(squeezed.contains("s1"), "the tree's sessions:\n{screen}");
    assert!(squeezed.contains("目录"), "the directory entry:\n{screen}");
    for label in ["会话", "智能体", "搜索"] {
        assert!(
            squeezed.contains(label),
            "missing domain {label}:\n{screen}"
        );
    }
}

#[test]
fn escape_leaves_the_catalog_only_from_the_list_root() {
    let mut catalog = opened(sessions(), json!({ "data": [{ "id": "s1" }] }));
    // A detail page swallows the first Esc.
    catalog.detail = Some(DetailView::from_value("s1", &json!({ "id": "s1" })));
    catalog.key(KeyCode::Esc, KeyModifiers::NONE, 10, None);
    assert!(catalog.detail.is_none());
    assert!(!catalog.take_exit());

    catalog.key(KeyCode::Esc, KeyModifiers::NONE, 10, None);
    assert!(catalog.take_exit());
}

#[test]
fn search_domain_prompts_for_a_query_before_listing() {
    let search =
        DomainSpec::by_list_method("search/all").expect("the search domain");
    let mut catalog = Catalog::new();
    let requests = catalog.open(search);
    assert!(requests.is_empty(), "no request before the query");
    assert_eq!(catalog.query.as_deref(), Some(""));

    type_text(&mut catalog, "rust");
    assert_eq!(catalog.query.as_deref(), Some("rust"));
    let requests = catalog.key(KeyCode::Enter, KeyModifiers::NONE, 10, None);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, RequestKind::Catalog("search/all"));
    assert_eq!(requests[0].params, json!({ "q": "rust", "limit": 20 }));
}

#[test]
fn the_json_gate_confirms_before_sending() {
    let mut catalog = opened(sessions(), json!({ "data": [{ "id": "s1" }] }));
    catalog.open_palette();
    type_text(&mut catalog, r#"json evals/create {"name":"nightly"}"#);
    let requests = catalog.key(KeyCode::Enter, KeyModifiers::NONE, 10, None);
    assert!(requests.is_empty(), "the confirm comes first");
    let confirm = catalog.confirm.as_ref().expect("the json confirm");
    assert_eq!(confirm.op_label, "json");
    assert_eq!(confirm.method, "evals/create");

    let requests =
        catalog.key(KeyCode::Char('y'), KeyModifiers::NONE, 10, None);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, RequestKind::Catalog("evals/create"));
    assert_eq!(requests[0].params, json!({ "name": "nightly" }));

    // Malformed params never reach a confirm.
    catalog.open_palette();
    type_text(&mut catalog, "json evals/create {");
    let requests = catalog.key(KeyCode::Enter, KeyModifiers::NONE, 10, None);
    assert!(requests.is_empty());
    assert!(catalog.confirm.is_none());
    assert!(catalog.status.contains("JSON"), "{}", catalog.status);
}

#[test]
fn an_unknown_json_method_is_refused_locally() {
    let mut catalog = Catalog::new();
    catalog.open_palette();
    type_text(&mut catalog, "json not/a-method {}");
    let requests = catalog.key(KeyCode::Enter, KeyModifiers::NONE, 10, None);
    assert!(requests.is_empty());
    assert!(catalog.confirm.is_none());
    assert!(catalog.status.contains("未知方法"), "{}", catalog.status);
}

#[test]
fn tab_cycles_the_domains() {
    let mut catalog = Catalog::new();
    catalog.open(&DOMAINS[0]);
    let requests = catalog.key(KeyCode::Tab, KeyModifiers::NONE, 10, None);
    assert_eq!(
        catalog.domain.map(|spec| spec.list_method),
        Some("agents/list")
    );
    assert_eq!(requests[0].kind, RequestKind::Catalog("agents/list"));
}

// --- Task 8: the bespoke envelopes and the `:json` gate ------------

#[test]
fn a_getless_domain_opens_the_row_as_detail() {
    let models =
        DomainSpec::by_list_method("models/list").expect("the models domain");
    let mut catalog = Catalog::new();
    let requests = catalog.open(models);
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, RequestKind::Catalog("models/list"));
    assert_eq!(requests[0].params, json!({}));

    catalog.apply_response(
        "models/list",
        &Ok(json!({
            "models": [{
                "provider": "openai", "model": "gpt",
                "context_window": 128000,
            }],
            "default_provider": "openai",
            "default_model": "gpt",
        })),
    );
    assert_eq!(catalog.list.rows.len(), 1);

    // No `get` method: Enter flattens the row itself.
    let requests = catalog.key(KeyCode::Enter, KeyModifiers::NONE, 10, None);
    assert!(requests.is_empty());
    let detail = catalog.detail.as_ref().expect("the row's detail");
    assert_eq!(detail.id, "gpt");
    assert!(
        detail
            .rows
            .iter()
            .any(|(path, text)| path == "provider" && text == "openai")
    );
}

#[test]
fn the_search_helper_sends_the_query() {
    let mut catalog = Catalog::new();
    let requests = catalog.search("rust");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].kind, RequestKind::Catalog("search/all"));
    assert_eq!(requests[0].params, json!({ "q": "rust", "limit": 20 }));
    assert_eq!(
        catalog.domain.map(|spec| spec.list_method),
        Some("search/all")
    );

    // The prompt entry (`open_search`) sends nothing until Enter.
    let mut catalog = Catalog::new();
    assert!(catalog.open_search().is_empty());
    assert_eq!(catalog.query.as_deref(), Some(""));
}

#[test]
fn the_palette_lists_chat_ops_without_a_domain() {
    let catalog = Catalog::new();
    let items = catalog.palette_items();
    for item in ["regenerate", "feedback-up", "feedback-down", "search <词>"] {
        assert!(
            items.iter().any(|candidate| candidate == item),
            "missing {item}: {items:?}"
        );
    }
}

#[test]
fn chat_ops_send_the_registered_params_from_the_palette() {
    let material = ResumeMaterial {
        session_id: "s1".to_string(),
        last_seq: 3,
        pending: Vec::new(),
        request_snapshot_hash: String::new(),
        // The rated row's ordinal — the row's identity in the log.
        last_message_id: Some("7".to_string()),
    };
    let mut catalog = Catalog::new();
    catalog.open_palette();
    type_text(&mut catalog, "regenerate");
    let requests =
        catalog.key(KeyCode::Enter, KeyModifiers::NONE, 10, Some(&material));
    assert_eq!(requests[0].kind, RequestKind::Catalog("chat/regenerate"));
    assert_eq!(requests[0].params, json!({ "session_id": "s1" }));

    catalog.open_palette();
    type_text(&mut catalog, "feedback-up");
    let requests =
        catalog.key(KeyCode::Enter, KeyModifiers::NONE, 10, Some(&material));
    assert_eq!(requests[0].kind, RequestKind::Catalog("chat/feedback"));
    assert_eq!(
        requests[0].params,
        json!({ "message_id": "7", "thumbs_up": true, "session_id": "s1" })
    );
}
