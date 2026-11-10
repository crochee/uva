use serde_json::{Value, json};

use super::{
    DOMAINS,
    DomainSpec,
    EnterOp,
    KNOWN_METHODS,
    cell_text,
    decode_list,
};

fn sessions() -> &'static DomainSpec {
    DomainSpec::by_list_method("sessions/list").expect("the sessions domain")
}

#[test]
fn the_sessions_row_decodes_from_the_list_envelope() {
    let page = decode_list(
        &json!({
            "data": [
                { "id": "s1", "status": "idle",
                  "created_at": "2026-11-09T10:00:00Z", "agent_name": "agent" },
                { "id": "s2", "status": "running" },
            ],
            "next_cursor": "Y3Vyc29y",
            "total": 12,
        }),
        sessions(),
    );
    assert_eq!(page.rows.len(), 2);
    assert_eq!(page.next_cursor.as_deref(), Some("Y3Vyc29y"));
    assert_eq!(page.total, Some(12));
}

#[test]
fn id_keys_are_declared_per_op_not_per_domain() {
    let keys: Vec<_> = sessions().ops.iter().map(|op| op.id_key).collect();
    assert!(keys.contains(&"id"));
    assert!(keys.contains(&"session_id"));
    // The per-op split matches the server's params structs:
    // `IdParams` for get/status/export/fork, `SessionIdParams` for
    // usage/compact/resume, and `delete` is `IdParams` again.
    assert_eq!(
        sessions().op("用量").map(|op| op.id_key),
        Some("session_id")
    );
    assert_eq!(sessions().op("详情").map(|op| op.id_key), Some("id"));
    assert_eq!(sessions().op("删除").map(|op| op.id_key), Some("id"));
}

#[test]
fn missing_rows_pointer_yields_an_empty_page() {
    let page = decode_list(&json!({ "unexpected": [] }), sessions());
    assert!(page.rows.is_empty());
    assert!(page.next_cursor.is_none());
    assert!(page.total.is_none());
}

#[test]
fn list_params_carry_the_cursor_and_limit() {
    let spec = sessions();
    assert_eq!(spec.list_params(None, None), json!({ "limit": 50 }));
    assert_eq!(
        spec.list_params(Some("Y3Vyc29y"), None),
        json!({ "limit": 50, "cursor": "Y3Vyc29y" }),
    );
}

#[test]
fn get_params_use_the_declared_id_key() {
    assert_eq!(sessions().get_params("s1"), json!({ "id": "s1" }));
    let agents = DomainSpec::by_list_method("agents/list").expect("agents");
    assert_eq!(agents.get_params("agent"), json!({ "name": "agent" }));
}

#[test]
fn cell_text_renders_scalars_and_array_lengths() {
    let row = json!({
        "id": "s1",
        "score": 1.5,
        "protected": true,
        "cases": [{}, {}, {}],
        "nothing": null,
    });
    assert_eq!(cell_text(&row, "/id"), "s1");
    assert_eq!(cell_text(&row, "/score"), "1.5");
    assert_eq!(cell_text(&row, "/protected"), "true");
    assert_eq!(cell_text(&row, "/cases"), "3 项");
    assert_eq!(cell_text(&row, "/missing"), "");
    assert_eq!(cell_text(&row, "/nothing"), "");
}

#[test]
fn the_seven_catalog_domains_declare_the_registered_methods() {
    for method in [
        "agents/list",
        "skills/list",
        "tools/list",
        "tasks/list",
        "schedules/list",
        "workflows/list",
        "evals/list",
    ] {
        assert!(
            DomainSpec::by_list_method(method).is_some(),
            "missing {method}"
        );
    }
    // The id key is "name" where the server registers `NameParams`
    // and "id" elsewhere — for the get method and every op.
    for method in ["agents/list", "skills/list", "tools/list", "evals/list"] {
        let spec = DomainSpec::by_list_method(method).expect(method);
        assert_eq!(spec.get_id_key, "name", "{method} get");
        for op in spec.ops.iter().filter(|op| !op.id_key.is_empty()) {
            assert_eq!(op.id_key, "name", "{method} op {}", op.label);
        }
    }
    for method in ["tasks/list", "schedules/list", "workflows/list"] {
        let spec = DomainSpec::by_list_method(method).expect(method);
        assert_eq!(spec.get_id_key, "id", "{method} get");
        for op in spec.ops.iter().filter(|op| !op.id_key.is_empty()) {
            assert_eq!(op.id_key, "id", "{method} op {}", op.label);
        }
    }
    // `tasks` is read-only on the server.
    let tasks = DomainSpec::by_list_method("tasks/list").expect("tasks");
    assert!(tasks.ops.is_empty());
}

#[test]
fn every_declared_method_is_a_real_registered_name() {
    for spec in DOMAINS {
        assert!(
            KNOWN_METHODS.contains(&spec.list_method),
            "list method {} is not in the registry copy",
            spec.list_method
        );
        if let Some(get) = spec.get_method {
            assert!(
                KNOWN_METHODS.contains(&get),
                "get method {get} is not in the registry copy"
            );
        }
        for op in spec.ops {
            assert!(
                KNOWN_METHODS.contains(&op.method),
                "op method {} is not in the registry copy",
                op.method
            );
        }
    }
}

#[test]
fn non_list_envelopes_decode_through_rows_pointers() {
    let models = DomainSpec::by_list_method("models/list").expect("models");
    let page = decode_list(
        &json!({
            "models": [
                { "provider": "openai", "model": "gpt", "context_window": 128000 },
            ],
            "default_provider": "openai",
            "default_model": "gpt",
        }),
        models,
    );
    assert_eq!(page.rows.len(), 1);
    assert_eq!(cell_text(&page.rows[0], "/provider"), "openai");

    let plugins = DomainSpec::by_list_method("plugins/list").expect("plugins");
    let page = decode_list(
        &json!({
            "resolved": [{ "name": "tool-read", "kind": "static" }],
            "skipped": [],
            "bundles": [],
        }),
        plugins,
    );
    assert_eq!(page.rows.len(), 1);
    assert_eq!(cell_text(&page.rows[0], "/name"), "tool-read");

    let ui = DomainSpec::by_list_method("ui/schema").expect("ui");
    let page = decode_list(
        &json!({ "schemas": [{ "slot": "chat.sidebar", "kind": "note" }] }),
        ui,
    );
    assert_eq!(page.rows.len(), 1);
    assert_eq!(cell_text(&page.rows[0], "/slot"), "chat.sidebar");
}

#[test]
fn the_search_domain_requires_a_query_and_pages_at_the_server_default() {
    let search = DomainSpec::by_list_method("search/all").expect("search");
    assert_eq!(search.query_key, Some("q"));
    assert_eq!(
        search.list_params(None, Some("rust")),
        json!({ "q": "rust", "limit": 20 })
    );
    // Without a query the params would be rejected by the server
    // (`q` is required); the catalog collects it first.
    assert_eq!(search.list_params(None, None), json!({ "limit": 20 }));
}

#[test]
fn sessions_open_sessions_and_the_rest_open_details() {
    assert_eq!(sessions().enter, EnterOp::Follow);
    for spec in DOMAINS.iter().filter(|spec| spec.label != "会话") {
        assert_eq!(spec.enter, EnterOp::Detail, "{} opens details", spec.label);
    }
}

#[test]
fn labels_are_unique_and_lookup_by_label_works() {
    for spec in DOMAINS {
        assert_eq!(
            DomainSpec::by_label(spec.label).map(|found| found.list_method),
            Some(spec.list_method)
        );
    }
    assert!(DomainSpec::by_label("不存在").is_none());
    assert!(DomainSpec::by_list_method("nope/list").is_none());
}

/// The seven directory domains' real wire rows, one fixture per
/// domain, taken from the route structs
/// (`routes/{agents,skills,tool,tasks,schedules,workflows}.rs`,
/// `crates/elysper-eval`). Each row must decode through its spec and
/// fill every column the table declares.
#[test]
fn the_seven_domains_decode_their_real_wire_shapes() {
    let cases: [(&str, Value, &[&str]); 7] = [
        (
            "agents/list",
            json!({
                "data": [{
                    "name": "agent", "description": "default",
                    "kind": "builtin", "version": "1.0",
                    "instructions": "", "capabilities": [],
                    "tools": ["shell"], "handoffs": [], "protected": true,
                }],
                "next_cursor": null,
                "total": 1,
            }),
            &["agent", "builtin", "1.0"],
        ),
        (
            "skills/list",
            json!({
                "data": [{ "name": "review", "description": "review code" }],
                "next_cursor": null,
                "total": 1,
            }),
            &["review", "review code"],
        ),
        (
            "tools/list",
            json!({
                "data": [{ "name": "shell", "description": "run a command" }],
                "next_cursor": null,
                "total": 1,
            }),
            &["shell", "run a command"],
        ),
        (
            "tasks/list",
            json!({
                "data": [{
                    "id": "p:c", "parent_session_id": "p",
                    "child_session_id": "c", "parent_depth": 1,
                    "agent": "reviewer", "prompt": "look",
                    "started_at": "2026-11-09T10:00:00Z",
                    "finished_at": null, "status": "done",
                }],
                "next_cursor": null,
                "total": 1,
            }),
            &["p:c", "reviewer", "done", "2026-11-09T10:00:00Z"],
        ),
        (
            "schedules/list",
            json!({
                "data": [{
                    "id": "s1", "name": "nightly",
                    "kind": "cron", "status": "active",
                    "next_fire_at": "2026-11-10T00:00:00Z",
                }],
                "next_cursor": null,
                "total": 1,
            }),
            &["s1", "nightly", "active", "2026-11-10T00:00:00Z"],
        ),
        (
            "workflows/list",
            json!({
                "data": [{ "id": "w1", "step_count": 3, "phase_count": 2 }],
                "next_cursor": null,
                "total": 1,
            }),
            &["w1", "3", "2"],
        ),
        (
            "evals/list",
            json!({
                "data": [{
                    "name": "smoke",
                    "cases": [{ "name": "one" }, { "name": "two" }],
                }],
                "next_cursor": null,
                "total": 1,
            }),
            &["smoke", "2 项"],
        ),
    ];
    for (method, envelope, expected) in cases {
        let spec = DomainSpec::by_list_method(method).expect(method);
        let page = decode_list(&envelope, spec);
        assert_eq!(page.rows.len(), 1, "{method} decodes one row");
        let row = &page.rows[0];
        let cells: Vec<String> = spec
            .columns
            .iter()
            .map(|column| cell_text(row, column.pointer))
            .collect();
        assert_eq!(cells, expected, "{method} fills its columns");
    }
}
