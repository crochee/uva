use serde_json::json;
use uva_app_server::ui::tree::Cardinality;

use super::{UiSchemaResponse, unplaced_slots};

fn response() -> serde_json::Value {
    json!({
        "slots": [
            { "id": "chat.sidebar", "cardinality": "many", "owner": "ui.shell" },
            { "id": "session.detail.extensions", "cardinality": "one", "owner": "ui.shell" },
            { "id": "debug.dump", "cardinality": "many", "owner": "ui.shell" },
        ],
        "schemas": [
            { "version": 1, "components": [] },
            { "version": 2, "components": [
                { "slot": "chat.sidebar", "kind": "panel", "props": { "title": "Notes" } },
                { "slot": "chat.sidebar", "kind": "weather-card", "props": { "city": "Berlin" } },
                { "slot": "session.detail.extensions", "kind": "key-value",
                  "props": { "title": "Facts", "entries": { "build": "knife8" } } },
                { "slot": "debug.dump", "kind": "list",
                  "props": { "title": "Dump", "items": ["one", "two"] } },
            ] },
        ],
    })
}

/// The answer is the server's own type: the terminal decodes it in
/// place, so the shape it reads is the shape the server publishes.
#[test]
fn the_wire_response_decodes_into_the_servers_type() {
    let tree: UiSchemaResponse =
        serde_json::from_value(response()).expect("the wire shape parses");
    assert_eq!(tree.slots.len(), 3);
    assert_eq!(tree.slots[1].cardinality, Cardinality::One);
    assert_eq!(tree.slots[1].id, "session.detail.extensions");
    assert_eq!(tree.schemas.len(), 2);
    // A row without components keeps its empty schema.
    assert_eq!(tree.schemas[0].version, 1);
    assert!(tree.schemas[0].components.is_empty());
    let kinds: Vec<&str> = tree.schemas[1]
        .components
        .iter()
        .map(|component| component.kind.as_str())
        .collect();
    assert_eq!(kinds, ["panel", "weather-card", "key-value", "list"]);
}

#[test]
fn components_in_returns_registration_order() {
    let tree: UiSchemaResponse =
        serde_json::from_value(response()).expect("the wire shape parses");
    let kinds: Vec<&str> = tree
        .components_in("chat.sidebar")
        .iter()
        .map(|component| component.kind.as_str())
        .collect();
    assert_eq!(kinds, ["panel", "weather-card"]);
    assert!(tree.components_in("no.such.slot").is_empty());
}

#[test]
fn unplaced_slots_names_only_unplaced() {
    let tree: UiSchemaResponse =
        serde_json::from_value(response()).expect("the wire shape parses");
    assert_eq!(unplaced_slots(&tree), vec!["debug.dump".to_owned()]);
}

/// The answer is the server's type, so it is the server's shape that
/// counts: an answer missing a member is not an answer this build can
/// read, and it says so instead of painting half a tree.
#[test]
fn an_answer_missing_a_member_is_not_readable() {
    let wire = json!({
        "schemas": [{ "version": 1, "components": [
            { "slot": "chat.sidebar", "kind": "panel", "props": null },
        ] }],
    });
    let error = serde_json::from_value::<UiSchemaResponse>(wire)
        .expect_err("the server's shape wants every member");
    assert!(
        error.to_string().contains("slots"),
        "the error names the missing member: {error}"
    );
}
