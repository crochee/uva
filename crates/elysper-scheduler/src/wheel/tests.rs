use chrono::{DateTime, TimeZone, Utc};

use crate::job::JobId;

fn at(secs: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, 0).single().unwrap()
}
/// A fractional instant — the shape `SystemClock` hands the scheduler.
fn at_ms(secs: i64, millis: u32) -> DateTime<Utc> {
    Utc.timestamp_opt(secs, millis * 1_000_000)
        .single()
        .unwrap()
}
fn id(n: u64) -> JobId {
    JobId::from_string(format!("j{n}"))
}

#[test]
fn fires_in_slot_order_and_reports_next_deadline() {
    let mut w = super::TimingWheel::new(at(0));
    w.insert(at(3), id(1));
    w.insert(at(9), id(2));
    assert_eq!(w.next_deadline(), Some(at(3)));
    assert_eq!(w.len(), 2);
    let fired = w.advance_to(at(5));
    assert_eq!(fired, vec![id(1)]);
    assert_eq!(w.next_deadline(), Some(at(9)));
    assert_eq!(w.advance_to(at(9)), vec![id(2)]);
    assert_eq!(w.next_deadline(), None);
}

#[test]
fn cancel_prevents_firing() {
    let mut w = super::TimingWheel::new(at(0));
    w.insert(at(10), id(1));
    assert!(w.cancel(&id(1)));
    assert!(!w.cancel(&id(1)));
    assert!(w.advance_to(at(20)).is_empty());
    assert_eq!(w.next_deadline(), None);
}

#[test]
fn same_second_slot_fires_together() {
    let mut w = super::TimingWheel::new(at(0));
    w.insert(at(7), id(1));
    w.insert(at(7), id(2));
    let mut fired = w.advance_to(at(7));
    fired.sort();
    assert_eq!(fired.len(), 2);
}

#[test]
fn long_delay_cascades_down_correctly() {
    // 4097s 落在层 2；到 4097s 时必须经级联落到层 0 后如期收走
    let mut w = super::TimingWheel::new(at(0));
    w.insert(at(4097), id(1));
    assert_eq!(w.next_deadline(), Some(at(4097)));
    assert_eq!(w.advance_to(at(4100)), vec![id(1)]);
}

#[test]
fn past_deadline_at_advance_collects_immediately() {
    let mut w = super::TimingWheel::new(at(0));
    w.insert(at(1), id(1));
    // 一次大步进（含边界）
    assert_eq!(w.advance_to(at(1)), vec![id(1)]);
}

#[test]
fn backward_advance_is_noop() {
    let mut w = super::TimingWheel::new(at(100));
    w.insert(at(200), id(1));
    assert!(w.advance_to(at(50)).is_empty());
    assert_eq!(w.next_deadline(), Some(at(200)));
    assert_eq!(w.len(), 1);
}

/// The `Scheduler` builds its wheel before it has a clock, so the
/// first tick has to move a cursor that starts far in the past onto
/// `now` — `advance_to` cannot walk there.
#[test]
fn reanchor_moves_the_cursor_and_keeps_every_entry() {
    let mut w = super::TimingWheel::new(DateTime::<Utc>::MIN_UTC);
    w.insert(at(10), id(1));
    w.insert(at(4097), id(2));
    assert_eq!(w.len(), 2);

    w.reanchor(at(9));
    assert_eq!(w.cursor(), at(9));
    assert_eq!(
        w.next_deadline(),
        Some(at(10)),
        "the deadlines are the entries' own, not the slots'"
    );
    // An entry at or past the new cursor stays due: it lands in the
    // slot the coming step visits, so the move loses nothing.
    assert_eq!(w.advance_to(at(10)), vec![id(1)]);
    assert_eq!(w.advance_to(at(4100)), vec![id(2)]);
    assert_eq!(w.len(), 0);
}

#[test]
fn reanchor_rebuilds_the_id_index() {
    let mut w = super::TimingWheel::new(at(0));
    w.insert(at(100), id(1));
    w.reanchor(at(50));
    assert!(w.cancel(&id(1)), "the index must survive a re-anchor");
    assert!(w.advance_to(at(200)).is_empty());
}

/// The at/past-cursor arm's whole job: an entry whose deadline is at
/// or behind the cursor is already due, so it must be reachable by the
/// coming step — not by the cursor's own slot, which the upward walk
/// never visits again. `advance_to`'s promise is "every pending entry
/// with `deadline <= now`", so it holds even when `now` is the cursor
/// itself and there is no step to take.
#[test]
fn entry_at_the_cursor_is_due_not_pending() {
    let mut w = super::TimingWheel::new(at(10));
    w.insert(at(10), id(1));
    w.insert(at(11), id(2));
    // No step: the cursor is already at `now` — and yet the entry that
    // has arrived fires while the future one stays pending.
    assert_eq!(w.advance_to(at(10)), vec![id(1)]);
    assert_eq!(w.next_deadline(), Some(at(11)));
    // The step that follows still finds the future entry exactly once.
    assert_eq!(w.advance_to(at(11)), vec![id(2)]);
    assert_eq!(w.len(), 0);
}

/// A deadline already behind the cursor is collected by the call that
/// finds it due: `insert_internal` files it in the slot after the
/// cursor, and `advance_to`'s sweep reaches that slot even when the
/// call has no step left to take.
#[test]
fn past_deadline_still_fires_at_the_coming_step() {
    let mut w = super::TimingWheel::new(at(100));
    w.insert(at(40), id(1));
    assert_eq!(w.advance_to(at(100)), vec![id(1)]);
    assert_eq!(w.len(), 0);
}

/// The sub-second remainder of the same boundary: `num_seconds`
/// truncates, so a deadline half a second past the cursor is
/// "delta 0" too and has to take the same next-slot route.
#[test]
fn sub_second_deadline_fires_on_the_next_step() {
    let mut w = super::TimingWheel::new(at(10));
    w.insert(at(10) + chrono::Duration::milliseconds(500), id(1));
    assert!(w.advance_to(at(10)).is_empty(), "still in the future");
    assert_eq!(w.advance_to(at(11)), vec![id(1)]);
}

/// A deadline filed in the slot the cursor is sitting on is the *last*
/// thing its level reaches, because that slot has just been serviced.
/// Scanning from it made `next_deadline` report a lap-away deadline
/// ahead of a nearer one in the same level; the host arms its timer
/// from that answer, so the near job ran up to a lap late.
///
/// One case per level, each pairing a lap-tail deadline (fractional,
/// so its slot rounds up onto the cursor's own) with a nearer one in
/// the same level.
#[test]
fn lap_tail_entry_does_not_mask_a_nearer_deadline() {
    // Level 0 (64s lap): the tail second rounds up onto slot 0.
    let mut w = super::TimingWheel::new(at(0));
    w.insert(at_ms(63, 900), id(1));
    w.insert(at(5), id(2));
    assert_eq!(w.next_deadline(), Some(at(5)));

    // Level 1 (4096s lap).
    let mut w = super::TimingWheel::new(at(0));
    w.insert(at_ms(4_095, 900), id(1));
    w.insert(at(100), id(2));
    assert_eq!(w.next_deadline(), Some(at(100)));

    // Level 2 (262144s lap).
    let mut w = super::TimingWheel::new(at(0));
    w.insert(at_ms(262_143, 900), id(1));
    w.insert(at(5_000), id(2));
    assert_eq!(w.next_deadline(), Some(at(5_000)));

    // Level 3 (the span itself, 16777216s).
    let mut w = super::TimingWheel::new(at(0));
    w.insert(at_ms(16_777_215, 900), id(1));
    w.insert(at(300_000), id(2));
    assert_eq!(w.next_deadline(), Some(at(300_000)));
}

/// The scan's last resort still reaches the passed slot: a lap-tail
/// entry is a real pending deadline, so it must stay visible rather
/// than be masked by scanning order in the other direction.
///
/// It also fires exactly on time: at its deadline the walk has reached
/// the second before the wrap, which puts the entry's slot first in
/// line for the post-walk sweep.
#[test]
fn a_lap_tail_entry_is_still_reported_and_fires_on_time() {
    let mut w = super::TimingWheel::new(at(0));
    w.insert(at_ms(63, 900), id(1));
    assert_eq!(w.next_deadline(), Some(at_ms(63, 900)));
    assert_eq!(
        w.advance_to(at_ms(63, 900)),
        vec![id(1)],
        "no lateness: the sweep reaches the wrapped slot at the deadline"
    );
    assert_eq!(w.next_deadline(), None);
}

/// Cross-level ordering: the nearest deadline wins whatever level
/// holds it (`best` is a `min`, and the per-level scans must not
/// short-circuit it).
#[test]
fn next_deadline_is_the_nearest_across_levels() {
    let mut w = super::TimingWheel::new(at(0));
    w.insert(at(5_000), id(1)); // level 2
    w.insert(at(100), id(2)); // level 1
    w.insert(at(9), id(3)); // level 0
    assert_eq!(w.next_deadline(), Some(at(9)));
    // Each level's own entry becomes the nearest as the nearer ones
    // are collected.
    assert_eq!(w.advance_to(at(9)), vec![id(3)]);
    assert_eq!(w.next_deadline(), Some(at(100)));
    assert_eq!(w.advance_to(at(100)), vec![id(2)]);
    assert_eq!(w.next_deadline(), Some(at(5_000)));
}

/// A fractional deadline below the cursor's fraction is filed under
/// its own second (the visit at `second + fraction` is already past
/// it), so it is collectible at its deadline — the rounding must not
/// shove it a second later.
#[test]
fn fractional_deadline_below_the_cursor_fraction_still_fires_on_time() {
    let mut w = super::TimingWheel::new(at_ms(0, 400));
    w.insert(at_ms(10, 200), id(1));
    assert_eq!(w.advance_to(at_ms(10, 200)), vec![id(1)]);
}

/// And when the fractions are equal, the deadline is due at that very
/// instant: the visit lands exactly on it.
#[test]
fn equal_fractions_fire_at_the_deadline() {
    let mut w = super::TimingWheel::new(at_ms(0, 400));
    w.insert(at_ms(10, 400), id(1));
    assert_eq!(w.advance_to(at_ms(10, 400)), vec![id(1)]);
}

#[test]
fn ten_thousand_jobs_empty_tick_is_cheap_and_correct() {
    // 等价性 + 规模烟囱测试：1 万任务、无到期 tick 立即返回
    let mut w = super::TimingWheel::new(at(0));
    for i in 0..10_000u64 {
        w.insert(at(10_000 + i as i64), id(i));
    }
    assert!(w.advance_to(at(5_000)).is_empty());
    let fired = w.advance_to(at(10_100));
    assert_eq!(fired.len(), 101);
}

/// A fractional deadline must not wait a lap for its slot to come
/// round again.
///
/// The cursor keeps its fraction as it steps, and a whole-second host
/// visits a slot only at the whole second: filing an entry under its
/// own truncated second put it in the slot the cursor had already
/// passed for that second (inspected just *before* it was due), and
/// that slot is not visited again until the level-0 lap wraps (~64 s)
/// — with `next_deadline` reporting a past deadline the whole time.
#[test]
fn fractional_deadline_is_not_stranded_for_a_lap() {
    let mut w = super::TimingWheel::new(at(0));
    w.insert(at_ms(10, 900), id(1));
    assert!(w.advance_to(at(10)).is_empty(), "not due at 10.0");
    assert_eq!(
        w.advance_to(at(11)),
        vec![id(1)],
        "the second after the deadline collects it — no lap wait"
    );
    assert_eq!(w.len(), 0);
}

/// The same, with a fractional cursor: the visit lands at
/// `second + fraction`, so the entry has to be filed under the second
/// whose visit reaches it (11.4), not the one it was inserted in.
#[test]
fn fractional_deadline_with_a_fractional_cursor_is_not_stranded() {
    let mut w = super::TimingWheel::new(at_ms(0, 400));
    w.insert(at_ms(10, 900), id(1));
    assert!(w.advance_to(at_ms(10, 400)).is_empty(), "not due yet");
    assert_eq!(w.advance_to(at_ms(11, 400)), vec![id(1)]);
}

/// A whole-second deadline is untouched by the cursor's fraction:
/// it is collected as soon as a call finds it due, never a second
/// late (rounding every integral deadline up would be the easy way
/// to get the fractional case wrong).
#[test]
fn whole_second_deadline_is_not_pushed_a_second_late() {
    let mut w = super::TimingWheel::new(at_ms(0, 400));
    w.insert(at(10), id(1));
    assert_eq!(w.advance_to(at(10)), vec![id(1)]);
}
