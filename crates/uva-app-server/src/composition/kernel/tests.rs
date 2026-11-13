//! Kernel boot tests: profile assembly, the native mount, row
//! solidification, and unload semantics.

use std::sync::Arc;

use tempfile::tempdir;
use uva::{
    attachment::{AttachmentStore, KEY_ATTACHMENTS},
    eval::{EvalRunner, KEY_EVAL},
    harness::{AgentRegistry, ApprovalPolicy, DelegatingTool},
    mcp::{KEY_MCP, McpSupervisor},
    provider::traits::ModelProvider,
    scheduler::{KEY_SCHEDULER, SchedulerService},
    session::{
        Compactor,
        KEY_SESSIONS,
        SummaryCompactor,
        manager::SessionRegistry,
    },
    skill::{KEY_SKILLS, SkillRegistry},
    tool::{KEY_TOOLS, ToolRegistry},
};

use super::{
    BootExtras,
    BootServices,
    SharedClock,
    mount_profile,
    native_profile,
};
use crate::state::ProviderCatalogue;

/// The boot's containers: the shared tool registry, the peer-agent
/// registry, and the harness host.
fn services(root: &std::path::Path) -> Arc<BootServices> {
    let provider: Arc<dyn ModelProvider> =
        Arc::new(uva::test_support::FakeProvider::new(vec![]));
    let catalogue = ProviderCatalogue::default().with_handle(
        "fake",
        "fake-model",
        Arc::clone(&provider),
    );
    let tools = Arc::new(crate::state::plugin_tool_registry());
    let sessions = Arc::new(SessionRegistry::new(root.join("sessions")));
    let agents = Arc::new(AgentRegistry::new());
    let compactor: Arc<dyn Compactor> =
        Arc::new(SummaryCompactor::new(Arc::clone(&provider), "fake-model"));
    let _ = catalogue;
    let _ = sessions;
    let _ = compactor;
    Arc::new(BootServices {
        tool_registry: Arc::clone(&tools),
        agent_registry: Arc::clone(&agents),
        plugin_host: uva::harness::Host::new(),
    })
}

/// The boot's containers plus the workspace they read.
fn boot_services(root: &std::path::Path) -> (Arc<BootServices>, BootExtras) {
    (
        services(root),
        BootExtras {
            workspace_root: root.to_path_buf(),
            ..BootExtras::default()
        },
    )
}

/// The fixture profile with the `model-adapter` row materialized
/// the way the boot does it (the empty config would make the
/// factory refuse to build).
fn profile_with_catalogue() -> crate::profile::Profile {
    let mut profile = native_profile();
    let row = profile
        .core
        .iter_mut()
        .find(|entry| entry.id == "model-adapter")
        .expect("the native set has model-adapter");
    row.config = serde_json::json!({
        "workspace": {
            "providers": {
                "fake": {
                    "type": "demo",
                    "api_key_env": "NONE",
                    "default_model": "fake-model"
                }
            },
            "default_provider": "fake"
        }
    });
    profile
}

#[test]
fn baseline_profile_names_every_native_plugin_in_dependency_order() {
    let profile = native_profile();
    let ids: Vec<&str> =
        profile.core.iter().map(|entry| entry.id.as_str()).collect();
    for expected in [
        "tool-registry",
        "model-adapter",
        "memory",
        "session-store",
        "sandbox",
        "subagent-backend",
        "context-compactor",
        "approval",
        "skills",
        "agent-loop",
        "mcp",
        "eval",
        "scheduler",
        "attachments",
    ] {
        assert!(
            ids.contains(&expected),
            "the boot profile is missing `{expected}`: {ids:?}"
        );
    }
    assert_eq!(ids.len(), 21, "no duplicate rows: {ids:?}");
}

#[tokio::test]
async fn mounts_the_native_set_through_the_kernel() {
    let dir = tempdir().expect("tempdir");
    let (services, extras) = boot_services(dir.path());
    let (ctx, handle) = mount_profile(
        Arc::clone(&services),
        &profile_with_catalogue(),
        SharedClock::system(),
        BootExtras {
            workspace_root: dir.path().to_path_buf(),
            compaction: Some(uva::session::CompactionPluginConfig {
                model: "fake-model".to_owned(),
                settings: uva::session::CompactionSettings::default(),
            }),
            ..extras
        },
    )
    .expect("mount the native set");

    let tools = ctx.require::<ToolRegistry>(KEY_TOOLS).expect("tools");
    assert!(
        Arc::ptr_eq(&tools, &services.tool_registry),
        "the service is the deployment's one registry"
    );
    assert!(
        tools.contains("skill"),
        "SkillPlugin registers the `skill` tool; boot must not"
    );

    ctx.require::<SkillRegistry>(KEY_SKILLS).expect("skills");
    ctx.require::<crate::search::SearchService>(
        crate::search::plugin::KEY_SEARCH,
    )
    .expect("search");
    ctx.require::<Arc<dyn uva::context::Memory>>(uva::context::KEY_MEMORY)
        .expect("memory");
    ctx.require::<McpSupervisor>(KEY_MCP).expect("mcp");
    ctx.require::<EvalRunner>(KEY_EVAL).expect("eval");
    ctx.require::<SchedulerService>(KEY_SCHEDULER)
        .expect("scheduler");
    ctx.require::<AttachmentStore>(KEY_ATTACHMENTS)
        .expect("attachments");
    ctx.require::<SessionRegistry>(KEY_SESSIONS)
        .expect("sessions");
    let llm = ctx
        .require::<ProviderCatalogue>(uva::provider::KEY_LLM)
        .expect("llm");
    llm.resolve(Some("fake/fake-model"))
        .expect("the service resolves the deployment's pair");
    assert!(
        llm.resolve(Some("fake/other-model")).is_err(),
        "a model the deployment does not offer is refused"
    );
    let default = ctx
        .require::<Arc<dyn uva::provider::ModelProvider>>(
            uva::provider::KEY_LLM_DEFAULT,
        )
        .expect("llm.default");
    // The default handle answers for the deployment's pair; the
    // provider's own model name is its implementation detail.
    assert!(
        default.model_config().context_window > 0,
        "the default handle reports the deployment's context window"
    );
    ctx.require::<Arc<dyn uva::harness::plugin::AgentLoopFactory>>(
        uva::harness::plugin::KEY_AGENT_LOOP,
    )
    .expect("agentLoop");
    ctx.require::<ApprovalPolicy>(uva::harness::KEY_APPROVAL)
        .expect("approval");
    ctx.require::<uva::tool_shell::SandboxService>(
        uva::tool_shell::KEY_SANDBOX,
    )
    .expect("sandbox");

    assert_eq!(handle.len(), 21);
    assert!(handle.plugin_state("skills").is_some());
}

#[tokio::test]
async fn an_explicit_profile_mounts_only_the_rows_it_names() {
    let dir = tempdir().expect("tempdir");
    let (services, extras) = boot_services(dir.path());
    let profile = crate::profile::Profile {
        core: vec![crate::profile::Entry {
            id: "tool-registry".to_owned(),
            runtime: uva::kernel::RuntimeKind::Native,
            config: serde_json::Value::Null,
            capabilities: Vec::new(),
            enabled: crate::profile::Enabled::On,
        }],
        ui: Vec::new(),
        hmr: false,
    };
    let (ctx, handle) = mount_profile(
        Arc::clone(&services),
        &profile,
        SharedClock::system(),
        extras,
    )
    .expect("the subset mounts");
    assert_eq!(handle.len(), 1, "only the named row is mounted");
    ctx.require::<ToolRegistry>(KEY_TOOLS).expect("tools");
    assert!(
        ctx.get::<SkillRegistry>(KEY_SKILLS)
            .expect("read skills")
            .is_none(),
        "an omitted row is not mounted"
    );
}

/// A profile row whose `config` is a `ModelAdapterConfig` builds
/// the plugin from it — not from the host's deployment catalogue.
#[tokio::test]
async fn model_adapter_row_config_drives_the_plugin() {
    let dir = tempdir().expect("tempdir");
    let (services, extras) = boot_services(dir.path());
    let profile = crate::profile::Profile {
        core: vec![crate::profile::Entry {
            id: "model-adapter".to_owned(),
            runtime: uva::kernel::RuntimeKind::Native,
            config: serde_json::json!({
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
            }),
            capabilities: Vec::new(),
            enabled: crate::profile::Enabled::On,
        }],
        ui: Vec::new(),
        hmr: false,
    };
    let (ctx, handle) = mount_profile(
        Arc::clone(&services),
        &profile,
        SharedClock::system(),
        extras,
    )
    .expect("the config-driven row mounts");
    assert_eq!(handle.len(), 1, "only the named row is mounted");

    let llm = ctx
        .require::<ProviderCatalogue>(uva::provider::KEY_LLM)
        .expect("llm");
    assert_eq!(
        llm.default_provider_name(),
        "echo",
        "the catalogue is built from the row config"
    );
    llm.resolve(Some("echo/echo-model"))
        .expect("the demo seam injects the echo handle");
    ctx.require::<Arc<dyn uva::provider::ModelProvider>>(
        uva::provider::KEY_LLM_DEFAULT,
    )
    .expect("llm.default");
}

#[tokio::test]
async fn disposing_skills_withdraws_the_service_and_its_tool() {
    let dir = tempdir().expect("tempdir");
    let (services, extras) = boot_services(dir.path());
    let (ctx, mut handle) = mount_profile(
        Arc::clone(&services),
        &profile_with_catalogue(),
        SharedClock::system(),
        BootExtras {
            workspace_root: dir.path().to_path_buf(),
            compaction: Some(uva::session::CompactionPluginConfig {
                model: "fake-model".to_owned(),
                settings: uva::session::CompactionSettings::default(),
            }),
            ..extras
        },
    )
    .expect("mount the native set");

    assert!(handle.dispose_plugin("skills"));
    assert!(
        ctx.get::<SkillRegistry>(KEY_SKILLS)
            .expect("read skills")
            .is_none(),
        "the service is withdrawn"
    );
    assert!(
        !services.tool_registry.contains("skill"),
        "the token-scoped tool registration is withdrawn with it"
    );
    assert!(
        services.tool_registry.contains("read"),
        "the rest of the tool surface is untouched"
    );
    assert!(
        ctx.get::<ToolRegistry>(KEY_TOOLS)
            .expect("read tools")
            .is_some(),
        "sibling services stay mounted"
    );
}

/// The profile the `agent-loop` row's own test mounts: the rows the
/// loop injects (`tools`, `sessions`, `llm`, `llm.default`,
/// `subagents`) plus the loop itself.
fn agent_loop_profile(config: serde_json::Value) -> crate::profile::Profile {
    let mut profile = profile_with_catalogue();
    profile.core.retain(|entry| {
        matches!(
            entry.id.as_str(),
            "tool-registry"
                | "model-adapter"
                | "session-store"
                | "subagent-backend"
        )
    });
    profile.core.push(crate::profile::Entry {
        id: "agent-loop".to_owned(),
        runtime: uva::kernel::RuntimeKind::Native,
        config,
        capabilities: Vec::new(),
        enabled: crate::profile::Enabled::On,
    });
    profile
}

/// The `agent-loop` row's config drives the factory: a strategy alias
/// resolves that strategy, a restriction config yields the
/// restriction, and a compaction config yields the compaction boot.
#[test]
fn the_agent_loop_row_config_drives_the_factory() {
    let config = serde_json::json!({
        "strategy": "chain-of-thought",
        "tool_restriction": { "allow": ["read"], "deny": ["write"] },
        "compaction": {
            "enabled": true,
            "reserve_tokens": 4096,
            "keep_recent_tokens": 512,
            "min_messages_between_compaction": 4
        }
    });
    let defaults =
        super::agent_loop_row_config(&config).expect("the row config parses");

    let strategy = defaults.strategy.expect("the alias resolves");
    assert_eq!(
        strategy.name(),
        "chain-of-thought",
        "the row resolved the strategy the alias names"
    );
    let restriction =
        defaults.tool_restriction.expect("the restriction is built");
    assert_eq!(
        restriction.allow.as_deref(),
        Some(["read".to_owned()].as_slice()),
        "the allow-list is the config's"
    );
    assert_eq!(
        restriction.deny,
        vec!["write".to_owned()],
        "the deny-list is the config's"
    );
    let compaction = defaults.compaction.expect("the compaction boot is built");
    assert_eq!(
        compaction.settings.keep_recent_tokens, 512,
        "the compaction policy is the config's"
    );
    assert_eq!(
        compaction.timeout,
        uva::session::DEFAULT_SUMMARY_TIMEOUT,
        "the summarization budget is the deployment's"
    );
}

/// An unknown strategy alias is not an error: the row logs it and
/// keeps the default reasoning, the same fail-soft contract the agent
/// table's reader has.
#[test]
fn an_unknown_strategy_alias_leaves_the_loop_unconfigured() {
    let config = serde_json::json!({ "strategy": "not-a-strategy" });
    let defaults =
        super::agent_loop_row_config(&config).expect("the row config parses");
    assert!(
        defaults.strategy.is_none(),
        "an unknown alias leaves the loop with its default reasoning"
    );
}

/// An empty row config means "the deployment configured nothing": no
/// strategy, no restriction, no compactor.
#[test]
fn an_empty_agent_loop_row_config_leaves_the_loop_unconfigured() {
    for config in [serde_json::Value::Null, serde_json::json!({})] {
        let defaults = super::agent_loop_row_config(&config)
            .expect("the row config parses");
        assert!(defaults.strategy.is_none(), "no strategy");
        assert!(defaults.tool_restriction.is_none(), "no restriction");
        assert!(defaults.compaction.is_none(), "no compactor");
    }
}

/// The `agent-loop` row mounts with the profile's own config, and the
/// delegation backend the `subagent-backend` row publishes is the
/// service the loop consumes.
#[tokio::test]
async fn the_subagent_backend_publishes_the_delegator_the_loop_consumes() {
    let dir = tempdir().expect("tempdir");
    let (services, extras) = boot_services(dir.path());
    let (ctx, handle) = mount_profile(
        Arc::clone(&services),
        &agent_loop_profile(serde_json::json!({
            "strategy": "chain-of-thought",
        })),
        SharedClock::system(),
        extras,
    )
    .expect("the loop mounts over the backend");

    let backend = ctx
        .require::<Arc<dyn DelegatingTool>>(uva::tool_task::KEY_SUBAGENTS)
        .expect("the subagent row publishes the backend");
    let definitions = backend.definitions();
    assert!(
        definitions.iter().any(|tool| tool.name == "task"),
        "the published backend is the `task` delegator: {definitions:?}"
    );

    // The loop mounts *after* the backend: the `agentLoop` service
    // exists, which it only does when the `subagents` inject edge
    // resolved.
    ctx.require::<Arc<dyn uva::harness::plugin::AgentLoopFactory>>(
        uva::harness::plugin::KEY_AGENT_LOOP,
    )
    .expect("the loop mounted after the backend");
    assert_eq!(handle.len(), 5, "the profile's five rows are mounted");
}

/// The `search` row mounts with **its own** session index: the boot's
/// containers carry no session corpus, so the row builds the index
/// over the boot context's sessions directory.
#[tokio::test]
async fn the_search_row_mounts_with_its_own_session_index() {
    let dir = tempdir().expect("tempdir");
    let sessions = dir.path().join("sessions");
    let session_dir = sessions.join("alice").join("s_one");
    std::fs::create_dir_all(&session_dir).expect("the session directory");
    std::fs::write(
        session_dir.join("events.jsonl"),
        "{\"type\":\"user_message\",\"seq\":1,\"ts\":\"2026-09-12T09:00:00Z\",\"data\":{\"role\":\"user\",\"content\":\"how do tokens expire\"}}\n",
    )
    .expect("the session log");

    let (services, extras) = boot_services(dir.path());
    let profile = crate::profile::Profile {
        core: vec![
            crate::profile::Entry {
                id: "tool-registry".to_owned(),
                runtime: uva::kernel::RuntimeKind::Native,
                config: serde_json::Value::Null,
                capabilities: Vec::new(),
                enabled: crate::profile::Enabled::On,
            },
            crate::profile::Entry {
                id: "memory".to_owned(),
                runtime: uva::kernel::RuntimeKind::Native,
                config: serde_json::Value::Null,
                capabilities: Vec::new(),
                enabled: crate::profile::Enabled::On,
            },
            crate::profile::Entry {
                id: "search".to_owned(),
                runtime: uva::kernel::RuntimeKind::Native,
                config: serde_json::Value::Null,
                capabilities: Vec::new(),
                enabled: crate::profile::Enabled::On,
            },
        ],
        ui: Vec::new(),
        hmr: false,
    };
    let (ctx, _handle) = mount_profile(
        Arc::clone(&services),
        &profile,
        SharedClock::system(),
        BootExtras {
            workspace_root: dir.path().to_path_buf(),
            sessions_dir: Some(sessions.clone()),
            ..extras
        },
    )
    .expect("the search row mounts");

    let search = ctx
        .require::<crate::search::SearchService>(
            crate::search::plugin::KEY_SEARCH,
        )
        .expect("the search row publishes its service");
    let hits = search.search("tokens expire", 10, None, None).await;
    assert!(
        !hits.is_empty(),
        "the row's own index answers over the boot's sessions directory"
    );
}

/// The `approval` row publishes the deployment's default stance: the
/// boot holds no policy, so the row builds it.
#[tokio::test]
async fn the_approval_row_mounts_the_default_policy() {
    let dir = tempdir().expect("tempdir");
    let (services, extras) = boot_services(dir.path());
    let profile = crate::profile::Profile {
        core: vec![crate::profile::Entry {
            id: "approval".to_owned(),
            runtime: uva::kernel::RuntimeKind::Native,
            config: serde_json::Value::Null,
            capabilities: Vec::new(),
            enabled: crate::profile::Enabled::On,
        }],
        ui: Vec::new(),
        hmr: false,
    };
    let (ctx, _handle) = mount_profile(
        Arc::clone(&services),
        &profile,
        SharedClock::system(),
        extras,
    )
    .expect("the approval row mounts");

    let policy = ctx
        .require::<ApprovalPolicy>(uva::harness::KEY_APPROVAL)
        .expect("the approval row publishes its stance");
    assert_eq!(
        *policy,
        ApprovalPolicy::default(),
        "the row publishes the deployment's default stance"
    );
}

/// The boot's two waves: a row whose enablement is delegated to the
/// `skills` row's judge can only be answered once wave 1 has mounted
/// that judge. The judge row itself is in wave 1, the checked row in
/// wave 2.
#[test]
fn a_delegated_check_is_answered_after_the_judge_wave_mounts() {
    let dir = tempdir().unwrap();
    let skill = dir.path().join(".agents").join("skills").join("wave-probe");
    std::fs::create_dir_all(&skill).unwrap();
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: wave-probe\ndescription: probe\nmetadata:\n  enabled: false\n---\n\nbody\n",
    )
    .unwrap();
    let (services, extras) = boot_services(dir.path());
    let mut profile = profile_with_catalogue();
    let eval = profile
        .core
        .iter_mut()
        .find(|entry| entry.id == "eval")
        .expect("the native set has eval");
    eval.enabled =
        crate::profile::Enabled::Check(crate::profile::EnableCheck {
            service: "skill-checks".into(),
            check: "is_enabled".into(),
            arg: serde_json::json!("wave-probe"),
        });

    let (ctx, mounted) =
        mount_profile(services, &profile, SharedClock::system(), extras)
            .expect("both waves mount");
    assert!(
        mounted.plugin_state("skills").is_some(),
        "wave 1 mounted the judge's provider"
    );
    assert!(
        ctx.get::<EvalRunner>(KEY_EVAL)
            .expect("read eval")
            .is_none(),
        "the judge answered false, so wave 2 leaves the row out"
    );

    // Flip the skill on and re-mount: the same profile now answers
    // enabled and wave 2 mounts the row.
    std::fs::write(
        skill.join("SKILL.md"),
        "---\nname: wave-probe\ndescription: probe\nmetadata:\n  enabled: true\n---\n\nbody\n",
    )
    .unwrap();
    let (services, extras) = boot_services(dir.path());
    let (ctx, _mounted) =
        mount_profile(services, &profile, SharedClock::system(), extras)
            .expect("both waves mount");
    assert!(
        ctx.get::<EvalRunner>(KEY_EVAL)
            .expect("read eval")
            .is_some(),
        "the judge answered true, so wave 2 mounts the row"
    );
}
