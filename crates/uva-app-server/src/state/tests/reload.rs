//! Hot reload: only the rows a profile diff names are remounted.
//!
//! Each test starts from the fixture's mounted native set and drives
//! [`AppState::reload_profile`] directly — the same call the core's
//! poll loop makes. A running turn keeps the service handles it
//! resolved, so these assertions are about the *mount* moving underneath
//! it: untouched services keep their allocation, removed ones withdraw,
//! and an added row mounts.

use std::sync::Arc;

use serde_json::{Value, json};
use uva::{
    attachment::KEY_ATTACHMENTS,
    eval::{EvalRunner, KEY_EVAL},
    session::manager::SessionRegistry,
};

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
        .assembly
        .service::<EvalRunner>(KEY_EVAL)
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
        .assembly
        .service::<EvalRunner>(KEY_EVAL)
        .expect("eval stays mounted");
    assert!(
        Arc::ptr_eq(&eval_before, &eval_after),
        "an unaffected service is not rebuilt"
    );
}

/// Two consecutive `profile/set` calls on the same row must both
/// answer `applied`. The live smoke that surfaced the bug: the second
/// write answered `restart-required: the mcp row changed` although the
/// edit touched `eval` only — the first write commits the materialized
/// document (configless rows carry no `config` key), the second
/// re-reads it, and a missing config key must mean "no opinion"
/// (`null`, inherit), not "explicit empty" (`{}`, override), or the
/// whole assembly looks remounted.
#[tokio::test]
async fn a_second_write_does_not_remount_the_world() {
    let (mut state, dir) = state().await;
    let doc = dir.path().join("profile.toml");
    state.assembly.set_store(Some(crate::profile::ProfileStore {
        bundle: None,
        doc: doc.clone(),
        home: None,
        overlay: None,
    }));

    let first = state
        .profile_apply(
            "ui",
            crate::profile::ProfileAction::Edit(crate::profile::RowEdit::Set {
                id: "eval".to_owned(),
                disabled: Some(true),
                config: None,
            }),
        )
        .await;
    assert_eq!(
        first.state,
        crate::profile::ChangeState::Applied,
        "{first:?}"
    );

    let second = state
        .profile_apply(
            "ui",
            crate::profile::ProfileAction::Edit(crate::profile::RowEdit::Set {
                id: "eval".to_owned(),
                disabled: Some(false),
                config: None,
            }),
        )
        .await;
    assert_eq!(
        second.state,
        crate::profile::ChangeState::Applied,
        "{second:?}"
    );
}

/// dsh's self-disable write-back: a row that loses its provider can no
/// longer run, so the document records `disabled = true` for it — the
/// document always reflects the real assembly.
#[tokio::test]
async fn a_row_that_loses_its_provider_is_written_back_as_disabled() {
    let (mut state, dir) = state().await;
    let doc = dir.path().join("profile.toml");
    // Seed the document with the current assembly, so the write-back
    // has a document to edit.
    let text = crate::profile::ProfileStore::to_toml(&state.current_profile())
        .expect("the profile serializes");
    std::fs::write(&doc, text).expect("seed the document");
    state.assembly.set_store(Some(crate::profile::ProfileStore {
        bundle: None,
        doc: doc.clone(),
        home: None,
        overlay: None,
    }));

    // Remove the provider row: `agent-loop` (it injects `llm.default`)
    // loses its provider.
    let mut next = state.current_profile();
    next.hmr = true;
    next.core.retain(|row| row.id != "model-adapter");
    state
        .reload_profile(next)
        .await
        .expect("the removal applies");

    let document = crate::profile::Profile::from_path(&doc)
        .expect("the written-back document parses");
    let loop_row = document
        .find("agent-loop")
        .expect("the loop row stays in the document");
    assert!(
        loop_row.disabled,
        "the row that lost its provider is written back disabled"
    );
    let untargeted = document
        .find("tool-read")
        .expect("an unrelated row stays in the document");
    assert!(
        !untargeted.disabled,
        "a row that kept its provider is not written back"
    );
}

/// TOML cannot carry a JSON `null` inside a config: serializing drops
/// the key, parsing it back yields a config without it. A `null`-valued
/// key is therefore *not representable state*, and a document round
/// trip over one must not read as a configuration change — or every
/// second write remounts the boot-materialized rows (`model-adapter`,
/// `agent-loop`) plus their injectors.
#[tokio::test]
async fn a_round_trip_over_an_unrepresentable_null_is_not_a_change() {
    let (state, dir) = state().await;
    let doc = dir.path().join("profile.toml");

    // Mount a config carrying a null-valued key, as the live provider
    // materialization produces (unset credentials, absent defaults).
    let mut mounted = state.current_profile();
    mounted.hmr = true;
    let skills = mounted
        .core
        .iter_mut()
        .find(|entry| entry.id == "skills")
        .expect("the native set has skills");
    skills.config = json!({ "unset": Value::Null, "real": 1 });
    state
        .reload_profile(mounted.clone())
        .await
        .expect("the first reload applies");

    // Now write that config out and read it back: the null-valued key
    // is gone from the document, and the second reload must see no
    // change at all.
    let text =
        crate::profile::ProfileStore::to_toml(&mounted).expect("serializes");
    std::fs::write(&doc, text).expect("writes");
    let round_tripped =
        crate::profile::Profile::from_path(&doc).expect("parses back");

    let diff = state
        .reload_profile(round_tripped)
        .await
        .expect("the round trip applies");
    assert!(
        diff.remounted.is_empty(),
        "a null-valued key is unrepresentable, not a change: {:?}",
        diff.remounted
    );
}

/// Changing the `model-adapter` config cascades: `agent-loop`
/// injects `llm`, so the reload rebuilds it against the new
/// catalogue instead of pinning the withdrawn one.
#[tokio::test]
async fn hmr_model_adapter_change_cascades_to_agent_loop() {
    let (state, _dir) = state().await;
    let loop_before = state
        .assembly
        .service::<Arc<dyn uva::harness::plugin::AgentLoopFactory>>(
            uva::harness::plugin::KEY_AGENT_LOOP,
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
        "providers": {
            "echo": {
                "type": "demo",
                "api_key_env": "NONE",
                "default_model": "echo-model"
            }
        },
        "default_provider": "echo"
    });

    let diff = state
        .reload_profile(next)
        .await
        .expect("the reload applies");
    let mut remounted = diff.remounted.clone();
    remounted.sort();
    assert_eq!(
        remounted,
        vec![
            "agent-loop".to_owned(),
            "context-compactor".to_owned(),
            "model-adapter".to_owned(),
        ],
        "the injectors join the changed row's remount set: {diff:?}"
    );

    let loop_after = state
        .assembly
        .service::<Arc<dyn uva::harness::plugin::AgentLoopFactory>>(
            uva::harness::plugin::KEY_AGENT_LOOP,
        )
        .expect("agent-loop is mounted again");
    assert!(
        !Arc::ptr_eq(&loop_before, &loop_after),
        "the dependent service is rebuilt, not the stale allocation"
    );

    let llm = state
        .assembly
        .service::<crate::state::ProviderCatalogue>(uva::provider::KEY_LLM)
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
        state.assembly.service::<EvalRunner>(KEY_EVAL).is_none(),
        "the removed service is withdrawn"
    );
    assert!(
        state
            .assembly
            .service::<uva::attachment::AttachmentStore>(KEY_ATTACHMENTS)
            .is_some(),
        "a sibling stays mounted"
    );

    let mut next = state.current_profile();
    next.hmr = true;
    next.core
        .push(crate::profile::Manifest::plain("eval", "eval"));
    let diff = state
        .reload_profile(next)
        .await
        .expect("the addition applies");
    assert_eq!(diff.added, vec!["eval".to_owned()]);
    assert!(
        state.assembly.service::<EvalRunner>(KEY_EVAL).is_some(),
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
            "providers": {
                provider: {
                    "type": "demo",
                    "api_key_env": "NONE",
                    "default_model": format!("{provider}-model")
                }
            },
            "default_provider": provider
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
    let prompt_context = state.prompt_context("swap").await;
    let run = |provider| uva::harness::plugin::RunConfig {
        provider,
        tool_registry: Arc::clone(&state.tool_registry),
        workspace_root: state.workspace_root.clone(),
        descriptor: uva::harness::default_descriptor("swap".into()),
        prompt_context: Arc::clone(&prompt_context),
        max_iterations: 1,
        tool_surface: None,
        strategy: None,
        tool_restriction: None,
        log_writer: None,
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
        vec![
            "agent-loop".to_owned(),
            "context-compactor".to_owned(),
            "model-adapter".to_owned(),
        ],
        "the provider swap cascades to the loop and the compactor: {diff:?}"
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
        "providers": {
            "alpha": {
                "type": "demo",
                "api_key_env": "NONE",
                "default_model": "alpha-model"
            }
        },
        "default_provider": "alpha"
    });
    state
        .reload_profile(next)
        .await
        .expect("the remount recovers the services");
    let factory = state.agent_loop().expect("agent-loop is mounted again");
    let provider = state.default_provider.read().clone();
    let prompt_context = state.prompt_context("recover").await;
    let run = uva::harness::plugin::RunConfig {
        provider,
        tool_registry: Arc::clone(&state.tool_registry),
        workspace_root: state.workspace_root.clone(),
        descriptor: uva::harness::default_descriptor("recover".into()),
        prompt_context,
        max_iterations: 1,
        tool_surface: None,
        strategy: None,
        tool_restriction: None,
        log_writer: None,
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
        matches!(error, uva::runtime::ModelSelectionError::Unavailable { .. }),
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
    next.core.push(crate::profile::Manifest {
        config: json!({
            "providers": {
                "demo": {
                    "type": "demo",
                    "api_key_env": "NONE",
                    "default_model": "demo-model"
                }
            },
            "default_provider": "demo"
        }),
        ..crate::profile::Manifest::plain("model-adapter", "model-adapter")
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
        .assembly
        .service::<EvalRunner>(KEY_EVAL)
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
        .assembly
        .service::<EvalRunner>(KEY_EVAL)
        .expect("eval stays mounted");
    assert!(Arc::ptr_eq(&eval_before, &eval_after));
}

/// The prompt manifest is a live service snapshot: unloading the
/// `skills` row changes what the next `prompt_context()` call
/// assembles, where a boot-frozen `Arc` would keep advertising the
/// withdrawn skills forever.
#[tokio::test]
async fn prompt_context_reads_the_live_skills_service() {
    let dir = tempfile::tempdir().expect("tempdir");
    let skill_dir =
        dir.path().join(".agents").join("skills").join("live-skill");
    std::fs::create_dir_all(&skill_dir).expect("skill dir");
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: live-skill\ndescription: A skill the live snapshot must see.\n---\nbody\n",
    )
    .expect("skill file");
    let registry = SessionRegistry::new(dir.path().join("sessions"));
    let state = AppState::for_test(registry, dir.path().to_path_buf()).await;

    let descriptor = uva::harness::default_descriptor("agent".into());
    let before = state.prompt_context("agent").await.assemble(&descriptor);
    assert!(
        before.contains("live-skill"),
        "the mounted skills service contributes the block: {before}"
    );

    assert!(
        state.unload_plugin("skills").await,
        "the fixture mounts the skills row"
    );
    let after = state.prompt_context("agent").await.assemble(&descriptor);
    assert!(
        !after.contains("live-skill"),
        "the snapshot follows the service, not the boot allocation: {after}"
    );
}

// -- enablement: the effective row set drives the mount ---------------

/// `disabled = true` filters the row out of the assembly at the
/// boundary: its fiber is unloaded and its service withdrawn, while the
/// row stays in the raw profile so the settings page can show the
/// switch.
#[tokio::test]
async fn a_disabled_row_unmounts_and_stays_in_the_profile() {
    let (state, _dir) = state().await;
    let mut next = state.current_profile();
    next.hmr = true;
    let skills = next
        .core
        .iter_mut()
        .find(|entry| entry.id == "skills")
        .expect("the native set has skills");
    skills.disabled = true;

    let reconciled = state.apply_resolved(next).await.expect("apply");
    assert_eq!(reconciled.diff.removed, vec!["skills".to_owned()]);
    assert!(!reconciled.mcp_touched);
    assert!(
        state.plugin_state("skills").is_none(),
        "the disabled row is not mounted"
    );
    assert!(
        state.assembly.current().find("skills").is_none(),
        "the effective assembly drops the row"
    );
    assert!(
        state.assembly.raw().find("skills").is_some(),
        "the raw profile keeps the row for the settings page"
    );
    assert!(
        state.current_profile().find("skills").is_some(),
        "the re-resolve base is the raw declaration: passing the \
         effective set back would drop the row for good"
    );
}

// -- the `[ui]` tree: HMR swaps the whole tree -----------------------

fn ui_row(id: &str, config: serde_json::Value) -> crate::profile::Manifest {
    crate::profile::Manifest {
        config,
        ..crate::profile::Manifest::plain(id, id)
    }
}

fn shell_row() -> crate::profile::Manifest {
    ui_row("ui.shell", json!({ "slots": [{ "id": "chat.sidebar" }] }))
}

fn notes_row(title: &str) -> crate::profile::Manifest {
    ui_row(
        "ui.slot.notes",
        json!({ "components": [{
            "slot": "chat.sidebar",
            "kind": "panel",
            "props": { "title": title },
        }] }),
    )
}

#[tokio::test]
async fn hmr_swaps_the_ui_tree_and_the_old_slots_roll_back() {
    let (state, _dir) = state().await;

    let mut first = state.current_profile();
    first.hmr = true;
    first.ui = vec![shell_row(), notes_row("before")];
    state
        .reload_profile(first)
        .await
        .expect("the first tree activates");
    let components = state.ui_tree().components_in("chat.sidebar");
    assert_eq!(components.len(), 1);
    assert_eq!(components[0].props["title"], json!("before"));

    let mut second = state.current_profile();
    second.hmr = true;
    second.ui = vec![shell_row(), notes_row("after")];
    state
        .reload_profile(second)
        .await
        .expect("the second tree activates");
    let components = state.ui_tree().components_in("chat.sidebar");
    assert_eq!(
        components.len(),
        1,
        "the previous generation rolled back, so nothing stacks"
    );
    assert_eq!(components[0].props["title"], json!("after"));
}

#[tokio::test]
async fn a_bad_ui_segment_is_rejected_and_the_old_tree_stays() {
    let (state, _dir) = state().await;

    let mut first = state.current_profile();
    first.hmr = true;
    first.ui = vec![shell_row(), notes_row("before")];
    state
        .reload_profile(first)
        .await
        .expect("the first tree activates");

    let mut second = state.current_profile();
    second.hmr = true;
    second.ui = vec![
        shell_row(),
        ui_row(
            "ui.slot.rogue",
            json!({ "components": [{
                "slot": "no.such.slot",
                "kind": "panel",
                "props": {},
            }] }),
        ),
    ];
    let error = state
        .reload_profile(second)
        .await
        .expect_err("an undeclared slot fails activation");
    assert!(
        error.contains("which no row declares"),
        "unexpected error: {error}"
    );

    let components = state.ui_tree().components_in("chat.sidebar");
    assert_eq!(components.len(), 1, "the old tree is still mounted");
    assert_eq!(components[0].props["title"], json!("before"));
}

#[tokio::test]
async fn a_reload_waits_for_the_write_transaction() {
    // The watcher and the write transaction share one writer: a
    // reload that ran beside a transaction would unload and mount
    // the same rows twice (double mount, wrong unload order).
    let (state, _dir) = state().await;
    let next = state.current_profile();

    // Hold the writer the way a write transaction does.
    let guard = state.assembly.writer().lock().await;
    let mut reload = Box::pin(state.reload_profile(next));
    assert!(
        tokio::time::timeout(
            std::time::Duration::from_millis(100),
            &mut reload
        )
        .await
        .is_err(),
        "the reload must wait for the write transaction"
    );
    drop(guard);

    let diff = tokio::time::timeout(std::time::Duration::from_secs(5), reload)
        .await
        .expect("the reload must finish once the writer is gone")
        .expect("the reload applies");
    assert!(diff.added.is_empty(), "nothing was added: {diff:?}");
    assert!(diff.removed.is_empty(), "nothing was removed: {diff:?}");
    assert!(diff.remounted.is_empty(), "nothing was remounted: {diff:?}");
}

/// A remount reuses the boot's build recipe. The fixture's session
/// corpus lives outside `<workspace>/sessions`; the remounted `search`
/// row must keep reading it — before the recipe was unified, the
/// rebuild constructed its build context without the boot's extras and
/// silently indexed the empty default directory.
#[tokio::test]
async fn a_remount_keeps_the_boots_session_corpus() {
    let dir = tempfile::tempdir().expect("tempdir");
    // A corpus only the boot's `sessions_dir` names: the workspace's
    // default `sessions/` stays empty, so a hit can only come from the
    // right directory.
    let corpus = dir.path().join("corpus");
    let probe = corpus.join("alice").join("s_probe").join("events.jsonl");
    std::fs::create_dir_all(probe.parent().expect("probe parent"))
        .expect("corpus dirs");
    let message = uva::provider::Message::user("the zephyr needle");
    std::fs::write(
        &probe,
        serde_json::json!({
            "type": "user_message",
            "seq": 1,
            "ts": "2026-11-18T00:00:00Z",
            "data": message,
        })
        .to_string()
            + "\n",
    )
    .expect("probe row");

    let registry = SessionRegistry::new(corpus);
    let state = AppState::for_test(registry, dir.path().to_path_buf()).await;

    // Touch the `search` row's config so the diff names it.
    let mut next = state.current_profile();
    next.hmr = true;
    let search = next
        .core
        .iter_mut()
        .find(|entry| entry.id == "search")
        .expect("the native set has search");
    search.config = json!({ "marker": true });
    let diff = state
        .reload_profile(next)
        .await
        .expect("the reload applies");
    assert!(
        diff.remounted.iter().any(|id| id == "search"),
        "the search row is remounted: {diff:?}"
    );

    let service = state.search().expect("search stays mounted");
    let hits = service
        .search(
            "zephyr",
            10,
            Some(vec!["session".to_owned()]),
            Some("alice"),
        )
        .await;
    assert!(
        !hits.is_empty(),
        "the remounted row reads the boot's session corpus"
    );
}

/// An edit that strands a required row fails the whole change: the
/// mount stanza's audit sees `agent-loop` parked (its provider left
/// the document in the same edit) and the previous rows come back
/// through the best-effort rollback. Before the audit moved into the
/// stanza this edit applied silently with a parked loop.
#[tokio::test]
async fn an_edit_that_strands_a_required_row_fails_and_rolls_back() {
    let (state, _dir) = state().await;
    let mut next = state.current_profile();
    next.hmr = true;
    next.core.retain(|entry| entry.id != "model-adapter");
    let agent_loop = next
        .core
        .iter_mut()
        .find(|entry| entry.id == "agent-loop")
        .expect("the native set has agent-loop");
    agent_loop.config = json!({ "marker": true });

    let error = state
        .reload_profile(next)
        .await
        .expect_err("the edit strands a required row");
    assert!(
        error.contains("agent-loop"),
        "the failure names the stranded row: {error}"
    );
    assert!(
        state.resolve_provider(None).is_ok(),
        "the rollback restored the provider row"
    );
    assert!(
        state.agent_loop().is_some(),
        "the rollback restored the loop"
    );
}
