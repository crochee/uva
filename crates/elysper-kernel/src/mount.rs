//! Group mounting: validation, dependency ordering, and reversible
//! fiber ownership.

use std::{
    cmp::Reverse,
    collections::{BinaryHeap, HashMap, HashSet},
    panic::{AssertUnwindSafe, catch_unwind},
    sync::{Arc, Mutex, PoisonError},
};

use cordis::{ErrorCode, Fiber, FiberState, Inject, PluginOutput, plugin_sync};
use tracing::warn;

use crate::{
    context::Context,
    effect::EffectHandle,
    error::{MountError, PluginError},
    manifest::PLUGIN_PROTOCOL_VERSION,
    plugin::Plugin,
};

/// Mount a group of plugins as Cordis fibers.
///
/// # Ordering
///
/// Plugins mount in dependency order: a plugin whose `inject` key is
/// provided inside the group mounts after its provider. Ties keep the
/// declaration order of `plugins`, so a group with no edges mounts
/// exactly as passed.
///
/// # Validation
///
/// Nothing mounts until the whole group passes:
///
/// - every id is non-empty and unique;
/// - every `version` shares the host protocol's major
///   ([`PLUGIN_PROTOCOL_VERSION`]);
/// - no manifest lists the same `provides` key twice;
/// - no two plugins provide the same service key;
/// - every `inject` key is either provided inside the group or already
///   available in the parent context;
/// - the `inject` edges are acyclic (the error lists the plugins left
///   in the cycle).
///
/// # Parent availability
///
/// A dependency outside the group is probed with
/// [`ReflectService::get_value`](cordis::ReflectService::get_value) on
/// [`Context::inner`], which resolves in the context's own isolation
/// scope. The probe is deliberately **not**
/// [`ReflectService::services`](cordis::ReflectService::services): that
/// lists implementations from *every* isolation scope, so a sibling
/// scope's service would falsely satisfy a dependency. The strict read
/// also returns `None` while the provider fiber is loading or unloading;
/// such a read is reported as a missing dependency rather than silently
/// accepted.
///
/// A service registered with `provide_checked` whose predicate is
/// currently false still probes as present: the strict read returns the
/// value without evaluating the predicate. The consumer's fiber then
/// stays `Pending`, and `mount_all` surfaces that as
/// [`MountError::PluginInactive`] after the mount attempt; the strict
/// read itself remains the correct scope-aware probe.
///
/// # Failure
///
/// If a plugin's [`apply`](Plugin::apply) fails (including by panicking),
/// the failed fiber is disposed first — withdrawing anything it
/// registered before failing — then every previously mounted plugin's
/// tracked effect handles are dropped in reverse, and finally their
/// fibers are disposed in reverse. The error names the plugin and
/// carries its own [`PluginError`]. A disposal failure during the
/// rollback is logged with `tracing::warn!` and does not replace the
/// original error.
///
/// # Unloading
///
/// The returned [`MountHandle`] owns the effect handles each `apply`
/// returned plus the fibers. Dropping it first drops those handles in
/// reverse (revoking each registration exactly once, clones included),
/// then disposes the fibers in reverse. An effect a plugin did not
/// return stays owned by its fiber, so cleanup still runs exactly once
/// when that fiber is disposed.
///
/// A host that unloads one plugin without unmounting the group calls
/// [`MountHandle::dispose_plugin`]: that plugin's handles and fiber go,
/// the framework parks every consumer that injected its services, and
/// the rest of the group keeps running.
pub fn mount_all(
    ctx: &Context,
    plugins: &[Arc<dyn Plugin>],
) -> Result<MountHandle, MountError> {
    validate(ctx, plugins)?;
    let order = topological_order(plugins)?;
    let mut fibers = Vec::with_capacity(order.len());
    let mut effects: Vec<Vec<EffectHandle>> = Vec::with_capacity(order.len());

    for index in order {
        let plugin = Arc::clone(&plugins[index]);
        let plugin_id = plugin.manifest().id.clone();
        let failure: Arc<Mutex<Option<PluginError>>> =
            Arc::new(Mutex::new(None));
        let applied: Arc<Mutex<Option<Vec<EffectHandle>>>> =
            Arc::new(Mutex::new(None));
        let failure_slot = Arc::clone(&failure);
        let applied_slot = Arc::clone(&applied);
        let handle = plugin_sync::<(), _>(
            plugin_id.clone(),
            Inject::new(plugin.manifest().inject.iter().cloned()),
            move |ctx, _config| {
                let ctx = Context::from_inner(ctx);
                // A panicking plugin is a failed plugin, not a reason to
                // leave the group half-mounted: convert the unwinding into
                // the same error path as a returned `Err`.
                let outcome =
                    catch_unwind(AssertUnwindSafe(|| plugin.apply(&ctx)));
                let error = match outcome {
                    Ok(Ok(handles)) => {
                        *applied_slot
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner) =
                            Some(handles);
                        return Ok(PluginOutput::none());
                    }
                    Ok(Err(error)) => error,
                    Err(payload) => PluginError::new(format!(
                        "plugin panicked: {}",
                        panic_message(payload.as_ref())
                    )),
                };
                *failure_slot.lock().unwrap_or_else(PoisonError::into_inner) =
                    Some(error.clone());
                Err(cordis::CordisError::with_message(
                    ErrorCode::Plugin,
                    error.to_string(),
                ))
            },
        );

        let fiber = ctx.inner().plugin(handle, ());
        let applied = take_effects(&applied);
        match fiber.state() {
            FiberState::Active => {
                effects.push(applied);
                fibers.push(fiber);
            }
            FiberState::Failed => {
                let source = failure
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take()
                    .unwrap_or_else(|| {
                        PluginError::new(
                            fiber
                                .error()
                                .map(|error| error.to_string())
                                .unwrap_or_else(|| {
                                    "plugin failed to start".to_owned()
                                }),
                        )
                    });
                effects.push(applied);
                fibers.push(fiber);
                rollback(&mut effects, &mut fibers);
                return Err(MountError::ApplyFailed { plugin_id, source });
            }
            state => {
                effects.push(applied);
                fibers.push(fiber);
                rollback(&mut effects, &mut fibers);
                return Err(MountError::PluginInactive {
                    plugin_id,
                    state: format!("{state:?}"),
                });
            }
        }
    }

    Ok(MountHandle { effects, fibers })
}

/// Lifecycle state of one plugin owned by a [`MountHandle`].
///
/// The kernel's own vocabulary: consumers do not need to name the
/// framework's fiber states to ask whether a plugin is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginState {
    /// Waiting for one or more injected services.
    Pending,
    /// The plugin callback is running.
    Loading,
    /// Mounted and providing services.
    Active,
    /// Startup failed.
    Failed,
    /// Effect cleanup is running.
    Unloading,
    /// Permanently removed.
    Disposed,
}

impl From<FiberState> for PluginState {
    fn from(state: FiberState) -> Self {
        match state {
            FiberState::Pending => Self::Pending,
            FiberState::Loading => Self::Loading,
            FiberState::Active => Self::Active,
            FiberState::Failed => Self::Failed,
            FiberState::Unloading => Self::Unloading,
            FiberState::Disposed => Self::Disposed,
        }
    }
}

/// Ownership handle for a mounted group.
///
/// Dropping it revokes the returned effect handles in reverse
/// registration order, then disposes every fiber in reverse mount order.
/// Each registration has exactly one cleanup path: a tracked handle is
/// dropped once, and any effect that was not returned remains owned by
/// its fiber. A disposal failure is logged with `tracing::warn!` (a
/// destructor cannot return it).
///
/// [`dispose_plugin`](Self::dispose_plugin) unloads one plugin in
/// place. Its consumers are parked (`PluginState::Pending`) by the
/// framework, not unmounted; the group's other plugins keep their
/// services and effects. Because each plugin's handles are kept
/// together, disposing one never touches a sibling's registration.
#[derive(Debug)]
pub struct MountHandle {
    /// One entry per mounted plugin, in mount order. The group keeps
    /// each plugin's returned handles together so a single plugin can
    /// be disposed without disturbing its siblings.
    effects: Vec<Vec<EffectHandle>>,
    fibers: Vec<Fiber>,
}

impl MountHandle {
    /// Number of plugins this handle owns.
    pub fn len(&self) -> usize {
        self.fibers.len()
    }

    /// Whether this handle owns no plugins (an empty mount group).
    pub fn is_empty(&self) -> bool {
        self.fibers.is_empty()
    }

    /// The state of the mounted plugin named `id`, or `None` when this
    /// handle does not own it.
    pub fn plugin_state(&self, id: &str) -> Option<PluginState> {
        self.fiber_index(id)
            .map(|index| self.fibers[index].state().into())
    }

    /// Dispose one plugin: drop the effect handles it returned, then
    /// dispose its fiber.
    ///
    /// Consumers that injected a service the plugin provided are parked
    /// (`Pending`) by the framework; they are *not* unmounted, and the
    /// other plugins in the group keep running. Returns `false` when no
    /// plugin with that id is owned (including a second call for an id
    /// already disposed), so disposal is idempotent.
    pub fn dispose_plugin(&mut self, id: &str) -> bool {
        let Some(index) = self.fiber_index(id) else {
            return false;
        };
        let mut effects = self.effects.remove(index);
        while let Some(effect) = effects.pop() {
            drop(effect);
        }
        let fiber = self.fibers.remove(index);
        dispose_fiber(fiber);
        true
    }

    fn fiber_index(&self, id: &str) -> Option<usize> {
        self.fibers.iter().position(|fiber| fiber.name() == id)
    }
}

impl Drop for MountHandle {
    fn drop(&mut self) {
        for effects in self.effects.iter_mut().rev() {
            while let Some(effect) = effects.pop() {
                drop(effect);
            }
        }
        dispose_reverse(&mut self.fibers);
    }
}

/// Roll back a group whose current (last) plugin did not activate.
///
/// The failed fiber is disposed first, then the tracked effect handles
/// of the previous plugins are dropped in reverse, then their fibers are
/// disposed in reverse.
fn rollback(effects: &mut Vec<Vec<EffectHandle>>, fibers: &mut Vec<Fiber>) {
    if let Some(failed) = fibers.pop() {
        dispose_fiber(failed);
    }
    while let Some(mut group) = effects.pop() {
        while let Some(effect) = group.pop() {
            drop(effect);
        }
    }
    dispose_reverse(fibers);
}

/// Take the handles a plugin returned from its activation slot.
fn take_effects(
    applied: &Mutex<Option<Vec<EffectHandle>>>,
) -> Vec<EffectHandle> {
    applied
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .unwrap_or_default()
}

/// Render a caught panic payload for [`PluginError`].
fn panic_message(payload: &(dyn std::any::Any + Send)) -> &str {
    payload
        .downcast_ref::<&str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or("panic with a non-string payload")
}

/// Dispose fibers last-in, first-out. Disposal is idempotent, so a fiber
/// whose tracked handles were already dropped is safe to include.
fn dispose_reverse(fibers: &mut Vec<Fiber>) {
    while let Some(fiber) = fibers.pop() {
        dispose_fiber(fiber);
    }
}

/// Dispose one fiber, logging rather than propagating a failure (this is
/// called from rollback paths and destructors).
fn dispose_fiber(fiber: Fiber) {
    if let Err(error) = fiber.dispose() {
        warn!(
            plugin = %fiber.name(),
            error = %error,
            "failed to dispose plugin fiber"
        );
    }
}

/// Run every check that must pass before the first plugin starts.
fn validate(
    ctx: &Context,
    plugins: &[Arc<dyn Plugin>],
) -> Result<(), MountError> {
    let mut ids = HashSet::with_capacity(plugins.len());
    let mut providers: HashMap<&str, &str> = HashMap::new();

    for (index, plugin) in plugins.iter().enumerate() {
        let manifest = plugin.manifest();

        if manifest.id.trim().is_empty() {
            return Err(MountError::EmptyPluginId { index });
        }

        if !ids.insert(manifest.id.as_str()) {
            return Err(MountError::DuplicatePluginId {
                id: manifest.id.clone(),
            });
        }

        if !protocol_compatible(&manifest.version) {
            return Err(MountError::ProtocolIncompatible {
                plugin_id: manifest.id.clone(),
                plugin_version: manifest.version.clone(),
                host_version: PLUGIN_PROTOCOL_VERSION.to_owned(),
            });
        }

        let mut declared = HashSet::new();
        for key in &manifest.provides {
            if !declared.insert(key.as_str()) {
                return Err(MountError::DuplicateDeclaration {
                    plugin_id: manifest.id.clone(),
                    key: key.clone(),
                });
            }
            if let Some(first) =
                providers.insert(key.as_str(), manifest.id.as_str())
            {
                return Err(MountError::DuplicateProvider {
                    key: key.clone(),
                    first: first.to_owned(),
                    second: manifest.id.clone(),
                });
            }
        }
    }

    check_dependencies(ctx, plugins, &providers)?;
    Ok(())
}

/// Whether `version` shares the host protocol's major component.
///
/// A version whose first component is not a number is incompatible, not
/// a panic: manifests come from configuration, which is untrusted.
fn protocol_compatible(version: &str) -> bool {
    let host_major = PLUGIN_PROTOCOL_VERSION
        .split('.')
        .next()
        .and_then(|major| major.parse::<u64>().ok());
    let Some(host_major) = host_major else {
        return false;
    };
    version
        .split('.')
        .next()
        .and_then(|major| major.parse::<u64>().ok())
        == Some(host_major)
}

/// Reject an `inject` key that neither the group nor the parent provides.
fn check_dependencies(
    ctx: &Context,
    plugins: &[Arc<dyn Plugin>],
    providers: &HashMap<&str, &str>,
) -> Result<(), MountError> {
    for plugin in plugins {
        let manifest = plugin.manifest();
        let mut missing = Vec::new();
        let mut seen = HashSet::new();

        for key in &manifest.inject {
            if providers.contains_key(key.as_str())
                || !seen.insert(key.as_str())
            {
                continue;
            }
            if parent_service_available(ctx, key) {
                continue;
            }
            missing.push(key.clone());
        }

        if !missing.is_empty() {
            return Err(MountError::MissingDependency {
                plugin_id: manifest.id.clone(),
                keys: missing,
            });
        }
    }
    Ok(())
}

/// Probe the parent context for a service in its own isolation scope.
fn parent_service_available(ctx: &Context, key: &str) -> bool {
    matches!(ctx.inner().reflect().get_value(key), Ok(Some(_)))
}

/// Kahn's algorithm, preferring the earliest declaration among ready
/// plugins so independent plugins keep their declared order.
fn topological_order(
    plugins: &[Arc<dyn Plugin>],
) -> Result<Vec<usize>, MountError> {
    let mut provider_index: HashMap<&str, usize> =
        HashMap::with_capacity(plugins.len());
    for (index, plugin) in plugins.iter().enumerate() {
        for key in &plugin.manifest().provides {
            provider_index.insert(key.as_str(), index);
        }
    }

    let mut indegree = vec![0usize; plugins.len()];
    let mut dependents: Vec<Vec<usize>> = vec![Vec::new(); plugins.len()];
    for (index, plugin) in plugins.iter().enumerate() {
        let mut seen = HashSet::new();
        for key in &plugin.manifest().inject {
            if !seen.insert(key.as_str()) {
                continue;
            }
            let Some(&provider) = provider_index.get(key.as_str()) else {
                continue;
            };
            dependents[provider].push(index);
            indegree[index] += 1;
        }
    }

    let mut ready: BinaryHeap<Reverse<usize>> = indegree
        .iter()
        .enumerate()
        .filter(|(_, degree)| **degree == 0)
        .map(|(index, _)| Reverse(index))
        .collect();

    let mut order = Vec::with_capacity(plugins.len());
    while let Some(Reverse(index)) = ready.pop() {
        order.push(index);
        for &dependent in &dependents[index] {
            indegree[dependent] -= 1;
            if indegree[dependent] == 0 {
                ready.push(Reverse(dependent));
            }
        }
    }

    if order.len() == plugins.len() {
        return Ok(order);
    }

    let stuck = indegree
        .iter()
        .enumerate()
        .filter(|(_, degree)| **degree > 0)
        .map(|(index, _)| plugins[index].manifest().id.clone())
        .collect();
    Err(MountError::Cycle { plugins: stuck })
}
