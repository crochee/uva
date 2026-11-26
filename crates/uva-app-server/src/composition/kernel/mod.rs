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
    agent::AgentRegistry,
    approval::ApprovalPlugin,
    attachment::AttachmentPlugin,
    core::SharedClock,
    eval::EvalPlugin,
    harness::Host,
    hook::{HookPlugin, HookRegistry},
    kernel::{
        Context,
        MountError,
        MountHandle,
        MountRow,
        Plugin as KernelPlugin,
        Realms,
        mount_all_with,
        parent_service_available,
    },
    mcp::McpPlugin,
    scheduler::SchedulerPlugin,
    session::CompactionPlugin,
    skill::SkillPlugin,
    tool::{ToolRegistry, ToolRegistryPlugin, ToolRestriction},
    tool_scheduler::ScheduleToolPlugin,
    tool_shell::SandboxPlugin,
    tool_task::{TaskTool, TaskToolPlugin},
};

use crate::{
    profile::{
        BootContext,
        Capability,
        Manifest,
        PluginError,
        PluginFactories,
        PluginFactory,
        PluginManifest,
        Profile,
        ProfileError,
        build_plugins,
        effective_inject,
    },
    session::{
        session_index::SessionIndexSlot,
        subagent_router::SubagentWriterSlot,
    },
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
    /// and the `tool-task` row's tool resolves names through.
    pub agent_registry: Arc<AgentRegistry>,
    /// The host's child-session writer source: the `tool-task` row hands
    /// it to the tool it registers, so a delegated child opens (and
    /// persists) its own session. The slot is filled immediately after
    /// the mount, when the `sessions` service exists.
    pub subagent_writers: Arc<SubagentWriterSlot>,
    /// The control surface's view of the sessions: the `tool-task`
    /// row hands the slot to the tool it registers, so the roster can
    /// be rebuilt from the logs and a cold wake can find the child it
    /// wakes. The slot is filled immediately after the mount, when the
    /// `sessions` service exists.
    pub session_index: Arc<SessionIndexSlot>,
    /// The deployment's usage counters. The `tool-task` row reports a
    /// delegated child's tokens and turns here: the child runs on its
    /// own harness, so its events never reach a session controller and
    /// would otherwise vanish from `/api/v1/chat/usage`.
    pub usage: Arc<crate::state::UsageMetrics>,
    /// The deployment's one interception table: the host below
    /// carries it, the default policy writes its logging hook into it,
    /// and the `hooks` row publishes this same instance so an
    /// out-of-tree registration lands on the table every run
    /// dispatches (ADR 0028).
    ///
    /// Taken from the host's read accessor at boot, so the two can
    /// never name different tables.
    pub hook_registry: HookRegistry,
    /// The deployment's host: the steering policy (guards / hints /
    /// tracker / output transformers / tool bindings) every agent this
    /// server builds receives, over the registry above.
    ///
    /// This is the boot's one host clone: it shares every
    /// registration with the host the run assembly receives, so a
    /// bind reaches the same registry.
    pub plugin_host: Host,
    /// The owner of every session's files: the deployment's disk, or
    /// the client's workspace when the client owns it.
    ///
    /// The `read` and `write` tools read and write through it, so a
    /// session a client claimed touches the client's files, not the
    /// deployment's copy of them.
    pub file_system: Arc<crate::session::files::SessionFileSystem>,
    /// The owner of every session's terminal. The `tool-shell` row
    /// asks it before it spawns anything, so a session whose client
    /// owns the terminal runs there instead of on this machine.
    pub terminals: Arc<crate::session::terminals::SessionTerminals>,
    /// The deployment's background jobs. The `tool-job` row files
    /// them, so a job started in one call is still there in the next.
    pub jobs: Arc<uva::tool_job::JobRegistry>,
    /// The sessions service's slot.
    ///
    /// The `sessions` service is published by a row during the mount,
    /// so it does not exist when these containers are built. The
    /// `tool-goal` row needs it (the goal lives in the session's
    /// log), so it resolves through this slot, filled immediately
    /// after the mount.
    pub sessions: SessionSlot,
    /// The deployment's language servers ✓. The `tool-lsp` row files
    /// them, so the tool can ask the server that speaks a language ✓.
    pub lsp_servers: Arc<dyn uva::tool_lsp::LspServers>,
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
    /// A required row is parked with missing services at boot.
    #[error("required row `{id}` is parked waiting for services: {missing:?}")]
    RequiredParked {
        /// The parked required row.
        id: String,
        /// The service keys it still waits for.
        missing: Vec<String>,
    },
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
    const ENTRIES: [&str; 28] = [
        "tool-registry",
        "tool-read",
        "tool-write",
        "tool-edit",
        "tool-job",
        "tool-glob",
        "tool-goal",
        "tool-grep",
        "tool-lsp",
        "tool-shell",
        "tool-todo",
        "tool-web",
        "model-adapter",
        "memory",
        "search",
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
        "schedule-tool",
        "attachments",
    ];
    Profile {
        core: ENTRIES
            .into_iter()
            .map(|id| Manifest::plain(id, id))
            .collect(),
        ui: Vec::new(),
        hmr: false,
    }
}

/// One boot mount.
pub(crate) struct MountedProfile {
    groups: Vec<MountHandle>,
    realms: Arc<Realms>,
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
/// registered in; the [`MountedProfile`] owns the group's effects and
/// fibers. `AppState` keeps both — the context for consumers
/// ([`Context::require`]) and the handle for unloading.
///
/// The profile is the boot's authoritative core assembly: a row whose
/// name has no registered factory fails the mount, a `disabled` row
/// never builds, and a row omitted from the profile is not mounted.
/// [`native_profile`] is the no-profile default.
///
/// # Missing dependencies park, they do not fail
///
/// dsh semantics: a row whose injected service is missing mounts as a
/// parked fiber instead of failing the group. After the mount the
/// stanza **audits** — on every mount path, not only the boot's (see
/// [`mount_built`]): every parked row is reported (its id and the
/// service keys it still waits for), and only [`REQUIRED_ROWS`] being
/// parked is fatal — an agent host without its model loop must never
/// look alive.
pub(crate) fn mount_profile(
    services: Arc<BootServices>,
    profile: &Profile,
    clock: SharedClock,
    extras: BootExtras,
) -> Result<(Context, MountedProfile), KernelBootError> {
    let context = Context::new();
    let realms = Arc::new(Realms::default());
    let built = build_rows(&services, &realms, clock, &extras, profile)?;
    let handle = mount_built(&context, &realms, built, profile)?;
    Ok((
        context,
        MountedProfile {
            groups: vec![handle],
            realms,
        },
    ))
}

/// Rows built through the one build recipe, ready to mount.
///
/// Opaque on purpose: how a built row is shaped (plugin value plus the
/// mount row it mounts under) is the stanza's implementation, not a
/// fact callers build on. It carries its own audit inputs (the
/// factory directory and the row manifests it built), so
/// [`mount_built`] audits without the caller re-deriving them.
pub(crate) struct BuiltRows {
    pairs: Vec<(Arc<dyn KernelPlugin>, MountRow)>,
    factories: PluginFactories,
    units: Vec<Manifest>,
}

/// Build one group of rows through the one recipe.
///
/// Every build — the boot's full profile and every hot-reload subset —
/// goes through here: the factory directory, the [`BootContext`] the
/// rows build from, and [`build_plugins`] are one stanza with one
/// owner. A remount therefore cannot drift from the boot's recipe: a
/// remounted `search` keeps reading the boot's session corpus, a
/// remounted `context-compactor` keeps the boot's compaction model.
///
/// Building is side-effect free on the mount: a caller that needs to
/// unload before mounting (the hot-reload path, so a bad row cannot
/// take a working plugin down) calls this first, unloads, then hands
/// the result to [`mount_built`].
pub(crate) fn build_rows(
    services: &Arc<BootServices>,
    realms: &Arc<Realms>,
    clock: SharedClock,
    extras: &BootExtras,
    rows: &Profile,
) -> Result<BuiltRows, KernelBootError> {
    let factories = native_factories(Arc::clone(services));
    let boot = BootContext::new(extras.workspace_root.clone())
        .with_host(Arc::clone(services))
        .with_clock(clock)
        .with_blocking(extras.blocking.clone())
        .with_compaction(extras.compaction.clone())
        .with_sessions_dir(extras.sessions_dir.clone())
        .with_approval(extras.approval.clone());
    let pairs = build_plugins(rows, &factories, &boot, realms)?
        .into_iter()
        .map(|built| (built.plugin, built.row))
        .collect();
    let units = rows.core_rows().into_iter().cloned().collect();
    Ok(BuiltRows {
        pairs,
        factories,
        units,
    })
}

/// Mount what [`build_rows`] built, as one group, and audit it.
///
/// The kernel orders and validates the group; the returned handle owns
/// its effects and fibers. After the group mounts the stanza audits
/// against `profile` (the effective assembly the mounted tree will
/// correspond to): every parked row is reported, and a parked
/// [`REQUIRED_ROWS`] row fails the mount — on every mount path, not
/// just the boot's. An agent host without its model loop must never
/// look alive.
pub(crate) fn mount_built(
    scope: &Context,
    realms: &Arc<Realms>,
    built: BuiltRows,
    profile: &Profile,
) -> Result<MountHandle, KernelBootError> {
    let handle = mount_all_with(scope, realms, &built.pairs)?;
    let units: Vec<&Manifest> = built.units.iter().collect();
    audit_pending(scope, &units, &built.factories, profile)?;
    Ok(handle)
}

impl MountedProfile {
    /// The isolation-label registry the boot mounted with — HMR
    /// remounts reuse it so a `Shared` label names one scope for the
    /// process's life.
    #[must_use]
    pub(crate) fn realms(&self) -> Arc<Realms> {
        Arc::clone(&self.realms)
    }
}

/// The rows whose being parked is fatal at boot: without them the
/// machine must not half-work.
const REQUIRED_ROWS: [&str; 2] = ["agent-loop", "model-adapter"];

/// Report every parked row and its still-missing service keys; a
/// parked required row is fatal.
fn audit_pending(
    context: &Context,
    rows: &[&Manifest],
    factories: &PluginFactories,
    profile: &Profile,
) -> Result<(), KernelBootError> {
    // Every row of the assembly — group children included, since they
    // mount beneath their group's fiber.
    let assembled: Vec<&Manifest> = profile
        .all_rows()
        .into_iter()
        .filter(|row| {
            rows.iter().any(|unit| {
                unit.id == row.id
                    || unit.rows().iter().any(|child| child.id == row.id)
            })
        })
        .collect();
    // What each row provides, and what it waits for.
    let provides: std::collections::BTreeMap<&str, Vec<String>> = assembled
        .iter()
        .map(|row| {
            let keys = factories
                .get(&row.name)
                .map(|factory| factory.manifest().provides)
                .unwrap_or_default();
            (row.id.as_str(), keys)
        })
        .collect();
    let edges: std::collections::BTreeMap<&str, Vec<String>> = assembled
        .iter()
        .map(|row| {
            let keys = factories
                .get(&row.name)
                .map(|factory| {
                    let manifest = factory.manifest();
                    effective_inject(&manifest.inject, row.inject.as_deref())
                })
                .unwrap_or_default();
            (row.id.as_str(), keys)
        })
        .collect();
    // A row is live when every key it waits for is provided by a live
    // row or already present in the parent scope. The fixpoint matters:
    // a row whose provider is itself parked is parked too.
    let mut live: std::collections::BTreeSet<&str> =
        std::collections::BTreeSet::new();
    loop {
        let mut changed = false;
        for row in &assembled {
            if live.contains(row.id.as_str()) {
                continue;
            }
            let missing = edges
                .get(row.id.as_str())
                .map(|keys| {
                    keys.iter()
                        .filter(|key| {
                            !live_provides(&provides, &live, key)
                                && !parent_service_available(context, key)
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if missing.is_empty() {
                live.insert(row.id.as_str());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    for row in &assembled {
        if live.contains(row.id.as_str()) {
            continue;
        }
        let missing: Vec<String> = edges
            .get(row.id.as_str())
            .map(|keys| {
                keys.iter()
                    .filter(|key| {
                        !live_provides(&provides, &live, key)
                            && !parent_service_available(context, key)
                    })
                    .cloned()
                    .collect()
            })
            .unwrap_or_default();
        tracing::warn!(
            row = %row.id,
            missing = ?missing,
            "the row is parked waiting for services (dsh PENDING)"
        );
        if REQUIRED_ROWS.contains(&row.id.as_str()) {
            return Err(KernelBootError::RequiredParked {
                id: row.id.clone(),
                missing,
            });
        }
    }
    Ok(())
}

/// One row of the deployment's plugin-tree audit: what the row is,
/// and — when it is not usable — why.
///
/// The wire shape of `profile/audit`: `id` and `name` identify the
/// row, `state` says which list it belongs to, `missing` names the
/// service keys a pending row still waits for.
#[derive(
    Debug,
    Clone,
    PartialEq,
    Eq,
    serde::Serialize,
    serde::Deserialize,
    schemars::JsonSchema,
)]
pub struct RowAudit {
    /// The row's id (its own, not its factory's).
    pub id: String,
    /// The row's factory name.
    pub name: String,
    /// `failed` (never started, or the activation rejected) or
    /// `pending` (waiting for a service the composition does not
    /// supply). A row in neither list is usable.
    pub state: &'static str,
    /// The service keys a pending row still waits for.
    ///
    /// Empty for a failed row — the failure is the activation's, not
    /// a dependency's.
    pub missing: Vec<String>,
}

/// Audit the deployment's plugin tree: the `failed` and `pending`
/// rows (dsh `auditRows`, `packages/preset/agent-preset-registry/
/// src/mount.ts:115-152`). Every row in neither list is usable.
///
/// The kernel is the truth for the state: `PluginState::Failed` is a
/// failed row, `PluginState::Pending` a pending one. The missing keys
/// are recomputed with the same fixpoint the mount's audit uses, so
/// the report names the same services the mount would.
pub fn plugin_tree_audit(
    context: &Context,
    profile: &Profile,
    services: &Arc<BootServices>,
) -> Vec<RowAudit> {
    let factories = profile_factories(Arc::clone(services));
    let rows: Vec<&Manifest> = profile.all_rows();
    // What each row provides, and what it waits for.
    let provides: std::collections::BTreeMap<&str, Vec<String>> = rows
        .iter()
        .map(|row| {
            let keys = factories
                .get(&row.name)
                .map(|factory| factory.manifest().provides)
                .unwrap_or_default();
            (row.id.as_str(), keys)
        })
        .collect();
    let edges: std::collections::BTreeMap<&str, Vec<String>> = rows
        .iter()
        .map(|row| {
            let keys = factories
                .get(&row.name)
                .map(|factory| {
                    let manifest = factory.manifest();
                    effective_inject(&manifest.inject, row.inject.as_deref())
                })
                .unwrap_or_default();
            (row.id.as_str(), keys)
        })
        .collect();
    // The fixpoint: a row is live when every key it waits for is
    // provided by a live row or already present in the parent scope.
    let mut live: std::collections::BTreeSet<&str> =
        std::collections::BTreeSet::new();
    loop {
        let mut changed = false;
        for row in &rows {
            if live.contains(row.id.as_str()) {
                continue;
            }
            let missing = edges
                .get(row.id.as_str())
                .map(|keys| {
                    keys.iter()
                        .filter(|key| {
                            !live_provides(&provides, &live, key)
                                && !parent_service_available(context, key)
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if missing.is_empty() {
                live.insert(row.id.as_str());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    // The report: a row that is not live names the keys it waits for.
    rows.iter()
        .filter(|row| !live.contains(row.id.as_str()))
        .map(|row| {
            let missing = edges
                .get(row.id.as_str())
                .map(|keys| {
                    keys.iter()
                        .filter(|key| {
                            !live_provides(&provides, &live, key)
                                && !parent_service_available(context, key)
                        })
                        .cloned()
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            RowAudit {
                id: row.id.clone(),
                name: row.name.clone(),
                state: if missing.is_empty() {
                    // Not live, and nothing it waits for: the row never
                    // started or its activation was rejected.
                    "failed"
                } else {
                    "pending"
                },
                missing,
            }
        })
        .collect()
}

/// Whether a live row provides `key`.
fn live_provides(
    provides: &std::collections::BTreeMap<&str, Vec<String>>,
    live: &std::collections::BTreeSet<&str>,
    key: &str,
) -> bool {
    provides
        .iter()
        .filter(|(row, _)| live.contains(*row))
        .any(|(_, keys)| keys.iter().any(|provided| provided == key))
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
    /// The deployment's approval stance: which calls are held, and
    /// how long a held call waits for an operator.
    ///
    /// `None` means the default stance — high-risk calls are asked
    /// about, and the ask waits for a human.
    pub approval: Option<uva::approval::ApprovalPolicy>,
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
        agent_registry: Arc::new(AgentRegistry::new()),
        subagent_writers: Arc::new(SubagentWriterSlot::new()),
        session_index: Arc::new(SessionIndexSlot::new()),
        usage: Arc::new(crate::state::UsageMetrics::default()),
        hook_registry: HookRegistry::new(),
        plugin_host: Host::default(),
        file_system: Arc::new(crate::session::files::SessionFileSystem::new()),
        terminals: Arc::new(crate::session::terminals::SessionTerminals::new()),
        jobs: Arc::new(uva::tool_job::JobRegistry::new()),
        sessions: Arc::new(std::sync::OnceLock::new()),
        lsp_servers: Arc::new(crate::session::lsp::DeploymentLspServers::new(
            Vec::new(),
        )),
    }))
}

/// The factory directory the boot registers: one entry per native
/// plugin, all sharing the boot's containers.
fn native_factories(services: Arc<BootServices>) -> PluginFactories {
    // Rows the boot constructs: their plugins build the capability in
    // `apply` from the boot context (config only — no containers).
    const BOOT_SPECS: [(&str, BootMake); 12] = [
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
    // Rows bound to the boot's containers: the shared tool registry,
    // the peer-agent registry the `task` tool resolves names through,
    // and the deployment's one interception table — the `hooks` row
    // publishes the same registry the host carries, and a remount
    // rebuilds the plugin over that same instance (ADR 0028).
    const SPECS: [(&str, Make); 12] = [
        ("tool-registry", tool_registry_plugin),
        ("tool-read", read_tool_plugin),
        ("tool-write", write_tool_plugin),
        ("tool-edit", edit_tool_plugin),
        ("tool-job", job_tool_plugin),
        ("tool-glob", glob_tool_plugin),
        ("tool-goal", goal_tool_plugin),
        ("tool-grep", grep_tool_plugin),
        ("tool-lsp", lsp_tool_plugin),
        ("tool-shell", shell_tool_plugin),
        ("tool-task", task_tool_plugin),
        ("hooks", hooks_plugin),
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

fn read_tool_plugin(services: &BootServices) -> Arc<dyn KernelPlugin> {
    Arc::new(uva::tool_read::ReadToolPlugin::with_owner(Arc::clone(
        &services.file_system,
    )
        as Arc<dyn uva::tool_files::FileSystemOwner>))
}

fn lsp_tool_plugin(services: &BootServices) -> Arc<dyn KernelPlugin> {
    // The language servers the deployment configured, and the owner of
    // the files the tool reads ✓.
    Arc::new(uva::tool_lsp::LspToolPlugin::with_owner(
        Arc::clone(&services.lsp_servers),
        Arc::clone(&services.file_system)
            as Arc<dyn uva::tool_files::FileSystemOwner>,
    ))
}

fn goal_tool_plugin(services: &BootServices) -> Arc<dyn KernelPlugin> {
    // The goal lives in the session's log: the store reads and writes
    // it through the deployment's session registry, resolved from the
    // slot the boot fills right after the mount.
    Arc::new(uva::tool_goal::GoalToolPlugin::new(Arc::new(
        crate::session::goals::SessionGoals::new(Arc::clone(
            &services.sessions,
        )),
    )))
}

/// The sessions service, resolved after the mount.
pub(crate) type SessionSlot =
    Arc<std::sync::OnceLock<Arc<uva::session::manager::SessionRegistry>>>;

fn glob_tool_plugin(services: &BootServices) -> Arc<dyn KernelPlugin> {
    // The file finder reads through the deployment's file owner ✓.
    Arc::new(uva::tool_glob::GlobToolPlugin::with_owner(Arc::clone(
        &services.file_system,
    )
        as Arc<dyn uva::tool_files::FileSystemOwner>))
}

fn grep_tool_plugin(services: &BootServices) -> Arc<dyn KernelPlugin> {
    // The content searcher reads through the same owner ✓.
    Arc::new(uva::tool_grep::GrepToolPlugin::with_owner(Arc::clone(
        &services.file_system,
    )
        as Arc<dyn uva::tool_files::FileSystemOwner>))
}

fn job_tool_plugin(services: &BootServices) -> Arc<dyn KernelPlugin> {
    // The registry is the deployment's: the tool is a deployment-level
    // singleton, so a job started in one call is still there in the
    // next one, and a session's controller can hear about it.
    Arc::new(uva::tool_job::JobToolPlugin::with_tool(Arc::new(
        uva::tool_job::JobTool::with_registry(Arc::clone(&services.jobs)),
    )))
}

fn edit_tool_plugin(services: &BootServices) -> Arc<dyn KernelPlugin> {
    Arc::new(uva::tool_edit::EditToolPlugin::with_owner(Arc::clone(
        &services.file_system,
    )
        as Arc<dyn uva::tool_files::FileSystemOwner>))
}

fn write_tool_plugin(services: &BootServices) -> Arc<dyn KernelPlugin> {
    Arc::new(uva::tool_write::WriteToolPlugin::with_owner(Arc::clone(
        &services.file_system,
    )
        as Arc<dyn uva::tool_files::FileSystemOwner>))
}

fn shell_tool_plugin(services: &BootServices) -> Arc<dyn KernelPlugin> {
    // The brick keeps its own execution-policy pair; the `sandbox`
    // service is a separate seam. The terminal owner is a third: a
    // session whose client owns the terminal runs there.
    Arc::new(uva::tool_shell::ShellToolPlugin::with_terminals(
        Arc::clone(&services.terminals)
            as Arc<dyn uva::tool_shell::TerminalOwner>,
    ))
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
            crate::state::catalogue_for_deployment(&parsed.catalogue),
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

/// The `tool-task` row's plugin: the `task` tool as an ordinary tool
/// brick.
///
/// The tool is the row's own object: the row reads the peer-agent
/// **registry** and the host's child-session writer source (containers)
/// and builds a [`TaskTool`] over them; the plugin files it in the
/// `tools` registry, so exposure, restriction and unload follow the
/// same path every other tool brick uses.
fn task_tool_plugin(services: &BootServices) -> Arc<dyn KernelPlugin> {
    let writers: Arc<dyn uva::agent::SubagentWriters> =
        Arc::clone(&services.subagent_writers)
            as Arc<dyn uva::agent::SubagentWriters>;
    let tool = TaskTool::new(Arc::clone(&services.agent_registry))
        .with_writers(writers)
        .with_session_index(Arc::clone(&services.session_index)
            as Arc<dyn uva::tool_task::SessionIndex>)
        // A child's tokens and turns are real consumption: the tool
        // reports them into the same counters `/api/v1/chat/usage`
        // reads, because the child's own events never reach a
        // controller.
        .with_usage(Arc::new(crate::state::DelegatedUsage::new(Arc::clone(
            &services.usage,
        ))))
        // The round tail of a dispatch whose future the harness drops
        // (parent cancelled mid-call) detaches here: the child session
        // still gets its exit footer and artefact set.
        .with_spawner(crate::spawner::server_spawner());
    // The lifecycle parks idle children on its own: the loop runs on
    // the server's own runtime seams, and the tool brick gives it the
    // session index (the artefact set's home) above.
    Arc::new(TaskToolPlugin::new(Arc::new(tool)).with_park_loop(
        crate::spawner::server_spawner(),
        crate::spawner::server_sleeper(),
    ))
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

/// The `approval` row's plugin: the deployment's approval stance.
///
/// The policy is the row's own object — the boot resolved it from the
/// deployment's configuration, so the row publishes *that* policy
/// rather than a hard-coded default. A deployment that configured
/// nothing gets the default stance: high-risk calls are asked about,
/// and the ask waits for a human.
fn approval_plugin(
    boot: &BootContext,
) -> Result<Arc<dyn KernelPlugin>, PluginError> {
    let policy = boot.approval().cloned().unwrap_or_default();
    Ok(Arc::new(ApprovalPlugin::new(policy)))
}

/// The `hooks` row's plugin: the deployment's one interception table,
/// published as `uva_hook::KEY_HOOKS`.
///
/// The plugin takes the boot's registry — the same instance the
/// deployment's host carries — so an out-of-tree registration made
/// through the service lands on the table every run dispatches, and a
/// remount republishes that instance instead of swapping in a fresh
/// one (ADR 0028). The row is optional and not an `inject` edge, so a
/// profile that omits it or switches it off still boots with the full
/// default policy.
fn hooks_plugin(services: &BootServices) -> Arc<dyn KernelPlugin> {
    Arc::new(HookPlugin::new(services.hook_registry.clone()))
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
///   "strategy": "react",
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
    /// The strategy alias (`react`, …) the row resolves through the
    /// deployment's strategy registry.
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
            tool_surface: None,
            strategy: parsed.strategy,
            tool_restriction: parsed.tool_restriction,
            log_writer: None,
            // The stateless request-level projection, one instance.
            context_manager: Some(Arc::new(TruncatingContextManager)),
            // The deployment's plugin host: the container every
            // `[plugins]` registration lives in, and the carrier of
            // the one interception table every run dispatches.
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
