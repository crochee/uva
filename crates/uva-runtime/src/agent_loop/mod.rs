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
//! [`RunConfig::compactor`](uva_harness::plugin::RunConfig) is
//! `Option`, so the factory fills it: a run on the deployment's
//! default provider shares the kernel's one compaction instance, and
//! a run that resolved an explicit provider gets a fresh
//! SummaryCompactor bound to
//! that provider (its summarization model is the provider's own).
//! A deployment without a valid compaction policy runs explicit
//! providers without a compactor, exactly like the old per-run
//! composition did.
//!
//! # The task tool is not a service
//!
//! Delegation is an ordinary tool brick (`tool-task`) that the boot
//! mounts into the tool registry; the loop has no delegation edge.
//!
//! # The hook registry
//!
//! The external hook registry is a service too: the `hooks` row
//! publishes [`uva_hook::KEY_HOOKS`] and this row resolves it at
//! mount into the defaults, which `ServerAgentLoop::create` hands
//! to every run's [`uva_harness::seams::Seams`].
//!
//! The lookup is **opportunistic** (`ctx.get`), not an `inject` edge:
//! the kernel validates `inject` keys statically and refuses the whole
//! group when a provider is absent, so an edge here would make the
//! `hooks` row mandatory — a profile document written before the row
//! existed, or one that switches it off, would fail at boot with no
//! way back in. Without the service the loop simply runs with no
//! external registrations. The native profile still lists `hooks`
//! before `agent-loop`, so the row mounts first when it is there.

use std::{sync::Arc, time::Duration};

use uva_harness::plugin::{AgentLoopFactory, RunConfig};
use uva_kernel::{Context, PluginError};
use uva_provider::{ModelProvider, plugin::KEY_LLM_DEFAULT};
use uva_session::{CompactionSettings, Compactor, SummaryCompactor};

/// The policy a fresh per-run compactor is built from when a run
/// names an explicit provider.
#[derive(Clone)]
pub struct CompactionBoot {
    /// The deployment's `[agents.<name>.compaction]` settings.
    pub settings: CompactionSettings,
    /// The sleeper the summarization retries wait on.
    pub sleeper: Arc<dyn uva_core::sleeper::Sleeper>,
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
    /// Deployment `[tools]` surface policy.
    pub tool_surface: Option<uva_tool::ToolSurfacePolicy>,
    /// Deployment reasoning loop.
    pub strategy: Option<Arc<dyn uva_harness::ReasoningStrategy>>,
    /// Deployment allow/deny list.
    pub tool_restriction: Option<Arc<uva_tool::ToolRestriction>>,
    /// The session's one writer; `None` keeps the run's own.
    pub log_writer: Option<Arc<dyn uva_session::SessionLog>>,
    /// The deployment's one context manager (request-level
    /// truncation); `None` keeps the run's own.
    pub context_manager: Option<Arc<dyn uva_context::ContextManager>>,
    /// The deployment's plugin host.
    pub host: Option<uva_harness::Host>,
    /// The deployment's external hook registry, resolved from the
    /// mounted `hooks` service at mount time.
    pub hooks: Option<uva_hook::HookRegistry>,
    /// Where the run's detached work executes.
    pub spawner: Option<Arc<dyn uva_core::spawn::Spawner>>,
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
    Ok(Arc::new(build_loop(ctx, defaults)?))
}

/// The concrete factory the plugin publishes: the defaults plus the
/// services the context resolved.
fn build_loop(
    ctx: &Context,
    defaults: AgentLoopDefaults,
) -> Result<ServerAgentLoop, PluginError> {
    let default_provider = ctx
        .require::<Arc<dyn ModelProvider>>(KEY_LLM_DEFAULT)
        .map(|wrapped| Arc::clone(&*wrapped))?;
    // The compactor is built here, over the provider this factory
    // resolved — not taken from a separate service: a provider swap
    // remounts this plugin, and the new factory must summarize through
    // the new provider. All default-provider runs share this instance.
    let compactor = build_compactor(&default_provider, &defaults);
    Ok(ServerAgentLoop {
        defaults,
        default_provider,
        compactor,
    })
}

/// The external hook registry the `hooks` row published, when it is
/// mounted.
///
/// The row publishes an `Arc<HookRegistry>` (the registry itself is
/// cheap to clone and shares one table), so the loop keeps its own
/// clone and the plugin's provenance is irrelevant.
fn hooks_registry(
    ctx: &Context,
) -> Result<Option<uva_hook::HookRegistry>, PluginError> {
    let registry = ctx.get::<uva_hook::HookRegistry>(uva_hook::KEY_HOOKS)?;
    Ok(registry.map(|registry| (*registry).clone()))
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
    fn create(&self, run: &RunConfig) -> Arc<dyn uva_harness::Agent> {
        let agent = self.resolve(run).build();
        // The registry reaches every run's seam layer here: a
        // registration dropped anywhere withdraws from every agent
        // that shares the table, including runs created later.
        let agent = match &self.defaults.hooks {
            Some(registry) => agent.with_hook_registry(registry.clone()),
            None => agent,
        };
        Arc::new(agent)
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
        if config.context_manager.is_none() {
            config.context_manager = self.defaults.context_manager.clone();
        }
        if config.host.is_none() {
            config.host = self.defaults.host.clone();
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
pub fn agent_loop_manifest() -> uva_kernel::PluginManifest {
    let mut manifest = uva_kernel::PluginManifest::new(
        "agent-loop",
        uva_kernel::PLUGIN_PROTOCOL_VERSION,
    );
    manifest.runtime = uva_kernel::RuntimeKind::Native;
    manifest
        .provides
        .push(uva_harness::plugin::KEY_AGENT_LOOP.to_string());
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
    manifest: uva_kernel::PluginManifest,
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

impl AgentLoopServicePlugin {
    /// The factory this plugin publishes: the deployment's defaults
    /// plus the services the context resolved.
    fn resolve(&self, ctx: &Context) -> Result<ServerAgentLoop, PluginError> {
        let mut defaults = self.defaults.clone();
        if let Some(registry) = hooks_registry(ctx)? {
            defaults.hooks = Some(registry);
        }
        build_loop(ctx, defaults)
    }
}

impl uva_kernel::Plugin for AgentLoopServicePlugin {
    fn manifest(&self) -> &uva_kernel::PluginManifest {
        &self.manifest
    }

    fn apply(
        &self,
        ctx: &Context,
    ) -> Result<Vec<uva_kernel::EffectHandle>, PluginError> {
        let factory: Arc<dyn AgentLoopFactory> = Arc::new(self.resolve(ctx)?);
        let service =
            ctx.provide(uva_harness::plugin::KEY_AGENT_LOOP, factory)?;
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
///
/// Documented above: `subagents` is **not** one of them — delegation is
/// an ordinary tool brick the boot files in the registry, not a service
/// the loop consumes.
///
/// `hooks` is **not** one either, and must never be added: an `inject`
/// key is a hard dependency (the kernel rejects the group when nothing
/// provides it), while the registry is optional. [`AgentLoopServicePlugin`]
/// resolves it with `ctx.get` at mount instead, so a missing or
/// switched-off `hooks` row boots a loop with no external registrations.
pub const AGENT_LOOP_INJECTS: [&str; 4] =
    ["llm", "llm.default", "tools", "sessions"];

#[cfg(test)]
mod tests;
