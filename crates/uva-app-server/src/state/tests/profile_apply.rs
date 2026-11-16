//! The transactional profile write path: one outcome per state.

use std::{path::Path, sync::Arc};

use serde_json::json;

use crate::{
    api::v1::profile::{ProfileEnabled, ProfileSegment},
    profile::{
        ChangeState,
        EnableCheck,
        Enabled,
        ProfileAction,
        ProfileStore,
        RowEdit,
    },
    rpc::RpcRuntime,
    state::AppState,
};

async fn state(dir: &Path) -> AppState {
    let registry =
        uva::session::manager::SessionRegistry::new(dir.join("sessions"));
    AppState::for_test(registry, dir.to_path_buf()).await
}

fn store(doc: &Path) -> ProfileStore {
    ProfileStore {
        bundle: None,
        doc: doc.to_path_buf(),
        home: None,
        overlay: None,
    }
}

/// The read model: the settings page's data source. The rows are
/// typed, so a renamed field is a compile error rather than a wire
/// surprise.
#[tokio::test]
async fn the_rows_are_typed_and_findable_by_id() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    let rows = state.profile_rows();
    assert!(rows.iter().any(|row| row.id == "skills"));
    let row = state.profile_row("skills").expect("the skills row");
    assert_eq!(row.id, "skills");
    assert_eq!(row.segment, ProfileSegment::Core);
    assert_eq!(row.status, "mounted");
    assert!(
        state.profile_row("nobody").is_none(),
        "an unknown id has no row"
    );
}

/// A row the assembly skipped says why, and a delegated enablement
/// keeps the check the plugin answers.
#[tokio::test]
async fn a_skipped_row_reports_its_reason_and_its_check() {
    let dir = tempfile::tempdir().unwrap();
    let state = state(dir.path()).await;
    {
        let mut profile = state.assembly.raw_mut();
        let row = profile
            .core
            .iter_mut()
            .find(|row| row.id == "skills")
            .expect("the skills row");
        row.enabled = Enabled::Check(EnableCheck {
            service: "skills".to_owned(),
            check: "present".to_owned(),
            arg: json!("writing"),
        });
    }
    let row = state.profile_row("skills").expect("the skills row");
    assert!(
        row.status.starts_with("off: "),
        "a skipped row says why: {row:?}"
    );
    assert!(
        matches!(row.enabled, ProfileEnabled::Check(_)),
        "a delegated row keeps its check: {row:?}"
    );
}

#[tokio::test]
async fn applied_commits_the_document_and_broadcasts() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = state(dir.path()).await;
    let (runtime, mut receiver) = RpcRuntime::channel();
    state.rpc = Arc::new(runtime);
    let doc = dir.path().join("profile.toml");
    state.assembly.set_store(Some(store(&doc)));

    let outcome = state
        .profile_apply(
            "ui",
            ProfileAction::Edit(RowEdit::SetConfig {
                id: "skills".to_owned(),
                config: json!({ "marker": true }),
            }),
        )
        .await;
    assert_eq!(outcome.state, ChangeState::Applied, "{outcome:?}");
    let text = std::fs::read_to_string(&doc).expect("the document exists");
    assert!(text.contains("marker"), "{text}");
    assert_eq!(
        state
            .assembly
            .current()
            .find("skills")
            .expect("skills is mounted")
            .config,
        json!({ "marker": true })
    );
    let frame = receiver.recv().await.expect("a notification arrives");
    assert_eq!(frame["method"], "profile/changed");
    assert_eq!(frame["params"]["actor"], "ui");
    assert_eq!(frame["params"]["ids"], json!(["skills"]));
    assert_eq!(frame["params"]["state"], "applied");
    assert!(frame["params"].get("session_id").is_none());
}

/// A write that asks for the value a row already holds changes
/// nothing, so it must not send every subscriber back to re-read the
/// same rows. The receipt still says `applied` (the requested state
/// holds); only the broadcast is withheld.
#[tokio::test]
async fn a_no_op_write_does_not_broadcast() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = state(dir.path()).await;
    let (runtime, mut receiver) = RpcRuntime::channel();
    state.rpc = Arc::new(runtime);
    let doc = dir.path().join("profile.toml");
    state.assembly.set_store(Some(store(&doc)));

    let edit = ProfileAction::Edit(RowEdit::SetConfig {
        id: "skills".to_owned(),
        config: json!({ "marker": true }),
    });
    let first = state.profile_apply("ui", edit.clone()).await;
    assert_eq!(first.state, ChangeState::Applied, "{first:?}");
    let frame = receiver.recv().await.expect("the change broadcasts");

    // The same value again: the tree does not move.
    let second = state.profile_apply("ui", edit).await;
    assert_eq!(second.state, ChangeState::Applied, "{second:?}");
    assert!(receiver.try_recv().is_err(), "a no-op must not broadcast");
    let _ = frame;
}

#[tokio::test]
async fn failed_leaves_the_document_and_the_tree_unchanged() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = state(dir.path()).await;
    let doc = dir.path().join("profile.toml");
    state.assembly.set_store(Some(store(&doc)));
    let before = state.plugin_state("model-adapter");

    let outcome = state
        .profile_apply(
            "ui",
            ProfileAction::Edit(RowEdit::SetConfig {
                id: "model-adapter".to_owned(),
                // Missing `default_provider`: the factory refuses it.
                config: json!({ "workspace": {} }),
            }),
        )
        .await;
    assert_eq!(outcome.state, ChangeState::Failed, "{outcome:?}");
    assert!(
        !doc.exists(),
        "a failed transaction never creates the document"
    );
    assert_eq!(
        state.plugin_state("model-adapter"),
        before,
        "the mounted row is untouched"
    );
}

#[tokio::test]
async fn overridden_writes_nothing_when_a_higher_layer_owns_the_field() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = state(dir.path()).await;
    let home = dir.path().join("home.toml");
    std::fs::write(
        &home,
        "[[replace]]\nid = \"skills\"\nconfig = { marker = true }\n",
    )
    .unwrap();
    let doc = dir.path().join("profile.toml");
    let mut path = store(&doc);
    path.home = Some(home);
    state.assembly.set_store(Some(path));
    let before = state
        .assembly
        .current()
        .find("skills")
        .expect("skills is mounted")
        .config
        .clone();

    let outcome = state
        .profile_apply(
            "ui",
            ProfileAction::Edit(RowEdit::SetConfig {
                id: "skills".to_owned(),
                config: json!({ "marker": false }),
            }),
        )
        .await;
    assert_eq!(outcome.state, ChangeState::Overridden, "{outcome:?}");
    assert!(!doc.exists(), "an overridden request writes nothing at all");
    assert_eq!(
        state
            .assembly
            .current()
            .find("skills")
            .expect("skills is mounted")
            .config,
        before,
        "the live tree is untouched"
    );
}

#[tokio::test]
async fn a_changed_mcp_row_reports_restart_required() {
    let dir = tempfile::tempdir().unwrap();
    let mut state = state(dir.path()).await;
    let doc = dir.path().join("profile.toml");
    state.assembly.set_store(Some(store(&doc)));

    let outcome = state
        .profile_apply(
            "ui",
            ProfileAction::Edit(RowEdit::SetConfig {
                id: "mcp".to_owned(),
                config: json!({ "marker": true }),
            }),
        )
        .await;
    assert_eq!(outcome.state, ChangeState::RestartRequired, "{outcome:?}");
    assert!(
        outcome
            .reasons
            .iter()
            .any(|reason| reason.contains("restart")),
        "{outcome:?}"
    );
    assert!(doc.exists(), "the change is committed");
}
