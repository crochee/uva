//! The named resolution policies: `must_exist` (the registry, the
//! transcript directory, or a refusal that never mints a session)
//! and `create_on_demand` (an unknown id is minted — directory and
//! registry entry, pinned as the policy's own contract), plus the
//! source gate keeping the sessions route from owning a transcript
//! answer production code may borrow.

use std::sync::Arc;

use crate::state::{AppState, MustExistError};

/// A state over a fresh workspace, for the policy's witness quadrants.
async fn resolution_state() -> (Arc<AppState>, tempfile::TempDir) {
    let dir = tempfile::TempDir::new().expect("a temp workspace");
    let manager =
        uva::session::manager::SessionRegistry::new(dir.path().to_path_buf());
    let state = AppState::for_test(manager, dir.path().to_path_buf()).await;
    (Arc::new(state), dir)
}

/// The registry alone passes the policy: a session this process
/// registered resolves without consulting the disk.
#[tokio::test]
async fn the_registry_alone_passes_the_must_exist_policy() {
    let (state, dir) = resolution_state().await;
    state
        .session_manager
        .create_with_user("s".to_string(), "u".to_string())
        .await
        .expect("the session is registered");
    let session_dir = dir.path().join("u").join("s");
    std::fs::remove_dir_all(&session_dir)
        .expect("the directory leaves the disk");
    let controller = state
        .must_exist_session_controller("u", "s")
        .await
        .expect("the registry entry alone proves the session exists");
    assert!(!controller.is_remote());
}

/// The transcript directory alone passes the policy: a process that
/// restarted holds no registry entry, and the durable directory is
/// the proof — restoring the session instead of refusing it.
#[tokio::test]
async fn the_transcript_directory_alone_passes_the_policy() {
    let (state, dir) = resolution_state().await;
    let session_dir = dir.path().join("u").join("restarted");
    std::fs::create_dir_all(&session_dir).expect("the session directory");
    let controller = state
        .must_exist_session_controller("u", "restarted")
        .await
        .expect("the directory is durable proof the session exists");
    assert!(!controller.is_remote());
    assert!(
        state.session_manager.get("restarted").await.is_some(),
        "a passed proof restores the registry entry"
    );
}

/// Neither witness knows the session: the policy refuses it, and the
/// refusal must not mint the directory a typo asked for.
#[tokio::test]
async fn a_session_no_witness_knows_is_refused_without_minting_it() {
    let (state, dir) = resolution_state().await;
    let error = match state.must_exist_session_controller("u", "typo").await {
        Ok(_) => panic!("a session no witness knows is refused"),
        Err(error) => error,
    };
    match error {
        MustExistError::Absent { session_id } => {
            assert_eq!(session_id, "typo");
        }
        MustExistError::Open(error) => panic!(
            "the refusal is the absence itself, not an open failure: {error}"
        ),
    }
    assert!(
        !dir.path().join("u").join("typo").exists(),
        "a refused id must not leave a minted directory behind"
    );
}

/// An id that cannot address a single directory entry (`..`, a
/// separator) has no path that could prove it, so the policy refuses
/// it like any other unknown session.
#[tokio::test]
async fn an_id_that_cannot_address_a_directory_is_refused() {
    let (state, _dir) = resolution_state().await;
    let error =
        match state.must_exist_session_controller("u", "../escape").await {
            Ok(_) => {
                panic!("an id that cannot address a directory is refused")
            }
            Err(error) => error,
        };
    assert!(
        matches!(error, MustExistError::Absent { .. }),
        "the refusal is an absence, not an open failure: {error:?}"
    );
}

/// The `create_on_demand` policy mints the session an unknown id
/// asked for. Both layers of the side effect are pinned — the
/// directory on disk **and** the registry entry — because the chat
/// surface relies on creation being this policy's contract, not an
/// accident of the resolve path.
#[tokio::test]
async fn the_create_on_demand_policy_mints_the_session_an_unknown_id_names() {
    let (state, dir) = resolution_state().await;
    let controller = state
        .create_on_demand_session_controller("u", "fresh")
        .await
        .expect("the policy opens the session it is asked to use");
    assert!(!controller.is_remote());
    assert!(
        dir.path().join("u").join("fresh").is_dir(),
        "the session's directory is minted on disk"
    );
    assert!(
        state.session_manager.get("fresh").await.is_some(),
        "the registry holds the session from now on"
    );
}

/// The transcript answers production code may ask have one exit:
/// the session-resolution module. Production code must not borrow
/// a path answer or a transcript read from the sessions route —
/// the scan keeps a second route-owned answer from growing back.
///
/// The scan pins what it can name — the module's functions and the
/// sessions-route borrows — not every byte of layout knowledge:
/// `session_index`'s bare `join` over the directory it is handed
/// predates this module and stays outside the gate.
#[test]
fn production_code_borrows_no_transcript_answer_from_the_sessions_route() {
    let src = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut scanned = 0usize;
    let mut offenders = Vec::new();
    scan_production_sources(&src, &mut |path| {
        scanned += 1;
        let text = std::fs::read_to_string(path).expect("readable source");
        for borrowed in [
            "routes::sessions::transcript_path",
            "routes::sessions::is_path_component",
            "routes::sessions::transcript_events",
            "routes::sessions::transcript_lines",
        ] {
            if text.contains(borrowed) {
                offenders.push(format!(
                    "{} borrows `{borrowed}` from the sessions route",
                    path.display()
                ));
            }
        }
        let one_owner = path.ends_with("state/app_state/session_resolution.rs");
        if !one_owner
            && (text.contains("fn transcript_path(")
                || text.contains("fn is_path_component(")
                || text.contains("fn user_sessions_dir(")
                || text.contains("fn transcript_events(")
                || text.contains("fn transcript_lines("))
        {
            offenders.push(format!(
                "{} defines transcript knowledge outside its \
                 one owner",
                path.display()
            ));
        }
    });
    assert!(scanned > 0, "the scan must reach the source tree");
    assert!(
        offenders.is_empty(),
        "the disk layout needs one owner: {offenders:#?}"
    );
}

/// Every production source file under `dir`, skipping the test
/// suites (`tests/` directories and `tests.rs` files).
fn scan_production_sources(
    dir: &std::path::Path,
    visit: &mut impl FnMut(&std::path::Path),
) {
    let entries = std::fs::read_dir(dir).expect("a readable source tree");
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir() {
            if name != "tests" {
                scan_production_sources(&path, visit);
            }
        } else if name.ends_with(".rs") && !name.ends_with("tests.rs") {
            visit(&path);
        }
    }
}
