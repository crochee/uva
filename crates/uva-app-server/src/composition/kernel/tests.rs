//! Kernel boot tests: profile assembly, the native mount, row
//! solidification, and unload semantics.

use std::sync::Arc;

use tempfile::tempdir;
use uva::{
    agent::AgentRegistry,
    attachment::{AttachmentStore, KEY_ATTACHMENTS},
    eval::{EvalRunner, KEY_EVAL},
    harness::ApprovalPolicy,
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
    KernelBootError,
    SharedClock,
    build_rows,
    mount_built,
    mount_profile,
    native_profile,
};
use crate::state::ProviderCatalogue;

/// The boot's containers: the shared tool registry, the peer-agent
/// registry, and the harness host.
fn services(root: &std::path::Path) -> Arc<BootServices> {
    services_over(root, uva::harness::Host::new())
}

/// The boot's containers with the production policy installed the way
/// the boot installs it: one registry first, then
/// [`Host::default_policy`] over it.
fn policy_services(root: &std::path::Path) -> Arc<BootServices> {
    let registry = uva::hook::HookRegistry::new();
    services_over(root, uva::harness::Host::default_policy(root, registry))
}

/// The boot's containers over `plugin_host`.
///
/// The `hooks` row publishes the host's registry (ADR 0028), so the
/// fixture takes it from the host's read accessor rather than
/// building a second table.
fn services_over(
    root: &std::path::Path,
    plugin_host: uva::harness::Host,
) -> Arc<BootServices> {
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
    let hook_registry = plugin_host.hook_registry().clone();
    Arc::new(BootServices {
        tool_registry: Arc::clone(&tools),
        agent_registry: Arc::clone(&agents),
        subagent_writers: Arc::new(
            crate::session::subagent_router::SubagentWriterSlot::new(),
        ),
        session_index: Arc::new(
            crate::session::session_index::SessionIndexSlot::new(),
        ),
        usage: Arc::new(crate::state::UsageMetrics::default()),
        hook_registry,
        plugin_host,
        file_system: Arc::new(crate::session::files::SessionFileSystem::new()),
        terminals: Arc::new(crate::session::terminals::SessionTerminals::new()),
        questions: Arc::new(crate::session::questions::SessionQuestions::new(
            Arc::new(crate::session::questions::RefusingQuestions::new(
                "the fixture mounts no operator",
            )),
        )),
        jobs: Arc::new(uva::tool_job::JobRegistry::new()),
        sessions: Arc::new(std::sync::OnceLock::new()),
        lsp_servers: Arc::new(crate::session::lsp::DeploymentLspServers::new(
            Vec::new(),
        )),
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

/// The boot's containers with the production policy installed, plus
/// the workspace they read.
fn policy_boot_services(
    root: &std::path::Path,
) -> (Arc<BootServices>, BootExtras) {
    (
        policy_services(root),
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
        "providers": {
            "fake": {
                "type": "demo",
                "api_key_env": "NONE",
                "default_model": "fake-model"
            }
        },
        "default_provider": "fake"
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
        "tool-ask",
        "tool-lsp",
        "model-adapter",
        "memory",
        "session-store",
        "sandbox",
        "tool-task",
        "context-compactor",
        "approval",
        "skills",
        "hooks",
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
    assert_eq!(ids.len(), 29, "no duplicate rows: {ids:?}");
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
    ctx.require::<uva::hook::HookRegistry>(uva::hook::KEY_HOOKS)
        .expect("hooks");

    assert_eq!(handle.len(), 29);
    assert!(handle.plugin_state("skills").is_some());
}

#[tokio::test]
async fn an_explicit_profile_mounts_only_the_rows_it_names() {
    let dir = tempdir().expect("tempdir");
    let (services, extras) = boot_services(dir.path());
    let profile = crate::profile::Profile {
        core: vec![crate::profile::Manifest::plain(
            "tool-registry",
            "tool-registry",
        )],
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

/// The `hooks` row is optional: a profile that omits it, and one that
/// switches it `Off`, both mount the loop. The kernel validates
/// `inject` keys statically and refuses the whole group, so a `hooks`
/// edge in `AGENT_LOOP_INJECTS` would fail boot for every profile
/// document written before the row existed — with no way into the
/// settings page that could have switched it back on.
#[tokio::test]
async fn a_profile_without_the_hooks_row_still_mounts_the_loop() {
    for off in [false, true] {
        let dir = tempdir().expect("tempdir");
        let (services, extras) = policy_boot_services(dir.path());
        let mut profile = profile_with_catalogue();
        if off {
            let row = profile
                .core
                .iter_mut()
                .find(|entry| entry.id == "hooks")
                .expect("the native set has hooks");
            row.disabled = true;
        } else {
            profile.core.retain(|entry| entry.id != "hooks");
        }
        let shape = if off { "off" } else { "absent" };
        let (ctx, _handle) = mount_profile(
            Arc::clone(&services),
            &profile,
            SharedClock::system(),
            extras,
        )
        .unwrap_or_else(|error| {
            panic!("the `{shape}` hooks row still mounts: {error}")
        });
        ctx.require::<Arc<dyn uva::harness::plugin::AgentLoopFactory>>(
            uva::harness::plugin::KEY_AGENT_LOOP,
        )
        .expect("agentLoop");
        assert!(
            ctx.get::<uva::hook::HookRegistry>(uva::hook::KEY_HOOKS)
                .expect("read hooks")
                .is_none(),
            "no external registry when the hooks row is {shape}"
        );
        // The deployment's policy does not depend on the row: the host
        // still carries its nine typed rows and the logging hook at
        // every event mount point.
        assert_eq!(
            services.plugin_host.registration_count(),
            9,
            "the default policy's typed tables survive a {shape} hooks row"
        );
        let observed = uva::hook::MountPoint::ALL
            .into_iter()
            .filter(|mount| {
                mount.is_event()
                    && services
                        .plugin_host
                        .hook_registry()
                        .has_entries_at(*mount)
            })
            .count();
        assert_eq!(
            observed, 11,
            "the logging hook still runs at every event point with a \
             {shape} hooks row"
        );
    }
}

/// A profile row whose `config` is a `ModelAdapterConfig` builds
/// the plugin from it — not from the host's deployment catalogue.
#[tokio::test]
async fn model_adapter_row_config_drives_the_plugin() {
    let dir = tempdir().expect("tempdir");
    let (services, extras) = boot_services(dir.path());
    let profile = crate::profile::Profile {
        core: vec![crate::profile::Manifest {
            config: serde_json::json!({
                "providers": {
                    "echo": {
                        "type": "demo",
                        "api_key_env": "NONE",
                        "default_model": "echo-model"
                    }
                },
                "default_provider": "echo"
            }),
            ..crate::profile::Manifest::plain("model-adapter", "model-adapter")
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
/// `subagents`, `hooks`) plus the loop itself.
fn agent_loop_profile(config: serde_json::Value) -> crate::profile::Profile {
    let mut profile = profile_with_catalogue();
    profile.core.retain(|entry| {
        matches!(
            entry.id.as_str(),
            "tool-registry"
                | "model-adapter"
                | "session-store"
                | "tool-task"
                | "hooks"
        )
    });
    profile.core.push(crate::profile::Manifest {
        config,
        ..crate::profile::Manifest::plain("agent-loop", "agent-loop")
    });
    profile
}

/// The `agent-loop` row's config drives the factory: a strategy alias
/// resolves that strategy, a restriction config yields the
/// restriction, and a compaction config yields the compaction boot.
#[test]
fn the_agent_loop_row_config_drives_the_factory() {
    let config = serde_json::json!({
        "strategy": "react",
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
        "react",
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
/// `tool-task` row's tool is an ordinary registry entry — the `task`
/// tool every run dispatches like any other tool.
#[tokio::test]
async fn the_task_row_registers_the_task_tool_and_the_loop_mounts() {
    let dir = tempdir().expect("tempdir");
    let (services, extras) = boot_services(dir.path());
    let (ctx, handle) = mount_profile(
        Arc::clone(&services),
        &agent_loop_profile(serde_json::json!({
            "strategy": "react",
        })),
        SharedClock::system(),
        extras,
    )
    .expect("the loop mounts over the tool brick");

    let tools = ctx
        .require::<ToolRegistry>(KEY_TOOLS)
        .expect("the tools service");
    assert!(
        tools.contains("task"),
        "the tool-task row files `task` in the registry"
    );
    for name in ["agents_list", "agents_message", "agents_interrupt"] {
        assert!(
            tools.contains(name),
            "the tool-task row files the control tool `{name}` in the registry"
        );
    }

    ctx.require::<Arc<dyn uva::harness::plugin::AgentLoopFactory>>(
        uva::harness::plugin::KEY_AGENT_LOOP,
    )
    .expect("the loop mounts without a delegation service");
    ctx.require::<uva::hook::HookRegistry>(uva::hook::KEY_HOOKS)
        .expect("the hooks row publishes the registry the loop injects");
    assert_eq!(handle.len(), 6, "the profile's six rows are mounted");
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
            crate::profile::Manifest::plain("tool-registry", "tool-registry"),
            crate::profile::Manifest::plain("memory", "memory"),
            crate::profile::Manifest::plain("search", "search"),
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

    // A profile-row-scoped tool is `Dynamic`: the name is the row's
    // to give away, so a later `Core` registration may take it.
    let tools = ctx
        .require::<uva::tool::ToolRegistry>(uva::tool::KEY_TOOLS)
        .expect("the tools service resolves");
    let record = tools
        .snapshot_with_provenance()
        .into_iter()
        .find(|record| record.metadata.name == "search")
        .expect("the brick's tool is registered");
    assert_eq!(
        record.provenance,
        uva::tool::ToolProvenance::Dynamic,
        "the `search` tool is the profile row's to give away"
    );
}

/// The `approval` row publishes the deployment's default stance: the
/// boot holds no policy, so the row builds it.
#[tokio::test]
async fn the_approval_row_mounts_the_default_policy() {
    let dir = tempdir().expect("tempdir");
    let (services, extras) = boot_services(dir.path());
    let profile = crate::profile::Profile {
        core: vec![crate::profile::Manifest::plain("approval", "approval")],
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

/// A parked required row is fatal on **every** mount path, not only
/// the boot's: an HMR subset mount that strands `agent-loop` (its
/// provider is not in the scope) must fail the mount, so a settings
/// edit cannot leave the host without its model loop looking alive.
#[tokio::test]
async fn a_subset_mount_that_parks_a_required_row_is_fatal() {
    let dir = tempdir().expect("tempdir");
    let (services, extras) = boot_services(dir.path());
    let profile = crate::profile::Profile {
        core: vec![crate::profile::Manifest::plain("agent-loop", "agent-loop")],
        ui: Vec::new(),
        hmr: false,
    };
    let context = uva::kernel::Context::new();
    let realms = Arc::new(uva::kernel::Realms::default());
    let built = build_rows(
        &services,
        &realms,
        SharedClock::system(),
        &extras,
        &profile,
    )
    .expect("the row builds without its provider present");
    let error = mount_built(&context, &realms, built, &profile)
        .expect_err("a parked required row is fatal on the HMR path");
    assert!(
        matches!(
            &error,
            KernelBootError::RequiredParked { id, .. } if id == "agent-loop"
        ),
        "the audit names the stranded row: {error:?}"
    );
}

// --- the one interception table (ADR 0028) ----------------------------

/// Counts the `before_run` observations it receives.
struct CountBeforeRun(Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl uva::harness::Hook for CountBeforeRun {
    fn name(&self) -> &str {
        "count-before-run"
    }

    async fn observe(
        &self,
        event: &uva::steering::HookEvent,
        _ctx: &uva::harness::HookContext<'_>,
    ) -> uva::steering::HookDecision {
        if matches!(event, uva::steering::HookEvent::BeforeRun { .. }) {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        uva::steering::HookDecision::Allow
    }
}

/// Build one agent through the mounted `agentLoop` factory and run it
/// to completion over the boot's host.
async fn run_one_turn(
    ctx: &uva::kernel::Context,
    services: &BootServices,
    workspace: &std::path::Path,
) {
    let factory = ctx
        .require::<Arc<dyn uva::harness::plugin::AgentLoopFactory>>(
            uva::harness::plugin::KEY_AGENT_LOOP,
        )
        .expect("agentLoop");
    let provider: Arc<dyn ModelProvider> =
        Arc::new(uva::test_support::FakeProvider::text("ok"));
    let run = uva::harness::plugin::RunConfig {
        provider,
        tool_registry: Arc::new(ToolRegistry::new()),
        workspace_root: workspace.to_path_buf(),
        descriptor: uva::harness::default_descriptor(String::new()),
        prompt_context: Arc::new(uva::harness::PromptContext::default()),
        max_iterations: 2,
        tool_surface: None,
        strategy: None,
        tool_restriction: None,
        log_writer: None,
        context_manager: None,
        compactor: None,
        host: Some(services.plugin_host.clone()),
        spawner: None,
    };
    let agent = factory.create(&run);
    let cancel: Arc<dyn uva::core::CancelToken> =
        uva::core::AtomicCancelToken::shared();
    let mut events = agent
        .run(uva::harness::AgentInput::text("hi"), cancel)
        .await;
    while futures::StreamExt::next(&mut events).await.is_some() {}
}

/// The `hooks` row publishes the boot's own table: a registration made
/// through the service lands on the host the run assembly receives, so
/// the run's seam layer observes it (ADR 0028, ticket 02).
#[tokio::test]
async fn an_out_of_tree_registration_on_the_hooks_row_reaches_a_run() {
    let dir = tempdir().expect("tempdir");
    let (services, extras) = policy_boot_services(dir.path());
    let profile = profile_with_catalogue();
    let (ctx, _handle) = mount_profile(
        Arc::clone(&services),
        &profile,
        SharedClock::system(),
        extras,
    )
    .expect("the native set mounts");

    let registry = ctx
        .require::<uva::hook::HookRegistry>(uva::hook::KEY_HOOKS)
        .expect("the hooks row publishes its table");
    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let _registration = registry.register_hook_at(
        uva::hook::MountPoint::BeforeRun,
        Arc::new(CountBeforeRun(Arc::clone(&calls))),
        uva::hook::RegistrationOrigin::Plugin,
    );

    // The service's table and the host's are one instance: the row
    // publishes a clone of the registry the host carries.
    assert!(
        services
            .plugin_host
            .hook_registry()
            .has_entries_at(uva::hook::MountPoint::BeforeRun),
        "the host dispatches the table the hooks row published"
    );

    run_one_turn(&ctx, &services, dir.path()).await;
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "an out-of-tree before_run observer reaches a real run"
    );
}

/// A hot remount of the `hooks` row republishes the boot's table: a
/// registration made before the remount still reaches a run created by
/// the remounted loop.
#[tokio::test]
async fn remounting_the_hooks_row_reuses_the_boots_table() {
    let dir = tempdir().expect("tempdir");
    let (services, extras) = policy_boot_services(dir.path());
    let profile = profile_with_catalogue();
    let (ctx, handle) = mount_profile(
        Arc::clone(&services),
        &profile,
        SharedClock::system(),
        extras.clone(),
    )
    .expect("the native set mounts");

    let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let registration = ctx
        .require::<uva::hook::HookRegistry>(uva::hook::KEY_HOOKS)
        .expect("the hooks row publishes its table")
        .register_hook_at(
            uva::hook::MountPoint::BeforeRun,
            Arc::new(CountBeforeRun(Arc::clone(&calls))),
            uva::hook::RegistrationOrigin::Plugin,
        );
    drop(handle);

    let (ctx, _handle) = mount_profile(
        Arc::clone(&services),
        &profile,
        SharedClock::system(),
        extras,
    )
    .expect("the remount succeeds");

    run_one_turn(&ctx, &services, dir.path()).await;
    assert_eq!(
        calls.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "the remounted row publishes the same table, not a fresh one"
    );
    drop(registration);
}

// --- the plugin-tree audit ------------------------------------------

/// A healthy tree reports nothing: every row is usable, and the audit
/// is the exception report, not the tree itself (dsh `auditRows`).
#[test]
fn the_audit_of_a_healthy_tree_is_empty() {
    let dir = tempdir().expect("tempdir");
    let (services, extras) = boot_services(dir.path());
    let profile = profile_with_catalogue();
    let (ctx, _handle) = mount_profile(
        Arc::clone(&services),
        &profile,
        SharedClock::system(),
        extras,
    )
    .expect("the native set mounts");

    let audit = super::plugin_tree_audit(&ctx, &profile, &services);
    assert!(
        audit.is_empty(),
        "a healthy tree reports nothing: {audit:?}"
    );
}

/// A row waiting for a service the composition does not supply is
/// `pending`, and the audit names the key it waits for — the same key
/// the mount's own audit parks the row on.
#[test]
fn the_audit_names_a_pending_row_and_its_missing_key() {
    let dir = tempdir().expect("tempdir");
    let (services, extras) = boot_services(dir.path());
    let mut profile = profile_with_catalogue();
    let row = profile
        .core
        .iter_mut()
        .find(|entry| entry.id == "tool-read")
        .expect("the native set has tool-read");
    row.inject
        .get_or_insert_with(Vec::new)
        .push("no.such.service".to_string());
    let (ctx, _handle) = mount_profile(
        Arc::clone(&services),
        &profile,
        SharedClock::system(),
        extras,
    )
    .expect("a non-required row parks instead of failing the mount");

    // The kernel holds the row in its pending state.
    assert_eq!(
        _handle.plugin_state("tool-read"),
        Some(uva::kernel::PluginState::Pending),
        "the row parks waiting for its service"
    );

    let audit = super::plugin_tree_audit(&ctx, &profile, &services);
    let row = audit
        .iter()
        .find(|row| row.id == "tool-read")
        .expect("the audit reports the parked row");
    assert_eq!(row.state, "pending");
    assert_eq!(
        row.missing,
        vec!["no.such.service".to_string()],
        "the audit names the key the row waits for"
    );
    assert!(
        audit.iter().all(|row| row.state == "pending"),
        "no row is failed: every row the mount reached is pending or \
         usable: {audit:?}"
    );
}
