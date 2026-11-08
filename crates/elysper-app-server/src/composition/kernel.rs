//! The native kernel plugin set assembled from the app-server's host
//! objects.
//!
//! The app-server owns every concrete piece a plugin needs — the
//! provider catalogue, the one [`ToolRegistry`], the session registry,
//! the sandbox, the `task` delegator, the compactor, the approval
//! policy, and the capability services (MCP supervisor, skill registry,
//! eval runner, scheduler service, attachment store). [`KernelHost`]
//! collects them, [`native_profile`] names the built-in plugin set, and
//! [`mount_profile`] hands the set to the kernel:
//! [`build_plugins`](crate::profile::build_plugins) resolves each row
//! through the host-bound factory directory, and
//! [`mount_all`](elysper::kernel::mount_all) validates, orders, and
//! mounts them in one group.
//!
//! # One registration path
//!
//! The boot no longer registers the `skill` tool itself: `skills` ([`SkillPlugin`]) owns that
//! registration, scoped to its own token, so the tool and the service
//! are withdrawn together when the plugin is unloaded. Registering the
//! tool in the boot as well would make the kernel refuse the plugin's
//! own `apply` — a loud failure by design, not a duplicate.
//!
//! # What the handle buys
//!
//! The returned [`MountHandle`] owns the group. Dropping it unloads
//! everything; [`MountHandle::dispose_plugin`] unloads one plugin and
//! lets the kernel park its consumers. The app-server stores the handle
//! on `AppState`, so the HMR loop can remount only the affected
//! fibers.

use std::{
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
};

use elysper::{
    attachment::{AttachmentPlugin, AttachmentStore},
    core::SharedClock,
    eval::{EvalPlugin, EvalRunner},
    harness::{
        ApprovalPlugin,
        ApprovalPolicy,
        DelegatingTool,
        Host,
        ReasoningStrategy,
        Registration,
    },
    kernel::{
        Context,
        MountError,
        MountHandle,
        Plugin as KernelPlugin,
        mount_all,
    },
    mcp::{McpPlugin, McpSupervisor},
    plugin_host::ProcessPluginsPlugin,
    scheduler::{ScheduleStore, SchedulerPlugin, SchedulerService},
    session::{
        CompactionPlugin,
        Compactor,
        SessionStorePlugin,
        manager::SessionRegistry,
    },
    skill::{SkillPlugin, SkillRegistry},
    tool::{ToolRegistry, ToolRegistryPlugin, ToolRestriction},
    tool_scheduler::ScheduleToolPlugin,
    tool_shell::{ExecutionPolicy, SandboxBackend, SandboxPlugin},
    tool_task::SubagentPlugin,
};
use serde_json::Value;

use crate::{
    profile::{
        BootContext,
        Entry,
        PluginError,
        PluginFactories,
        PluginFactory,
        PluginManifest,
        Profile,
        ProfileError,
        RuntimeKind,
        build_plugins,
    },
    state::ProviderCatalogue,
};

/// Every concrete object a native factory may need.
///
/// The boot builds it once from the pieces it already owns, mounts the
/// plugin set over it, and keeps the same handles on `AppState` — the
/// service a plugin publishes is the object the rest of the server
/// uses, never a clone with its own state.
pub(crate) struct KernelHost {
    pub workspace_root: PathBuf,
    /// The provider catalogue published as the `llm` service.
    pub provider_catalogue: Arc<ProviderCatalogue>,
    /// The deployment's one tool registry, published as `tools`.
    pub tool_registry: Arc<ToolRegistry>,
    /// The session registry published as `sessions`.
    pub session_registry: Arc<SessionRegistry>,
    /// The confinement seam published as `sandbox`.
    pub sandbox_backend: Arc<dyn SandboxBackend>,
    /// The boundary the sandbox service claims the backend enforces.
    pub sandbox_policy: ExecutionPolicy,
    /// The `task` tool's delegator, published as `subagents`.
    pub delegator: Arc<elysper::tool_task::TaskDelegator>,
    /// The deployment's compactor, published as `compaction`.
    pub compactor: Arc<dyn Compactor>,
    /// The approval stance published as `approval`.
    pub approval_policy: ApprovalPolicy,
    /// The supervisor published as `mcp`.
    pub mcp_supervisor: Arc<McpSupervisor>,
    /// The skill registry published as `skills`.
    pub skill_registry: Arc<SkillRegistry>,
    /// The eval engine published as `eval`.
    pub eval_runner: Arc<EvalRunner>,
    /// The schedule store/scheduler pair published as `scheduler`.
    pub scheduler_service: Arc<SchedulerService>,
    /// The attachment store published as `attachments`.
    pub attachment_store: Arc<AttachmentStore>,
    /// The deployment's `[plugins]` host, adopted by the
    /// `process-plugins` entry.
    ///
    /// This is the boot's one host clone: it shares every
    /// registration with the host the run assembly receives, so the
    /// entry's bind reaches the same registry and its unload
    /// withdraws the same tools.
    pub plugin_host: Host,
    /// The registrations the `[plugins]` composition installed.
    ///
    /// The `process-plugins` factory takes them when the row is
    /// built; `None` after that. An empty slot is the explicit
    /// profile case — the composition keeps its handles when the
    /// profile omits the row.
    pub process_registrations: Mutex<Option<Vec<Registration>>>,
    // -- Transitional agent-loop inputs ---------------------------------
    // The `agent-loop` factory reads these through its defaults until
    // the loop's config moves into the profile row (stage 3); they
    // die with the rest of the host then.
    /// The deployment's reasoning loop.
    pub default_strategy: Option<Arc<dyn ReasoningStrategy>>,
    /// The deployment's allow/deny list.
    pub default_tool_restriction: Option<Arc<ToolRestriction>>,
    /// The policy a per-run compactor builds from.
    pub compaction_boot: Option<elysper::runtime::CompactionBoot>,
    /// Where the loop's detached work executes.
    pub spawner: Option<Arc<dyn elysper::core::spawn::Spawner>>,
    /// The delegating tools every run that installs none gets.
    pub delegators: Vec<Arc<dyn DelegatingTool>>,
}

/// Why a native mount failed — a profile-assembly or kernel verdict.
///
/// Neither is recoverable at boot: the built-in profile is a program
/// constant and the host objects are already constructed, so a failure
/// is a wiring bug the operator must see rather than a degraded server.
#[derive(Debug, thiserror::Error)]
pub(crate) enum KernelBootError {
    /// The profile could not be turned into plugin values.
    #[error("native plugin profile: {0}")]
    Profile(#[from] ProfileError),
    /// The kernel refused the group.
    #[error("native plugin mount: {0}")]
    Mount(#[from] MountError),
}

type Make = fn(&KernelHost) -> Arc<dyn KernelPlugin>;

/// The built-in native plugin set, in the order the boot declares it.
///
/// The kernel reorders by `inject` edges; this order only keeps
/// independent plugins stable and makes the boot's intent readable.
/// Every row is a native plugin constructed from [`KernelHost`], so a
/// row never names configuration the app-server did not already resolve.
#[must_use]
pub(crate) fn native_profile() -> Profile {
    const ENTRIES: [&str; 15] = [
        "tool-registry",
        "model-adapter",
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
        "schedule-tool",
        "attachments",
        "process-plugins",
    ];
    Profile {
        core: ENTRIES
            .into_iter()
            .map(|id| Entry {
                id: id.to_owned(),
                runtime: RuntimeKind::Native,
                config: Value::Null,
                capabilities: Vec::new(),
            })
            .collect(),
        ui: Vec::new(),
        hmr: false,
    }
}

/// Build the native plugin values and mount them in one kernel group.
///
/// The returned [`Context`] is the isolation scope every service was
/// registered in; the [`MountHandle`] owns the group's effects and
/// fibers. `AppState` keeps both — the context for consumers
/// ([`Context::require`]) and the handle for unloading.
///
/// The profile is the boot's authoritative core assembly: a row whose
/// id has no registered factory fails the mount (the kernel vocabulary
/// for a bad profile), and a row omitted from the profile is not
/// mounted. [`native_profile`] is the no-profile default.
pub(crate) fn mount_profile(
    host: Arc<KernelHost>,
    profile: &Profile,
    clock: SharedClock,
) -> Result<(Context, MountHandle), KernelBootError> {
    let context = Context::new();
    let factories = native_factories(Arc::clone(&host));
    let boot = BootContext::new(host.workspace_root.clone())
        .with_host(host)
        .with_clock(clock);
    let plugins = build_plugins(profile, &factories, &boot)?;
    let mounted = mount_all(&context, &plugins)?;
    Ok((context, mounted))
}

/// The factory directory the boot registers, over an already-built
/// host.
pub(crate) fn profile_factories(host: Arc<KernelHost>) -> PluginFactories {
    native_factories(host)
}

/// A factory directory for `--dump-config` and other manifest-only
/// readers.
///
/// Manifests come from the concrete plugins' declarations, which do
/// not read their host objects at construction time. The host built
/// here is therefore a placeholder: it points every field at an empty
/// or default value, and nothing is mounted.
pub(crate) fn dump_factories(
    workspace_root: &std::path::Path,
) -> PluginFactories {
    native_factories(Arc::new(KernelHost {
        workspace_root: workspace_root.to_path_buf(),
        provider_catalogue: Arc::new(crate::state::ProviderCatalogue::default()),
        tool_registry: Arc::new(ToolRegistry::new()),
        session_registry: Arc::new(SessionRegistry::new(
            workspace_root.join("sessions"),
        )),
        sandbox_backend: Arc::new(elysper::tool_shell::NoopBackend::new()),
        sandbox_policy: ExecutionPolicy::default(),
        delegator: Arc::new(elysper::tool_task::TaskDelegator::new(Arc::new(
            elysper::harness::AgentRegistry::new(),
        ))),
        compactor: Arc::new(elysper::session::SummaryCompactor::new(
            Arc::new(ManifestOnlyProvider),
            "manifest-only".to_owned(),
        )),
        approval_policy: ApprovalPolicy::default(),
        mcp_supervisor: Arc::new(McpSupervisor::new()),
        skill_registry: Arc::new(SkillRegistry::discover_files(workspace_root)),
        eval_runner: Arc::new(EvalRunner::new()),
        scheduler_service: Arc::new(SchedulerService::new(Arc::new(
            ScheduleStore::new(workspace_root.join("schedules")),
        ))),
        attachment_store: Arc::new(AttachmentStore::default()),
        plugin_host: Host::default(),
        process_registrations: Mutex::new(None),
        // Transitional agent-loop inputs (die with the host).
        default_strategy: None,
        default_tool_restriction: None,
        compaction_boot: None,
        spawner: None,
        delegators: Vec::new(),
    }))
}

/// A provider that exists only so the manifest-only host can be
/// constructed; every call is unreachable because nothing runs.
struct ManifestOnlyProvider;

#[async_trait::async_trait]
impl elysper::provider::ModelProvider for ManifestOnlyProvider {
    async fn initialize(
        &mut self,
        _config: elysper::provider::ProviderConfig,
    ) -> Result<(), elysper::core::Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "manifest-only"
    }

    fn model_config(&self) -> elysper::provider::ModelConfig {
        elysper::provider::ModelConfig {
            name: "manifest-only".to_owned(),
            provider: "manifest-only".to_owned(),
            context_window: 1,
            max_output_tokens: 1,
            supports_tools: false,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: elysper::provider::CompletionRequest,
    ) -> Result<elysper::provider::CompletionResponse, elysper::core::Error>
    {
        Err(elysper::core::Error::Internal {
            message: "the manifest-only provider never runs".to_owned(),
        })
    }
}

/// The factory directory the boot registers: one entry per native
/// plugin, all sharing the host.
fn native_factories(host: Arc<KernelHost>) -> PluginFactories {
    const SPECS: [(&str, Make); 12] = [
        ("tool-registry", tool_registry_plugin),
        ("session-store", session_store_plugin),
        ("sandbox", sandbox_plugin),
        ("subagent-backend", subagent_plugin),
        ("context-compactor", compaction_plugin),
        ("approval", approval_plugin),
        ("skills", skill_plugin),
        ("mcp", mcp_plugin),
        ("eval", eval_plugin),
        ("scheduler", scheduler_plugin),
        ("schedule-tool", schedule_tool_plugin),
        ("attachments", attachment_plugin),
    ];
    let mut factories = PluginFactories::new();
    for (id, make) in SPECS {
        factories
            .register(Arc::new(HostFactory {
                host: Arc::clone(&host),
                id,
                make,
            }))
            .expect("the built-in factory ids are unique");
    }
    // `model-adapter` is config-driven, not host-bound: its factory
    // reads the profile row's config (falling back to the host
    // catalogue for an empty one), so it cannot go through
    // `HostFactory`'s ignore-the-config `make`.
    factories
        .register(Arc::new(ModelAdapterFactory))
        .expect("the built-in factory ids are unique");
    // `agent-loop` resolves its services at mount, so it goes through
    // its own entry rather than a host-bound `make`.
    factories
        .register(Arc::new(AgentLoopFactoryEntry))
        .expect("the built-in factory ids are unique");
    // The process-plugins row adopts the composition's live
    // registrations, so it cannot go through `HostFactory`: reading a
    // manifest would call `make`, and a `make` that drains the slot
    // would uninstall the plugins it is describing. Its manifest is
    // built over an empty set instead.
    factories
        .register(Arc::new(ProcessPluginsFactory { host }))
        .expect("the built-in factory ids are unique");
    factories
}

/// The `process-plugins` factory: adopts the registrations the
/// `[plugins]` composition installed, without letting a manifest-only
/// probe touch them.
///
/// [`ProcessPluginsPlugin::manifest`] does not depend on the
/// registrations, so [`Self::manifest`] builds it over an empty set.
/// Only [`Self::build`] drains [`KernelHost::process_registrations`],
/// and only the kernel's `mount_all` calls `build`: the returned
/// effect is then the registrations' one owner, which is what makes
/// unloading the entry withdraw the remote tools.
struct ProcessPluginsFactory {
    host: Arc<KernelHost>,
}

impl PluginFactory for ProcessPluginsFactory {
    fn id(&self) -> &'static str {
        "process-plugins"
    }

    fn manifest(&self) -> PluginManifest {
        ProcessPluginsPlugin::new(self.host.plugin_host.clone(), Vec::new())
            .manifest()
            .clone()
    }

    fn build(
        &self,
        _config: &Value,
        boot: &BootContext,
    ) -> Result<Arc<dyn KernelPlugin>, PluginError> {
        let host = boot.host::<KernelHost>().ok_or_else(|| {
            PluginError::new(
                "the boot context carries no kernel host; cannot build \
                 `process-plugins`"
                    .to_owned(),
            )
        })?;
        let registrations = host
            .process_registrations
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
            .unwrap_or_default();
        Ok(Arc::new(ProcessPluginsPlugin::new(
            host.plugin_host.clone(),
            registrations,
        )) as Arc<dyn KernelPlugin>)
    }
}

/// One concrete factory: the manifest comes from a throwaway plugin
/// built over the same host, and `build` re-reads the host from the
/// [`BootContext`] the caller supplied.
struct HostFactory {
    host: Arc<KernelHost>,
    id: &'static str,
    make: Make,
}

impl PluginFactory for HostFactory {
    fn id(&self) -> &'static str {
        self.id
    }

    fn manifest(&self) -> PluginManifest {
        (self.make)(&self.host).manifest().clone()
    }

    fn build(
        &self,
        _config: &Value,
        boot: &BootContext,
    ) -> Result<Arc<dyn KernelPlugin>, PluginError> {
        let host = boot.host::<KernelHost>().ok_or_else(|| {
            PluginError::new(format!(
                "the boot context carries no kernel host; cannot build `{}`",
                self.id
            ))
        })?;
        Ok((self.make)(host))
    }
}

fn tool_registry_plugin(host: &KernelHost) -> Arc<dyn KernelPlugin> {
    Arc::new(ToolRegistryPlugin::new(Arc::clone(&host.tool_registry)))
}

/// The `model-adapter` factory: the one core entry whose plugin is
/// built from the profile row's `config`, not the host.
///
/// A non-empty config parses as the runtime crate's
/// [`ModelAdapterConfig`] — the catalogue builds from it and the
/// tier-1 sections wrap the default handle. An empty config (the
/// [`native_profile`](super::native_profile) default, zero-config
/// boots) falls back to publishing the host's already-built
/// deployment catalogue with no tier-1 wrap: the transitional seam
/// until the profile is the only source.
struct ModelAdapterFactory;

impl PluginFactory for ModelAdapterFactory {
    fn id(&self) -> &'static str {
        "model-adapter"
    }

    fn manifest(&self) -> PluginManifest {
        elysper::runtime::model_adapter_manifest()
    }

    fn build(
        &self,
        config: &Value,
        boot: &BootContext,
    ) -> Result<Arc<dyn KernelPlugin>, PluginError> {
        use elysper::runtime::{ModelAdapterConfig, ModelAdapterServicePlugin};

        let sleeper: std::sync::Arc<dyn elysper::core::sleeper::Sleeper> =
            std::sync::Arc::new(elysper::harness_tokio::TokioSleeper);
        let clock = boot.clock().clone();

        let empty = match config {
            Value::Null => true,
            Value::Object(map) => map.is_empty(),
            other => {
                return Err(PluginError::new(format!(
                    "`model-adapter` config must be an object or null, \
                     found {other}"
                )));
            }
        };
        if empty {
            let host = boot.host::<KernelHost>().ok_or_else(|| {
                PluginError::new(
                    "the boot context carries no kernel host; cannot \
                     build `model-adapter`",
                )
            })?;
            let plugin = ModelAdapterServicePlugin::new(
                std::sync::Arc::clone(&host.provider_catalogue),
                &ModelAdapterConfig::default(),
                clock,
                sleeper,
            )
            .map_err(|error| {
                PluginError::new(format!(
                    "the deployment's default provider did not build: {error}"
                ))
            })?;
            return Ok(std::sync::Arc::new(plugin));
        }

        let parsed: ModelAdapterConfig = serde_json::from_value(config.clone())
            .map_err(|error| {
                PluginError::new(format!(
                    "`model-adapter` config is not a ModelAdapterConfig: \
                     {error}"
                ))
            })?;
        // The demo seam: under test configurations the catalogue gets
        // the demo / fake handles injected, exactly like the boot's
        // host catalogue.
        #[cfg(any(test, feature = "test-utils"))]
        let catalogue = std::sync::Arc::new(
            crate::state::catalogue_for_deployment(&parsed.workspace),
        );
        #[cfg(not(any(test, feature = "test-utils")))]
        let catalogue = std::sync::Arc::new(
            elysper::runtime::catalogue_from_config(&parsed),
        );
        let plugin =
            ModelAdapterServicePlugin::new(catalogue, &parsed, clock, sleeper)
                .map_err(|error| {
                    PluginError::new(format!(
                        "the configured default provider did not build: {error}"
                    ))
                })?;
        Ok(std::sync::Arc::new(plugin))
    }
}

fn session_store_plugin(host: &KernelHost) -> Arc<dyn KernelPlugin> {
    Arc::new(SessionStorePlugin::new(Arc::clone(&host.session_registry)))
}

fn sandbox_plugin(host: &KernelHost) -> Arc<dyn KernelPlugin> {
    Arc::new(SandboxPlugin::new(
        Arc::clone(&host.sandbox_backend),
        host.sandbox_policy,
    ))
}

fn subagent_plugin(host: &KernelHost) -> Arc<dyn KernelPlugin> {
    let delegator: Arc<dyn DelegatingTool> =
        Arc::clone(&host.delegator) as Arc<dyn DelegatingTool>;
    Arc::new(SubagentPlugin::new(delegator))
}

fn compaction_plugin(host: &KernelHost) -> Arc<dyn KernelPlugin> {
    Arc::new(CompactionPlugin::new(Arc::clone(&host.compactor)))
}

fn approval_plugin(host: &KernelHost) -> Arc<dyn KernelPlugin> {
    Arc::new(ApprovalPlugin::new(host.approval_policy.clone()))
}

fn skill_plugin(host: &KernelHost) -> Arc<dyn KernelPlugin> {
    Arc::new(SkillPlugin::new(Arc::clone(&host.skill_registry)))
}

/// The `agent-loop` factory: builds the runtime's service plugin,
/// whose apply resolves `llm.default` + `compaction` and publishes
/// the deployment's one [`AgentLoopFactory`].
///
/// The defaults come from the host's transitional fields — the same
/// values the four assembly sites used to thread themselves — so a
/// remount (a provider swap) rebuilds the factory against the new
/// services without touching them.
struct AgentLoopFactoryEntry;

impl PluginFactory for AgentLoopFactoryEntry {
    fn id(&self) -> &'static str {
        "agent-loop"
    }

    fn manifest(&self) -> PluginManifest {
        elysper::runtime::agent_loop_manifest()
    }

    fn build(
        &self,
        _config: &Value,
        boot: &BootContext,
    ) -> Result<Arc<dyn KernelPlugin>, PluginError> {
        use elysper::{
            context::TruncatingContextManager,
            runtime::AgentLoopDefaults,
        };

        let host = boot.host::<KernelHost>().ok_or_else(|| {
            PluginError::new(
                "the boot context carries no kernel host; cannot build \
                 `agent-loop`",
            )
        })?;
        let defaults = AgentLoopDefaults {
            delegators: host.delegators.clone(),
            tool_surface: None,
            strategy: host.default_strategy.clone(),
            tool_restriction: host.default_tool_restriction.clone(),
            log_writer: None,
            subagent_writers: None,
            // The stateless request-level projection, one instance.
            context_manager: Some(Arc::new(TruncatingContextManager)),
            host: Some(host.plugin_host.clone()),
            spawner: host.spawner.clone(),
            compaction: host.compaction_boot.clone(),
        };
        Ok(Arc::new(elysper::runtime::agent_loop_service_plugin(
            defaults,
        )))
    }
}

fn mcp_plugin(host: &KernelHost) -> Arc<dyn KernelPlugin> {
    Arc::new(McpPlugin::new(Arc::clone(&host.mcp_supervisor)))
}

fn eval_plugin(host: &KernelHost) -> Arc<dyn KernelPlugin> {
    Arc::new(EvalPlugin::new(Arc::clone(&host.eval_runner)))
}

fn scheduler_plugin(host: &KernelHost) -> Arc<dyn KernelPlugin> {
    Arc::new(SchedulerPlugin::new(Arc::clone(&host.scheduler_service)))
}

fn schedule_tool_plugin(_host: &KernelHost) -> Arc<dyn KernelPlugin> {
    Arc::new(ScheduleToolPlugin::new())
}

fn attachment_plugin(host: &KernelHost) -> Arc<dyn KernelPlugin> {
    Arc::new(AttachmentPlugin::new(Arc::clone(&host.attachment_store)))
}

/// The parts of the host the scheduler service should share with the
/// HTTP schedule surface.
///
/// Both read one `ScheduleStore`; the service's `Scheduler` is the one
/// the background tick drives, while the route handlers use
/// [`SchedulesState`](crate::state::SchedulesState) for the same store.
#[must_use]
pub(crate) fn scheduler_service(
    store: Arc<ScheduleStore>,
) -> Arc<SchedulerService> {
    Arc::new(SchedulerService::new(store))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use elysper::{
        attachment::{AttachmentStore, KEY_ATTACHMENTS},
        eval::{EvalRunner, KEY_EVAL},
        harness::{AgentRegistry, ApprovalPolicy},
        mcp::{KEY_MCP, McpSupervisor},
        provider::traits::ModelProvider,
        scheduler::{KEY_SCHEDULER, ScheduleStore, SchedulerService},
        session::{
            Compactor,
            KEY_SESSIONS,
            SummaryCompactor,
            manager::SessionRegistry,
        },
        skill::{KEY_SKILLS, SkillRegistry},
        tool::{KEY_TOOLS, ToolRegistry},
        tool_shell::{ExecutionPolicy, NoopBackend},
        tool_task::TaskDelegator,
    };
    use tempfile::tempdir;

    use super::{KernelHost, SharedClock, mount_profile, native_profile};
    use crate::state::ProviderCatalogue;

    fn host(root: &std::path::Path) -> Arc<KernelHost> {
        let provider: Arc<dyn ModelProvider> =
            Arc::new(elysper::test_support::FakeProvider::new(vec![]));
        let catalogue = ProviderCatalogue::default().with_handle(
            "fake",
            "fake-model",
            Arc::clone(&provider),
        );
        let tools = Arc::new(crate::state::plugin_tool_registry());
        let sessions = Arc::new(SessionRegistry::new(root.join("sessions")));
        let agents = Arc::new(AgentRegistry::new());
        let compactor: Arc<dyn Compactor> = Arc::new(SummaryCompactor::new(
            Arc::clone(&provider),
            "fake-model",
        ));
        Arc::new(KernelHost {
            workspace_root: root.to_path_buf(),
            provider_catalogue: Arc::new(catalogue),
            tool_registry: Arc::clone(&tools),
            session_registry: sessions,
            sandbox_backend: Arc::new(NoopBackend::new()),
            sandbox_policy: ExecutionPolicy::default(),
            delegator: Arc::new(TaskDelegator::new(agents)),
            compactor,
            approval_policy: ApprovalPolicy::default(),
            mcp_supervisor: Arc::new(McpSupervisor::new()),
            skill_registry: Arc::new(SkillRegistry::discover_files(root)),
            eval_runner: Arc::new(EvalRunner::new()),
            scheduler_service: Arc::new(SchedulerService::new(Arc::new(
                ScheduleStore::new(root.join("schedules")),
            ))),
            attachment_store: Arc::new(AttachmentStore::default()),
            plugin_host: elysper::harness::Host::new(),
            process_registrations: std::sync::Mutex::new(None),
            // Transitional agent-loop inputs (die with the host).
            default_strategy: None,
            default_tool_restriction: None,
            compaction_boot: None,
            spawner: None,
            delegators: Vec::new(),
        })
    }

    #[test]
    fn baseline_profile_names_every_native_plugin_in_dependency_order() {
        let profile = native_profile();
        let ids: Vec<&str> =
            profile.core.iter().map(|entry| entry.id.as_str()).collect();
        for expected in [
            "tool-registry",
            "model-adapter",
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
            "process-plugins",
        ] {
            assert!(
                ids.contains(&expected),
                "the boot profile is missing `{expected}`: {ids:?}"
            );
        }
        assert_eq!(ids.len(), 15, "no duplicate rows: {ids:?}");
    }

    #[tokio::test]
    async fn mounts_the_native_set_through_the_kernel() {
        let dir = tempdir().expect("tempdir");
        let host = host(dir.path());
        let (ctx, handle) = mount_profile(
            Arc::clone(&host),
            &native_profile(),
            SharedClock::system(),
        )
        .expect("mount the native set");

        let tools = ctx.require::<ToolRegistry>(KEY_TOOLS).expect("tools");
        assert!(
            Arc::ptr_eq(&tools, &host.tool_registry),
            "the service is the deployment's one registry"
        );
        assert!(
            tools.contains("skill"),
            "SkillPlugin registers the `skill` tool; boot must not"
        );

        ctx.require::<SkillRegistry>(KEY_SKILLS).expect("skills");
        ctx.require::<McpSupervisor>(KEY_MCP).expect("mcp");
        ctx.require::<EvalRunner>(KEY_EVAL).expect("eval");
        ctx.require::<SchedulerService>(KEY_SCHEDULER)
            .expect("scheduler");
        ctx.require::<AttachmentStore>(KEY_ATTACHMENTS)
            .expect("attachments");
        ctx.require::<SessionRegistry>(KEY_SESSIONS)
            .expect("sessions");
        let llm = ctx
            .require::<ProviderCatalogue>(elysper::provider::KEY_LLM)
            .expect("llm");
        assert!(
            Arc::ptr_eq(&llm, &host.provider_catalogue),
            "the empty native row config falls back to the host catalogue"
        );
        llm.resolve(Some("fake/fake-model"))
            .expect("the service resolves the deployment's pair");
        assert!(
            llm.resolve(Some("fake/other-model")).is_err(),
            "a model the deployment does not offer is refused"
        );
        let default = ctx
            .require::<Arc<dyn elysper::provider::ModelProvider>>(
                elysper::provider::KEY_LLM_DEFAULT,
            )
            .expect("llm.default");
        assert_eq!(
            default.model_config().name,
            "fake-model",
            "the default handle is the catalogue's default"
        );
        ctx.require::<Arc<dyn elysper::harness::plugin::AgentLoopFactory>>(
            elysper::harness::plugin::KEY_AGENT_LOOP,
        )
        .expect("agentLoop");
        ctx.require::<ApprovalPolicy>(elysper::harness::KEY_APPROVAL)
            .expect("approval");
        ctx.require::<elysper::tool_shell::SandboxService>(
            elysper::tool_shell::KEY_SANDBOX,
        )
        .expect("sandbox");

        assert_eq!(handle.len(), 15);
        assert!(handle.plugin_state("skills").is_some());
    }

    #[tokio::test]
    async fn an_explicit_profile_mounts_only_the_rows_it_names() {
        let dir = tempdir().expect("tempdir");
        let host = host(dir.path());
        let profile = crate::profile::Profile {
            core: vec![crate::profile::Entry {
                id: "tool-registry".to_owned(),
                runtime: elysper::kernel::RuntimeKind::Native,
                config: serde_json::Value::Null,
                capabilities: Vec::new(),
            }],
            ui: Vec::new(),
            hmr: false,
        };
        let (ctx, handle) =
            mount_profile(Arc::clone(&host), &profile, SharedClock::system())
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
        let host = host(dir.path());
        let profile = crate::profile::Profile {
            core: vec![crate::profile::Entry {
                id: "model-adapter".to_owned(),
                runtime: elysper::kernel::RuntimeKind::Native,
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
            }],
            ui: Vec::new(),
            hmr: false,
        };
        let (ctx, handle) =
            mount_profile(Arc::clone(&host), &profile, SharedClock::system())
                .expect("the config-driven row mounts");
        assert_eq!(handle.len(), 1, "only the named row is mounted");

        let llm = ctx
            .require::<ProviderCatalogue>(elysper::provider::KEY_LLM)
            .expect("llm");
        assert_eq!(
            llm.default_provider_name(),
            "echo",
            "the catalogue is built from the row config"
        );
        assert!(
            !Arc::ptr_eq(&llm, &host.provider_catalogue),
            "a config-driven row is not the host's file-built catalogue"
        );
        llm.resolve(Some("echo/echo-model"))
            .expect("the demo seam injects the echo handle");
        ctx.require::<Arc<dyn elysper::provider::ModelProvider>>(
            elysper::provider::KEY_LLM_DEFAULT,
        )
        .expect("llm.default");
    }

    #[tokio::test]
    async fn disposing_skills_withdraws_the_service_and_its_tool() {
        let dir = tempdir().expect("tempdir");
        let host = host(dir.path());
        let (ctx, mut handle) = mount_profile(
            Arc::clone(&host),
            &native_profile(),
            SharedClock::system(),
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
            !host.tool_registry.contains("skill"),
            "the token-scoped tool registration is withdrawn with it"
        );
        assert!(
            host.tool_registry.contains("read"),
            "the rest of the tool surface is untouched"
        );
        assert!(
            ctx.get::<ToolRegistry>(KEY_TOOLS)
                .expect("read tools")
                .is_some(),
            "sibling services stay mounted"
        );
    }
}
