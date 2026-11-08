//! The deployment's `agent-loop`: an [`AgentLoopFactory`] whose
//! optional knobs come from one mounted place instead of four
//! hand-threaded call sites.
//!
//! [`server_agent_loop`] resolves the `llm.default` and `compaction`
//! services from the kernel context and combines them with the
//! deployment's [`AgentLoopDefaults`] (strategy, tool surface,
//! restriction, spawner, compaction policy …) into one factory. A run
//! then supplies only its per-run inputs; every `None` the run leaves
//! is filled from the defaults, so the assembly sites the server used
//! to maintain cannot drift apart anymore.
//!
//! # The compactor rule
//!
//! [`RunConfig::compactor`](elysper_harness::plugin::RunConfig) is
//! `Option`, so the factory fills it: a run on the deployment's
//! default provider shares the kernel's one compaction instance, and
//! a run that resolved an explicit provider gets a fresh
//! SummaryCompactor bound to
//! that provider (its summarization model is the provider's own).
//! A deployment without a valid compaction policy runs explicit
//! providers without a compactor, exactly like the old per-run
//! composition did.

use std::{sync::Arc, time::Duration};

use elysper_harness::plugin::{AgentLoopFactory, RunConfig};
use elysper_kernel::{Context, PluginError};
use elysper_provider::{ModelProvider, plugin::KEY_LLM_DEFAULT};
use elysper_session::{CompactionSettings, Compactor, SummaryCompactor};

/// The policy a fresh per-run compactor is built from when a run
/// names an explicit provider.
#[derive(Clone)]
pub struct CompactionBoot {
    /// The deployment's `[agents.<name>.compaction]` settings.
    pub settings: CompactionSettings,
    /// The sleeper the summarization retries wait on.
    pub sleeper: Arc<dyn elysper_core::sleeper::Sleeper>,
    /// The wall-clock budget of one summarization call.
    pub timeout: Duration,
}

/// The deployment-level knobs the factory fills a run's `None`s
/// with. The service-owned values (default provider, compaction
/// instance) are **not** here: they resolve from the kernel context
/// at mount time, so a remounted `model-adapter` is picked up without
/// rebuilding the defaults.
#[derive(Clone, Default)]
pub struct AgentLoopDefaults {
    /// Delegating tools (the `task` delegator); a run that installs
    /// none gets these.
    pub delegators: Vec<Arc<dyn elysper_harness::DelegatingTool>>,
    /// Deployment `[tools]` surface policy.
    pub tool_surface: Option<elysper_tool::ToolSurfacePolicy>,
    /// Deployment reasoning loop.
    pub strategy: Option<Arc<dyn elysper_harness::ReasoningStrategy>>,
    /// Deployment allow/deny list.
    pub tool_restriction: Option<Arc<elysper_tool::ToolRestriction>>,
    /// The session's one writer; `None` keeps the run's own.
    pub log_writer: Option<Arc<elysper_session::LogWriter>>,
    /// Writer source for this agent's delegated children.
    pub subagent_writers: Option<Arc<dyn elysper_harness::SubagentWriters>>,
    /// The deployment's one context manager (request-level
    /// truncation); `None` keeps the run's own.
    pub context_manager: Option<Arc<dyn elysper_context::ContextManager>>,
    /// The deployment's plugin host.
    pub host: Option<elysper_harness::Host>,
    /// Where the run's detached work executes.
    pub spawner: Option<Arc<dyn elysper_core::spawn::Spawner>>,
    /// The policy a per-run compactor is built from when the run
    /// names an explicit provider; `None` leaves such runs without a
    /// compactor.
    pub compaction: Option<CompactionBoot>,
}

/// Resolve the `agentLoop` factory's service inputs and combine them
/// with `defaults`.
///
/// # Errors
///
/// [`PluginError`] when `llm.default` is not mounted — the plugin's
/// own apply reports it, and the kernel parks the loop until a
/// provider returns.
pub fn server_agent_loop(
    ctx: &Context,
    defaults: AgentLoopDefaults,
) -> Result<Arc<dyn AgentLoopFactory>, PluginError> {
    let default_provider = ctx
        .require::<Arc<dyn ModelProvider>>(KEY_LLM_DEFAULT)
        .map(|wrapped| Arc::clone(&*wrapped))?;
    // The compactor is built here, over the provider this factory
    // resolved — not taken from a separate service: a provider swap
    // remounts this plugin, and the new factory must summarize through
    // the new provider. All default-provider runs share this instance.
    let compactor = build_compactor(&default_provider, &defaults);
    Ok(Arc::new(ServerAgentLoop {
        defaults,
        default_provider,
        compactor,
    }))
}

/// The mounted `agent-loop` factory: one per deployment, rebuilt by a
/// remount (which is how a provider swap reaches new runs).
pub(crate) struct ServerAgentLoop {
    defaults: AgentLoopDefaults,
    default_provider: Arc<dyn ModelProvider>,
    /// The deployment's one compaction engine over the resolved
    /// default provider; `None` without a valid policy.
    compactor: Option<Arc<dyn Compactor>>,
}

impl AgentLoopFactory for ServerAgentLoop {
    fn create(&self, run: &RunConfig) -> Arc<dyn elysper_harness::Agent> {
        Arc::new(self.resolve(run).build())
    }
}

impl ServerAgentLoop {
    /// The run with every `None` filled from the defaults and the
    /// compactor rule applied. [`Self::create`] is this plus
    /// [`RunConfig::build`].
    fn resolve(&self, run: &RunConfig) -> RunConfig {
        let mut config = run.clone();
        if config.tool_surface.is_none() {
            config.tool_surface = self.defaults.tool_surface.clone();
        }
        if config.strategy.is_none() {
            config.strategy = self.defaults.strategy.clone();
        }
        if config.tool_restriction.is_none() {
            config.tool_restriction = self.defaults.tool_restriction.clone();
        }
        if config.log_writer.is_none() {
            config.log_writer = self.defaults.log_writer.clone();
        }
        if config.subagent_writers.is_none() {
            config.subagent_writers = self.defaults.subagent_writers.clone();
        }
        if config.context_manager.is_none() {
            config.context_manager = self.defaults.context_manager.clone();
        }
        if config.host.is_none() {
            config.host = self.defaults.host.clone();
        }
        if config.delegators.is_empty() {
            config.delegators = self.defaults.delegators.clone();
        }
        // Deliberate exception to the fill-only rule: the deployment's
        // spawner replaces a run's, because every run this factory
        // serves detaches onto the server's runtime (the old assembly
        // installed it unconditionally too).
        if let Some(spawner) = &self.defaults.spawner {
            config.spawner = Some(Arc::clone(spawner));
        }
        if config.compactor.is_none() {
            config.compactor =
                if Arc::ptr_eq(&config.provider, &self.default_provider) {
                    // The deployment's one engine, shared across
                    // default-provider runs.
                    self.compactor.clone()
                } else {
                    build_compactor(&config.provider, &self.defaults)
                };
        }
        config
    }
}

/// A compactor bound to `provider`, or `None` when the deployment
/// configured no (valid, enabled) policy. The explicit-selection rule:
/// its summarization model is the provider's own.
fn build_compactor(
    provider: &Arc<dyn ModelProvider>,
    defaults: &AgentLoopDefaults,
) -> Option<Arc<dyn Compactor>> {
    let boot = defaults.compaction.as_ref()?;
    if !boot.settings.enabled {
        tracing::debug!("compaction disabled; the run keeps no compactor");
        return None;
    }
    if let Err(error) = boot.settings.validate() {
        tracing::warn!(
            %error,
            "invalid CompactionSettings; the run keeps no compactor"
        );
        return None;
    }
    let model = provider.model_config().name;
    Some(Arc::new(
        SummaryCompactor::new(Arc::clone(provider), model)
            .with_settings(boot.settings)
            .with_sleeper(Arc::clone(&boot.sleeper))
            .with_timeout(boot.timeout),
    ))
}

/// The manifest every [`AgentLoopServicePlugin`] carries: the native
/// `agent-loop` plugin providing `agentLoop` over the services the
/// factory reads.
#[must_use]
pub fn agent_loop_manifest() -> elysper_kernel::PluginManifest {
    let mut manifest = elysper_kernel::PluginManifest::new(
        "agent-loop",
        elysper_kernel::PLUGIN_PROTOCOL_VERSION,
    );
    manifest.runtime = elysper_kernel::RuntimeKind::Native;
    manifest
        .provides
        .push(elysper_harness::plugin::KEY_AGENT_LOOP.to_string());
    manifest.inject = AGENT_LOOP_INJECTS
        .iter()
        .map(|key| (*key).to_string())
        .collect();
    manifest
}

/// The `agent-loop` service plugin: resolves its inputs at mount and
/// publishes the deployment's [`AgentLoopFactory`].
///
/// `apply` resolves `llm.default` and `compaction` from the context
/// — so the dependency order (the manifest's `inject`) mounts the
/// providers first — and a mount failure parks the loop until both
/// return. A remount re-runs `apply`, which is how a provider swap
/// reaches new runs without restarting the process.
pub struct AgentLoopServicePlugin {
    manifest: elysper_kernel::PluginManifest,
    defaults: AgentLoopDefaults,
}

/// Build the `agent-loop` service plugin over `defaults`.
#[must_use]
pub fn agent_loop_service_plugin(
    defaults: AgentLoopDefaults,
) -> AgentLoopServicePlugin {
    AgentLoopServicePlugin {
        manifest: agent_loop_manifest(),
        defaults,
    }
}

impl elysper_kernel::Plugin for AgentLoopServicePlugin {
    fn manifest(&self) -> &elysper_kernel::PluginManifest {
        &self.manifest
    }

    fn apply(
        &self,
        ctx: &Context,
    ) -> Result<Vec<elysper_kernel::EffectHandle>, PluginError> {
        let factory = server_agent_loop(ctx, self.defaults.clone())?;
        let service =
            ctx.provide(elysper_harness::plugin::KEY_AGENT_LOOP, factory)?;
        Ok(vec![service])
    }
}

/// The services the loop's plugin depends on, declared as the
/// manifest's `inject` edges so the kernel orders the mounts and parks
/// the loop while any provider is gone.
///
/// `llm` is the remount-cascade edge (a changed provider remounts the
/// loop), `llm.default` is the handle the factory actually resolves,
/// and `tools` / `sessions` are the loop's other inputs. The
/// compactor is built by the factory itself, so `compaction` is not
/// injected.
pub const AGENT_LOOP_INJECTS: [&str; 4] =
    ["llm", "llm.default", "tools", "sessions"];

#[cfg(test)]
mod tests {
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

    /// Publish `llm.default` + `compaction` over a fresh context.
    /// The mount handle stays alive for the test's scope: dropping
    /// it withdraws the services.
    fn context_with(
        provider: Arc<dyn ModelProvider>,
    ) -> (Context, elysper_kernel::MountHandle) {
        struct FixturePlugin {
            manifest: PluginManifest,
            provider: Arc<dyn ModelProvider>,
        }
        impl Plugin for FixturePlugin {
            fn manifest(&self) -> &PluginManifest {
                &self.manifest
            }

            fn apply(
                &self,
                ctx: &Context,
            ) -> Result<
                Vec<elysper_kernel::EffectHandle>,
                elysper_kernel::PluginError,
            > {
                let llm = ctx.provide(
                    elysper_provider::plugin::KEY_LLM_DEFAULT,
                    Arc::clone(&self.provider),
                )?;
                Ok(vec![llm])
            }
        }
        let mut manifest = PluginManifest::new(
            "fixture",
            elysper_kernel::PLUGIN_PROTOCOL_VERSION,
        );
        manifest.runtime = elysper_kernel::RuntimeKind::Native;
        manifest
            .provides
            .push(elysper_provider::plugin::KEY_LLM_DEFAULT.to_string());
        let ctx = Context::new();
        let plugin = FixturePlugin { manifest, provider };
        let mounted = mount_all(&ctx, &[Arc::new(plugin) as Arc<dyn Plugin>])
            .expect("the fixture mounts");
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

        let kernel_instance =
            loop_.resolve(&run(Arc::clone(&default))).compactor;
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
}
