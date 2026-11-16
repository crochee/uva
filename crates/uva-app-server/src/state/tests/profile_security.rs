//! The capability write path must not escape the workspace.
//!
//! A capability name becomes a directory under `.agents/agents/` (or
//! `.agents/skills/`), and the delete path removes that directory
//! recursively. A name carrying a path separator — or an absolute
//! path — would write or delete outside the workspace, so both entry
//! points refuse it before touching the disk.

use std::path::Path;

use serde_json::json;

use crate::{
    profile::{ChangeState, ProfileAction, ProfileStore},
    state::AppState,
};

async fn state(dir: &Path) -> AppState {
    let registry =
        uva::session::manager::SessionRegistry::new(dir.join("sessions"));
    let mut state = AppState::for_test(registry, dir.to_path_buf()).await;
    // The capability path needs a profile document configured — without
    // one it refuses before it ever reaches the file system, and the
    // test would pass without exercising anything.
    state.assembly.set_store(Some(ProfileStore {
        bundle: None,
        doc: dir.join("profile.toml"),
        home: None,
        overlay: None,
    }));
    state
}

/// Names that must never reach the file system.
const TRAVERSALS: &[&str] = &[
    "../../escape",
    "../escape",
    "nested/name",
    "..",
    ".",
    "/absolute",
];

#[tokio::test]
async fn creating_an_agent_refuses_a_traversing_name() {
    for name in TRAVERSALS {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path()).await;
        let outcome = state
            .profile_apply(
                "mcp",
                ProfileAction::Capability(
                    crate::profile::CapabilityAction::Create {
                        domain: crate::profile::CapabilityDomain::Agent,
                        definition: json!({
                            "name": name,
                            "description": "a definition that must not be written",
                        }),
                    },
                ),
            )
            .await;
        assert_eq!(
            outcome.state,
            ChangeState::Failed,
            "the name `{name}` must be refused: {outcome:?}"
        );
        // Nothing landed on disk: not inside `.agents/agents/`, and
        // not one level up either — `.agents/agents/../../escape`
        // resolves to `<workspace>/escape`.
        let agents = dir.path().join(".agents").join("agents");
        assert!(
            !agents.join(name).exists(),
            "`{name}` must not exist on disk"
        );
        let escaped = dir.path().join("escape");
        assert!(
            !escaped.exists(),
            "`{name}` must not escape into the workspace root"
        );
        let outside = dir.path().parent().map(|parent| parent.join("escape"));
        assert!(
            !outside.as_deref().is_some_and(Path::exists),
            "`{name}` must not escape the workspace"
        );
    }
}

#[tokio::test]
async fn deleting_an_agent_refuses_a_traversing_name() {
    for name in TRAVERSALS {
        let dir = tempfile::tempdir().unwrap();
        let state = state(dir.path()).await;
        // A directory that *could* be removed if the name were joined
        // blindly.
        let victim = dir.path().join("victim");
        std::fs::create_dir_all(&victim).unwrap();
        std::fs::write(victim.join("agent.md"), "keep me").unwrap();

        let outcome = state
            .profile_apply(
                "mcp",
                ProfileAction::Capability(
                    crate::profile::CapabilityAction::Delete {
                        domain: crate::profile::CapabilityDomain::Agent,
                        id: name.to_string(),
                    },
                ),
            )
            .await;
        assert_eq!(
            outcome.state,
            ChangeState::Failed,
            "the id `{name}` must be refused: {outcome:?}"
        );
        assert!(
            victim.join("agent.md").exists(),
            "`{name}` must not delete anything"
        );
    }
}
