//! The enable bit has two faces, and a failed write changes nothing.
//!
//! Design §8 (刀 7) asks for two things this file pins:
//!
//! 1. **One row, two faces**: setting `enabled = false` withdraws the
//!    capability *and* removes its interface block — the same row, the
//!    same bit, two manifestations.
//! 2. **A failed write changes nothing**: the MCP write path that
//!    cannot mount reports the failure, leaves the document and the
//!    mounted tree exactly as they were, and does not announce a
//!    change (a subscriber must never re-read on a no-op).

use std::path::Path;

use serde_json::json;
use uva::session::manager::SessionRegistry;

use crate::{
    profile::{ChangeState, ProfileAction, ProfileStore, RowEdit},
    rpc::RpcRuntime,
    state::AppState,
};

async fn state(dir: &Path) -> AppState {
    let registry = SessionRegistry::new(dir.join("sessions"));
    let mut state = AppState::for_test(registry, dir.to_path_buf()).await;
    state.assembly.set_store(Some(ProfileStore {
        bundle: None,
        doc: dir.join("profile.toml"),
        home: None,
        overlay: None,
    }));
    state
}

/// A `ui-bundle` row with one slot and one component.
fn ui_row() -> crate::profile::Entry {
    crate::profile::Entry {
        id: "ui.demo".to_owned(),
        runtime: crate::profile::RuntimeKind::UiBundle,
        config: json!({
            "slots": [{ "id": "chat.sidebar" }],
            "components": [{ "kind": "panel", "slot": "chat.sidebar", "props": { "text": "demo" } }],
        }),
        capabilities: Vec::new(),
        enabled: crate::profile::Enabled::On,
    }
}

#[tokio::test]
async fn one_disabled_row_withdraws_the_capability_and_its_block() {
    let dir = tempfile::tempdir().expect("tempdir");
    let state = state(dir.path()).await;

    // The capability face: the skills service is mounted today.
    assert!(
        state
            .assembly
            .service::<uva::skill::SkillRegistry>(uva::skill::KEY_SKILLS)
            .is_some(),
        "the fixture mounts skills"
    );

    // The interface face: a ui row contributes a block to the tree.
    let mut next = state.current_profile();
    next.hmr = true;
    next.ui.push(ui_row());
    state.reload_profile(next).await.expect("the ui row mounts");
    assert!(
        !state.ui_tree().schemas().is_empty(),
        "the ui row contributes its block"
    );

    // The same bit, twice: the core row loses its service, the ui row
    // loses its block.
    // The profile already carries the ui row from the reload above —
    // flip that row, do not add a second one.
    let mut disabled = state.current_profile();
    disabled.hmr = true;
    let skills = disabled
        .core
        .iter_mut()
        .find(|entry| entry.id == "skills")
        .expect("the native set has skills");
    skills.enabled = crate::profile::Enabled::Off;
    let ui = disabled
        .ui
        .iter_mut()
        .find(|entry| entry.id == "ui.demo")
        .expect("the ui row is there");
    ui.enabled = crate::profile::Enabled::Off;

    state
        .reload_profile(disabled)
        .await
        .expect("the disabled rows reload");
    assert!(
        state
            .assembly
            .service::<uva::skill::SkillRegistry>(uva::skill::KEY_SKILLS)
            .is_none(),
        "the disabled row withdraws its capability"
    );
    assert!(
        state.ui_tree().schemas().is_empty(),
        "the disabled ui row withdraws its block"
    );
}

#[tokio::test]
async fn a_failed_mcp_write_changes_neither_the_document_nor_the_tree() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut state = state(dir.path()).await;
    let (runtime, mut receiver) = RpcRuntime::channel();
    state.rpc = std::sync::Arc::new(runtime);
    let doc = dir.path().join("profile.toml");

    // A good write first, so there is a document and a tree to protect.
    let good = state
        .profile_apply(
            "ui",
            ProfileAction::Edit(RowEdit::SetConfig {
                id: "skills".to_owned(),
                config: json!({ "marker": true }),
            }),
        )
        .await;
    assert_eq!(good.state, ChangeState::Applied, "{good:?}");
    let before = std::fs::read_to_string(&doc).expect("the document exists");
    let mounted_before = state.plugin_state("skills");
    // The good write announced itself; drain it, so the assertion
    // below is about the *failed* write only.
    let announced = receiver.recv().await.expect("the good write announces");
    assert_eq!(announced["method"], "profile/changed");

    // Now a write that cannot mount: the model's provider config is
    // structurally wrong.
    let broken = state
        .profile_apply(
            "ui",
            ProfileAction::Edit(RowEdit::SetConfig {
                id: "model-adapter".to_owned(),
                config: json!({ "workspace": { "providers": "not-an-object" } }),
            }),
        )
        .await;
    assert_eq!(broken.state, ChangeState::Failed, "{broken:?}");
    assert_eq!(
        std::fs::read_to_string(&doc).expect("the document still exists"),
        before,
        "a failed write leaves the document byte-identical"
    );
    assert_eq!(
        state.plugin_state("model-adapter"),
        mounted_before,
        "a failed write leaves the mounted tree alone"
    );
    // And nothing was announced: a subscriber must not re-read.
    assert!(
        receiver.try_recv().is_err(),
        "a failed write must not broadcast"
    );
}
