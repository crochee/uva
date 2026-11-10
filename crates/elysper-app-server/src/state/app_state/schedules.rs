//! Schedules state: wraps [`ScheduleStore`] (disk-backed) and
//! exposes the operations the `/api/v1/schedules/*` handlers need.
//!
//! The store persists jobs to `<workspace>/schedules/<id>.json`
//! under PID-locked atomic writes (`crates/elysper-scheduler/src/store.rs`).
//! Restart picks the rows back up via `ScheduleStore::load`.

use std::sync::Arc;

use elysper::{
    core::Clock as _,
    scheduler::{Job, ScheduleStore, Scheduler},
};

/// `SchedulesState` — the schedule subsystem's shared handle.
///
/// `store` is the durable layer (`ScheduleStore`) and `scheduler`
/// is the runtime-neutral `Scheduler` that the server uses for
/// tick operations. `Arc` so both can be cloned into the route
/// handlers cheaply.
#[derive(Clone)]
pub struct SchedulesState {
    pub store: Arc<ScheduleStore>,
    pub scheduler: Arc<Scheduler>,
    /// Wall clock the tick reads "now" from (AGENTS.md §3.8: a
    /// library never calls `Utc::now()` itself — the deployment's
    /// one clock is injected, so a test can pin time).
    clock: elysper::core::SharedClock,
}

impl SchedulesState {
    /// Build the state from a workspace root. The store's directory
    /// is `<root>/schedules`; `load` is called once to pick up any
    /// jobs the previous process left behind. A load failure is
    /// logged but does not panic — the operator sees a warning and
    /// the empty directory is the operational state.
    pub fn build(workspace_root: &std::path::Path) -> Self {
        Self::build_with_clock(
            workspace_root,
            elysper::core::SharedClock::system(),
        )
    }

    /// [`Self::build`] with an explicit clock — the seam a test
    /// uses to pin "now" and make a due job deterministic.
    pub fn build_with_clock(
        workspace_root: &std::path::Path,
        clock: elysper::core::SharedClock,
    ) -> Self {
        let store = Arc::new(ScheduleStore::new(workspace_root));
        match store.load() {
            Ok(n) => tracing::info!(
                "schedules: loaded {n} job(s) from {}",
                workspace_root.display()
            ),
            Err(e) => tracing::warn!(
                "schedules: load failed ({e}); starting with empty store"
            ),
        }
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&store)));
        Self {
            store,
            scheduler,
            clock,
        }
    }

    /// Build the state over the store the `scheduler` plugin
    /// published, so the route surface and the background driver share
    /// one store and one clock. `load` is called once, as in
    /// [`Self::build`].
    pub fn from_store(
        store: Arc<ScheduleStore>,
        clock: elysper::core::SharedClock,
    ) -> Self {
        match store.load() {
            Ok(n) => tracing::info!("schedules: loaded {n} job(s)"),
            Err(e) => tracing::warn!(
                "schedules: load failed ({e}); starting with empty store"
            ),
        }
        let scheduler = Arc::new(Scheduler::new(Arc::clone(&store)));
        Self {
            store,
            scheduler,
            clock,
        }
    }

    /// Re-read the store so this node never answers from a cache a
    /// peer's write invalidated. A load failure is reported once and
    /// leaves an empty view rather than a stale one.
    fn reload(&self) -> bool {
        match self.store.load() {
            Ok(_) => true,
            Err(e) => {
                tracing::warn!(
                    "schedules: store reload failed ({e});                      reporting an empty catalogue"
                );
                false
            }
        }
    }

    /// List every job (in id order), straight from the store.
    pub fn list(&self) -> Vec<Job> {
        if !self.reload() {
            return Vec::new();
        }
        self.store.list()
    }

    /// One job by id, straight from the store.
    pub fn get(&self, id: &elysper::scheduler::JobId) -> Option<Job> {
        if !self.reload() {
            return None;
        }
        self.store.get(id)
    }

    /// Add a new job. Returns the persisted `Job` on success.
    pub fn add(&self, job: Job) -> Result<(), String> {
        self.store.add(job).map_err(|e| e.to_string())?;
        self.reload();
        Ok(())
    }

    /// Mutate a job in place. Returns the new `Job` after the
    /// mutation, or `None` when no job with that id exists.
    pub fn update<F>(
        &self,
        id: &elysper::scheduler::JobId,
        mutator: F,
    ) -> Result<Option<Job>, String>
    where
        F: FnOnce(&mut Job),
    {
        let updated =
            self.store.update(id, mutator).map_err(|e| e.to_string())?;
        self.reload();
        Ok(updated)
    }

    /// Remove a job by id. Returns true if it existed.
    pub fn remove(
        &self,
        id: &elysper::scheduler::JobId,
    ) -> Result<bool, String> {
        let removed = self.store.remove(id).map_err(|e| e.to_string())?;
        self.reload();
        Ok(removed)
    }

    /// Drive a single tick and return the jobs that fired.
    pub fn tick(&self) -> Vec<elysper::scheduler::scheduler::FiredJob> {
        // The tick plans over what the store holds now: a peer's
        // write must not wait for this node to restart.
        self.reload();
        let now = self.clock.now();
        let fired = self.scheduler.tick(now).fired;
        // The tick persists due jobs, so the next read sees them.
        self.reload();
        fired
    }

    /// The state's wall clock, for handlers that must read the same
    /// "now" the tick does.
    #[must_use]
    pub fn clock(&self) -> &elysper::core::SharedClock {
        &self.clock
    }

    /// Drive one pass as the cluster's scheduler leader.
    ///
    /// Returns the jobs that fired. Only the lease holder calls
    /// this: a due job fires once across the deployment, not once
    /// per node.
    pub fn tick_as_leader(
        &self,
    ) -> Vec<elysper::scheduler::scheduler::FiredJob> {
        self.tick()
    }
}

/// One leader pass: reload the store from disk, then tick the service.
///
/// The store's version counter only sees this process's writes, so a
/// schedule another node wrote into a shared workspace (or an operator
/// hand-edited) is invisible to the in-memory wheel until `load`
/// re-reads it. The leader is exactly the component that must see the
/// deployment's writes, so the reload runs before every pass; the
/// scheduler's wheel then rebuilds only when the revision actually
/// moved.
fn leader_tick(
    service: &elysper::scheduler::SchedulerService,
    now: chrono::DateTime<chrono::Utc>,
) -> Vec<elysper::scheduler::scheduler::FiredJob> {
    if let Err(error) = service.store().load() {
        tracing::warn!(
            target: "elysper.scheduler",
            %error,
            "scheduler: store reload failed; ticking the cached view"
        );
    }
    service.tick(now).fired
}

/// Start the background tick.
///
/// One node holds `scheduler_leader_lease`; only that node runs the
/// pass, so a due job fires exactly once across the deployment. The
/// task holds the state weakly: when the state goes, the tick goes
/// with it.
pub(crate) fn spawn_scheduler_driver(
    state: std::sync::Arc<crate::state::AppState>,
) {
    let period = std::time::Duration::from_secs(
        state
            .server_config
            .as_ref()
            .map(|config| config.scheduler.tick_secs)
            .unwrap_or(30)
            .max(1),
    );
    let namespace = state.cluster.namespace.clone();
    let weak = std::sync::Arc::downgrade(&state);
    tokio::spawn(async move {
        let lease = crate::cluster::scheduler_leader_lease(&namespace);
        let mut holding = false;
        loop {
            tokio::time::sleep(period).await;
            let Some(state) = weak.upgrade() else {
                break;
            };
            // The service the `scheduler` plugin publishes is the
            // tick's source of truth: unloading the plugin withdraws
            // it, and the loop stops instead of ticking a service the
            // deployment no longer mounts.
            let Some(service) = state
                .kernel
                .get::<elysper::scheduler::SchedulerService>(
                    elysper::scheduler::KEY_SCHEDULER,
                )
                .ok()
                .flatten()
            else {
                break;
            };
            let acquired = state
                .cluster
                .bus
                .try_acquire(
                    &lease,
                    &state.cluster.node_id,
                    state.cluster.lease_ttl,
                )
                .await;
            match acquired {
                Ok(true) => {
                    holding = true;
                    let fired = leader_tick(&service, state.clock.now());
                    if !fired.is_empty() {
                        tracing::info!(
                            fired = fired.len(),
                            "scheduler: leader pass fired due job(s)"
                        );
                    }
                }
                Ok(false) => {}
                Err(error) => tracing::warn!(
                    %error,
                    "scheduler: could not acquire the leader lease"
                ),
            }
        }
        // Stop holding the leadership lease: another node may take
        // the pass instead of waiting out the TTL.
        if holding && let Some(state) = weak.upgrade() {
            state
                .cluster
                .bus
                .release(&lease, &state.cluster.node_id)
                .await
                .map_err(|error| {
                    tracing::warn!(
                        %error,
                        "scheduler: could not release the leader lease"
                    );
                })
                .ok();
        }
    });
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use elysper::scheduler::{Job, JobKind, ScheduleStore, SchedulerService};
    use serde_json::json;

    use super::leader_tick;

    /// A leader pass observes a schedule file another process wrote:
    /// the store reload runs before the tick, so the peer's job fires
    /// even though this process's version counter never moved.
    #[test]
    fn leader_tick_reloads_out_of_process_writes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let service =
            SchedulerService::new(Arc::new(ScheduleStore::new(dir.path())));
        let peer = ScheduleStore::new(dir.path());
        peer.add(Job::new(
            "from-peer",
            JobKind::Once,
            json!({"agent": "maintainer"}),
            chrono::Utc::now() - chrono::Duration::seconds(1),
        ))
        .expect("the peer writes the job file");

        let fired = leader_tick(&service, chrono::Utc::now());
        assert_eq!(fired.len(), 1, "the peer-written due job fires");
        assert_eq!(fired[0].name, "from-peer");
    }
}
