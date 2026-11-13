use serde_json::json;

use super::*;
use crate::{events::SurfaceOp, typed_event_builders::usage};

/// Build a surface-eligible append event carrying a
/// provider-shaped message payload.
fn surface_event(seq: u64, message: &Message) -> SessionEvent {
    let data = serde_json::to_value(message).expect("serialize");
    SessionEvent::UserMessage {
        seq,
        ts: "2026-09-12T00:00:00Z".to_string(),
        data,
        source: None,
        surface_op: Some(SurfaceOp::append()),
    }
}

fn assistant_event(seq: u64, text: &str) -> SessionEvent {
    let data = serde_json::to_value(Message::assistant(text)).expect("ser");
    SessionEvent::AssistantMessage {
        seq,
        ts: "2026-09-12T00:00:00Z".to_string(),
        data,
        surface_op: Some(SurfaceOp::append()),
    }
}

fn compaction_event(
    seq: u64,
    start: usize,
    end: usize,
    sources: &[u64],
) -> SessionEvent {
    SessionEvent::Compaction {
        seq,
        ts: "2026-09-12T00:00:00Z".to_string(),
        data: serde_json::to_value(Message::assistant("sum")).expect("ser"),
        surface_op: SurfaceOp::Replace {
            start,
            end,
            source_event_seqs: sources.to_vec(),
        },
    }
}

fn usage_event(seq: u64, buckets: UsageBuckets) -> SessionEvent {
    let mut event = usage(
        buckets.input_tokens as usize,
        buckets.output_tokens as usize,
        buckets.total_tokens() as usize,
        None,
        buckets.cache_read_tokens.map(|n| n as usize),
        buckets.cache_write_tokens.map(|n| n as usize),
    );
    // The builder stamps seq 0; tests need real seqs.
    if let SessionEvent::Usage { seq: slot, .. } = &mut event {
        *slot = seq;
    }
    event
}

const BIG: &str = "lorem ipsum dolor sit amet consectetur adipiscing elit sed";

const LONG: &str =
    "a longer assistant reply that prices to a non-zero estimate";

#[test]
fn usage_buckets_prompt_side_excludes_output() {
    let buckets = UsageBuckets {
        input_tokens: 100,
        output_tokens: 40,
        cache_read_tokens: Some(10),
        cache_write_tokens: Some(5),
    };
    assert_eq!(buckets.prompt_side_tokens(), 115);
    assert_eq!(buckets.total_tokens(), 155);
}

#[test]
fn usage_buckets_from_token_usage_maps_disjoint_fields() {
    let wire = TokenUsage {
        prompt_tokens: 90,
        completion_tokens: 10,
        total_tokens: 100,
        cached_prompt_tokens: None,
        cache_read_tokens: Some(30),
        cache_write_tokens: None,
        reasoning_tokens: None,
    };
    let buckets = UsageBuckets::from_token_usage(&wire);
    assert_eq!(buckets.input_tokens, 90);
    assert_eq!(buckets.output_tokens, 10);
    assert_eq!(buckets.cache_read_tokens, Some(30));
    assert_eq!(buckets.cache_write_tokens, None);
}

#[test]
fn usage_buckets_add_saturates_and_merges_options() {
    let mut left = UsageBuckets {
        input_tokens: u64::MAX - 1,
        output_tokens: 5,
        cache_read_tokens: Some(1),
        cache_write_tokens: None,
    };
    left.add(&UsageBuckets {
        input_tokens: 10,
        output_tokens: 5,
        cache_read_tokens: Some(2),
        cache_write_tokens: Some(7),
    });
    assert_eq!(left.input_tokens, u64::MAX);
    assert_eq!(left.output_tokens, 10);
    assert_eq!(left.cache_read_tokens, Some(3));
    assert_eq!(left.cache_write_tokens, Some(7));
}

#[test]
fn empty_log_measures_none_baseline() {
    let meter = TokenMeter::new();
    let m = meter.measure();
    assert_eq!(m.baseline, MeasurementBaseline::None);
    assert_eq!(m.total_tokens, 0);
    assert_eq!(m.surface_tokens, 0);
}

#[test]
fn heuristic_surface_prices_messages_without_anchor() {
    let log = vec![surface_event(0, &Message::user(BIG))];
    let mut meter = TokenMeter::new();
    meter.observe(&log).expect("fold");
    let m = meter.measure();
    assert_eq!(
        m.baseline,
        MeasurementBaseline::Estimated {
            tokens: m.surface_tokens
        }
    );
    assert!(m.surface_tokens > 0);
    assert_eq!(m.total_tokens, m.surface_tokens);
}

#[test]
fn plausible_usage_sample_is_adopted_as_anchor() {
    // Surface the request saw: one user message. The provider
    // counted strictly more prompt tokens than the heuristic
    // price (envelope + exact tokenization), so it is adopted.
    let user = surface_event(0, &Message::user(BIG));
    let heuristic = estimate_message_tokens(user_data(&user));
    let assistant = assistant_event(1, LONG);
    let response = estimate_message_tokens(user_data(&assistant));
    let pressure = heuristic * 2;
    let sample = usage_event(
        2,
        UsageBuckets {
            input_tokens: pressure,
            output_tokens: 12,
            cache_read_tokens: None,
            cache_write_tokens: None,
        },
    );
    let mut meter = TokenMeter::new();
    meter.observe(&[user, assistant, sample]).expect("fold");
    let m = meter.measure();
    let MeasurementBaseline::Usage {
        pressure_tokens, ..
    } = m.baseline
    else {
        panic!("expected usage baseline, got {:?}", m.baseline);
    };
    assert_eq!(pressure_tokens, pressure);
    // Projection: pressure covers the surface the request saw
    // plus the assistant response that joined after the stamp.
    assert_eq!(m.surface_delta_tokens, response as i64);
    assert_eq!(m.total_tokens, pressure + response);
}

#[test]
fn stale_usage_sample_below_heuristic_is_rejected() {
    // The provider count is below the heuristic price of the
    // surface it claims to have priced — cannot correspond to
    // this surface, so the meter keeps the heuristic baseline.
    let log = vec![
        surface_event(0, &Message::user(BIG)),
        assistant_event(1, "ok"),
        usage_event(
            2,
            UsageBuckets {
                input_tokens: 1,
                output_tokens: 1,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        ),
    ];
    let mut meter = TokenMeter::new();
    meter.observe(&log).expect("fold");
    let m = meter.measure();
    assert!(
        matches!(m.baseline, MeasurementBaseline::Estimated { .. }),
        "stale sample must not anchor: {:?}",
        m.baseline
    );
    // Totals still accumulate the report (billing truth).
    assert_eq!(meter.usage_totals().input_tokens, 1);
}

#[test]
fn projection_tracks_surface_growth_and_shrink() {
    let user0 = surface_event(0, &Message::user(BIG));
    let user1 = surface_event(1, &Message::user(BIG));
    let assistant = assistant_event(2, LONG);
    let heuristic = estimate_message_tokens(user_data(&user0));
    let response = estimate_message_tokens(user_data(&assistant));
    let pressure = heuristic * 4;
    let sample = usage_event(
        3,
        UsageBuckets {
            input_tokens: pressure,
            output_tokens: 5,
            cache_read_tokens: None,
            cache_write_tokens: None,
        },
    );

    // Growth: a third user message joins after the sample.
    let grown = vec![
        user0.clone(),
        user1.clone(),
        assistant.clone(),
        sample.clone(),
        surface_event(4, &Message::user(BIG)),
    ];
    let mut meter = TokenMeter::new();
    meter.observe(&grown).expect("fold");
    let m = meter.measure();
    assert_eq!(m.surface_delta_tokens, (heuristic + response) as i64);
    assert_eq!(m.total_tokens, pressure + heuristic + response);

    // Shrink: compaction replaces both pre-boundary user
    // messages with a short summary, so the surface drops
    // below the anchored sample.
    let summary = Message::assistant("sum");
    let summary_price =
        estimate_message_tokens(&serde_json::to_value(&summary).expect("ser"));
    let shrunk = vec![
        user0,
        user1,
        assistant,
        sample,
        compaction_event(5, 0, 2, &[0, 1]),
    ];
    let mut meter = TokenMeter::new();
    meter.observe(&shrunk).expect("fold");
    let m = meter.measure();
    let expected_delta =
        (summary_price + response) as i64 - (2 * heuristic) as i64;
    assert!(expected_delta < 0, "surface must shrink");
    assert_eq!(m.surface_delta_tokens, expected_delta);
    assert_eq!(m.total_tokens, (pressure as i64 + expected_delta) as u64);
}

#[test]
fn request_header_drift_drops_the_anchor() {
    let log = vec![
        surface_event(0, &Message::user(BIG)),
        assistant_event(1, "ok"),
        usage_event(
            2,
            UsageBuckets {
                input_tokens: 10_000,
                output_tokens: 20,
                cache_read_tokens: None,
                cache_write_tokens: None,
            },
        ),
        SessionEvent::RequestHeader {
            seq: 3,
            ts: "2026-09-12T00:00:00Z".to_string(),
            data: json!({"reason": "change"}),
        },
    ];
    let mut meter = TokenMeter::new();
    meter.observe(&log).expect("fold");
    let m = meter.measure();
    assert!(
        matches!(m.baseline, MeasurementBaseline::Estimated { .. }),
        "envelope drift must invalidate the anchor"
    );
    assert!(meter.context_pressure(None).is_none());
}

#[test]
fn usage_totals_sum_every_report_disjointly() {
    let log = vec![
        usage_event(
            0,
            UsageBuckets {
                input_tokens: 100,
                output_tokens: 50,
                cache_read_tokens: Some(10),
                cache_write_tokens: Some(5),
            },
        ),
        usage_event(
            1,
            UsageBuckets {
                input_tokens: 200,
                output_tokens: 25,
                cache_read_tokens: Some(40),
                cache_write_tokens: None,
            },
        ),
    ];
    let mut meter = TokenMeter::new();
    meter.observe(&log).expect("fold");
    let totals = meter.usage_totals();
    assert_eq!(totals.input_tokens, 300);
    assert_eq!(totals.output_tokens, 75);
    assert_eq!(totals.cache_read_tokens, Some(50));
    assert_eq!(totals.cache_write_tokens, Some(5));
}

#[test]
fn context_pressure_projection_reflects_anchor_and_window() {
    let log = vec![
        surface_event(0, &Message::user(BIG)),
        assistant_event(1, "ok"),
        usage_event(
            2,
            UsageBuckets {
                input_tokens: 5_000,
                output_tokens: 10,
                cache_read_tokens: Some(500),
                cache_write_tokens: None,
            },
        ),
    ];
    let mut meter = TokenMeter::new();
    meter.observe(&log).expect("fold");
    let pressure = meter.context_pressure(Some(200_000)).expect("anchored");
    assert_eq!(pressure.pressure_tokens, 5_500);
    assert_eq!(pressure.projected_tokens, meter.measure().total_tokens);
    assert_eq!(pressure.context_window, Some(200_000));
}

#[test]
fn context_pressure_absent_before_first_sample() {
    let log = vec![surface_event(0, &Message::user(BIG))];
    let mut meter = TokenMeter::new();
    meter.observe(&log).expect("fold");
    assert!(meter.context_pressure(Some(1_000)).is_none());
}

#[test]
fn observe_is_idempotent_catch_up() {
    let log = vec![surface_event(0, &Message::user(BIG))];
    let mut meter = TokenMeter::new();
    meter.observe(&log).expect("first");
    let first = meter.measure();
    meter.observe(&log).expect("second");
    assert_eq!(meter.measure(), first);
    let mut extended = log;
    extended.push(assistant_event(1, "hi"));
    meter.observe(&extended).expect("extended");
    assert_eq!(meter.measure().log_revision, 2);
}

#[test]
fn shrunken_log_is_an_error() {
    let log = vec![surface_event(0, &Message::user(BIG))];
    let mut meter = TokenMeter::new();
    meter.observe(&log).expect("fold");
    let err = meter.observe(&[]).expect_err("must reject");
    assert_eq!(err, TokenMeterError::LogShrank { folded: 1, len: 0 });
}

#[test]
fn bad_replace_provenance_surfaces_fold_error() {
    let log = vec![
        surface_event(0, &Message::user(BIG)),
        // Cites seq 9 — not on the surface yet.
        compaction_event(1, 0, 1, &[9]),
    ];
    let mut meter = TokenMeter::new();
    let err = meter.observe(&log).expect_err("must reject");
    assert!(matches!(err, TokenMeterError::Fold(_)));
    // The failing event stays unconsumed; the prefix did fold.
    assert_eq!(meter.measure().log_revision, 1);
}

#[test]
fn legacy_usage_payload_without_typed_field_still_folds() {
    // Pre-R30 row: buckets live only inside `data`.
    let row = json!({
        "type": "usage",
        "seq": 3,
        "ts": "2026-09-12T00:00:00Z",
        "data": {
            "prompt_tokens": 700,
            "completion_tokens": 30,
            "total_tokens": 730,
            "cache_read_tokens": 40
        }
    });
    let event = SessionEvent::from_value(&row).expect("old row must parse");
    let mut meter = TokenMeter::new();
    meter.observe(std::slice::from_ref(&event)).expect("fold");
    let totals = meter.usage_totals();
    assert_eq!(totals.input_tokens, 700);
    assert_eq!(totals.output_tokens, 30);
    assert_eq!(totals.cache_read_tokens, Some(40));
    assert_eq!(totals.cache_write_tokens, None);
}

/// Extract the `data` payload of a surface event (test helper).
fn user_data(event: &SessionEvent) -> &Value {
    match event {
        SessionEvent::UserMessage { data, .. }
        | SessionEvent::AssistantMessage { data, .. } => data,
        _ => panic!("not a message event"),
    }
}
