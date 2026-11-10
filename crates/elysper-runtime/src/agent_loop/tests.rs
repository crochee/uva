//! The `agent-loop` row's behavior: the compactor rules and the
//! delegation backend the loop consumes as a service.

use std::sync::Arc;

use elysper_harness::plugin::RunConfig;
use elysper_kernel::{Context, Plugin, PluginManifest, mount_all};
use elysper_provider::ModelProvider;
use elysper_session::{CompactionPlan, Compactor, TokenBudget};

use super::{AgentLoopDefaults, CompactionBoot, ServerAgentLoop};

/// A stateless compactor stand-in the service publishes; it never
/// plans a compaction.
struct NoopCompactor;

impl Compactor for NoopCompactor {
    fn plan(
        &self,
        _history: &[elysper_provider::Message],
        _budget: TokenBudget,
    ) -> Option<CompactionPlan> {
        None
    }

    fn summarize(
        &self,
        _plan: &CompactionPlan,
        _cancel: Arc<dyn elysper_core::CancelToken>,
    ) -> futures::future::BoxFuture<
        'static,
        Result<
            elysper_session::CompactionOutcome,
            elysper_session::CompactionError,
        >,
    > {
        Box::pin(async {
            Err(elysper_session::CompactionError::Provider(
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
    ) -> Result<Vec<elysper_kernel::EffectHandle>, elysper_kernel::PluginError>
    {
        let llm = ctx.provide(
            elysper_provider::plugin::KEY_LLM_DEFAULT,
            Arc::clone(&self.provider),
        )?;
        Ok(vec![llm])
    }
}

/// The fixture that publishes the `subagents` service the
/// `subagent-backend` row publishes.
struct SubagentsPlugin {
    manifest: PluginManifest,
    delegator: Arc<dyn elysper_harness::DelegatingTool>,
}

impl Plugin for SubagentsPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(
        &self,
        ctx: &Context,
    ) -> Result<Vec<elysper_kernel::EffectHandle>, elysper_kernel::PluginError>
    {
        let service = ctx.provide(
            elysper_tool_task::KEY_SUBAGENTS,
            Arc::clone(&self.delegator),
        )?;
        Ok(vec![service])
    }
}

fn provider_manifest() -> PluginManifest {
    let mut manifest =
        PluginManifest::new("fixture", elysper_kernel::PLUGIN_PROTOCOL_VERSION);
    manifest.runtime = elysper_kernel::RuntimeKind::Native;
    manifest
        .provides
        .push(elysper_provider::plugin::KEY_LLM_DEFAULT.to_string());
    manifest
}

fn subagents_manifest() -> PluginManifest {
    let mut manifest = PluginManifest::new(
        "subagent-backend",
        elysper_kernel::PLUGIN_PROTOCOL_VERSION,
    );
    manifest.runtime = elysper_kernel::RuntimeKind::Native;
    manifest
        .provides
        .push(elysper_tool_task::KEY_SUBAGENTS.to_string());
    manifest
}

/// Publish `llm.default` + `compaction` over a fresh context.
/// The mount handle stays alive for the test's scope: dropping
/// it withdraws the services.
fn context_with(
    provider: Arc<dyn ModelProvider>,
) -> (Context, elysper_kernel::MountHandle) {
    let ctx = Context::new();
    let plugin = ProviderPlugin {
        manifest: provider_manifest(),
        provider,
    };
    let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn Plugin>])
        .expect("the fixture mounts");
    (ctx, mounted)
}

/// A context that publishes the `subagents` service the
/// `subagent-backend` row publishes, next to `llm.default`.
///
/// The mount handle stays alive for the test's scope: dropping it
/// withdraws the services.
fn context_with_subagents(
    provider: Arc<dyn ModelProvider>,
    delegator: Arc<dyn elysper_harness::DelegatingTool>,
) -> (Context, elysper_kernel::MountHandle) {
    let ctx = Context::new();
    let plugins: Vec<Arc<dyn Plugin>> = vec![
        Arc::new(ProviderPlugin {
            manifest: provider_manifest(),
            provider,
        }),
        Arc::new(SubagentsPlugin {
            manifest: subagents_manifest(),
            delegator,
        }),
    ];
    let mounted =
        mount_all(&ctx, &plugins).expect("the subagent fixture mounts");
    (ctx, mounted)
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
        tool_registry: Arc::new(elysper_tool::ToolRegistry::new()),
        workspace_root: std::path::PathBuf::new(),
        descriptor: elysper_harness::default_descriptor("test".into()),
        prompt_context: Arc::new(elysper_harness::PromptContext::default()),
        max_iterations: 4,
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
    }
}

/// A run on the service's default provider shares the kernel's
/// one compaction instance across creates — but only when the
/// deployment has a compaction policy at all.
#[test]
fn default_runs_share_the_kernel_compactor() {
    let provider: Arc<dyn ModelProvider> =
        Arc::new(elysper_test_support::FakeProvider::text("ok"));
    let boot = CompactionBoot {
        settings: elysper_session::CompactionSettings::default(),
        sleeper: Arc::new(elysper_core::sleeper::FixedSleeper),
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
        Arc::new(elysper_test_support::FakeProvider::text("ok"));
    let other: Arc<dyn ModelProvider> =
        Arc::new(elysper_test_support::FakeProvider::text("other"));
    let boot = CompactionBoot {
        settings: elysper_session::CompactionSettings::default(),
        sleeper: Arc::new(elysper_core::sleeper::FixedSleeper),
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

/// The `subagents` service the `subagent-backend` row publishes
/// becomes one of the factory's delegators: the loop consumes the
/// delegation backend as a **service**, not as an object the host
/// pre-built, so the kernel orders the two rows.
#[test]
fn the_subagents_service_becomes_a_delegator() {
    let provider: Arc<dyn ModelProvider> =
        Arc::new(elysper_test_support::FakeProvider::text("ok"));
    let delegator: Arc<dyn elysper_harness::DelegatingTool> =
        Arc::new(elysper_tool_task::TaskDelegator::new(Arc::new(
            elysper_harness::AgentRegistry::new(),
        )));
    let (ctx, _mounted) =
        context_with_subagents(Arc::clone(&provider), Arc::clone(&delegator));
    let plugin = super::agent_loop_service_plugin(AgentLoopDefaults::default());
    let loop_ = plugin.resolve(&ctx).expect("the loop resolves");
    assert_eq!(
        loop_.defaults.delegators.len(),
        1,
        "the published backend is the loop's delegator"
    );
    assert!(
        Arc::ptr_eq(&loop_.defaults.delegators[0], &delegator),
        "the very backend the row published"
    );
}

/// The loop declares the delegation backend as an inject edge, so
/// the kernel mounts the loop after the `subagent-backend` row.
#[test]
fn the_loop_injects_the_subagents_service() {
    assert!(
        super::AGENT_LOOP_INJECTS.contains(&elysper_tool_task::KEY_SUBAGENTS),
        "the loop waits for the delegation backend: {:?}",
        super::AGENT_LOOP_INJECTS
    );
}
