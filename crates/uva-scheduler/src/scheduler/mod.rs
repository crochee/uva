//! `Scheduler` — runtime-neutral dispatcher.
//!
//! The scheduler never owns a timer; it never spawns a task.
//! Every [`Scheduler::tick`] is driven by the caller, who
//! passes `now` (typically `chrono::Utc::now()`) and gets
//! back the [`ScheduleTick`] of jobs whose `next_fire_at`
//! has arrived. The caller then executes the firing on
//! whichever executor it uses (`tokio::spawn_local`,
//! `smol::spawn`, `std::thread::spawn`, …). This is the
//! same "callers arm their own timer" discipline the rest
//! of uva's library crates follow — and is what makes
//! the crate usable from any runtime.
//!
//! ## Reentrancy
//!
//! `tick` borrows `&self` and returns a snapshot. The
//! in-memory `Job` cache is updated under a single
//! `parking_lot::Mutex`, so concurrent ticks from multiple
//! callers serialise on the lock but never deadlock
//! (the lock is never held across an await).
//!
//! ## The timing wheel
//!
//! `tick` does not scan the store: the scheduler keeps a
//! [`TimingWheel`] of the active jobs, so a
//! pass with nothing due costs a couple of hash lookups instead of a
//! visit per job. The wheel is an *index*, never a second source of
//! truth — [`Job::is_due`] still decides whether a candidate fires, and
//! every firing still goes through one `store.update`, so the row is
//! persisted exactly as before.
//!
//! The wheel is a **snapshot of the store**, taken by
//! [`Scheduler::new`] (the constructor reads no clock, so the first
//! `tick` anchors it) and rebuilt by `tick` itself when the host clock
//! moves backwards. A write that bypasses the scheduler — the
//! `schedule` tool's actions, an operator's own store handle, another
//! process's rows after a `load` — is picked up by the next `tick` (and
//! by `next_deadline` / `tick_dry_run`), which compares the store's
//! [`ScheduleStore::version`] against the value it last saw and rebuilds
//! when they differ. [`Scheduler::resync`] stays available as an
//! explicit rebuild for hosts that want the wheel reconciled *now*
//! rather than at the next pass.

use std::sync::{
    Arc,
    atomic::{AtomicU64, Ordering},
};

use chrono::{DateTime, Duration, Utc};
use parking_lot::Mutex;
use tracing::warn;

use crate::{
    job::{Job, JobId, JobStatus, ScheduleError},
    store::ScheduleStore,
    wheel::TimingWheel,
};

/// The cursor a wheel starts at when its owner has no clock to anchor it
/// with: [`Scheduler::new`] takes no `now`, so it builds the wheel here
/// and the first tick re-anchors it. `MIN_UTC` is unmistakable as a
/// sentinel — no job deadline can be earlier than the clock's origin.
const UNPRIMED: DateTime<Utc> = DateTime::<Utc>::MIN_UTC;

/// One second: the wheel's smallest step. A wheel anchored this far
/// *behind* `now` hands back everything already due on the single
/// `advance_to(now)` that follows, because the wheel collects
/// at-or-past-cursor entries on its coming step, not on its current one.
const PRIME_STEP: Duration = Duration::seconds(1);

/// Insert every active job `store` holds into `wheel`.
///
/// Only active jobs: a paused or completed row has no future fire to
/// wait on, and the wheel has no notion of status — whatever it holds is
/// a firing candidate.
fn insert_active_into(store: &ScheduleStore, wheel: &mut TimingWheel) {
    for job in store.list() {
        if job.status == JobStatus::Active {
            wheel.insert(job.next_fire_at, job.id);
        }
    }
}

/// One [`Scheduler::tick`] result — the set of jobs that
/// fired in this round and the bookkeeping the caller
/// needs to deliver them.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScheduleTick {
    /// Jobs whose `next_fire_at` has arrived. Each entry
    /// carries the original `payload` so the caller can
    /// deliver it verbatim.
    pub fired: Vec<FiredJob>,
    /// Earliest `next_fire_at` across every still-active
    /// job. `None` when no jobs are scheduled. Callers use
    /// this to arm their timer — `next_deadline(now)` is
    /// the helper that takes clock skew into account.
    pub next_deadline: Option<DateTime<Utc>>,
}

impl ScheduleTick {
    /// True when no jobs fired and no future fire is
    /// queued. Callers can drop their timer in this case.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.fired.is_empty() && self.next_deadline.is_none()
    }
}

/// One entry in a [`ScheduleTick::fired`] list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FiredJob {
    pub id: JobId,
    pub name: String,
    pub payload: serde_json::Value,
}

/// Runtime-neutral dispatcher around a [`ScheduleStore`].
pub struct Scheduler {
    store: Arc<ScheduleStore>,
    /// Index of the active jobs by fire time — see the module docs for
    /// what it does and does not know about.
    wheel: Mutex<TimingWheel>,
    /// Serialises the compound operations on the wheel (a tick's
    /// advance, a rebuild) so two callers cannot interleave them.
    tick_lock: Mutex<()>,
    /// The store revision `wheel` was built from. Anything else in the
    /// store means a write went around this scheduler, and the next
    /// pass rebuilds before it answers. Read and written under
    /// `tick_lock`; atomic only so the check costs one load.
    last_seen_version: AtomicU64,
    /// How many times the wheel has been rebuilt from the store. Test
    /// instrumentation: the point of caching the revision is that a
    /// rebuild is *not* the normal path, and the only way to assert a
    /// pass did not take it is to count.
    #[cfg(test)]
    resyncs: AtomicU64,
}

impl Scheduler {
    /// Construct a scheduler backed by `store`.
    ///
    /// The handle is shared (`Arc`), not owned, because the store is
    /// the one thing a deployment must not duplicate: `list` serves
    /// from the store's in-memory cache, so a second `ScheduleStore`
    /// over the same root would answer with a stale view until it
    /// reloaded. Hand the same `Arc` to every component that reads or
    /// writes the schedule — a `SchedulerTool`, an operator endpoint —
    /// and they see one state. Against *other processes* the per-job
    /// file locks keep concurrent writes from clobbering each other,
    /// and every mutation re-reads under the lock; a peer's writes
    /// become visible to this process's reads at the next
    /// [`ScheduleStore::load`], and the pass after that (or an explicit
    /// [`Scheduler::resync`]) puts them in the wheel.
    ///
    /// The wheel is built from the store's active jobs here, at an
    /// unanchored cursor: a constructor that reads no clock cannot know
    /// where "now" is, so the *first* [`Scheduler::tick`] anchors it.
    /// That first tick is the only one that touches the whole schedule;
    /// afterwards the wheel tracks the caller's clock and a tick costs
    /// what has fallen due.
    #[must_use]
    pub fn new(store: Arc<ScheduleStore>) -> Self {
        // Read the revision *before* the snapshot below: a write that
        // lands between the two then looks newer than what the wheel
        // holds, so the next pass rebuilds — a redundant rebuild, never
        // a missed row. The other order could swallow it.
        let version = store.version();
        let mut wheel = TimingWheel::new(UNPRIMED);
        insert_active_into(&store, &mut wheel);
        Self {
            store,
            wheel: Mutex::new(wheel),
            tick_lock: Mutex::new(()),
            last_seen_version: AtomicU64::new(version),
            #[cfg(test)]
            resyncs: AtomicU64::new(0),
        }
    }

    /// Borrow the underlying store. Use this to
    /// `add` / `update` / `remove` jobs from operator code;
    /// those routes do their own locking and stay
    /// independent of the scheduler's tick state.
    #[must_use]
    pub fn store(&self) -> &ScheduleStore {
        &self.store
    }

    /// Drive one scheduling pass. Returns the jobs whose
    /// `next_fire_at <= now`, after advancing their
    /// `next_fire_at` and stamping `last_fired_at`.
    ///
    /// The bookkeeping is durable: every fired job is
    /// re-persisted to disk before `tick` returns, so a
    /// crash between `tick` and the caller's delivery does
    /// not lose the fire-record (the job simply fires
    /// again on the next tick — same semantics as
    /// `pi-subagents`).
    ///
    /// The work is proportional to the jobs the wheel hands back, not
    /// to the size of the schedule.
    pub fn tick(&self, now: DateTime<Utc>) -> ScheduleTick {
        let _guard = self.tick_lock.lock();
        let mut wheel = self.wheel.lock();
        self.sync_with_store_locked(&mut wheel, now);
        self.anchor_locked(&mut wheel, now);
        // The revision this pass starts from — after every rebuild
        // above, so it is the one the wheel reflects. `tick`'s own
        // store writes bump the counter mid-pass; leaving it there
        // would make every following tick see a change it did not make
        // and rebuild the whole wheel (O(n) per pass), so the pass
        // reports back exactly the revisions it produced itself.
        let base = self.last_seen_version.load(Ordering::Acquire);
        let mut own_writes = 0u64;
        let mut fired = Vec::new();
        // The wheel locates the candidates; the store still decides
        // which of them fire. `advance_to` has already dropped each
        // entry it hands over, so every candidate goes back into the
        // wheel below unless the store says it is done.
        for id in wheel.advance_to(now) {
            let Some(job) = self.store.get(&id) else {
                continue;
            };
            if !job.is_due(now) {
                // Stale entry: the wheel's snapshot says due, the store
                // says otherwise — a manual fire, a pause, or a direct
                // schedule edit landed between the two. Re-checking
                // `is_due` here is the whole point of the indirection;
                // re-arming keeps the job from dropping out of the
                // schedule entirely, which is what a bare discard would
                // do to it.
                self.rearm(&mut wheel, &job);
                continue;
            }
            // Each advance is a discrete per-job mutation,
            // so we route through the store's `update` to
            // keep the on-disk + cache view in lockstep.
            let mut advance_failed: Option<ScheduleError> = None;
            let advanced = self
                .store
                .update(&job.id, |j| {
                    // `is_due` already filtered on the
                    // pre-snapshot value. Re-check inside
                    // the lock so a manual fire that ran
                    // between snapshot and update does not
                    // double-fire us.
                    if !j.is_due(now) {
                        return;
                    }
                    // `advance` is total but fallible: an interval outside
                    // the representable range is reported, not panicked on
                    // (a panic here would abort the host's tick loop).
                    if let Err(err) = j.advance(now) {
                        // Park it. Leaving the job `Active` with its past
                        // `next_fire_at` would keep it permanently due, so
                        // `next_deadline` would report an elapsed deadline,
                        // the host would re-tick at once, and every pass
                        // would take the per-job file lock and rewrite this
                        // row — a disk-write hot loop, not just log noise.
                        // `Paused` is not due and drops out of
                        // `next_deadline`, which is what breaks the loop.
                        //
                        // Deliberately *not* `Completed`: that state means
                        // "this unit of work fired" and is terminal (the
                        // store's `gc_completed` sweep is the only thing
                        // that removes such rows, and the tool layer
                        // refuses to pause or resume it). This job ran
                        // nothing, so `list` must not report it as
                        // finished work. It stays visible and addressable
                        // instead: `remove` clears it, and `resume`
                        // re-attempts — which parks it again, since the
                        // stored interval is what cannot be represented.
                        j.status = JobStatus::Paused;
                        advance_failed = Some(err);
                    }
                })
                .ok()
                .flatten();
            if advanced.is_some() {
                // One persisted row, one revision.
                own_writes += 1;
            }
            if let Some(err) = advance_failed {
                // Do NOT report it as fired: the job did not advance, so
                // emitting a fire would claim a schedule change that did
                // not happen. It is now paused, so this is logged once
                // rather than on every tick.
                warn!(
                    job = %job.name,
                    job_id = %job.id,
                    error = %err,
                    "scheduled job could not be advanced; paused it instead of firing",
                );
                continue;
            }
            match advanced {
                Some(snapshot) if snapshot.status == JobStatus::Completed => {
                    // `Once` jobs fire and complete in the
                    // same tick. Nothing to come back for, so
                    // nothing goes back into the wheel.
                    fired.push(FiredJob {
                        id: snapshot.id,
                        name: snapshot.name,
                        payload: snapshot.payload,
                    });
                }
                Some(snapshot) => {
                    // Recurring: `advance` computed the next fire, the
                    // wheel is what remembers it.
                    wheel.insert(snapshot.next_fire_at, snapshot.id.clone());
                    fired.push(FiredJob {
                        id: snapshot.id,
                        name: snapshot.name,
                        payload: snapshot.payload,
                    });
                }
                None => {
                    // The closure's own re-check refused it (the row
                    // changed between the snapshot above and the
                    // update). The wheel already gave the entry up, so
                    // take the store's current word on where it belongs.
                    if let Some(current) = self.store.get(&id) {
                        self.rearm(&mut wheel, &current);
                    }
                }
            }
        }
        // Everything this pass persisted is now in the wheel, so the
        // wheel reflects `base + own_writes`. A write from anywhere else
        // that landed during the pass bumped the store past that — it is
        // not in the wheel, and the next pass rebuilds to pick it up.
        self.last_seen_version
            .store(base.wrapping_add(own_writes), Ordering::Release);
        ScheduleTick {
            fired,
            next_deadline: wheel.next_deadline(),
        }
    }

    /// Rebuild the wheel from the store *now*, instead of letting the
    /// next pass notice the change on its own.
    ///
    /// `tick`, `next_deadline`, and `tick_dry_run` already reconcile
    /// the wheel with the store before they answer, so this is not
    /// required for a write to become visible: a job the `schedule`
    /// tool created, a row an operator paused through their own
    /// [`ScheduleStore`] handle, or rows ingested by
    /// [`ScheduleStore::load`] are all picked up by the next pass. What
    /// this retains is the rebuild itself: forcing the wheel correct at
    /// an instant of the caller's choosing — an operator endpoint
    /// answering "what fires at T" — instead of waiting for the next
    /// pass to notice. `now` is the instant the rebuild is taken against
    /// (the same `now` the caller's next `tick` will use).
    ///
    /// Takes `&self`: the wheel has its own lock, so a scheduler
    /// already shared behind an `Arc` can be resynced in place.
    pub fn resync(&self, now: DateTime<Utc>) {
        let _guard = self.tick_lock.lock();
        let mut wheel = self.wheel.lock();
        self.resync_locked(&mut wheel, now);
    }

    /// Rebuild the wheel when the store has moved on without this
    /// scheduler: every mutating store route bumps its revision, so one
    /// comparison answers "is my snapshot still current?" in O(1) — a
    /// tick over an unchanged store never scans it. Normal operation,
    /// not an anomaly, so it logs nothing.
    ///
    /// Callers hold the tick lock and the wheel.
    fn sync_with_store_locked(
        &self,
        wheel: &mut TimingWheel,
        now: DateTime<Utc>,
    ) {
        if self.last_seen_version.load(Ordering::Acquire)
            != self.store.version()
        {
            self.resync_locked(wheel, now);
        }
    }

    /// The rebuild behind [`Scheduler::resync`], under the tick lock its
    /// callers hold.
    fn resync_locked(&self, wheel: &mut TimingWheel, now: DateTime<Utc>) {
        // Read the revision *before* the snapshot: a write landing
        // between the two then looks newer than the wheel, so the next
        // check rebuilds again (a redundant pass, never a missed row).
        let version = self.store.version();
        // Anchored a step behind `now` so that the `advance_to(now)` a
        // tick runs next still collects what this rebuild found due.
        *wheel = TimingWheel::new(now - PRIME_STEP);
        insert_active_into(&self.store, wheel);
        #[cfg(test)]
        self.resyncs.fetch_add(1, Ordering::Relaxed);
        self.last_seen_version.store(version, Ordering::Release);
    }

    /// Put the wheel's clock in step with `now` before a pass runs.
    ///
    /// The wheel steps forward one second at a time and never backward,
    /// so two states need this help: one that has never been anchored
    /// (see [`Scheduler::new`]), and one whose clock is *ahead* of `now`
    /// because the host clock was set back. Callers hold the tick lock
    /// and the wheel.
    fn anchor_locked(&self, wheel: &mut TimingWheel, now: DateTime<Utc>) {
        if wheel.cursor() == UNPRIMED {
            // Re-home the entries the wheel already holds. The store is
            // not re-read here: the caller has just reconciled the wheel
            // with it (`sync_with_store_locked`), and re-reading would
            // duplicate that at the cost of a second scan.
            wheel.reanchor(now - PRIME_STEP);
        } else if now < wheel.cursor() {
            // The clock moved backwards: the wheel is anchored to an
            // instant that has been undone, so its slots locate nothing
            // trustworthy. The store is the schedule's truth — rebuild
            // from it rather than guess.
            warn!(
                now = %now,
                wheel_cursor = %wheel.cursor(),
                "scheduler clock moved backwards; rebuilding the timing wheel",
            );
            self.resync_locked(wheel, now);
        }
    }

    /// Put a wheel candidate back on the deadline the store holds for
    /// it.
    ///
    /// `advance_to` removes what it hands over, so a candidate that the
    /// store no longer calls due has to be either re-armed or left out
    /// deliberately: dropping an *active* job's entry would mean it
    /// never fires again. A job that is no longer active is left out —
    /// only active jobs belong in the wheel.
    fn rearm(&self, wheel: &mut TimingWheel, job: &Job) {
        if job.status == JobStatus::Active {
            wheel.insert(job.next_fire_at, job.id.clone());
        }
    }

    /// Compute the wall-clock duration the caller should
    /// sleep before the next tick. Returns `None` when
    /// there is nothing scheduled. The returned duration
    /// is clamped at zero — a deadline that has already
    /// passed means "tick immediately".
    ///
    /// This is the helper callers use to arm `tokio::time`
    /// / `async_io::Timer` / `std::thread::sleep` from the
    /// scheduler's deadline. The scheduler itself never
    /// sleeps.
    #[must_use]
    pub fn next_deadline(
        &self,
        now: DateTime<Utc>,
    ) -> Option<std::time::Duration> {
        let tick = self.tick_dry_run(now);
        tick.next_deadline
            .map(|t| (t - now).to_std().unwrap_or(std::time::Duration::ZERO))
    }

    /// Read-only peek at the next tick — does not mutate job state.
    /// Useful for tests and for "when does anything fire next?"
    /// operator queries.
    ///
    /// It can move the *wheel* (never a job row): a peek that arrives
    /// after a direct write or a clock rollback has to rebuild it exactly
    /// as `tick` would, or the answer would come from slots that no
    /// longer describe the schedule.
    #[must_use]
    pub fn tick_dry_run(&self, now: DateTime<Utc>) -> ScheduleTick {
        let _guard = self.tick_lock.lock();
        let mut wheel = self.wheel.lock();
        self.sync_with_store_locked(&mut wheel, now);
        self.anchor_locked(&mut wheel, now);
        ScheduleTick {
            fired: Vec::new(),
            next_deadline: wheel.next_deadline(),
        }
    }

    /// How many times this scheduler has rebuilt its wheel from the
    /// store. Test instrumentation for the revision check: "a pass over
    /// an unchanged store does not rescan it" is only assertable by
    /// counting the rebuilds.
    #[cfg(test)]
    fn resync_count(&self) -> u64 {
        self.resyncs.load(Ordering::Relaxed)
    }

    /// The scan `tick` ran before the wheel: every active job whose
    /// `next_fire_at` has arrived, straight from the store.
    ///
    /// Retained as the equivalence oracle — the wheel is an index over
    /// the same schedule, and the tests hold the two to the same answer
    /// on every probe. Test-only so it cannot drift into a second
    /// production path.
    #[cfg(test)]
    fn linear_due(&self, now: DateTime<Utc>) -> Vec<Job> {
        self.store
            .list()
            .into_iter()
            .filter(|j| j.is_due(now))
            .collect()
    }
}

impl std::fmt::Debug for Scheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Scheduler")
            .field("store", &self.store)
            .field("wheel_cursor", &self.wheel.lock().cursor())
            .field("tick_lock", &"<mutex>")
            .finish()
    }
}

#[cfg(test)]
mod tests;
