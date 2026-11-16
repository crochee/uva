//! The `agent-loop` row's behavior: the compactor rules and the
//! delegation backend the loop consumes as a service.

use std::sync::Arc;

use uva_harness::plugin::RunConfig;
use uva_kernel::{Context, Plugin, PluginManifest, mount_all};
use uva_provider::ModelProvider;
use uva_session::{CompactionPlan, Compactor, TokenBudget};

use super::{AgentLoopDefaults, CompactionBoot, ServerAgentLoop};

/// A stateless compactor stand-in the service publishes; it never
/// plans a compaction.
struct NoopCompactor;

impl Compactor for NoopCompactor {
    fn plan(
        &self,
        _history: &[uva_provider::Message],
        _budget: TokenBudget,
    ) -> Option<CompactionPlan> {
        None
    }

    fn summarize(
        &self,
        _plan: &CompactionPlan,
        _cancel: Arc<dyn uva_core::CancelToken>,
    ) -> futures::future::BoxFuture<
        'static,
        Result<uva_session::CompactionOutcome, uva_session::CompactionError>,
    > {
        Box::pin(async {
            Err(uva_session::CompactionError::Provider(
                "the fixture never plans a compaction".to_owned(),
            ))
        })
    }
}

/// The fixture that publishes `llm.default`.
struct ProviderPlugin {
    manifest: PluginManifest,
    provider: Arc<dyn ModelProvider>,
}

impl Plugin for ProviderPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(
        &self,
        ctx: &Context,
    ) -> Result<Vec<uva_kernel::EffectHandle>, uva_kernel::PluginError> {
        let llm = ctx.provide(
            uva_provider::plugin::KEY_LLM_DEFAULT,
            Arc::clone(&self.provider),
        )?;
        Ok(vec![llm])
    }
}

fn provider_manifest() -> PluginManifest {
    let mut manifest =
        PluginManifest::new("fixture", uva_kernel::PLUGIN_PROTOCOL_VERSION);
    manifest.runtime = uva_kernel::RuntimeKind::Native;
    manifest
        .provides
        .push(uva_provider::plugin::KEY_LLM_DEFAULT.to_string());
    manifest
}

/// Publish `llm.default` + `compaction` over a fresh context.
/// The mount handle stays alive for the test's scope: dropping
/// it withdraws the services.
fn context_with(
    provider: Arc<dyn ModelProvider>,
) -> (Context, uva_kernel::MountHandle) {
    let ctx = Context::new();
    let plugin = ProviderPlugin {
        manifest: provider_manifest(),
        provider,
    };
    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn Plugin>])
        .expect("the fixture mounts");
    (ctx, mounted)
}

/// The `agent-loop` row declares no delegation edge: `task` is an
/// ordinary tool brick, not a service the loop consumes.
#[test]
fn the_loop_does_not_inject_a_delegation_service() {
    assert!(
        !super::AGENT_LOOP_INJECTS.contains(&"subagents"),
        "delegation is a registry tool now: {:?}",
        super::AGENT_LOOP_INJECTS
    );
}

/// The factory under test, built over a context whose services
/// resolve from `provider`.
fn loop_for(
    provider: &Arc<dyn ModelProvider>,
    compaction: Option<CompactionBoot>,
) -> ServerAgentLoop {
    let (_ctx, _mounted) = context_with(Arc::clone(provider));
    let defaults = AgentLoopDefaults {
        compaction: compaction.clone(),
        ..AgentLoopDefaults::default()
    };
    let compactor = defaults
        .compaction
        .as_ref()
        .map(|_| Arc::new(NoopCompactor) as Arc<dyn Compactor>);
    ServerAgentLoop {
        defaults,
        default_provider: Arc::clone(provider),
        compactor,
    }
}

fn run(provider: Arc<dyn ModelProvider>) -> RunConfig {
    RunConfig {
        provider,
        tool_registry: Arc::new(uva_tool::ToolRegistry::new()),
        workspace_root: std::path::PathBuf::new(),
        descriptor: uva_harness::default_descriptor("test".into()),
        prompt_context: Arc::new(uva_harness::PromptContext::default()),
        max_iterations: 4,
        tool_surface: None,
        strategy: None,
        tool_restriction: None,
        log_writer: None,
        context_manager: None,
        compactor: None,
        host: None,
        spawner: None,
    }
}

/// A run on the service's default provider shares the kernel's
/// one compaction instance across creates — but only when the
/// deployment has a compaction policy at all.
#[test]
fn default_runs_share_the_kernel_compactor() {
    let provider: Arc<dyn ModelProvider> =
        Arc::new(uva_test_support::FakeProvider::text("ok"));
    let boot = CompactionBoot {
        settings: uva_session::CompactionSettings::default(),
        sleeper: Arc::new(uva_core::sleeper::FixedSleeper),
        timeout: std::time::Duration::from_secs(1),
    };
    let loop_ = loop_for(&provider, Some(boot));

    let first = loop_.resolve(&run(Arc::clone(&provider)));
    let second = loop_.resolve(&run(Arc::clone(&provider)));
    let (Some(a), Some(b)) = (first.compactor, second.compactor) else {
        panic!("a default run installs the kernel compactor");
    };
    assert!(
        Arc::ptr_eq(&a, &b),
        "one compaction instance, shared across runs"
    );
}

/// A run that resolved an explicit provider gets its own
/// compactor — built from the boot policy, not the kernel
/// instance — and none when the deployment configured no policy.
#[test]
fn explicit_provider_runs_get_their_own_compactor() {
    let default: Arc<dyn ModelProvider> =
        Arc::new(uva_test_support::FakeProvider::text("ok"));
    let other: Arc<dyn ModelProvider> =
        Arc::new(uva_test_support::FakeProvider::text("other"));
    let boot = CompactionBoot {
        settings: uva_session::CompactionSettings::default(),
        sleeper: Arc::new(uva_core::sleeper::FixedSleeper),
        timeout: std::time::Duration::from_secs(1),
    };
    let loop_ = loop_for(&default, Some(boot));

    let kernel_instance = loop_.resolve(&run(Arc::clone(&default))).compactor;
    let on_other = loop_.resolve(&run(Arc::clone(&other)));
    let (Some(kernel_instance), Some(own)) =
        (kernel_instance, on_other.compactor)
    else {
        panic!("both runs install a compactor");
    };
    assert!(
        !Arc::ptr_eq(&kernel_instance, &own),
        "an explicit provider's run compacts through its own engine"
    );

    // No policy configured: an explicit provider runs without a
    // compactor, exactly like the old per-run composition.
    let bare = loop_for(&default, None);
    let on_other = bare.resolve(&run(Arc::clone(&other)));
    assert!(
        on_other.compactor.is_none(),
        "no compaction policy means no per-run compactor"
    );
}

/// A gate fixture: it never decides anything.
struct NoopGate;

#[async_trait::async_trait]
impl uva_hook::Gate for NoopGate {
    fn name(&self) -> &str {
        "noop"
    }

    async fn check(
        &self,
        _event: &uva_hook::HookEvent,
        _ctx: &uva_hook::HookContext<'_>,
    ) -> Result<uva_hook::HookDecision, String> {
        Ok(uva_hook::HookDecision::Allow)
    }
}

/// The `hooks` service the `hooks` row publishes reaches the loop's
/// defaults as a **shared** table: the row's registrations are the
/// loop's, and dropping a registration withdraws it from both.
#[test]
fn the_hooks_service_reaches_the_loop() {
    let provider: Arc<dyn ModelProvider> =
        Arc::new(uva_test_support::FakeProvider::text("ok"));
    let ctx = Context::new();
    let registry = uva_hook::HookRegistry::new();
    let registration = registry
        .register_gate(uva_hook::seam::TOOL_PRE, Arc::new(NoopGate))
        .expect("the gate registers on the row's table");
    let plugins: Vec<Arc<dyn Plugin>> = vec![
        Arc::new(ProviderPlugin {
            manifest: provider_manifest(),
            provider,
        }),
        Arc::new(uva_hook::HookPlugin::new(registry)),
    ];
    let mounted = mount_all(&ctx, &plugins).expect("the fixtures mount");

    let plugin = super::agent_loop_service_plugin(AgentLoopDefaults::default());
    let loop_ = plugin.resolve(&ctx).expect("the loop resolves");
    let hooks = loop_
        .defaults
        .hooks
        .clone()
        .expect("the loop resolved the hooks service");
    assert!(
        !hooks.is_empty(),
        "the loop shares the row's table, so its registration is there"
    );
    drop(registration);
    assert!(
        hooks.is_empty(),
        "one dropped handle withdraws the row from every clone"
    );

    // Unmounting the row withdraws the service; a remount re-resolves
    // the loop against the new table.
    drop(mounted);
    assert!(
        ctx.get::<uva_hook::HookRegistry>(uva_hook::KEY_HOOKS)
            .expect("the lookup itself succeeds")
            .is_none(),
        "dropping the mount handle withdraws the hooks service"
    );
}

/// The `hooks` row is optional, not an inject edge: the kernel rejects
/// a group whose `inject` key has no provider, so an edge here would
/// make a profile document written before the row existed — or one
/// that switches the row off — fail at boot with no way into the
/// settings page. A deployment without the row still gets a loop.
#[test]
fn the_loop_boots_without_the_hooks_row() {
    let provider: Arc<dyn ModelProvider> =
        Arc::new(uva_test_support::FakeProvider::text("ok"));
    let (ctx, _mounted) = context_with(provider);
    let plugin = super::agent_loop_service_plugin(AgentLoopDefaults::default());

    assert!(
        !super::AGENT_LOOP_INJECTS.contains(&uva_hook::KEY_HOOKS),
        "hooks is resolved opportunistically, not injected: {:?}",
        super::AGENT_LOOP_INJECTS
    );
    assert!(
        !plugin
            .manifest()
            .inject
            .iter()
            .any(|key| key == uva_hook::KEY_HOOKS),
        "the manifest's inject list has no hooks edge: {:?}",
        plugin.manifest().inject
    );

    let loop_ = plugin
        .resolve(&ctx)
        .expect("a context without the hooks row resolves the loop");
    assert!(
        loop_.defaults.hooks.is_none(),
        "no hooks row means no external registrations"
    );
}
