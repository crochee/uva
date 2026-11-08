//! Hot reload: only the rows a profile diff names are remounted.
//!
//! Each test starts from the fixture's mounted native set and drives
//! [`AppState::reload_profile`] directly — the same call the core's
//! poll loop makes. A running turn keeps the service handles it
//! resolved, so these assertions are about the *mount* moving underneath
//! it: untouched services keep their allocation, removed ones withdraw,
//! and an added row mounts.

use std::sync::Arc;

use elysper::{
    attachment::KEY_ATTACHMENTS,
    eval::{EvalRunner, KEY_EVAL},
    kernel::RuntimeKind,
    session::manager::SessionRegistry,
};
use serde_json::json;

use crate::state::AppState;

async fn state() -> (AppState, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let registry = SessionRegistry::new(dir.path().join("sessions"));
    let state = AppState::for_test(registry, dir.path().to_path_buf()).await;
    (state, dir)
}

#[tokio::test]
async fn hmr_remounts_only_the_changed_plugin() {
    let (state, _dir) = state().await;
    let eval_before = state
        .kernel
        .require::<EvalRunner>(KEY_EVAL)
        .expect("the fixture mounts eval");

    let mut next = state.current_profile();
    next.hmr = true;
    let skills = next
        .core
        .iter_mut()
        .find(|entry| entry.id == "skills")
        .expect("the native set has skills");
    skills.config = json!({ "marker": true });

    let diff = state
        .reload_profile(next)
        .await
        .expect("the reload applies");
    assert_eq!(diff.remounted, vec!["skills".to_owned()]);
    assert!(diff.added.is_empty());
    assert!(diff.removed.is_empty());
    assert!(!diff.restart_required);
    assert!(
        state.plugin_state("skills").is_some(),
        "the changed row is mounted again"
    );

    // The untouched consumer keeps the allocation the fixture mounted:
    // a reload did not re-register it.
    let eval_after = state
        .kernel
        .require::<EvalRunner>(KEY_EVAL)
        .expect("eval stays mounted");
    assert!(
        Arc::ptr_eq(&eval_before, &eval_after),
        "an unaffected service is not rebuilt"
    );
}

/// Changing the `model-adapter` config cascades: `agent-loop`
/// injects `llm`, so the reload rebuilds it against the new
/// catalogue instead of pinning the withdrawn one.
#[tokio::test]
async fn hmr_model_adapter_change_cascades_to_agent_loop() {
    let (state, _dir) = state().await;
    let loop_before = state
        .kernel
        .require::<Arc<dyn elysper::harness::plugin::AgentLoopFactory>>(
            elysper::harness::plugin::KEY_AGENT_LOOP,
        )
        .expect("the fixture mounts agent-loop");

    let mut next = state.current_profile();
    next.hmr = true;
    let adapter = next
        .core
        .iter_mut()
        .find(|entry| entry.id == "model-adapter")
        .expect("the native set has model-adapter");
    adapter.config = json!({
        "workspace": {
            "providers": {
                "echo": {
                    "type": "demo",
                    "api_key_env": "NONE",
                    "default_model": "echo-model"
                }
            },
            "default_provider": "echo"
        }
    });

    let diff = state
        .reload_profile(next)
        .await
        .expect("the reload applies");
    let mut remounted = diff.remounted.clone();
    remounted.sort();
    assert_eq!(
        remounted,
        vec!["agent-loop".to_owned(), "model-adapter".to_owned()],
        "the injector joins the changed row's remount set: {diff:?}"
    );

    let loop_after = state
        .kernel
        .require::<Arc<dyn elysper::harness::plugin::AgentLoopFactory>>(
            elysper::harness::plugin::KEY_AGENT_LOOP,
        )
        .expect("agent-loop is mounted again");
    assert!(
        !Arc::ptr_eq(&loop_before, &loop_after),
        "the dependent service is rebuilt, not the stale allocation"
    );

    let llm = state
        .kernel
        .require::<crate::state::ProviderCatalogue>(elysper::provider::KEY_LLM)
        .expect("llm is mounted again");
    assert_eq!(
        llm.default_provider_name(),
        "echo",
        "the service carries the new row config"
    );
}

#[tokio::test]
async fn hmr_removes_and_readds_a_row() {
    let (state, _dir) = state().await;

    let mut next = state.current_profile();
    next.hmr = true;
    next.core.retain(|entry| entry.id != "eval");
    let diff = state
        .reload_profile(next)
        .await
        .expect("the removal applies");
    assert_eq!(diff.removed, vec!["eval".to_owned()]);
    assert!(
        state
            .kernel
            .get::<EvalRunner>(KEY_EVAL)
            .expect("read eval")
            .is_none(),
        "the removed service is withdrawn"
    );
    assert!(
        state
            .kernel
            .get::<elysper::attachment::AttachmentStore>(KEY_ATTACHMENTS)
            .expect("read attachments")
            .is_some(),
        "a sibling stays mounted"
    );

    let mut next = state.current_profile();
    next.hmr = true;
    next.core.push(crate::profile::Entry {
        id: "eval".to_owned(),
        runtime: RuntimeKind::Native,
        config: serde_json::Value::Null,
        capabilities: Vec::new(),
    });
    let diff = state
        .reload_profile(next)
        .await
        .expect("the addition applies");
    assert_eq!(diff.added, vec!["eval".to_owned()]);
    assert!(
        state
            .kernel
            .get::<EvalRunner>(KEY_EVAL)
            .expect("read eval")
            .is_some(),
        "the added row is mounted"
    );
}

/// Design acceptance ①: a profile-driven provider swap reaches new
/// runs. The agent created before the reload keeps provider A; a
/// factory run created after resolves provider B.
#[tokio::test]
async fn hmr_provider_swap_reaches_new_runs_only() {
    let (state, _dir) = state().await;
    let adapter_config = |provider: &str| {
        json!({
            "workspace": {
                "providers": {
                    provider: {
                        "type": "demo",
                        "api_key_env": "NONE",
                        "default_model": format!("{provider}-model")
                    }
                },
                "default_provider": provider
            }
        })
    };

    let mut first = state.current_profile();
    first.hmr = true;
    let adapter = first
        .core
        .iter_mut()
        .find(|entry| entry.id == "model-adapter")
        .expect("the native set has model-adapter");
    adapter.config = adapter_config("alpha");
    state.reload_profile(first).await.expect("alpha mounts");

    let loop_before = state.agent_loop().expect("agent-loop is mounted");
    let run = |provider| elysper::harness::plugin::RunConfig {
        provider,
        tool_registry: Arc::clone(&state.tool_registry),
        workspace_root: state.workspace_root.clone(),
        descriptor: elysper::harness::default_descriptor("swap".into()),
        prompt_context: Arc::clone(&state.prompt_context),
        max_iterations: 1,
        delegators: Vec::new(),
        tool_surface: None,
        strategy: None,
        tool_restriction: None,
        log_writer: None,
        subagent_writers: None,
        context_manager: None,
        compactor: None,
        host: None,
        spawner: None,
    };
    let provider_before = state.default_provider.read().clone();
    let _before = loop_before.create(&run(Arc::clone(&provider_before)));

    let mut second = state.current_profile();
    second.hmr = true;
    let adapter = second
        .core
        .iter_mut()
        .find(|entry| entry.id == "model-adapter")
        .expect("the native set has model-adapter");
    adapter.config = adapter_config("beta");
    let diff = state.reload_profile(second).await.expect("beta mounts");
    let mut remounted = diff.remounted.clone();
    remounted.sort();
    assert_eq!(
        remounted,
        vec!["agent-loop".to_owned(), "model-adapter".to_owned()],
        "the provider swap cascades to the loop: {diff:?}"
    );

    assert_eq!(
        state.provider_catalogue.read().default_provider_name(),
        "beta",
        "AppState re-syncs with the remounted service"
    );
    let loop_after = state.agent_loop().expect("agent-loop remounted");
    assert!(
        !Arc::ptr_eq(&loop_before, &loop_after),
        "the cascade rebuilt the factory"
    );
    let provider_after = state.default_provider.read().clone();
    let _after = loop_after.create(&run(Arc::clone(&provider_after)));
    assert!(
        !Arc::ptr_eq(&provider_before, &provider_after),
        "a run created after the swap resolves the new default handle"
    );
    assert!(
        state
            .provider_catalogue
            .read()
            .resolve(Some("beta/beta-model"))
            .is_ok(),
        "the remounted catalogue offers provider beta"
    );
    assert!(
        state
            .provider_catalogue
            .read()
            .resolve(Some("alpha/alpha-model"))
            .is_err(),
        "the withdrawn alpha entry is gone from the live catalogue"
    );
}

/// Design acceptance ②: disposing the `model-adapter` parks the
/// agent-loop (it injects `llm`), so no new agent can be built
/// until a provider is mounted again.
#[tokio::test]
async fn disposing_model_adapter_parks_the_agent_loop() {
    let (state, _dir) = state().await;
    assert!(
        state.agent_loop().is_some(),
        "the fixture mounts agent-loop"
    );
    assert!(state.dispose_plugin("model-adapter"), "the plugin unloads");
    assert!(
        state.agent_loop().is_none(),
        "the loop is parked: its llm dependency is gone"
    );

    // Remount through a real profile change (the watcher's flow):
    // the row's config changes, the diff remounts `model-adapter`,
    // and the cascade brings the loop back.
    let mut next = state.current_profile();
    next.hmr = true;
    let adapter = next
        .core
        .iter_mut()
        .find(|entry| entry.id == "model-adapter")
        .expect("the native set has model-adapter");
    adapter.config = json!({
        "workspace": {
            "providers": {
                "alpha": {
                    "type": "demo",
                    "api_key_env": "NONE",
                    "default_model": "alpha-model"
                }
            },
            "default_provider": "alpha"
        }
    });
    state
        .reload_profile(next)
        .await
        .expect("the remount recovers the services");
    let factory = state.agent_loop().expect("agent-loop is mounted again");
    let run = elysper::harness::plugin::RunConfig {
        provider: state.default_provider.read().clone(),
        tool_registry: Arc::clone(&state.tool_registry),
        workspace_root: state.workspace_root.clone(),
        descriptor: elysper::harness::default_descriptor("recover".into()),
        prompt_context: Arc::clone(&state.prompt_context),
        max_iterations: 1,
        delegators: Vec::new(),
        tool_surface: None,
        strategy: None,
        tool_restriction: None,
        log_writer: None,
        subagent_writers: None,
        context_manager: None,
        compactor: None,
        host: None,
        spawner: None,
    };
    let _agent = factory.create(&run);
    assert!(
        state.agent_loop().is_some(),
        "a new agent builds against the recovered services"
    );
}
/// Removing the provider row: no cascade (the kernel parks the
/// loop), the removal reload itself succeeds, selections and session
/// creation fail loudly instead of serving the withdrawn handles, and
/// re-adding the row recovers everything.
#[tokio::test]
async fn removing_the_provider_row_parks_the_loop_and_fails_loudly() {
    let (state, _dir) = state().await;
    let mut next = state.current_profile();
    next.hmr = true;
    next.core.retain(|entry| entry.id != "model-adapter");

    let diff = state
        .reload_profile(next)
        .await
        .expect("the removal applies");
    assert_eq!(diff.removed, vec!["model-adapter".to_owned()]);
    assert!(
        diff.remounted.is_empty(),
        "a removal does not cascade: {diff:?}"
    );
    assert!(
        state.agent_loop().is_none(),
        "the kernel parks the loop when its provider is gone"
    );

    let error = match state.resolve_provider(None) {
        Ok(_) => panic!("a selection cannot resolve without a provider"),
        Err(error) => error,
    };
    assert!(
        matches!(
            error,
            elysper::runtime::ModelSelectionError::Unavailable { .. }
        ),
        "{error:?}"
    );
    let error = match state.get_or_create_session_controller("u", "fresh").await
    {
        Ok(_) => panic!("a session cannot be created without the services"),
        Err(error) => error,
    };
    assert!(
        format!("{error}").contains("llm"),
        "the error names the missing service: {error}"
    );

    // Re-adding the row recovers the loop and the provider surface.
    let mut next = state.current_profile();
    next.hmr = true;
    next.core.push(crate::profile::Entry {
        id: "model-adapter".to_owned(),
        runtime: RuntimeKind::Native,
        config: json!({
            "workspace": {
                "providers": {
                    "demo": {
                        "type": "demo",
                        "api_key_env": "NONE",
                        "default_model": "demo-model"
                    }
                },
                "default_provider": "demo"
            }
        }),
        capabilities: Vec::new(),
    });
    state
        .reload_profile(next)
        .await
        .expect("the re-add applies");
    assert!(state.agent_loop().is_some(), "the loop is mounted again");
    assert!(state.resolve_provider(None).is_ok(), "the provider is back");
}

#[tokio::test]
async fn hmr_off_reports_a_restart_without_touching_the_mount() {
    let (state, _dir) = state().await;
    let eval_before = state
        .kernel
        .require::<EvalRunner>(KEY_EVAL)
        .expect("the fixture mounts eval");
    // The fixture's built-in profile starts with HMR off; turn it on
    // first so the next change is a real "hot reload is disabled" case.
    let mut enabled = state.current_profile();
    enabled.hmr = true;
    state.reload_profile(enabled).await.expect("HMR turns on");

    let mut next = state.current_profile();
    next.hmr = false;
    let skills = next
        .core
        .iter_mut()
        .find(|entry| entry.id == "skills")
        .expect("the native set has skills");
    skills.config = json!({ "marker": true });

    let diff = state
        .reload_profile(next)
        .await
        .expect("the diff is reported");
    assert!(diff.restart_required);
    assert!(state.current_profile().hmr, "the old profile is kept");
    let eval_after = state
        .kernel
        .require::<EvalRunner>(KEY_EVAL)
        .expect("eval stays mounted");
    assert!(Arc::ptr_eq(&eval_before, &eval_after));
}
