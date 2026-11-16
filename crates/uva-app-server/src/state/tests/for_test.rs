//! Tests for `AppState::for_test`'s tool registry wiring —
//! pin the contract that the test surface mirrors the
//! production wiring (the `skill` tool lands on the
//! default registry).

use uva::session::manager::SessionRegistry;

use crate::state::AppState;

/// `for_test`'s tool registry MUST include the agent-facing
/// `skill` tool so the test surface mirrors the production
/// wiring. If the registration ever silently regresses
/// (e.g. the call gets moved off the `plugin_tool_registry`
/// path), this test fails loudly instead of letting the LLM
/// silently lose access to skill workflows during
/// route-level testing.
#[tokio::test]
async fn for_test_tool_registry_includes_skill_tool() {
    let dir = tempfile::tempdir().unwrap();
    let session_registry = SessionRegistry::new(dir.path().join("sessions"));
    let state =
        AppState::for_test(session_registry, dir.path().to_path_buf()).await;
    let reg = state.tool_registry.as_ref();
    let entry = reg
        .get("skill")
        .expect("skill tool must be registered in the default registry");
    assert_eq!(entry.tool_instance().name(), "skill");
}

/// The capability services are resolvable through the mounted kernel,
/// and unloading a plugin withdraws exactly its service (and the tool
/// it token-registered), leaving its siblings mounted.
#[tokio::test]
async fn for_test_mounts_the_native_services_and_unload_is_scoped() {
    let dir = tempfile::tempdir().unwrap();
    let session_registry = SessionRegistry::new(dir.path().join("sessions"));
    let state =
        AppState::for_test(session_registry, dir.path().to_path_buf()).await;

    assert!(
        state
            .assembly
            .service::<uva::skill::SkillRegistry>(uva::skill::KEY_SKILLS)
            .is_some(),
        "the fixture mounts the native plugin set"
    );
    assert!(state.attachments().is_some());
    assert!(state.agent_loop().is_some());

    assert!(state.unload_plugin("skills").await);
    assert!(state.attachments().is_some(), "siblings stay mounted");
    assert!(
        !state.tool_registry.contains("skill"),
        "the token-scoped registration is withdrawn"
    );
    assert!(
        state
            .assembly
            .service::<uva::skill::SkillRegistry>(uva::skill::KEY_SKILLS)
            .is_none(),
        "the service is withdrawn"
    );

    assert!(state.unload_plugin("attachments").await);
    assert!(
        state.attachments().is_none(),
        "the multimodal gate closes with the service"
    );

    assert!(state.unload_plugin("mcp").await);
    assert!(state.mcp.supervisor().is_none());
    assert!(!state.mcp.is_ticking());
}
