//! `TimingWheel` — hierarchical timing wheel for due-job lookup.
//!
//! A pure data structure: no locks, no threads, no timers. The
//! caller drives it with injected `DateTime<Utc>` values (the
//! `Scheduler` host `tick`), which keeps it testable with a
//! fixed clock and free of any runtime dependency.
//!
//! Design (see the wheel-cron design doc §2):
//!
//! - **4 levels × 64 slots.** Level `i` has granularity
//!   `64^i` seconds (1s / 64s / 4096s / 262144s), so the wheel
//!   spans `64^4` seconds (~194 days) — beyond any real
//!   scheduling horizon. A `BTreeMap` overflow tail exists
//!   purely as defense.
//! - **Entries carry their own deadline as ground truth.** The
//!   slot placement only locates candidates; firing is decided
//!   by `deadline <= now`, which sidesteps every boundary
//!   alignment error a slot-only scheme would invite.
//! - **Cascading.** When the cursor crosses a level-`i` lap
//!   boundary, the level-`i` slot it lands on is demoted entry
//!   by entry into lower levels via the general insert path, so
//!   every entry settles into level 0 no later than its own
//!   deadline.
//! - **Backward advance is a noop.** Clock rollback is the
//!   caller's problem (the `Scheduler` rebuilds the wheel).

use std::collections::{BTreeMap, HashMap};

use chrono::{DateTime, Duration, Utc};

use crate::job::JobId;

/// Number of hierarchy levels.
const LEVELS: usize = 4;
/// Slots per level.
const SLOTS: i64 = 64;
/// Total wheel span in seconds (`64^4`, ~194 days). Inserts
/// beyond it go to the overflow tail; stepping is capped at it.
const SPAN_SECS: i64 = 16_777_216;
/// Sentinel `index_of` level for entries parked in `overflow`.
const OVERFLOW: usize = LEVELS;

/// Granularity of `level` in seconds: `64^level`.
const fn granularity(level: usize) -> i64 {
    64_i64.pow(level as u32)
}

/// One slot's pending entries, deadline truth included.
type Slot = Vec<(JobId, DateTime<Utc>)>;
/// Hierarchical timing wheel of pending [`JobId`]s, driven
/// entirely by caller-injected timestamps.
///
/// Entries live as `(JobId, deadline)` pairs; the slot math
/// only decides *where to look*, the embedded deadline decides
/// *whether to fire*.
pub struct TimingWheel {
    /// `levels[i][slot]` holds every entry whose deadline maps
    /// to `(deadline / 64^i) % 64` while its distance from the
    /// cursor still belongs to level `i`.
    levels: [Vec<Slot>; LEVELS],
    /// The wheel's notion of "now"; only moves forward.
    cursor: DateTime<Utc>,
    /// `id -> (level, slot)` so cancel/re-insert never scans
    /// the wheel. Level `OVERFLOW` marks overflow entries.
    index_of: HashMap<JobId, (usize, usize)>,
    /// Deadlines beyond the wheel span, keyed by deadline.
    /// Defensive only — the span (~194 days) exceeds every
    /// real scheduling horizon.
    overflow: BTreeMap<DateTime<Utc>, Vec<JobId>>,
}

impl TimingWheel {
    /// Build an empty wheel whose cursor starts at `now`.
    #[must_use]
    pub fn new(now: DateTime<Utc>) -> Self {
        Self {
            levels: std::array::from_fn(|_| vec![Vec::new(); SLOTS as usize]),
            cursor: now,
            index_of: HashMap::new(),
            overflow: BTreeMap::new(),
        }
    }

    /// Schedule `id` to fire at `deadline`.
    ///
    /// The contract is `deadline > cursor` (the `Scheduler`
    /// guarantees `next_fire_at > now`); an at/past deadline is
    /// still accepted defensively and fires at the next 1s
    /// step. Re-inserting an id already pending replaces its
    /// old entry so no ghost copy can fire later.
    pub fn insert(&mut self, deadline: DateTime<Utc>, id: JobId) {
        self.remove_indexed(&id);
        self.insert_internal(deadline, id);
    }

    /// Remove a pending `id`. Returns `false` if it was not in
    /// the wheel.
    pub fn cancel(&mut self, id: &JobId) -> bool {
        self.remove_indexed(id)
    }

    /// Move the cursor to `now` (stepping 1s at a time to keep
    /// the cascade boundaries honest) and return every pending
    /// entry whose deadline is `<= now`.
    ///
    /// `now` earlier than the cursor is a noop: the caller
    /// owns rollback recovery.
    ///
    /// One call advances the cursor by at most one wheel span
    /// (`64^4` seconds, ~194 days): a jump larger than that is
    /// walked in span-sized pieces across calls rather than in one
    /// step. Entries due on the other side are still returned by the
    /// first of those calls (the overflow tail is swept by deadline,
    /// not by stepping).
    pub fn advance_to(&mut self, now: DateTime<Utc>) -> Vec<JobId> {
        if now < self.cursor {
            return Vec::new();
        }
        // Stepping is capped at one wheel span; beyond that
        // (theoretically impossible with real horizons) the
        // overflow tail is drained as a fallback.
        let limit =
            std::cmp::min(now, self.cursor + Duration::seconds(SPAN_SECS));
        let step = Duration::seconds(1);
        let mut fired = Vec::new();
        while self.cursor + step <= limit {
            self.cursor += step;
            let secs = self.cursor.timestamp();
            // Lap crossings: demote level i's landed slot into
            // level i-1, highest level first so a boundary hit
            // at several levels settles in one pass.
            for level in (1..LEVELS).rev() {
                let gran = granularity(level);
                if secs.rem_euclid(gran) == 0 {
                    let slot = secs.div_euclid(gran).rem_euclid(SLOTS) as usize;
                    let demoted = std::mem::take(&mut self.levels[level][slot]);
                    for (id, deadline) in demoted {
                        self.insert_internal(deadline, id);
                    }
                }
            }
            // Collect level-0: the slot locates candidates, the
            // embedded deadline is the firing truth.
            let slot = secs.rem_euclid(SLOTS) as usize;
            fired.extend(self.take_due_from_slot(slot, now));
        }
        // The walk only ever collects the slots it steps onto, and a call
        // whose `now` *is* the cursor takes no step at all. But entries
        // that are already due park in the slot the coming step would
        // visit (see `insert_internal`), so sweeping it here is what makes
        // the promise above hold for every call: nothing with
        // `deadline <= now` is left behind, and the caller is never handed
        // a deadline it cannot collect. Future entries stay put (the sweep
        // partitions on the same embedded deadline the steps do).
        let coming = (self.cursor.timestamp() + 1).rem_euclid(SLOTS) as usize;
        fired.extend(self.take_due_from_slot(coming, now));
        // Overflow fallback: with the step cap hit (theoretical
        // only — the span exceeds every real horizon) or with
        // entries left over from an earlier capped walk, sweep
        // everything due out of the tail. A no-op whenever the
        // overflow is empty or not yet due.
        while let Some((&deadline, _)) = self.overflow.first_key_value() {
            if deadline > now {
                break;
            }
            if let Some(ids) = self.overflow.remove(&deadline) {
                for id in ids {
                    self.index_of.remove(&id);
                    fired.push(id);
                }
            }
        }
        fired
    }

    /// Move the cursor to `cursor` outright, re-homing every pending
    /// entry relative to the new position.
    ///
    /// [`advance_to`](Self::advance_to) walks one second at a time and
    /// refuses to move backward — that is what keeps the cascade
    /// boundaries honest — so a wheel whose cursor is out of step with
    /// the caller's clock cannot be walked to it: from a cursor that far
    /// behind, the step cap alone costs a full wheel span per call. This
    /// is the way out. It is deliberately *not* a firing operation:
    /// nothing is collected, and an entry at or past the new cursor
    /// simply stays due (it lands in the slot the coming step visits).
    ///
    /// The `Scheduler` needs it for the wheel it builds before it has a
    /// clock: a constructor that reads no clock cannot anchor the cursor
    /// at its caller's "now", so the first tick re-anchors it here.
    pub(crate) fn reanchor(&mut self, cursor: DateTime<Utc>) {
        let mut pending: Vec<(JobId, DateTime<Utc>)> =
            Vec::with_capacity(self.len());
        for level in &mut self.levels {
            for slot in level.iter_mut() {
                pending.append(&mut std::mem::take(slot));
            }
        }
        for (deadline, ids) in std::mem::take(&mut self.overflow) {
            pending.extend(ids.into_iter().map(|id| (id, deadline)));
        }
        self.index_of.clear();
        self.cursor = cursor;
        for (id, deadline) in pending {
            // The general insert path: ids are unique (the index
            // guaranteed it), so nothing needs replacing here.
            self.insert_internal(deadline, id);
        }
    }

    /// Earliest pending deadline, looking at most 64 slots per
    /// level (O(256), queries only) plus the overflow head.
    #[must_use]
    pub fn next_deadline(&self) -> Option<DateTime<Utc>> {
        let mut best = self.overflow.keys().next().copied();
        let secs = self.cursor.timestamp();
        for level in 0..LEVELS {
            let start = secs.div_euclid(granularity(level)).rem_euclid(SLOTS);
            // Nearest non-empty slot *the cursor has yet to reach*. The
            // slot it is sitting on has just been serviced, so a deadline
            // filed there (a lap tail — see `insert_internal`) is the last
            // this level reaches, not the first: starting the scan on it
            // reported a deadline a lap away ahead of one a few seconds
            // out, and the host, arming its timer from this answer, ran
            // the near job late. Scanning from the coming slot keeps the
            // order monotone in deadline and still reaches the passed slot
            // as the final `off`, so a genuinely stale entry stays visible.
            for off in 0..SLOTS {
                let slot = (start + 1 + off).rem_euclid(SLOTS) as usize;
                if let Some(min) =
                    self.levels[level][slot].iter().map(|(_, d)| *d).min()
                {
                    if best.is_none_or(|b| min < b) {
                        best = Some(min);
                    }
                    break;
                }
            }
        }
        best
    }

    /// The wheel's current time (the caller uses this to detect
    /// clock rollback).
    #[must_use]
    pub fn cursor(&self) -> DateTime<Utc> {
        self.cursor
    }

    /// Number of pending entries (wheel + overflow).
    #[must_use]
    pub fn len(&self) -> usize {
        self.levels
            .iter()
            .flat_map(|level| level.iter())
            .map(Vec::len)
            .sum::<usize>()
            + self.overflow.values().map(Vec::len).sum::<usize>()
    }

    /// Whether no entry is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Collect and return every entry in level-0 `slot` whose deadline has
    /// arrived; future entries stay where they are.
    ///
    /// Shared by the stepping walk and the post-walk sweep so both decide
    /// firing the same way: the slot locates candidates, the embedded
    /// deadline is the truth.
    fn take_due_from_slot(
        &mut self,
        slot: usize,
        now: DateTime<Utc>,
    ) -> Vec<JobId> {
        if self.levels[0][slot].is_empty() {
            return Vec::new();
        }
        let entries = std::mem::take(&mut self.levels[0][slot]);
        let (keep, due): (Vec<_>, Vec<_>) =
            entries.into_iter().partition(|(_, d)| *d > now);
        for (id, _) in &due {
            self.index_of.remove(id);
        }
        self.levels[0][slot] = keep;
        due.into_iter().map(|(id, _)| id).collect()
    }

    /// Place one entry by its distance from the cursor:
    /// `level = floor(log64(delta))`, slot by the deadline's
    /// own time. Shared by `insert` and cascade demotion.
    fn insert_internal(&mut self, deadline: DateTime<Utc>, id: JobId) {
        let delta = deadline - self.cursor;
        let level = match delta.num_seconds() {
            // At/past the cursor (or a sub-second remainder; the
            // seconds are truncated, so `0` covers both ends of the
            // boundary): park in the next level-0 slot so it fires at
            // the coming step. The cursor's *own* slot is not visited —
            // stepping moves to `cursor + 1` — so an entry left there
            // would wait for the 64-slot lap to wrap.
            ..=0 => {
                let slot =
                    (self.cursor.timestamp() + 1).rem_euclid(SLOTS) as usize;
                self.levels[0][slot].push((id.clone(), deadline));
                self.index_of.insert(id, (0, slot));
                return;
            }
            secs if secs < granularity(1) => 0,
            secs if secs < granularity(2) => 1,
            secs if secs < granularity(3) => 2,
            secs if secs < SPAN_SECS => 3,
            _ => OVERFLOW,
        };
        if level == OVERFLOW {
            self.overflow.entry(deadline).or_default().push(id.clone());
            self.index_of.insert(id, (OVERFLOW, 0));
            return;
        }
        // The cursor visits a level-0 slot at `second + cursor_fraction`,
        // so an entry filed under its own truncated second is inspected
        // just *before* it is due whenever its fraction is larger — and
        // then waits a full lap (~64 s) for the next visit. File it under
        // the first second whose visit is at or past the deadline.
        let due_second = deadline.timestamp()
            + i64::from(
                deadline.timestamp_subsec_nanos()
                    > self.cursor.timestamp_subsec_nanos(),
            );
        let slot = due_second.div_euclid(granularity(level)).rem_euclid(SLOTS)
            as usize;
        self.levels[level][slot].push((id.clone(), deadline));
        self.index_of.insert(id, (level, slot));
    }

    /// Drop `id` wherever it lives. Returns whether it was
    /// pending.
    fn remove_indexed(&mut self, id: &JobId) -> bool {
        let Some((level, slot)) = self.index_of.remove(id) else {
            return false;
        };
        if level == OVERFLOW {
            let key = self
                .overflow
                .iter()
                .find(|(_, ids)| ids.iter().any(|e| e == id))
                .map(|(k, _)| *k);
            if let Some(key) = key {
                let drained = self
                    .overflow
                    .get_mut(&key)
                    .map(|ids| {
                        ids.retain(|e| e != id);
                        ids.is_empty()
                    })
                    .unwrap_or(false);
                if drained {
                    self.overflow.remove(&key);
                }
            }
        } else {
            self.levels[level][slot].retain(|(eid, _)| eid != id);
        }
        true
    }
}

#[cfg(test)]
mod tests;
