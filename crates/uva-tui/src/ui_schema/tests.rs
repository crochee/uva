use serde_json::json;

use super::{Cardinality, UiSchemaResponse};

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

#[test]
fn the_wire_response_deserializes() {
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
    assert_eq!(tree.unplaced_slots(), vec!["debug.dump".to_owned()]);
}

#[test]
fn a_missing_slots_member_defaults_to_empty() {
    // An older server sends `schemas` only; the terminal still paints
    // the components it understands.
    let tree: UiSchemaResponse = serde_json::from_value(json!({
        "schemas": [{ "version": 1, "components": [
            { "slot": "chat.sidebar", "kind": "panel" },
        ] }],
    }))
    .expect("a slots-less response parses");
    assert!(tree.slots.is_empty());
    assert_eq!(tree.components_in("chat.sidebar").len(), 1);
    // A component without props answers null rather than failing.
    assert_eq!(tree.components_in("chat.sidebar")[0].props, json!(null));
}
