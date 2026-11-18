//! The assembly sub-state: the mounted tree, the two read models that
//! describe it, and the one change path that moves them.
//!
//! # Two read models, one assembly
//!
//! The **raw profile** is the deployment's declaration: every row,
//! switched on or off. The **assembly** is the effective set — the
//! rows that are actually mounted. Both are [`Profile`]s and they are
//! two different values: the settings page reads the raw one (a
//! switched-off row stays visible), the mount reads the assembly (a
//! switched-off row is gone).
//!
//! # One writer
//!
//! [`Assembly::apply`] is the only way the mounted tree moves. It
//! diffs the next effective rows against the current assembly,
//! remounts the ids the diff names, and moves the two read models and
//! the UI tree with it — so the tree and the read models cannot drift
//! apart.

use std::{collections::BTreeSet, sync::Arc};

use parking_lot::RwLock;
use uva::{
    core::SharedClock,
    kernel::{Context, MountHandle, PluginState},
};

use super::mcp::McpRuntime;
use crate::{
    api::v1::profile::{ProfileRow, ProfileSegment},
    composition::kernel::{
        BootExtras,
        BootServices,
        build_rows,
        mount_built,
        profile_factories,
    },
    profile::{
        Profile,
        ProfileDiff,
        ProfileStore,
        Realms,
        diff_profiles,
        effective,
    },
    ui::tree::{UiTree, load_ui_tree},
};

/// The process handles an assembly change needs but does not own.
///
/// The assembly owns the mounted tree and its read models; the clock
/// the rows stamp with and the MCP runtime the `mcp` row's teardown
/// stops belong to the process state that built them, so a change
/// borrows them. The workspace the rows build from is not borrowed
/// per change: it lives in the assembly's stored build recipe, so a
/// remount builds from what the boot built from.
pub(crate) struct AssemblyContext<'a> {
    /// The process clock every row stamps with.
    pub(crate) clock: &'a SharedClock,
    /// The MCP runtime the `mcp` row's teardown stops.
    pub(crate) mcp: &'a McpRuntime,
}

/// The result of one [`Assembly::apply`] reconcile.
#[derive(Debug, Clone)]
pub struct Reconciled {
    /// What changed between the previous effective assembly and the
    /// new one.
    pub diff: ProfileDiff,
    /// The `mcp` row was remounted or removed: supervision needs a
    /// process restart (see [`crate::state::AppState::reload_profile`]'s
    /// Residuals).
    pub mcp_touched: bool,
    /// Rows that lost a provider because that provider left the
    /// document: they can no longer run, so the host writes
    /// `disabled = true` back for them (dsh's self-disable write-back).
    pub dep_lost: Vec<String>,
}

/// Flattens one segment's row tree (group children included) into wire
/// rows, carrying the group `disabled` inheritance.
struct RowCollector<'a> {
    out: Vec<ProfileRow>,
    assembled: &'a Profile,
    factories: &'a crate::profile::PluginFactories,
}

impl RowCollector<'_> {
    fn push(
        &mut self,
        assembly: &Assembly,
        segment: ProfileSegment,
        row: &crate::profile::Manifest,
        inherited_disabled: bool,
    ) {
        let off = row.disabled || inherited_disabled;
        let status = if off || self.assembled.find(&row.id).is_none() {
            "off: disabled".to_owned()
        } else {
            "mounted".to_owned()
        };
        let plugin_state = assembly
            .plugin_state(&row.id)
            .map(|state| format!("{state:?}"));
        let inject = self
            .factories
            .get(&row.name)
            .map(|factory| {
                let manifest = factory.manifest();
                crate::profile::effective_inject(
                    &manifest.inject,
                    row.inject.as_deref(),
                )
            })
            .unwrap_or_else(|| row.inject.clone().unwrap_or_default());
        self.out.push(ProfileRow::new(
            segment,
            row,
            inject,
            status,
            plugin_state,
        ));
        for child in &row.children {
            self.push(assembly, segment, child, off);
        }
    }
}

/// The mounted tree and the two read models that describe it.
///
/// The kernel is the isolation scope every native service is
/// registered in; the mount holds the groups the boot and every
/// remount produced; the two profiles are the read models (the raw
/// declaration and the effective assembly); the UI tree is the slot
/// tree the `[ui]` segment assembled.
pub struct Assembly {
    /// The kernel isolation scope every native service is registered
    /// in. Consumers resolve services here; the graph is what the
    /// mount validated.
    kernel: Context,
    /// The containers the factory directory builds from, kept so a
    /// hot reload can construct an `added` or `remounted` plugin
    /// without rebuilding the boot.
    boot_services: Arc<BootServices>,
    /// The process's shared isolation-label registry: the boot mounts
    /// with it and every HMR remount reuses it, so a `Shared` label
    /// keeps naming one scope for the process's life.
    realms: Arc<Realms>,
    /// The boot's build recipe: the workspace, blocking pool,
    /// compaction policy and session corpus every row builds from. A
    /// hot reload builds with exactly this recipe — [`Self::apply`]
    /// and [`Self::remount_row`] go through the one
    /// [`build_rows`](crate::composition::kernel::build_rows) stanza —
    /// so a remounted row is built the way the boot built it.
    extras: BootExtras,
    /// The mounted plugin groups. The boot group comes first; an HMR
    /// remount appends one group for the rows it rebuilt, so an
    /// untouched plugin's fiber and effects stay where they were.
    /// Dropping a handle unloads every service in that group.
    mount: parking_lot::Mutex<Vec<MountHandle>>,
    /// The raw resolved profile (rows carry `disabled`); the settings
    /// page's read model, so a disabled row stays visible.
    profile: RwLock<Profile>,
    /// The effective assembly the mounted tree corresponds to —
    /// assembled rows only. Boot and every apply update both; the HMR
    /// diff's previous side is this value.
    assembly: RwLock<Profile>,
    /// Layer paths for the transactional write path; `None` in fixtures
    /// (a write then fails with a clear error instead of guessing a
    /// document path).
    profile_store: Option<ProfileStore>,
    /// Serializes profile writes: one writer, so two surfaces cannot
    /// interleave a read-modify-write of the document.
    profile_write: tokio::sync::Mutex<()>,
    /// The `[ui]` segment assembled into the slot tree. Swapping the
    /// tree (HMR) drops the old one, which rolls every declaration and
    /// registration back.
    ui_tree: RwLock<UiTree>,
}

/// What the boot hands the assembly: the kernel context, the containers
/// the factories build from, and the mounted tree with its shared
/// isolation-label registry.
pub(crate) struct BootedAssembly {
    /// The kernel context every service was registered in.
    pub kernel: Context,
    /// The containers a hot reload builds new rows from.
    pub boot_services: Arc<BootServices>,
    /// The mounted groups, in mount order.
    pub mount: Vec<MountHandle>,
    /// The process-wide isolation-label registry.
    pub realms: Arc<Realms>,
    /// The build recipe a hot reload reuses, so a remounted row is
    /// built the way the boot built it.
    pub extras: BootExtras,
}

impl Assembly {
    /// Build the sub-state from the boot's mounted tree.
    pub(crate) fn new(
        booted: BootedAssembly,
        profile: Profile,
        assembly: Profile,
        profile_store: Option<ProfileStore>,
        ui_tree: UiTree,
    ) -> Self {
        Self {
            kernel: booted.kernel,
            boot_services: booted.boot_services,
            realms: booted.realms,
            mount: parking_lot::Mutex::new(booted.mount),
            extras: booted.extras,
            profile: RwLock::new(profile),
            assembly: RwLock::new(assembly),
            profile_store,
            profile_write: tokio::sync::Mutex::new(()),
            ui_tree: RwLock::new(ui_tree),
        }
    }

    /// The effective assembly: the enabled rows the mounted tree
    /// corresponds to.
    ///
    /// This is the HMR diff's previous side: a row the assembly
    /// filtered out reads as `removed`, a row it filtered in reads as
    /// `added`.
    #[must_use]
    pub fn current(&self) -> Profile {
        self.assembly.read().clone()
    }

    /// The layer paths a write goes through, when the deployment has
    /// them.
    #[must_use]
    pub(crate) fn profile_store(&self) -> Option<&ProfileStore> {
        self.profile_store.as_ref()
    }

    /// The settings page's read model: every row of the raw profile,
    /// with the assembly's verdict and the kernel's plugin state.
    ///
    /// A switched-off row is here too — the settings page shows the
    /// switch, so it must see the row that is not mounted.
    #[must_use]
    pub fn rows(&self) -> Vec<ProfileRow> {
        let raw = self.raw();
        let assembled = effective(&raw);
        let factories = profile_factories(Arc::clone(&self.boot_services));
        let mut collector = RowCollector {
            out: Vec::new(),
            assembled: &assembled,
            factories: &factories,
        };
        for (segment, tree) in [
            (ProfileSegment::Core, &raw.core),
            (ProfileSegment::Ui, &raw.ui),
        ] {
            for row in tree {
                collector.push(self, segment, row, false);
            }
        }
        collector.out
    }

    /// A mounted service, by key.
    ///
    /// `None` when the service is not mounted — its row is off, its
    /// plugin was unloaded — or when the key names a service of
    /// another type.
    #[must_use]
    pub fn service<T>(&self, key: &str) -> Option<Arc<T>>
    where
        T: Send + Sync + 'static,
    {
        self.kernel.get::<T>(key).ok().flatten()
    }

    /// The raw profile: the deployment's declaration, switched-off
    /// rows included.
    pub(crate) fn raw(&self) -> Profile {
        self.profile.read().clone()
    }

    /// The raw profile, mutably.
    ///
    /// Only the fixtures reach for it: a test that needs a row to
    /// carry a delegated enablement check writes it here.
    #[cfg(test)]
    pub(crate) fn raw_mut(&self) -> parking_lot::RwLockWriteGuard<'_, Profile> {
        self.profile.write()
    }

    /// The single-writer lock: one profile write at a time.
    ///
    /// The watcher and the write transaction share it, so two
    /// concurrent reconciles cannot unload and mount the same rows
    /// twice.
    pub(crate) fn writer(&self) -> &tokio::sync::Mutex<()> {
        &self.profile_write
    }

    /// The document layer the transactional write path commits to.
    pub(crate) fn store(&self) -> Option<&ProfileStore> {
        self.profile_store.as_ref()
    }

    /// Point the transactional write path at a document layer.
    ///
    /// Only the fixtures repoint it: the boot hands the layer stack
    /// over through [`Self::new`] and never moves it again.
    #[cfg(test)]
    pub(crate) fn set_store(&mut self, store: Option<ProfileStore>) {
        self.profile_store = store;
    }

    /// The deployment's UI tree, as the last activation assembled it.
    pub(crate) fn ui_tree(&self) -> UiTree {
        self.ui_tree.read().clone()
    }

    /// The lifecycle state of `id` in any mounted group.
    ///
    /// `None` means the id is not mounted: never mounted, disposed by
    /// [`Self::dispose_plugin`] (the empty group is pruned), or the
    /// kernel was shut down.
    pub(crate) fn plugin_state(&self, id: &str) -> Option<PluginState> {
        self.mount
            .lock()
            .iter()
            .find_map(|handle| handle.plugin_state(id))
    }

    /// Unload one mounted native plugin.
    ///
    /// Returns `false` when no plugin with `id` is owned (including a
    /// second call for an id already disposed). Consumers the plugin
    /// served are parked by the kernel, not unmounted.
    pub(crate) fn dispose_plugin(&self, id: &str) -> bool {
        let mut handles = self.mount.lock();
        let mut disposed = false;
        for handle in handles.iter_mut().rev() {
            if handle.dispose_plugin(id) {
                disposed = true;
                break;
            }
        }
        // A group whose rows were all disposed owns nothing; dropping it
        // now keeps the vector from growing with every reload.
        handles.retain(|handle| !handle.is_empty());
        disposed
    }

    /// Unload one plugin with the teardown its service needs.
    ///
    /// `mcp` is the one service whose value owns live resources: its
    /// tick loop, the generation registrations, and the transports /
    /// child processes. Stop all three **before** withdrawing the
    /// service so no remote tool outlives it and no timer keeps
    /// running against a dropped supervisor. Every other id is a
    /// plain [`Self::dispose_plugin`].
    pub(crate) async fn unload_plugin(
        &self,
        context: &AssemblyContext<'_>,
        id: &str,
    ) -> bool {
        if id == uva::mcp::KEY_MCP {
            // The boot's tool registry is the one the `mcp` plugin
            // registered its remote tools into.
            context
                .mcp
                .shutdown(&self.boot_services.tool_registry)
                .await;
        }
        self.dispose_plugin(id)
    }

    /// Drop every mounted group, in reverse order.
    ///
    /// A later group (a reload's remount) goes before the boot group
    /// its rows may inject from.
    pub(crate) fn unmount_all(&self) {
        for mounted in self.mount.lock().drain(..).rev() {
            drop(mounted);
        }
    }

    /// Swap the UI tree outright.
    ///
    /// Test-only: the production tree moves with
    /// [`Self::apply`], which assembles it from the rows it just
    /// mounted. A route test that pins the projection of one given
    /// tree installs it here.
    #[cfg(test)]
    pub(crate) fn set_ui_tree(&self, tree: UiTree) {
        *self.ui_tree.write() = tree;
    }

    /// Remount one already-enabled row from the current effective
    /// assembly.
    ///
    /// A capability write changes the files a row builds from, not its
    /// config, so the diff cannot name it: the row is rebuilt and
    /// mounted on its own.
    pub(crate) async fn remount_row(
        &self,
        context: &AssemblyContext<'_>,
        id: &str,
    ) -> Result<(), String> {
        let assembly = self.current();
        let Some(entry) = assembly.find(id).cloned() else {
            return Ok(());
        };
        let one = Profile {
            core: vec![entry],
            ui: Vec::new(),
            hmr: assembly.hmr,
        };
        let built = build_rows(
            &self.boot_services,
            &self.realms,
            context.clock.clone(),
            &self.extras,
            &one,
        )
        .map_err(|error| error.to_string())?;
        self.unload_plugin(context, id).await;
        match mount_built(&self.kernel, &self.realms, built, &assembly) {
            Ok(mounted) => {
                self.mount.lock().push(mounted);
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// One assembly change.
    ///
    /// Only the ids the diff names are touched: `removed` rows are
    /// unloaded, and `added` / `remounted` rows are built and mounted
    /// as one new group (the kernel orders and validates that group).
    /// A `volatile_changed` row is not remounted; the stored profile
    /// moves forward so the next diff is against the new value. If the
    /// new profile turns HMR off, nothing is touched and
    /// [`ProfileDiff::restart_required`] tells the caller to report a
    /// restart.
    ///
    /// The build happens **before** any unload, so a bad row cannot
    /// take a working plugin down. A mount failure after disposal
    /// remounts the previous rows as a best-effort rollback.
    ///
    /// The diff's previous side is [`Self::current`] — the enabled row
    /// set the mounted tree actually corresponds to. A row filtered
    /// out by its `enabled` bit therefore reads as `removed`, and a
    /// row filtered in reads as `added`; the watcher needs no
    /// knowledge of the field.
    ///
    /// `ignore_hmr` is the only difference between the watcher entry
    /// and the explicit-write entry: the diff is computed against an
    /// HMR-on view of the next effective rows, so an `hmr = false`
    /// profile still applies.
    ///
    /// # Residuals
    ///
    /// Remounting the `mcp` row republishes the service, but the
    /// boot-owned tick loop stays stopped: `McpRuntime` is not
    /// restartable in place and its boot-time clients were dropped by
    /// the unload. A process restart is needed to re-supervise the
    /// configured servers. A turn that started before the reload keeps
    /// the service handles it resolved; only the next turn sees the
    /// new graph (this is the desired snapshot behaviour, not a gap).
    ///
    /// # Errors
    ///
    /// Returns a message when the new `[ui]` segment or a changed core
    /// row cannot be built. The mounted set is then restored
    /// **best-effort**: the removed rows are rebuilt from the previous
    /// profile and remounted, and a rebuild that itself fails leaves
    /// those rows unmounted (the error the caller sees is the first
    /// failure, not the rollback's).
    pub(crate) async fn apply(
        &self,
        context: &AssemblyContext<'_>,
        next_raw: Profile,
        ignore_hmr: bool,
    ) -> Result<Reconciled, String> {
        let factories = profile_factories(Arc::clone(&self.boot_services));
        let previous = self.assembly.read().clone();
        // The boot materializes the two row configs it owns
        // (`model-adapter`, `agent-loop`) from the deployment config;
        // a document that leaves them empty keeps that materialization
        // across reconciles, so an unrelated edit does not look like
        // "clear the provider configuration".
        let mut next_raw = next_raw;
        carry_forward_materialized(&mut next_raw, &previous);
        let next = effective(&next_raw);
        let diff_view = if ignore_hmr {
            Profile {
                hmr: true,
                ..next.clone()
            }
        } else {
            next.clone()
        };
        let diff = diff_profiles(&previous, &diff_view, &factories);
        if diff.restart_required {
            return Ok(Reconciled {
                diff,
                mcp_touched: false,
                dep_lost: Vec::new(),
            });
        }
        let tree = load_ui_tree(&next.ui).map_err(|error| error.to_string())?;

        let changed: BTreeSet<&str> = diff
            .added
            .iter()
            .chain(diff.remounted.iter())
            .map(String::as_str)
            .collect();
        let changed_rows = rows_for(&next, &changed);
        let built = build_rows(
            &self.boot_services,
            &self.realms,
            context.clock.clone(),
            &self.extras,
            &changed_rows,
        )
        .map_err(|error| error.to_string())?;

        for id in diff.removed.iter().chain(diff.remounted.iter()) {
            self.unload_plugin(context, id).await;
        }
        if !changed.is_empty() {
            match mount_built(&self.kernel, &self.realms, built, &next) {
                Ok(mounted) => self.mount.lock().push(mounted),
                Err(error) => {
                    tracing::error!(
                        %error,
                        "the profile remount failed; restoring the old rows"
                    );
                    // The rollback set includes the removed ids: the
                    // failed group disposed them, and restoring only
                    // `changed` would leave them unmounted.
                    let rollback_ids: BTreeSet<&str> = diff
                        .added
                        .iter()
                        .chain(diff.remounted.iter())
                        .chain(diff.removed.iter())
                        .map(String::as_str)
                        .collect();
                    let rollback_rows = rows_for(&previous, &rollback_ids);
                    if let Ok(built) = build_rows(
                        &self.boot_services,
                        &self.realms,
                        context.clock.clone(),
                        &self.extras,
                        &rollback_rows,
                    ) && let Ok(mounted) = mount_built(
                        &self.kernel,
                        &self.realms,
                        built,
                        &previous,
                    ) {
                        self.mount.lock().push(mounted);
                    }
                    return Err(error.to_string());
                }
            }
        }

        let mcp_touched = diff
            .removed
            .iter()
            .chain(diff.remounted.iter())
            .any(|id| id == uva::mcp::KEY_MCP);

        let dep_lost = dep_lost_rows(&previous, &next_raw, &diff, &factories);

        *self.ui_tree.write() = tree;
        *self.profile.write() = next_raw;
        *self.assembly.write() = next;
        Ok(Reconciled {
            diff,
            mcp_touched,
            dep_lost,
        })
    }
}

/// The rows that lost a provider because that provider left the
/// document.
///
/// dsh's self-disable write-back: a row whose injected service is gone
/// can no longer run, so the document records `disabled = true` for it
/// instead of leaving a row that silently never mounts. Only rows that
/// are still in the document qualify — a row that left with its
/// provider is a removal, not a write-back.
fn dep_lost_rows(
    previous: &Profile,
    next: &Profile,
    diff: &ProfileDiff,
    factories: &crate::profile::PluginFactories,
) -> Vec<String> {
    use std::collections::BTreeSet;

    // The service keys the removed rows used to provide.
    let mut lost_keys: BTreeSet<String> = BTreeSet::new();
    for row in previous.all_rows() {
        if !diff.removed.iter().any(|id| id == &row.id) {
            continue;
        }
        if let Some(factory) = factories.get(&row.name) {
            lost_keys.extend(factory.manifest().provides);
        }
    }
    if lost_keys.is_empty() {
        return Vec::new();
    }
    previous
        .all_rows()
        .into_iter()
        .filter(|row| !diff.removed.iter().any(|id| id == &row.id))
        .filter(|row| {
            let Some(factory) = factories.get(&row.name) else {
                return false;
            };
            let manifest = factory.manifest();
            crate::profile::effective_inject(
                &manifest.inject,
                row.inject.as_deref(),
            )
            .iter()
            .any(|key| lost_keys.contains(key))
        })
        .filter(|row| {
            // Still in the document, and not already off.
            next.find(&row.id).is_some_and(|row| !row.disabled)
        })
        .map(|row| row.id.clone())
        .collect()
}

/// Carry the boot's materialized row configs forward.
///
/// A row whose document config is empty takes the previous effective
/// row's config when that one is non-empty: the boot owns the
/// materialization, and a document that never spells it out is not
/// asking to clear it.
fn carry_forward_materialized(next: &mut Profile, previous: &Profile) {
    for entry in &mut next.core {
        let empty = match &entry.config {
            serde_json::Value::Null => true,
            serde_json::Value::Object(map) => map.is_empty(),
            _ => false,
        };
        if !empty {
            continue;
        }
        let Some(previous) =
            previous.core.iter().find(|row| row.id == entry.id)
        else {
            continue;
        };
        let previous_empty = match &previous.config {
            serde_json::Value::Null => true,
            serde_json::Value::Object(map) => map.is_empty(),
            _ => false,
        };
        if !previous_empty {
            entry.config = previous.config.clone();
        }
    }
}

/// The rows of `profile` whose ids are in `ids`, with no UI rows.
fn rows_for(profile: &Profile, ids: &BTreeSet<&str>) -> Profile {
    Profile {
        core: profile
            .core
            .iter()
            .filter(|entry| ids.contains(entry.id.as_str()))
            .cloned()
            .collect(),
        ui: Vec::new(),
        hmr: profile.hmr,
    }
}
