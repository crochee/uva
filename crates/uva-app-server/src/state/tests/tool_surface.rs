//! The tools the deployment mounts: the registry is the agent's
//! surface, so a tool that is not in it is a tool the model cannot
//! call.

use crate::state::AppState;

/// The deployment mounts the file tools — including `edit`, the
/// in-place editor: a one-line change must not need a whole-file
/// rewrite.
#[tokio::test]
async fn the_deployment_mounts_the_file_tools() {
    let dir = tempfile::tempdir().expect("a temp workspace");
    let manager = uva::session::manager::SessionRegistry::new(
        dir.path().join("sessions"),
    );
    let state = AppState::for_test(manager, dir.path().to_path_buf()).await;

    for tool in ["read", "write", "edit", "job"] {
        assert!(
            state.tool_registry.contains(tool),
            "the deployment mounts `{tool}`"
        );
    }
}
