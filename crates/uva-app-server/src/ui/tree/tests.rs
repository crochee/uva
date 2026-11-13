use serde_json::json;
use uva::kernel::RuntimeKind;

use super::{
    Cardinality,
    SlotDecl,
    UiComponent,
    UiError,
    UiTree,
    load_ui_tree,
};
use crate::profile::Entry;

fn row(id: &str, config: serde_json::Value) -> Entry {
    Entry {
        id: id.to_owned(),
        runtime: RuntimeKind::UiBundle,
        config,
        capabilities: Vec::new(),
        enabled: crate::profile::Enabled::On,
    }
}

fn shell() -> Entry {
    row(
        "ui.shell",
        json!({ "version": 1, "slots": [
            { "id": "chat.sidebar", "cardinality": "many" },
            { "id": "session.detail.extensions", "cardinality": "many" },
        ] }),
    )
}

fn notes(version: u32, title: &str) -> Entry {
    row(
        "ui.slot.notes",
        json!({ "version": version, "components": [
            { "slot": "chat.sidebar", "kind": "panel", "props": { "title": title } },
        ] }),
    )
}

fn panel(title: &str) -> UiComponent {
    UiComponent {
        slot: "chat.sidebar".to_owned(),
        kind: "panel".to_owned(),
        props: json!({ "title": title }),
    }
}

#[test]
fn ui_loader_rejects_core_key() {
    let entries = vec![row(
        "tools",
        json!({ "components": [{ "slot": "chat.sidebar", "kind": "panel" }] }),
    )];
    let error = load_ui_tree(&entries)
        .expect_err("a core service key is not a UI entry");
    assert!(
        matches!(error, UiError::NamespaceViolation { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn unknown_schema_component_falls_back() {
    // The loader keeps an unknown kind: the *renderer* owns the
    // fallback, so a schema a newer UI plugin ships still reaches
    // the client instead of failing the whole `[ui]` segment.
    let entries = vec![
        shell(),
        row(
            "ui.slot.chat.sidebar",
            json!({
                "version": 2,
                "components": [{
                    "slot": "chat.sidebar",
                    "kind": "not-yet-known",
                    "props": { "title": "Hello" },
                }],
            }),
        ),
    ];
    let tree = load_ui_tree(&entries).expect("the schema loads");
    let schemas = tree.schemas();
    assert_eq!(schemas.len(), 2);
    assert_eq!(schemas[1].version, 2);
    assert_eq!(schemas[1].components[0].slot, "chat.sidebar");
    assert_eq!(schemas[1].components[0].kind, "not-yet-known");
    assert_eq!(schemas[1].components[0].props, json!({ "title": "Hello" }));
}

#[test]
fn ui_loader_rejects_a_non_ui_runtime() {
    let mut entry = row(
        "ui.slot.chat.sidebar",
        json!({ "components": [{ "slot": "chat.sidebar", "kind": "panel" }] }),
    );
    entry.runtime = RuntimeKind::Native;
    let error = load_ui_tree(&[entry])
        .expect_err("the UI loader owns `ui-bundle` rows only");
    assert!(
        matches!(error, UiError::WrongRuntime { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn ui_loader_rejects_an_executable_payload() {
    let entries = vec![row(
        "ui.slot.chat.sidebar",
        json!({
            "components": [],
            "script": "https://example.invalid/panel.js",
        }),
    )];
    let error =
        load_ui_tree(&entries).expect_err("v1 never executes remote JS/CSS");
    assert!(
        matches!(error, UiError::ExecutablePayload { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn an_empty_config_is_an_empty_schema() {
    let entries = vec![row("ui.slot.chat.sidebar", json!({}))];
    let tree = load_ui_tree(&entries).expect("loads");
    assert_eq!(
        tree.schemas(),
        vec![super::UiSchema {
            version: 1,
            components: Vec::new(),
        }]
    );
    assert!(tree.slots().is_empty());
}

#[test]
fn a_row_declares_slots_and_a_later_row_registers_into_them() {
    let tree = load_ui_tree(&[shell(), notes(1, "Notes")]).expect("loads");
    let slots: Vec<String> =
        tree.slots().into_iter().map(|slot| slot.id).collect();
    assert_eq!(
        slots,
        ["chat.sidebar", "session.detail.extensions"],
        "declaration order"
    );
    assert_eq!(tree.slots()[0].owner, "ui.shell");
    assert_eq!(tree.components_in("chat.sidebar"), vec![panel("Notes")]);
    assert!(tree.components_in("session.detail.extensions").is_empty());
    let schemas = tree.schemas();
    assert_eq!(schemas.len(), 2, "one schema per row");
    assert_eq!(schemas[1].version, 1);
    assert_eq!(schemas[1].components, vec![panel("Notes")]);
}

#[test]
fn a_slot_declaration_defaults_to_many() {
    let entries = vec![
        row("ui.shell", json!({ "slots": [{ "id": "chat.sidebar" }] })),
        notes(1, "Notes"),
    ];
    let tree = load_ui_tree(&entries).expect("loads");
    assert_eq!(tree.slots()[0].cardinality, Cardinality::Many);
    assert_eq!(tree.components_in("chat.sidebar").len(), 1);
}

#[test]
fn registering_into_an_undeclared_slot_fails_activation() {
    let error = load_ui_tree(&[notes(1, "Notes")])
        .expect_err("nothing declares chat.sidebar");
    assert!(
        matches!(
            &error,
            UiError::UnknownSlot { id, slot }
                if id == "ui.slot.notes" && slot == "chat.sidebar"
        ),
        "unexpected error: {error}"
    );
    assert!(error.to_string().contains("which no row declares"));
}

#[test]
fn declaring_an_occupied_slot_fails_activation() {
    let second = row(
        "ui.shell.again",
        json!({ "slots": [{ "id": "chat.sidebar" }] }),
    );
    let error =
        load_ui_tree(&[shell(), second]).expect_err("the slot is taken");
    assert!(
        matches!(
            &error,
            UiError::SlotOccupied { id, slot, owner }
                if id == "ui.shell.again"
                    && slot == "chat.sidebar"
                    && owner == "ui.shell"
        ),
        "unexpected error: {error}"
    );
    assert!(error.to_string().contains("already declared by"));
}

#[test]
fn a_one_cardinality_slot_rejects_a_second_registration() {
    let one = row(
        "ui.shell",
        json!({ "slots": [{ "id": "chat.sidebar", "cardinality": "one" }] }),
    );
    let error = load_ui_tree(&[one, notes(1, "first"), notes(1, "second")])
        .expect_err("a `one` slot holds one component");
    assert!(
        matches!(
            &error,
            UiError::CardinalityExceeded { id, slot, owner }
                if id == "ui.slot.notes"
                    && slot == "chat.sidebar"
                    && owner == "ui.slot.notes"
        ),
        "unexpected error: {error}"
    );
    assert!(
        error
            .to_string()
            .contains("already holding a component from")
    );
}

#[test]
fn dropping_a_component_handle_rolls_the_registration_back() {
    let tree = UiTree::new();
    let slot = tree
        .declare(
            "ui.shell",
            SlotDecl {
                id: "chat.sidebar".to_owned(),
                cardinality: Cardinality::Many,
            },
        )
        .expect("the slot is free");
    let handle = tree
        .register("ui.slot.notes", 1, panel("Notes"))
        .expect("the slot is declared");
    assert_eq!(tree.components_in("chat.sidebar").len(), 1);
    drop(handle);
    assert!(
        tree.components_in("chat.sidebar").is_empty(),
        "dropping the handle withdraws the registration"
    );
    drop(slot);
    assert!(tree.slots().is_empty());
}

#[test]
fn dropping_a_slot_handle_rolls_the_declaration_and_its_registrations_back() {
    let tree = UiTree::new();
    let slot = tree
        .declare(
            "ui.shell",
            SlotDecl {
                id: "chat.sidebar".to_owned(),
                cardinality: Cardinality::Many,
            },
        )
        .expect("the slot is free");
    let _component = tree
        .register("ui.slot.notes", 1, panel("Notes"))
        .expect("the slot is declared");
    drop(slot);
    assert!(tree.slots().is_empty());
    assert!(tree.components_in("chat.sidebar").is_empty());
}

#[test]
fn a_cloned_tree_shares_the_table() {
    let tree = load_ui_tree(&[shell()]).expect("loads");
    let clone = tree.clone();
    let handle = clone
        .register("ui.slot.notes", 1, panel("Notes"))
        .expect("the clone sees the declaration");
    assert_eq!(tree.components_in("chat.sidebar").len(), 1);
    // The registration stays while the cloned tree holds its handle.
    drop(tree);
    assert_eq!(clone.components_in("chat.sidebar").len(), 1);
    drop(handle);
    assert!(clone.components_in("chat.sidebar").is_empty());
}

#[test]
fn executable_fields_are_rejected_in_rows_slots_and_components() {
    let in_slot = load_ui_tree(&[row(
        "ui.shell",
        json!({ "slots": [{ "id": "chat.sidebar", "script": "x.js" }] }),
    )])
    .expect_err("a slot declaration may not name a script");
    assert!(
        matches!(in_slot, UiError::ExecutablePayload { .. }),
        "unexpected error: {in_slot}"
    );

    let in_component = load_ui_tree(&[
        shell(),
        row(
            "ui.slot.notes",
            json!({ "components": [{
                "slot": "chat.sidebar",
                "kind": "panel",
                "css": "body { color: red }",
            }] }),
        ),
    ])
    .expect_err("a component may not name a stylesheet");
    assert!(
        matches!(in_component, UiError::ExecutablePayload { .. }),
        "unexpected error: {in_component}"
    );
}

#[test]
fn every_row_keeps_its_schema_even_without_components() {
    let bare = row("ui.slot.bare", serde_json::Value::Null);
    let versioned = row("ui.slot.versioned", json!({ "version": 3 }));
    let tree = load_ui_tree(&[bare, versioned]).expect("loads");
    let schemas = tree.schemas();
    assert_eq!(schemas.len(), 2);
    assert_eq!(schemas[0].version, 1);
    assert!(schemas[0].components.is_empty());
    assert_eq!(schemas[1].version, 3);
    assert!(schemas[1].components.is_empty());
}

#[test]
fn schemas_are_grouped_by_row_in_assembly_order() {
    let facts = row(
        "ui.slot.facts",
        json!({ "version": 4, "components": [{
            "slot": "session.detail.extensions",
            "kind": "key-value",
            "props": { "title": "Facts" },
        }] }),
    );
    let tree =
        load_ui_tree(&[shell(), notes(1, "Notes"), facts]).expect("loads");
    let schemas = tree.schemas();
    assert_eq!(schemas.len(), 3);
    assert_eq!(schemas[0].version, 1, "the declaring row keeps its schema");
    assert!(schemas[0].components.is_empty());
    assert_eq!(schemas[1].components, vec![panel("Notes")]);
    assert_eq!(schemas[2].version, 4);
    assert_eq!(schemas[2].components[0].slot, "session.detail.extensions");
}
