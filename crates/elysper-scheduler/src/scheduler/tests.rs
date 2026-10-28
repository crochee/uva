use std::{collections::HashSet, sync::Arc, time::Duration as StdDuration};

use chrono::{Duration as ChronoDuration, TimeZone};
use tempfile::TempDir;

use super::*;
use crate::job::{Job, JobKind};

fn now_plus(secs: i64) -> DateTime<Utc> {
    Utc::now() + ChronoDuration::seconds(secs)
}

fn once_job(name: &str, secs: i64) -> Job {
    Job::new(
        name,
        JobKind::Once,
        serde_json::json!({"agent": name}),
        now_plus(secs),
    )
}

#[test]
fn tick_emits_due_job_and_advances() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(ScheduleStore::new(dir.path()));
    store.load().unwrap();
    let job = once_job("t-0", 0);
    store.add(job.clone()).unwrap();
    let sched = Scheduler::new(store);
    let tick = sched.tick(Utc::now());
    assert_eq!(tick.fired.len(), 1);
    assert_eq!(tick.fired[0].id, job.id);
    // `Once` jobs have no future fire.
    assert!(tick.next_deadline.is_none());
    // Cache must reflect `Completed` post-tick.
    let snapshot = sched.store().get(&job.id).unwrap();
    assert_eq!(snapshot.status, JobStatus::Completed);
}

#[test]
fn interval_rolls_next_fire() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(ScheduleStore::new(dir.path()));
    store.load().unwrap();
    let job = Job::new(
        "every-5s",
        JobKind::Interval {
            interval: StdDuration::from_secs(5),
        },
        serde_json::json!({}),
        now_plus(0),
    );
    store.add(job.clone()).unwrap();
    let sched = Scheduler::new(store);
    let tick = sched.tick(Utc::now());
    assert_eq!(tick.fired.len(), 1);
    let deadline = tick.next_deadline.expect("interval must rearm");
    let now = Utc::now();
    let delta = (deadline - now).num_seconds();
    // Accept any value in [3, 6] — wall-clock drift
    // between the two `now` reads is real.
    assert!((3..=6).contains(&delta), "expected ~5s rearm, got {delta}s");
}

#[test]
fn paused_job_does_not_fire() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(ScheduleStore::new(dir.path()));
    store.load().unwrap();
    let mut job = once_job("paused", -10);
    job.status = JobStatus::Paused;
    store.add(job).unwrap();
    let sched = Scheduler::new(store);
    let tick = sched.tick(Utc::now());
    assert!(tick.fired.is_empty());
}

#[test]
fn next_deadline_helper_returns_zero_for_imminent() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(ScheduleStore::new(dir.path()));
    store.load().unwrap();
    store.add(once_job("due", -1)).unwrap();
    let sched = Scheduler::new(store);
    let delta = sched.next_deadline(Utc::now()).unwrap();
    assert_eq!(delta, StdDuration::ZERO);
}

/// A job whose `advance` fails must not stay permanently due: that
/// would make `next_deadline` report an elapsed deadline, the host
/// would re-tick immediately, and every pass would take the file lock
/// and rewrite the row — a disk-write hot loop.
#[test]
fn an_unadvanceable_job_is_parked_not_left_due() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(ScheduleStore::new(dir.path()));
    store.load().unwrap();
    // Pinned, so the test is deterministic (and adds no wall-clock
    // read to the `check-clock` ratchet).
    let now = DateTime::parse_from_rfc3339("2026-09-19T09:00:00Z")
        .unwrap()
        .with_timezone(&Utc);
    // The library API accepts this; `advance` cannot represent it.
    let job = Job::new(
        "hostile",
        JobKind::Interval {
            interval: StdDuration::from_secs(u64::MAX),
        },
        serde_json::json!({}),
        now,
    );
    store.add(job.clone()).unwrap();
    let sched = Scheduler::new(Arc::clone(&store));

    let tick = sched.tick(now);
    assert!(tick.fired.is_empty(), "a failed advance is not a firing");
    assert_eq!(
        tick.next_deadline, None,
        "a parked job must not contribute a deadline"
    );
    assert_eq!(
        sched.next_deadline(now),
        None,
        "so the host has nothing to arm — no busy loop"
    );

    // Paused, not Completed. It is not merely cosmetic: `Completed`
    // means "this unit of work fired" and is terminal, whereas this
    // job ran nothing. The parking also keeps the row *addressable*
    // (`remove` clears it, `resume` re-attempts) instead of handing it
    // to a sweep that exists to retire finished work.
    let parked = sched.store().get(&job.id).expect("job");
    assert_eq!(parked.status, JobStatus::Paused);
    assert_eq!(
        parked.last_fired_at, None,
        "a job that never fired must not carry a fire time"
    );
    assert!(
        sched
            .store()
            .gc_completed(now + ChronoDuration::days(365))
            .unwrap()
            .is_empty(),
        "a parked job must not be swept as completed"
    );

    // And a second tick is quiet — the log line is once, not per pass.
    assert!(sched.tick(now).fired.is_empty());
    assert_eq!(sched.next_deadline(now), None);
}

fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).single().unwrap()
}

/// The wheel lives behind the scheduler, so a host that shares one
/// `Arc<Scheduler>` across threads must keep working — that is the
/// compatibility the integration promised.
#[test]
fn scheduler_is_send_and_sync() {
    fn _assert<T: Send + Sync>() {}
    _assert::<Scheduler>();
}

/// The tool's primary flow: `create` stamps `next_fire_at = now` and
/// the host ticks a second later. The wheel is then anchored a second
/// behind that deadline, so the entry sits exactly at the cursor —
/// the boundary where "the deadline has arrived" must not turn into
/// "wait for the slot lap". Both anchor sites (`new` → first tick,
/// and `resync`) go through it.
#[test]
fn a_deadline_one_step_behind_now_fires_now() {
    for resynced in [false, true] {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(ScheduleStore::new(dir.path()));
        store
            .add(Job::new(
                "created-this-second",
                JobKind::Once,
                serde_json::json!({}),
                at(60),
            ))
            .unwrap();
        let sched = Scheduler::new(Arc::clone(&store));
        let now = at(61);
        if resynced {
            sched.resync(now);
        }
        let linear: HashSet<String> = sched
            .linear_due(now)
            .into_iter()
            .map(|j| j.id.to_string())
            .collect();
        let tick = sched.tick(now);
        let via_wheel: HashSet<String> =
            tick.fired.iter().map(|f| f.id.to_string()).collect();
        assert_eq!(linear, via_wheel, "resynced={resynced}");
        assert_eq!(via_wheel.len(), 1, "resynced={resynced}");
        assert_eq!(
            tick.next_deadline, None,
            "a fired one-shot must not leave a past deadline armed \
             (resynced={resynced})",
        );
    }
}

/// `linear_due` is the oracle for this shape too: a deadline that has
/// already elapsed when the tick runs is due, and the wheel's cursor is
/// anchored one step behind `now` — the same boundary from the other
/// side, plus the store's answer to "is it still due".
#[test]
fn an_elapsed_deadline_fires_and_matches_the_oracle() {
    for probe in [7i64, 8, 9] {
        let dir = TempDir::new().unwrap();
        let store = Arc::new(ScheduleStore::new(dir.path()));
        store
            .add(Job::new(
                "elapsed",
                JobKind::Interval {
                    interval: StdDuration::from_secs(3600),
                },
                serde_json::json!({}),
                at(8),
            ))
            .unwrap();
        let sched = Scheduler::new(Arc::clone(&store));
        let now = at(probe);
        let linear = sched.linear_due(now).len();
        let via_wheel = sched.tick(now).fired.len();
        assert_eq!(linear, via_wheel, "probe t={probe}");
    }
}

/// A tick that arrives exactly where the previous anchor left the
/// wheel's cursor: no second to step over, and yet the schedule is
/// not empty — the store holds a job that is due *now*. It must
/// fire, and (the hazard the deadline report feeds) the caller must
/// not be handed a past deadline, which would arm a zero-length
/// timer and spin the host against a tick that can never progress.
#[test]
fn a_zero_step_tick_delivers_what_is_due() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(ScheduleStore::new(dir.path()));
    store
        .add(Job::new(
            "due-now",
            JobKind::Once,
            serde_json::json!({}),
            at(59),
        ))
        .unwrap();
    let sched = Scheduler::new(Arc::clone(&store));
    // `resync` anchors the cursor at `now - 1` and files the job at
    // exactly that second; the tick then lands on it with nothing to
    // step over.
    sched.resync(at(60));
    let tick = sched.tick(at(59));
    assert_eq!(tick.fired.len(), 1, "a due job must not wait for a step");
    assert_eq!(
        tick.next_deadline, None,
        "and no elapsed deadline may be left armed"
    );
    assert_eq!(sched.next_deadline(at(59)), None);
}

/// The tool's `create` without `first_fire_at` stamps
/// `SystemClock`'s fractional now, so a real deployment's very first
/// deadline is fractional. The wheel must not sit on it while
/// `next_deadline` reports an elapsed instant (which arms a
/// zero-length timer and spins the host) — the whole schedule would
/// stall for a lap.
#[test]
fn a_fractional_creation_time_does_not_strand_the_job() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(ScheduleStore::new(dir.path()));
    // 09:00:00.400 "now", created with next_fire_at 10.9s out.
    let created_at = at(0) + ChronoDuration::milliseconds(400);
    store
        .add(Job::new(
            "fractional",
            JobKind::Once,
            serde_json::json!({}),
            created_at + ChronoDuration::milliseconds(10_500),
        ))
        .unwrap();
    let sched = Scheduler::new(Arc::clone(&store));

    // Whole-second host ticks, as a real host would drive them.
    let deadline = created_at + ChronoDuration::milliseconds(10_500);
    let mut fired_at = None;
    for secs in 1..=20i64 {
        let now = at(secs);
        if !sched.tick(now).fired.is_empty() {
            fired_at = Some(now);
            break;
        }
        assert_eq!(
            sched.next_deadline(now),
            (deadline - now).to_std().ok(),
            "the armed timer must point at the deadline, never at a \
             past instant (a zero timer spins the host) — t={secs}",
        );
    }
    assert_eq!(
        fired_at,
        Some(at(11)),
        "the job fires at the first whole second past its deadline"
    );
}

/// The wheel is an index over the same schedule, not a second
/// scheduler: on a fixed mix of intervals and one-shots, every probe
/// has to report exactly the jobs the retained linear scan
/// (`linear_due`, this file's pre-wheel implementation) calls due.
/// Set equality is the assertion because the wheel walks slots, not
/// the store's id order.
#[test]
fn wheel_tick_matches_linear_scan_on_fixed_schedule() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(ScheduleStore::new(dir.path()));
    for i in 0..10u64 {
        store
            .add(Job::new(
                format!("ivl-{i}"),
                JobKind::Interval {
                    interval: StdDuration::from_secs(5 + i * 5),
                },
                serde_json::json!({"i": i}),
                at(0),
            ))
            .unwrap();
    }
    for i in 0..10u64 {
        store
            .add(Job::new(
                format!("once-{i}"),
                JobKind::Once,
                serde_json::json!({"i": i}),
                at(10 + i as i64 * 7),
            ))
            .unwrap();
    }
    let sched = Scheduler::new(Arc::clone(&store));
    for probe in [0i64, 7, 30, 60, 100, 200] {
        let now = at(probe);
        let linear: HashSet<String> = sched
            .linear_due(now)
            .into_iter()
            .map(|j| j.id.to_string())
            .collect();
        let via_wheel: HashSet<String> = sched
            .tick(now)
            .fired
            .iter()
            .map(|f| f.id.to_string())
            .collect();
        assert_eq!(linear, via_wheel, "probe t={probe}");
    }
}

#[test]
fn next_deadline_matches_full_scan_minimum() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(ScheduleStore::new(dir.path()));
    for (name, t) in [("a", 30i64), ("b", 10), ("c", 70)] {
        store
            .add(Job::new(name, JobKind::Once, serde_json::json!({}), at(t)))
            .unwrap();
    }
    let sched = Scheduler::new(store);
    let expected = sched
        .store()
        .list()
        .into_iter()
        .filter(|j| j.status == JobStatus::Active)
        .map(|j| j.next_fire_at)
        .min();
    assert_eq!(sched.tick_dry_run(at(0)).next_deadline, expected);
    assert_eq!(sched.next_deadline(at(0)).map(|d| d.as_secs()), Some(10));
}

#[test]
fn backward_tick_rebuilds_wheel_and_keeps_firing() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(ScheduleStore::new(dir.path()));
    store
        .add(Job::new("o", JobKind::Once, serde_json::json!({}), at(50)))
        .unwrap();
    let sched = Scheduler::new(store);
    // Forward past 50s: the one-shot fires and completes.
    assert_eq!(sched.tick(at(60)).fired.len(), 1);
    // A tick behind the wheel's cursor must rebuild, not re-fire.
    assert!(
        sched.tick(at(30)).fired.is_empty(),
        "rollback must not replay"
    );
    // Nor does the rewind resurrect it once time moves on again.
    assert!(sched.tick(at(90)).fired.is_empty());
}

/// The wheel follows the store. A write that bypasses the
/// scheduler — the `schedule` tool's, an operator's own store
/// handle — is visible to the *next* pass, with no manual rebuild:
/// the store's revision is what tells the scheduler its snapshot is
/// stale.
///
/// The wheel held nothing when that write landed (the job was added
/// after `new`), so this only passes if the pass reconciles with the
/// store before it answers — the read R117's wheel traded for O(1)
/// ticks, restored without giving the O(1) back.
#[test]
fn a_direct_store_write_is_visible_to_the_next_tick() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(ScheduleStore::new(dir.path()));
    let sched = Scheduler::new(Arc::clone(&store));
    store
        .add(Job::new(
            "late",
            JobKind::Once,
            serde_json::json!({}),
            at(40),
        ))
        .unwrap();
    let tick = sched.tick(at(45));
    assert_eq!(tick.fired.len(), 1, "the next tick must see the write");
    assert_eq!(tick.fired[0].name, "late");
}

/// The peek paths read the same wheel, so they reconcile first too:
/// a host that arms its timer from `next_deadline` between ticks must
/// not be told "nothing scheduled" for a job that was just created.
#[test]
fn a_direct_store_write_moves_the_peeked_deadline() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(ScheduleStore::new(dir.path()));
    let sched = Scheduler::new(Arc::clone(&store));
    assert_eq!(sched.tick_dry_run(at(0)).next_deadline, None);

    store
        .add(Job::new(
            "later",
            JobKind::Once,
            serde_json::json!({}),
            at(500),
        ))
        .unwrap();
    assert_eq!(sched.tick_dry_run(at(0)).next_deadline, Some(at(500)));
    assert_eq!(
        sched.next_deadline(at(0)),
        Some(StdDuration::from_secs(500))
    );
}

/// The revision is a *hint*, not a duty to rescan: a pass whose own
/// `store.update` calls advanced it must not make the next pass
/// rebuild. Getting this wrong is invisible in the results — every
/// tick still fires the right jobs — and shows up only as an O(n)
/// scan per pass, so it is asserted by counting rebuilds.
#[test]
fn a_ticks_own_writes_do_not_trigger_a_resync() {
    let dir = TempDir::new().unwrap();
    let store = Arc::new(ScheduleStore::new(dir.path()));
    store
        .add(Job::new(
            "hourly",
            JobKind::Interval {
                interval: StdDuration::from_secs(3600),
            },
            serde_json::json!({}),
            at(0),
        ))
        .unwrap();
    let sched = Scheduler::new(Arc::clone(&store));
    // Built after the `add`, so the first pass starts in step.
    assert_eq!(sched.resync_count(), 0);

    // Fires and re-arms: one row persisted, one revision bumped.
    assert_eq!(sched.tick(at(0)).fired.len(), 1);
    assert_eq!(
        sched.resync_count(),
        0,
        "a pass must not rebuild the wheel it just built"
    );

    // And the *next* pass does not mistake that write for someone
    // else's: it fires the re-armed job and still does not rebuild.
    assert_eq!(sched.tick(at(3600)).fired.len(), 1);
    assert_eq!(
        sched.resync_count(),
        0,
        "the write-back is what keeps this at zero"
    );
    assert_eq!(
        sched.tick_dry_run(at(7200)).next_deadline,
        Some(at(7200)),
        "the re-armed job is in the wheel, from the pass that armed it"
    );
    assert_eq!(sched.resync_count(), 0, "nor do the peek paths rebuild");
}
