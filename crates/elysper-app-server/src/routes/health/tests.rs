use super::*;

/// A hermetic `AppState` for route tests.
///
/// `AppState::new` loads the workspace configuration and builds a
/// real provider, so it fails without ambient provider
/// credentials by design (`WorkspaceConfig::load_from_dir` refuses
/// to start a server with no provider). Route behaviour has
/// nothing to do with which provider is configured, so these tests
/// build the state over in-memory components and a `FakeProvider`
/// instead — with a temp workspace whose lifetime the caller
/// keeps.
async fn test_state()
-> (std::sync::Arc<crate::state::AppState>, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("temp workspace");
    let sessions = elysper::session::manager::SessionRegistry::new(
        dir.path().join("sessions"),
    );
    let state =
        crate::state::AppState::for_test(sessions, dir.path().to_path_buf())
            .await;
    (std::sync::Arc::new(state), dir)
}

#[test]
fn model_entry_serializes_all_fields() {
    let entry = ModelEntry {
        provider: "openai".to_string(),
        model: "gpt-4".to_string(),
        context_window: 8192,
        supports_tools: true,
        supports_streaming: false,
    };
    let json = serde_json::to_value(&entry).unwrap();
    assert_eq!(json["provider"], "openai");
    assert_eq!(json["model"], "gpt-4");
    assert_eq!(json["context_window"], 8192);
    assert_eq!(json["supports_tools"], true);
    assert_eq!(json["supports_streaming"], false);
}

#[test]
fn models_response_wraps_entries_with_defaults() {
    let resp = ModelsResponse {
        models: vec![],
        default_provider: "anthropic".to_string(),
        default_model: "claude-opus-4".to_string(),
    };
    let json = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["models"].as_array().unwrap().len(), 0);
    assert_eq!(json["default_provider"], "anthropic");
    assert_eq!(json["default_model"], "claude-opus-4");
}
/// `list_models` MUST answer the workspace's catalogue: the
/// models array and the configured default model.
#[tokio::test]
async fn list_models_reports_the_workspace_catalogue() {
    let (state, _dir) = test_state().await;
    let expected_default = state.workspace_config.read().default_model.clone();
    let response = list_models(std::sync::Arc::clone(&state));
    let body = serde_json::to_value(&response).unwrap();
    assert!(body["models"].is_array(), "got: {body}");
    assert_eq!(body["default_model"], expected_default.as_str());
}

/// `livez` MUST answer `status: "ok"` — liveness is a
/// process-alive fact.
#[test]
fn livez_reports_ok() {
    let response = livez();
    let body = serde_json::to_value(&response).unwrap();
    assert_eq!(body["status"], "ok");
    // `failed` is skipped when empty — the liveness body has
    // no failed-checks field at all.
    assert!(body.get("failed").is_none(), "got: {body}");
}

/// `readyz` MUST answer ok once every readiness check passes.
/// The agent-registry check passes because `AppState::for_test`
/// registers the canonical ReAct agent.
#[tokio::test]
async fn readyz_reports_ready_after_state_initialized() {
    let (state, _dir) = test_state().await;
    let response = readyz(std::sync::Arc::clone(&state));
    let body = serde_json::to_value(&response).unwrap();
    assert_eq!(body["status"], "ok");
    assert!(
        body.get("failed").is_none(),
        "readyz must report no failed checks once AppState is initialised, got: {body}"
    );
}

/// `ProbeResponse` with failed checks MUST serialize the
/// names so operators can see *why* the probe fails from the
/// response body alone.
#[test]
fn probe_response_serializes_failed_check_names() {
    let resp = ProbeResponse {
        status: "unavailable",
        failed: vec!["agent_registry"],
    };
    let json = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["status"], "unavailable");
    assert_eq!(json["failed"][0], "agent_registry");
}
