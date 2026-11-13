//! Tests for the self-management MCP server the boot wires:
//! the deployment's own management surface, served to its agents
//! over MCP (see `state::app_state::self_mcp`).

use std::sync::Arc;

use uva::session::manager::SessionRegistry;

use crate::state::{AppState, app_state::self_mcp};

/// The boot publishes one `mcp__self__<domain>` tool per
/// management domain, with `Deferred` exposure (the same
/// cold-start policy every `mcp__*` tool gets) — and each one is
/// a working MCP client call away from the real `AppState`.
#[tokio::test]
async fn boot_registers_self_management_tools() {
    let dir = tempfile::tempdir().expect("tempdir");
    // Use the `test-utils` constructor: `FakeProvider` sidesteps the
    // requirement that a real LLM provider be configured. The
    // production `with_server_config` path (used by `main.rs`) is
    // gated on `OPENAI_API_KEY` / `ANTHROPIC_API_KEY` being set, which
    // would couple this test to the operator's shell — wrong contract
    // for an in-tree test that asserts the *boot's wiring*, not the
    // LLM resolution.
    let session_manager = SessionRegistry::new(dir.path().join("sessions"));
    let state = Arc::new(
        AppState::for_test(session_manager, dir.path().to_path_buf()).await,
    );
    // `with_server_config` calls `register_self_mcp` for the
    // production boot; the test-utils constructor stops at the
    // shared wiring (registry + provider + skill tool) so the
    // management surface stays under explicit test control.
    self_mcp::register_self_mcp(&state).await;

    let registry = state.tool_registry.as_ref();
    let self_tools: Vec<String> = registry
        .descriptors()
        .iter()
        .filter(|d| uva::tool::is_mcp_self_tool(&d.name))
        .map(|d| d.name.clone())
        .collect();
    for domain in [
        "agents",
        "tools",
        "skills",
        "schedules",
        "evals",
        "tasks",
        "models",
        "attachments",
        "profile",
    ] {
        let name = format!("{}{domain}", uva::tool::MCP_SELF_TOOL_PREFIX);
        assert!(
            self_tools.contains(&name),
            "missing {name}; registered: {self_tools:?}"
        );
    }
    let deferred = registry
        .descriptors()
        .iter()
        .filter(|d| uva::tool::is_mcp_self_tool(&d.name))
        .all(|d| d.exposure == uva::tool::ToolExposure::Deferred);
    assert!(
        deferred,
        "self-management tools must be Deferred like every mcp__* tool"
    );
    let profile = registry
        .descriptors()
        .into_iter()
        .find(|descriptor| descriptor.name == "mcp__self__profile")
        .expect("profile tool must be registered");
    assert_eq!(
        profile
            .annotations
            .and_then(|annotations| annotations.destructive_hint),
        Some(true),
        "the profile tool's writes are held by the approval stance"
    );

    // End to end through the whole client stack: the loopback
    // transport, the MCP handshake, and a real dispatch into
    // `AppState`. `models` is the cheapest read that proves it.
    let entry = registry
        .get("mcp__self__models")
        .expect("models tool must be registered");
    let tool = entry.tool_instance();
    let output: uva::tool::ToolOutput = tool
        .call(
            serde_json::json!({"action": "list"}),
            &uva::tool::Context::default(),
        )
        .await;
    let text = format!("{output:?}");
    assert!(
        text.contains("default_provider"),
        "models tool must answer from the workspace config: {text}"
    );

    // `skills` reads the catalog on the blocking pool; the answer
    // still has to cross the MCP stack and come back shaped.
    let skills = registry
        .get("mcp__self__skills")
        .expect("skills tool must be registered")
        .tool_instance();
    let output: uva::tool::ToolOutput = skills
        .call(
            serde_json::json!({"action": "list"}),
            &uva::tool::Context::default(),
        )
        .await;
    let text = format!("{output:?}");
    assert!(
        !text.contains("skills task failed"),
        "the blocking-pool read must complete: {text}"
    );
}

/// The `profile` tool reads the same row model the JSON-RPC surface
/// does, and its write action lands on the shared transaction.
#[tokio::test]
async fn the_profile_tool_reads_rows_and_writes_through_the_transaction() {
    let dir = tempfile::tempdir().expect("tempdir");
    let session_manager = SessionRegistry::new(dir.path().join("sessions"));
    let mut state =
        AppState::for_test(session_manager, dir.path().to_path_buf()).await;
    state.profile_store = Some(crate::profile::ProfileStore {
        bundle: None,
        doc: dir.path().join("profile.toml"),
        home: None,
        overlay: None,
    });
    let state = Arc::new(state);
    self_mcp::register_self_mcp(&state).await;

    let entry = state
        .tool_registry
        .as_ref()
        .get("mcp__self__profile")
        .expect("profile tool must be registered");
    let tool = entry.tool_instance();
    let listed: uva::tool::ToolOutput = tool
        .call(
            serde_json::json!({"action": "list"}),
            &uva::tool::Context::default(),
        )
        .await;
    assert!(
        format!("{listed:?}").contains("skills"),
        "the list action reads the profile rows: {listed:?}"
    );

    let written: uva::tool::ToolOutput = tool
        .call(
            serde_json::json!({
                "action": "set",
                "id": "skills",
                "config": {"marker": true},
            }),
            &uva::tool::Context::default(),
        )
        .await;
    let text = format!("{written:?}");
    assert!(text.contains("applied"), "{text}");
    assert!(dir.path().join("profile.toml").exists());
}
