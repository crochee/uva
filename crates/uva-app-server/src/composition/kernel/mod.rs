//! The native kernel plugin set assembled from the app-server's boot
//! services.
//!
//! Every capability a row needs is built **inside** that row: the
//! provider catalogue ([`model-adapter`](uva::runtime::model_adapter_manifest)),
//! the delegation backend (`subagent-backend`), the approval stance
//! (`approval`), the session index (`search`), and the loop's own
//! defaults (`agent-loop`, whose row `config` carries them). What the
//! boot still owns are the **containers** — the one
//! [`ToolRegistry`], the peer-agent registry, the harness host, and the
//! `[plugins]` composition's registrations — collected in
//! [`BootServices`].
//!
//! [`native_profile`] names the built-in plugin set, and
//! [`mount_profile`] hands the set to the kernel:
//! [`build_plugins`](crate::profile::build_plugins) resolves each row
//! through the factory directory, and
//! [`mount_all`](uva::kernel::mount_all) validates, orders, and
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

use std::{path::PathBuf, sync::Arc};

use serde::Deserialize;
use serde_json::Value;
use uva::{
    attachment::AttachmentPlugin,
    core::SharedClock,
    eval::EvalPlugin,
    harness::{ApprovalPlugin, ApprovalPolicy, DelegatingTool, Host},
    kernel::{
        Context,
        MountError,
        MountHandle,
        Plugin as KernelPlugin,
        mount_all,
    },
    mcp::McpPlugin,
    scheduler::SchedulerPlugin,
    session::CompactionPlugin,
    skill::SkillPlugin,
    tool::{ToolRegistry, ToolRegistryPlugin, ToolRestriction},
    tool_scheduler::ScheduleToolPlugin,
    tool_shell::SandboxPlugin,
    tool_task::SubagentPlugin,
};

use crate::profile::{
    BootContext,
    Capability,
    Enabled,
    Entry,
    KernelChecks,
    PluginError,
    PluginFactories,
    PluginFactory,
    PluginManifest,
    Profile,
    ProfileError,
    RuntimeKind,
    build_plugins,
    effective,
    partition_waves,
};

/// The containers the boot owns and the rows share.
///
/// A **container** is a mutable home several parties write into — the
/// tool registry the bricks register into, the peer-agent registry the
/// default member joins, the harness host the `[plugins]` composition
/// installs into. A **capability** is a policy or an engine a row
/// builds for itself; none of those live here.
///
/// The boot builds it once, mounts the plugin set over it, and keeps
/// the same handles on `AppState` — the service a plugin publishes is
/// the object the rest of the server uses, never a clone with its own
/// state.
pub(crate) struct BootServices {
    /// The shared tool container the brick rows register into; the
    /// `tool-registry` row publishes it.
    pub tool_registry: Arc<ToolRegistry>,
    /// The peer-agent registry the `search` row's agent domain reads
    /// and the `subagent-backend` row's delegator resolves names
    /// through.
    pub agent_registry: Arc<uva::harness::AgentRegistry>,
    /// The deployment's host: the steering policy (guards / hints /
    /// tracker / logging hook) every agent this server builds
    /// receives.
    ///
    /// This is the boot's one host clone: it shares every
    /// registration with the host the run assembly receives, so a
    /// bind reaches the same registry.
    pub plugin_host: Host,
}

/// Why a native mount failed — a profile-assembly or kernel verdict.
///
/// Neither is recoverable at boot: the built-in profile is a program
/// constant and the boot's containers are already constructed, so a
/// failure is a wiring bug the operator must see rather than a
/// degraded server.
#[derive(Debug, thiserror::Error)]
pub(crate) enum KernelBootError {
    /// The profile could not be turned into plugin values.
    #[error("native plugin profile: {0}")]
    Profile(#[from] ProfileError),
    /// The kernel refused the group.
    #[error("native plugin mount: {0}")]
    Mount(#[from] MountError),
}

type Make = fn(&BootServices) -> Arc<dyn KernelPlugin>;

/// The built-in native plugin set, in the order the boot declares it.
///
/// The kernel reorders by `inject` edges; this order only keeps
/// independent plugins stable and makes the boot's intent readable.
/// Every row is a native plugin built from the row's own `config` plus
/// the boot's containers, so a row never names configuration the
/// app-server did not already resolve.
#[must_use]
pub(crate) fn native_profile() -> Profile {
    const ENTRIES: [&str; 21] = [
        "tool-registry",
        "tool-read",
        "tool-write",
        "tool-shell",
        "tool-todo",
        "tool-web",
        "model-adapter",
        "memory",
        "search",
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
    ];
    Profile {
        core: ENTRIES
            .into_iter()
            .map(|id| Entry {
                id: id.to_owned(),
                runtime: RuntimeKind::Native,
                config: Value::Null,
                capabilities: Vec::new(),
                enabled: crate::profile::Enabled::On,
            })
            .collect(),
        ui: Vec::new(),
        hmr: false,
    }
}

/// One boot mount, possibly in two kernel groups.
///
/// A profile without delegated enablement checks degenerates to one
/// group (the pre-two-wave behaviour). With checks, wave 1 holds the
/// static rows, wave 2 the check rows and their static dependents; each
/// wave is one ordinary `mount_all` on the same [`Context`].
pub(crate) struct MountedProfile {
    groups: Vec<MountHandle>,
}

impl MountedProfile {
    /// Number of plugins across every group.
    pub fn len(&self) -> usize {
        self.groups.iter().map(MountHandle::len).sum()
    }

    /// The state of `id` in any group. (Test seam: the boot does not
    /// ask a group it just created.)
    #[cfg(test)]
    pub fn plugin_state(&self, id: &str) -> Option<uva::kernel::PluginState> {
        self.groups.iter().find_map(|group| group.plugin_state(id))
    }

    /// Dispose `id` in the group that owns it. (Test seam.)
    #[cfg(test)]
    pub fn dispose_plugin(&mut self, id: &str) -> bool {
        for group in self.groups.iter_mut().rev() {
            if group.dispose_plugin(id) {
                return true;
            }
        }
        false
    }

    /// The individual groups, for `AppState` to own.
    pub fn into_groups(self) -> Vec<MountHandle> {
        self.groups
    }
}

impl std::fmt::Debug for MountedProfile {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MountedProfile")
            .field("groups", &self.groups.len())
            .field("plugins", &self.len())
            .finish()
    }
}

/// Build the native plugin values and mount them.
///
/// The returned [`Context`] is the isolation scope every service was
/// registered in; the [`MountedProfile`] owns the groups' effects and
/// fibers. `AppState` keeps both — the context for consumers
/// ([`Context::require`]) and the handles for unloading.
///
/// The profile is the boot's authoritative core assembly: a row whose
/// id has no registered factory fails the mount (the kernel vocabulary
/// for a bad profile), and a row omitted from the profile is not
/// mounted. [`native_profile`] is the no-profile default.
///
/// # The two waves
///
/// A delegated check (`enabled = { service, check, arg }`) is answered
/// by a plugin, and that plugin is itself part of this profile — so a
/// single group could never answer it. Wave 1 mounts the static rows;
/// the mounted context then answers the checks and wave 2 (the enabled
/// check rows plus the static rows that inject their services) mounts
/// on the same context. A check row that fails closed stays out; a
/// static dependent left with a missing provider fails the wave, which
/// is the profile's own broken-dependency verdict.
///
/// A boot failure is fatal: both waves belong to one process start, so
/// there is no cross-wave rollback to perform.
pub(crate) fn mount_profile(
    services: Arc<BootServices>,
    profile: &Profile,
    clock: SharedClock,
    extras: BootExtras,
) -> Result<(Context, MountedProfile), KernelBootError> {
    let context = Context::new();
    let factories = native_factories(Arc::clone(&services));
    let boot = BootContext::new(extras.workspace_root.clone())
        .with_host(services)
        .with_clock(clock)
        .with_blocking(extras.blocking)
        .with_compaction(extras.compaction)
        .with_sessions_dir(extras.sessions_dir);
    let (mut first, second) = partition_waves(profile, &factories);
    // Wave 1 never holds a check row; an explicitly-off row is filtered
    // here (the boot recomputes the effective row set afterwards for
    // `AppState::assembly` and the UI schema).
    first.core.retain(|entry| entry.enabled != Enabled::Off);
    let mut groups = Vec::new();
    if !first.core.is_empty() {
        let plugins = build_plugins(&first, &factories, &boot)?;
        groups.push(mount_all(&context, &plugins)?);
    }
    if !second.core.is_empty() {
        let checks = KernelChecks(&context);
        let enabled = effective(&second, &checks).profile;
        if !enabled.core.is_empty() {
            let plugins = build_plugins(&enabled, &factories, &boot)?;
            groups.push(mount_all(&context, &plugins)?);
        }
    }
    Ok((context, MountedProfile { groups }))
}

/// The factory directory the boot registers, over already-built
/// containers.
pub(crate) fn profile_factories(
    services: Arc<BootServices>,
) -> PluginFactories {
    native_factories(services)
}

/// The deployment seams a mount needs beyond the containers: the
/// workspace the rows read, the blocking pool disk appends run on, and
/// the compaction policy the `context-compactor` row builds its engine
/// from.
#[derive(Clone, Default)]
pub(crate) struct BootExtras {
    /// The workspace the rows read their files from.
    pub workspace_root: PathBuf,
    pub blocking: Option<Arc<dyn uva::core::blocking::Blocking>>,
    pub compaction: Option<uva::session::CompactionPluginConfig>,
    /// The session corpus root; defaults to
    /// `<workspace_root>/sessions`. The fixture passes the root its
    /// caller-supplied registry was built with, so the row and the
    /// fixture agree on where sessions live.
    pub sessions_dir: Option<std::path::PathBuf>,
}

/// A factory directory for `--dump-config` and other manifest-only
/// readers.
///
/// Manifests come from the concrete plugins' declarations, which do
/// not read the boot's containers at construction time. The services
/// built here are therefore a placeholder: every container is empty,
/// and nothing is mounted.
pub(crate) fn dump_factories() -> PluginFactories {
    native_factories(Arc::new(BootServices {
        tool_registry: Arc::new(ToolRegistry::new()),
        agent_registry: Arc::new(uva::harness::AgentRegistry::new()),
        plugin_host: Host::default(),
    }))
}

/// The factory directory the boot registers: one entry per native
/// plugin, all sharing the boot's containers.
fn native_factories(services: Arc<BootServices>) -> PluginFactories {
    // Rows the boot constructs: their plugins build the capability in
    // `apply` from the boot context (config only — no containers).
    const BOOT_SPECS: [(&str, BootMake); 15] = [
        ("tool-read", read_tool_plugin),
        ("tool-write", write_tool_plugin),
        ("tool-shell", shell_tool_plugin),
        ("tool-todo", todo_tool_plugin),
        ("tool-web", web_tool_plugin),
        ("session-store", session_store_plugin),
        ("sandbox", sandbox_plugin),
        ("context-compactor", compaction_plugin),
        ("approval", approval_plugin),
        ("skills", skill_plugin),
        ("mcp", mcp_plugin),
        ("eval", eval_plugin),
        ("scheduler", scheduler_plugin),
        ("schedule-tool", schedule_tool_plugin),
        ("attachments", attachment_plugin),
    ];
    // Rows bound to the boot's containers: the shared tool registry
    // and the peer-agent registry the delegator resolves names
    // through.
    const SPECS: [(&str, Make); 2] = [
        ("tool-registry", tool_registry_plugin),
        ("subagent-backend", subagent_plugin),
    ];
    let mut factories = PluginFactories::new();
    for (id, make) in BOOT_SPECS {
        factories
            .register(Arc::new(BootFactory { id, make }))
            .expect("the built-in factory ids are unique");
    }
    for (id, make) in SPECS {
        factories
            .register(Arc::new(ServicesFactory {
                services: Arc::clone(&services),
                id,
                make,
            }))
            .expect("the built-in factory ids are unique");
    }
    // `model-adapter` is config-driven: it builds the catalogue from
    // the profile row's config (the boot materializes an empty row
    // from the workspace configuration), so it cannot go through
    // `ServicesFactory`'s ignore-the-config `make`.
    factories
        .register(Arc::new(ModelAdapterFactory))
        .expect("the built-in factory ids are unique");
    // `agent-loop` resolves its services at mount, so it goes through
    // its own entry rather than a services-bound `make`.
    factories
        .register(Arc::new(AgentLoopFactoryEntry))
        .expect("the built-in factory ids are unique");
    // `memory` builds the file tier from the boot context's
    // workspace root; the boot carries no memory object.
    factories
        .register(Arc::new(MemoryFactory))
        .expect("the built-in factory ids are unique");
    // `search` builds the six-domain service from the injected
    // `tools`/`memory` services, the peer-agent registry, and its own
    // session index.
    factories
        .register(Arc::new(SearchFactory {
            services: Arc::clone(&services),
        }))
        .expect("the built-in factory ids are unique");
    factories
}

/// A boot-bound builder: builds the plugin from the boot context
/// alone (workspace root, clock, blocking seam, compaction policy).
type BootMake = fn(&BootContext) -> Result<Arc<dyn KernelPlugin>, PluginError>;

/// One concrete factory whose plugin is built from the boot context.
struct BootFactory {
    id: &'static str,
    make: BootMake,
}

impl PluginFactory for BootFactory {
    fn id(&self) -> &'static str {
        self.id
    }

    fn manifest(&self) -> PluginManifest {
        // The built-in manifests are configuration-independent; an
        // empty boot context is enough to read them.
        (self.make)(&BootContext::default())
            .expect("a built-in manifest builds from an empty context")
            .manifest()
            .clone()
    }

    fn build(
        &self,
        _config: &Value,
        _capabilities: &[Capability],
        boot: &BootContext,
    ) -> Result<Arc<dyn KernelPlugin>, PluginError> {
        (self.make)(boot)
    }
}

/// One concrete factory: the manifest comes from a throwaway plugin
/// built over the same containers, and `build` re-reads them from the
/// [`BootContext`] the caller supplied.
struct ServicesFactory {
    services: Arc<BootServices>,
    id: &'static str,
    make: Make,
}

impl PluginFactory for ServicesFactory {
    fn id(&self) -> &'static str {
        self.id
    }

    fn manifest(&self) -> PluginManifest {
        (self.make)(&self.services).manifest().clone()
    }

    fn build(
        &self,
        _config: &Value,
        _capabilities: &[Capability],
        boot: &BootContext,
    ) -> Result<Arc<dyn KernelPlugin>, PluginError> {
        let services = boot.host::<BootServices>().ok_or_else(|| {
            PluginError::new(format!(
                "the boot context carries no boot services; cannot build `{}`",
                self.id
            ))
        })?;
        Ok((self.make)(services))
    }
}

/// The `memory` factory: builds the file tier from the boot
/// context's workspace root, degrading to the in-memory tier inside
/// the plugin's `apply`.
struct MemoryFactory;

impl PluginFactory for MemoryFactory {
    fn id(&self) -> &'static str {
        "memory"
    }

    fn manifest(&self) -> PluginManifest {
        memory_plugin(std::path::Path::new("")).manifest().clone()
    }

    fn build(
        &self,
        _config: &Value,
        _capabilities: &[Capability],
        boot: &BootContext,
    ) -> Result<Arc<dyn KernelPlugin>, PluginError> {
        Ok(memory_plugin(boot.workspace_root()))
    }
}

/// The `search` factory: the plugin builds the service in `apply`
/// from the injected services, the peer-agent registry and its own
/// session index.
struct SearchFactory {
    services: Arc<BootServices>,
}

impl PluginFactory for SearchFactory {
    fn id(&self) -> &'static str {
        "search"
    }

    fn manifest(&self) -> PluginManifest {
        search_plugin(&self.services, &BootContext::new(""))
            .manifest()
            .clone()
    }

    fn build(
        &self,
        _config: &Value,
        _capabilities: &[Capability],
        boot: &BootContext,
    ) -> Result<Arc<dyn KernelPlugin>, PluginError> {
        Ok(search_plugin(self.services.as_ref(), boot))
    }
}

/// The `search` row's plugin: the six-domain service over the injected
/// `tools` / `memory` services, the peer-agent registry, and the
/// row's **own** session index.
///
/// The session corpus is the row's business: the row builds the index
/// over the boot context's sessions directory, so the boot hands the
/// row a directory and a blocking seam instead of a live object.
fn search_plugin(
    services: &BootServices,
    boot: &BootContext,
) -> Arc<dyn KernelPlugin> {
    let sessions_dir = boot
        .sessions_dir()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| boot.workspace_root().join("sessions"));
    let mut index = uva::session::JsonlSessionSearch::new(sessions_dir);
    if let Some(blocking) = boot.blocking() {
        index = index.with_blocking(blocking);
    }
    let session_search: uva::session::SharedSessionSearch = Arc::new(index);
    Arc::new(crate::search::plugin::SearchServicePlugin::new(
        Arc::clone(&services.agent_registry),
        session_search,
        boot.workspace_root(),
        boot.clock().clone(),
    ))
}

fn memory_plugin(workspace_root: &std::path::Path) -> Arc<dyn KernelPlugin> {
    Arc::new(uva::context::MemoryPlugin::new(
        uva::context::FileMemoryConfig::new(workspace_root, "uva").with_scopes(
            vec![
                uva::context::MemoryScope::Local,
                uva::context::MemoryScope::Project,
            ],
        ),
    ))
}

fn tool_registry_plugin(services: &BootServices) -> Arc<dyn KernelPlugin> {
    // The registry is the shared container host plugins and the
    // brick rows register into, so the boot owns it before the mount
    // — the one row that stays a declarative publication.
    Arc::new(ToolRegistryPlugin::new(Arc::clone(&services.tool_registry)))
}

fn read_tool_plugin(
    _boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    Ok(Arc::new(uva::tool_read::ReadToolPlugin::new()))
}

fn write_tool_plugin(
    _boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    Ok(Arc::new(uva::tool_write::WriteToolPlugin::new()))
}

fn shell_tool_plugin(
    _boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    // The brick keeps its own execution-policy pair; the `sandbox`
    // service is a separate seam.
    Ok(Arc::new(uva::tool_shell::ShellToolPlugin::new()))
}

fn todo_tool_plugin(
    _boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    Ok(Arc::new(uva::tool_todo::TodoWriteToolPlugin::new()))
}

fn web_tool_plugin(
    _boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    Ok(Arc::new(uva::tool_web::WebFetchToolPlugin::new()))
}

/// The `model-adapter` factory: the core entry built from the
/// profile row's `config`.
///
/// The boot materializes the row's config from the workspace
/// configuration when the profile leaves it empty, so this factory
/// never falls back to a host-built catalogue. The parsed
/// [`ModelAdapterConfig`](uva::runtime::ModelAdapterConfig)
/// builds the catalogue and the tier-1 sections wrap the default
/// handle.
struct ModelAdapterFactory;

impl PluginFactory for ModelAdapterFactory {
    fn id(&self) -> &'static str {
        "model-adapter"
    }

    fn manifest(&self) -> PluginManifest {
        uva::runtime::model_adapter_manifest()
    }

    fn build(
        &self,
        config: &Value,
        _capabilities: &[Capability],
        boot: &BootContext,
    ) -> Result<Arc<dyn KernelPlugin>, PluginError> {
        use uva::runtime::{ModelAdapterConfig, ModelAdapterServicePlugin};

        let sleeper: std::sync::Arc<dyn uva::core::sleeper::Sleeper> =
            std::sync::Arc::new(uva::harness_tokio::TokioSleeper);
        let clock = boot.clock().clone();

        if let Value::Null = config {
            return Err(PluginError::new(
                "`model-adapter` config is empty; the boot must materialize \
                 the deployment's workspace configuration into the row",
            ));
        }
        if let Value::Object(map) = config
            && map.is_empty()
        {
            return Err(PluginError::new(
                "`model-adapter` config is empty; the boot must \
                 materialize the deployment's workspace configuration \
                 into the row",
            ));
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
        let catalogue =
            std::sync::Arc::new(uva::runtime::catalogue_from_config(&parsed));
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

fn session_store_plugin(
    boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    Ok(Arc::new(uva::session::SessionStorePlugin::new(
        uva::session::SessionStoreConfig {
            sessions_dir: boot
                .sessions_dir()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_else(|| boot.workspace_root().join("sessions")),
            blocking: boot.blocking(),
        },
    )))
}

fn sandbox_plugin(
    _boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    Ok(Arc::new(SandboxPlugin::new(
        uva::tool_shell::SandboxPluginConfig::default(),
    )))
}

/// The `subagent-backend` row's plugin: the delegation backend the
/// `subagents` service publishes.
///
/// The backend is the row's own object: the row reads the peer-agent
/// **registry** (a container) and builds the delegator over it, so
/// the boot hands the row a registry instead of a pre-built
/// delegator.
fn subagent_plugin(services: &BootServices) -> Arc<dyn KernelPlugin> {
    let delegator: Arc<dyn DelegatingTool> =
        Arc::new(uva::tool_task::TaskDelegator::new(Arc::clone(
            &services.agent_registry,
        )));
    Arc::new(SubagentPlugin::new(delegator))
}

fn compaction_plugin(
    boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    // The manifest probe runs with an empty boot context; the policy
    // itself is only needed to build a real mount.
    let config = boot.compaction().cloned().unwrap_or_else(|| {
        uva::session::CompactionPluginConfig {
            model: String::new(),
            settings: uva::session::CompactionSettings::default(),
        }
    });
    Ok(Arc::new(CompactionPlugin::new(config)))
}

/// The `approval` row's plugin: the deployment's default approval
/// stance.
///
/// The policy is the row's own object — the boot holds no stance, so
/// the row publishes the default and the deployment's grants layer
/// over it.
fn approval_plugin(
    _boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    Ok(Arc::new(ApprovalPlugin::new(ApprovalPolicy::default())))
}

fn skill_plugin(
    boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    Ok(Arc::new(SkillPlugin::new(uva::skill::SkillPluginConfig {
        workspace_root: boot.workspace_root().to_path_buf(),
    })))
}

/// The `agent-loop` row's `config`: the deployment's reasoning loop,
/// its tool restriction, and its compaction policy.
///
/// ```json
/// {
///   "strategy": "chain-of-thought",
///   "tool_restriction": { "allow": ["read"], "deny": ["write"] },
///   "compaction": { "enabled": true, "reserve_tokens": 16384,
///                   "keep_recent_tokens": 20000,
///                   "min_messages_between_compaction": 4 }
/// }
/// ```
///
/// Every field is optional: an absent field means "the deployment
/// configured nothing", which is not the same as an empty value. The
/// compaction object is
/// [`CompactionSettings`](uva::session::CompactionSettings) in
/// its JSON form.
#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct AgentLoopRowConfig {
    /// The strategy alias (`chain-of-thought`, `best-of-n`, `react`,
    /// …) the row resolves through the deployment's strategy registry.
    strategy: Option<String>,
    /// The allow/deny list the loop installs.
    tool_restriction: Option<ToolRestriction>,
    /// The policy a per-run compactor builds from.
    compaction: Option<uva::session::CompactionSettings>,
}

/// The `agent-loop` factory: builds the runtime's service plugin,
/// whose apply resolves `llm.default` + `compaction` and publishes
/// the deployment's one [`AgentLoopFactory`].
///
/// The defaults come from the **row's own config** — the boot
/// materializes the deployment's agent table into the row when the
/// profile leaves it empty — so a remount (a provider swap, a config
/// edit) rebuilds the factory against the new services without
/// touching them.
struct AgentLoopFactoryEntry;

impl PluginFactory for AgentLoopFactoryEntry {
    fn id(&self) -> &'static str {
        "agent-loop"
    }

    fn manifest(&self) -> PluginManifest {
        uva::runtime::agent_loop_manifest()
    }

    fn build(
        &self,
        config: &Value,
        _capabilities: &[Capability],
        boot: &BootContext,
    ) -> Result<Arc<dyn KernelPlugin>, PluginError> {
        use uva::{
            context::TruncatingContextManager,
            runtime::AgentLoopDefaults,
        };

        let services = boot.host::<BootServices>().ok_or_else(|| {
            PluginError::new(
                "the boot context carries no boot services; cannot build \
                 `agent-loop`",
            )
        })?;
        let parsed = agent_loop_row_config(config)?;
        let defaults = AgentLoopDefaults {
            // The delegation backend arrives as a service: the
            // runtime's plugin resolves `subagents` and appends it.
            delegators: Vec::new(),
            tool_surface: None,
            strategy: parsed.strategy,
            tool_restriction: parsed.tool_restriction,
            log_writer: None,
            subagent_writers: None,
            // The stateless request-level projection, one instance.
            context_manager: Some(Arc::new(TruncatingContextManager)),
            // The deployment's plugin host: the container every
            // `[plugins]` registration lives in.
            host: Some(services.plugin_host.clone()),
            // The server's spawner: a run's detached work executes on
            // the deployment's runtime.
            spawner: Some(crate::spawner::server_spawner()),
            compaction: parsed.compaction,
        };
        Ok(Arc::new(uva::runtime::agent_loop_service_plugin(defaults)))
    }
}

/// The `agent-loop` row's resolved defaults: the strategy the alias
/// names, the restriction, and the compaction policy.
struct AgentLoopRowDefaults {
    strategy: Option<Arc<dyn uva::harness::ReasoningStrategy>>,
    tool_restriction: Option<Arc<ToolRestriction>>,
    compaction: Option<uva::runtime::CompactionBoot>,
}

/// Parse the `agent-loop` row's config into the deployment's loop
/// defaults.
///
/// An absent config (the row's default) is the deployment's "nothing
/// configured": no strategy, no restriction, no compaction. A config
/// that names an unknown strategy alias is **not** an error: the alias
/// is logged and the loop keeps its default reasoning, the same
/// fail-soft contract the agent table's reader has.
fn agent_loop_row_config(
    config: &Value,
) -> Result<AgentLoopRowDefaults, PluginError> {
    let parsed = match config {
        Value::Null => AgentLoopRowConfig::default(),
        Value::Object(map) if map.is_empty() => AgentLoopRowConfig::default(),
        other => serde_json::from_value(other.clone()).map_err(|error| {
            PluginError::new(format!(
                "`agent-loop` config is not an AgentLoopRowConfig: {error}"
            ))
        })?,
    };
    Ok(AgentLoopRowDefaults {
        strategy: parsed.strategy.and_then(|alias| {
            match crate::state::resolve_strategy(&alias) {
                Ok(strategy) => Some(strategy),
                Err(error) => {
                    tracing::warn!(
                        target: "uva.agent",
                        configured = %alias,
                        error = %error,
                        "unknown strategy in the agent-loop row config; \
                         falling back to react"
                    );
                    None
                }
            }
        }),
        tool_restriction: parsed.tool_restriction.map(Arc::new),
        compaction: parsed.compaction.and_then(|settings| {
            if !settings.enabled {
                return None;
            }
            if let Err(error) = settings.validate() {
                tracing::warn!(
                    error = %error,
                    "invalid compaction settings in the agent-loop row \
                     config; the loop keeps no compactor"
                );
                return None;
            }
            Some(uva::runtime::CompactionBoot {
                settings,
                sleeper: crate::spawner::server_sleeper(),
                timeout: uva::session::DEFAULT_SUMMARY_TIMEOUT,
            })
        }),
    })
}

fn mcp_plugin(
    _boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    Ok(Arc::new(McpPlugin::new()))
}

fn eval_plugin(
    boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    Ok(Arc::new(EvalPlugin::new(uva::eval::EvalPluginConfig {
        keyword_metric: true,
        clock: Some(boot.clock().clone()),
    })))
}

fn scheduler_plugin(
    boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    Ok(Arc::new(SchedulerPlugin::new(
        uva::scheduler::SchedulerPluginConfig {
            schedules_dir: boot.workspace_root().join("schedules"),
        },
    )))
}

fn schedule_tool_plugin(
    _boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    Ok(Arc::new(ScheduleToolPlugin::new()))
}

fn attachment_plugin(
    boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    Ok(Arc::new(AttachmentPlugin::new(
        uva::attachment::AttachmentPluginConfig {
            dir: boot.workspace_root().join(".attachments"),
            clock: Some(boot.clock().clone()),
        },
    )))
}

#[cfg(test)]
mod tests;
