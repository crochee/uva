//! Tests for the transactional store's pure half.

use serde_json::json;

use crate::profile::{
    CapabilityDomain,
    Enabled,
    Entry,
    Profile,
    ProfileStore,
    RowEdit,
    RuntimeKind,
};

fn doc_row(id: &str) -> Entry {
    Entry {
        id: id.to_owned(),
        runtime: RuntimeKind::Native,
        config: json!({}),
        capabilities: Vec::new(),
        enabled: Enabled::On,
    }
}

fn doc() -> Profile {
    Profile {
        core: vec![doc_row("skills"), doc_row("eval")],
        ui: Vec::new(),
        hmr: true,
    }
}

fn store(dir: &std::path::Path) -> ProfileStore {
    ProfileStore {
        bundle: None,
        doc: dir.join("profile.toml"),
        home: None,
        overlay: None,
    }
}

#[test]
fn a_missing_document_materializes_the_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let store = store(dir.path());
    let fallback = doc();
    let loaded = store.load_doc(&fallback).expect("load");
    assert_eq!(loaded, fallback, "a missing file is the fallback");

    let mut edited = loaded;
    ProfileStore::apply_edit(
        &mut edited,
        &RowEdit::SetEnabled {
            id: "skills".to_owned(),
            enabled: Enabled::Off,
        },
    )
    .expect("edit");
    assert_eq!(edited.core[0].enabled, Enabled::Off);
}

#[test]
fn an_enabled_edit_round_trips_through_toml() {
    let dir = tempfile::tempdir().unwrap();
    let _store = store(dir.path());
    let mut document = doc();
    ProfileStore::apply_edit(
        &mut document,
        &RowEdit::SetEnabled {
            id: "skills".to_owned(),
            enabled: Enabled::Off,
        },
    )
    .expect("edit");
    ProfileStore::apply_edit(
        &mut document,
        &RowEdit::SetConfig {
            id: "eval".to_owned(),
            config: json!({ "limit": 3, "nested": { "a": true } }),
        },
    )
    .expect("edit");

    let text = ProfileStore::to_toml(&document).expect("serialize");
    let parsed = Profile::from_toml_str(&text).expect("the document parses");
    assert_eq!(parsed.core[0].enabled, Enabled::Off);
    assert_eq!(
        parsed.core[1].config,
        json!({ "limit": 3, "nested": { "a": true } })
    );
    assert!(parsed.hmr);
}

#[test]
fn a_home_patch_overrides_the_document_edit() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home.toml");
    std::fs::write(
        &home,
        "[[replace]]\nid = \"skills\"\nconfig = { marker = true }\n",
    )
    .unwrap();
    let store = ProfileStore {
        bundle: None,
        doc: dir.path().join("profile.toml"),
        home: Some(home),
        overlay: None,
    };
    let mut document = doc();
    let edit = RowEdit::SetConfig {
        id: "skills".to_owned(),
        config: json!({ "marker": false }),
    };
    ProfileStore::apply_edit(&mut document, &edit).expect("edit");
    let resolved = store.resolve_with(&document).expect("resolve");
    assert!(
        !edit.is_satisfied_by(&resolved),
        "the home layer owns the field, so the requested value is not what resolves"
    );
}

#[test]
fn serialization_refuses_a_value_toml_cannot_express() {
    let mut document = doc();
    document.core[0].config = json!({ "items": [1, null] });
    let error = ProfileStore::to_toml(&document)
        .expect_err("a null array member must not serialize");
    assert!(error.contains("null"), "{error}");

    document.core[0].config = json!({ "items": [1, "two"] });
    let error = ProfileStore::to_toml(&document)
        .expect_err("a mixed array must not serialize");
    assert!(error.contains("mixed"), "{error}");
}

#[test]
fn the_domain_wire_names_are_stable() {
    assert_eq!(
        serde_json::to_value(CapabilityDomain::Agent).unwrap(),
        json!("agent")
    );
    assert_eq!(
        serde_json::to_value(CapabilityDomain::Skill).unwrap(),
        json!("skill")
    );
}
