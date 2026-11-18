//! The committed profile document must restate the plugin manifests.
//!
//! `.agents/profile.toml` is a project asset: the deployment's own
//! statement of the whole plugin tree. A plugin that gains (or loses) a
//! dependency edge must show up in that document — otherwise the
//! document drifts from what the kernel actually orders, and a reader
//! of the document sees a tree that does not exist.
//!
//! The check is the **effective** edge set (the factory manifest's
//! `inject` unioned with the row's declaration) against the document's
//! own `inject`: they must be equal. The document may legitimately add
//! an edge the plugin does not declare (that is the feature); it may
//! not miss one the plugin declares (that is drift).

use std::path::PathBuf;

use serde_json::Value;
use uva_app_server::{boot::dump_profile, profile::Profile};

/// The committed document, relative to the crate manifest.
fn document_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.agents/profile.toml")
}

#[test]
fn the_committed_document_restates_every_plugin_edge() {
    let path = document_path();
    let profile = Profile::from_path(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    let dump: Value = serde_json::from_str(&dump_profile(&profile))
        .expect("the dump is JSON");

    let dumped = dump
        .get("core")
        .and_then(Value::as_array)
        .expect("the dump has a core segment");
    let mut checked = 0usize;
    for row in dumped {
        let id = row
            .get("id")
            .and_then(Value::as_str)
            .expect("every dumped row has an id");
        let declared = profile
            .find(id)
            .unwrap_or_else(|| panic!("the document has no row `{id}`"))
            .inject
            .clone()
            .unwrap_or_default();
        let effective = row
            .get("inject")
            .and_then(Value::as_array)
            .expect("every dumped row has its effective edges")
            .iter()
            .map(|key| key.as_str().expect("an edge is a string").to_owned())
            .collect::<Vec<_>>();
        assert_eq!(
            effective, declared,
            "row `{id}`: the document's `inject` must restate the plugin \
             manifest's edges (the effective set differs)"
        );
        checked += 1;
    }
    assert_eq!(
        checked,
        profile.core.len(),
        "the document and the dump must name the same rows"
    );
}

#[test]
fn every_committed_row_names_a_plugin() {
    let path = document_path();
    let profile = Profile::from_path(&path)
        .unwrap_or_else(|error| panic!("{}: {error}", path.display()));
    for row in profile.all_rows() {
        assert!(
            !row.name.is_empty(),
            "row `{}` must name the plugin it loads",
            row.id
        );
    }
}
