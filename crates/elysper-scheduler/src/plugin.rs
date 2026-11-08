//! The `scheduler` service: the schedule store and its dispatcher as a
//! native kernel plugin.
//!
//! # One store, one scheduler
//!
//! [`SchedulerService`] holds the [`ScheduleStore`] and the
//! [`Scheduler`] built over it. Every consumer — the `schedule` tool
//! brick, an operator endpoint, the host's tick loop — resolves the
//! service instead of constructing a second store over the same root:
//! `list` serves from the store's cache, so two stores over one root
//! would answer from stale views.
//!
//! # The tick loop stays with the host
//!
//! This crate is deliberately runtime-free: it never spawns a task and
//! never sleeps. [`SchedulerService::tick`] performs one pass at the
//! instant the caller passes, and [`SchedulerService::next_deadline`]
//! reports how long to wait before the next one; arming a timer with
//! those is the app-server boot's job, exactly as
//! [`Scheduler`] documents. Adding a spawned loop
//! here would put a runtime in the library's public lifecycle and make
//! the plugin's unload dependent on task cancellation.
//!
//! # The service value's shape
//!
//! [`SchedulerService`] is `Sized`, so `apply` calls
//! `ctx.provide_arc(KEY_SCHEDULER, Arc<SchedulerService>)` and a
//! consumer writes `ctx.require::<SchedulerService>(KEY_SCHEDULER)`.
//!
//! # Lifecycle
//!
//! The plugin returns its provide handle from
//! [`apply`](elysper_kernel::Plugin::apply), so unloading it revokes
//! the service. The store and scheduler arrive with the plugin value,
//! so the manifest injects nothing. Unloading stops nothing at the
//! store level: the rows stay on disk for the next mount.

use std::{sync::Arc, time::Duration};

use chrono::{DateTime, Utc};
use elysper_kernel::{
    Context,
    EffectHandle,
    PLUGIN_PROTOCOL_VERSION,
    Plugin as KernelPlugin,
    PluginError,
    PluginManifest,
    RuntimeKind,
};

use crate::{ScheduleStore, ScheduleTick, Scheduler};

/// The service key the `scheduler` service is registered under.
///
/// The kernel's manifest `provides` list carries the same string, so
/// the key a consumer passes to `Context::require` and the key the
/// kernel validates are one constant.
pub const KEY_SCHEDULER: &str = "scheduler";

/// The plugin id the `scheduler` manifest declares.
const SCHEDULER_ID: &str = "scheduler";

/// The schedule store and the dispatcher built over it.
///
/// The two are one value because they must be one truth: the store is
/// the durable schedule, the scheduler is its in-memory timing index,
/// and a consumer that took only the store would be unable to tick
/// while a consumer that took only the scheduler could not read jobs.
#[derive(Debug)]
pub struct SchedulerService {
    store: Arc<ScheduleStore>,
    scheduler: Scheduler,
}

impl SchedulerService {
    /// Pair `store` with a dispatcher over the same store.
    ///
    /// The host builds the store over its data root and passes the same
    /// `Arc` to every consumer before mounting this service.
    #[must_use]
    pub fn new(store: Arc<ScheduleStore>) -> Self {
        let scheduler = Scheduler::new(Arc::clone(&store));
        Self { store, scheduler }
    }

    /// The store the service was built over — for consumers that write
    /// jobs (the `schedule` tool brick) or read the durable rows.
    #[must_use]
    pub fn store(&self) -> &Arc<ScheduleStore> {
        &self.store
    }

    /// The dispatcher the service was built over.
    #[must_use]
    pub fn scheduler(&self) -> &Scheduler {
        &self.scheduler
    }

    /// Drive one scheduling pass; see [`Scheduler::tick`].
    #[must_use]
    pub fn tick(&self, now: DateTime<Utc>) -> ScheduleTick {
        self.scheduler.tick(now)
    }

    /// How long the caller should wait before the next tick; see
    /// [`Scheduler::next_deadline`]. `None` means nothing is scheduled.
    #[must_use]
    pub fn next_deadline(&self, now: DateTime<Utc>) -> Option<Duration> {
        self.scheduler.next_deadline(now)
    }
}

/// The `scheduler` kernel plugin: provides [`KEY_SCHEDULER`].
///
/// Mount it with [`elysper_kernel::mount_all`]. See the [module
/// docs](crate::plugin) for the tick-loop boundary and the unload
/// contract.
pub struct SchedulerPlugin {
    manifest: PluginManifest,
    /// The store/scheduler pair the service hands out.
    pub service: Arc<SchedulerService>,
}

impl SchedulerPlugin {
    /// Publish `service` as the `scheduler` service.
    ///
    /// The host builds the pair and keeps its own handle for the tick
    /// loop; the plugin only publishes it.
    pub fn new(service: Arc<SchedulerService>) -> Self {
        let mut manifest =
            PluginManifest::new(SCHEDULER_ID, PLUGIN_PROTOCOL_VERSION);
        manifest.runtime = RuntimeKind::Native;
        manifest.provides.push(KEY_SCHEDULER.to_string());
        Self { manifest, service }
    }
}

impl KernelPlugin for SchedulerPlugin {
    fn manifest(&self) -> &PluginManifest {
        &self.manifest
    }

    fn apply(&self, ctx: &Context) -> Result<Vec<EffectHandle>, PluginError> {
        Ok(vec![
            ctx.provide_arc(KEY_SCHEDULER, Arc::clone(&self.service))?,
        ])
    }
}
