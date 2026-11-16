//! Unit tests for the L1 merge window and the activity mapping.

use std::time::{Duration, Instant};

use uva_events::{AgentEvent, SystemEvent};
use uva_provider::{
    ContentPart,
    ReasoningContent,
    SamplingResult,
    TextContent,
    ToolUse,
};
use uva_tool::ToolOutput;

use super::{L1Feed, L1Observer, PROGRESS_WINDOW, clamp, first_line};
use crate::control::{ChildStatus, RoundOutcome};

fn text_of(output: &ToolOutput) -> String {
    output
        .content
        .iter()
        .filter_map(ContentPart::text)
        .collect::<String>()
}

fn text_delta(text: &str) -> AgentEvent {
    AgentEvent::Model(ContentPart::Text(TextContent {
        text: text.to_string(),
        cache_control: None,
    }))
}

fn tool_use(name: &str) -> AgentEvent {
    AgentEvent::Model(ContentPart::ToolUse(ToolUse {
        id: "call-1".to_string(),
        name: name.to_string(),
        input: serde_json::Value::Null,
    }))
}

fn step(message: &str, step: usize, total: usize) -> AgentEvent {
    AgentEvent::System(SystemEvent::Progress {
        message: message.to_string(),
        step,
        total,
    })
}

#[test]
fn a_window_merges_every_update_into_the_latest_line() {
    let feed = L1Feed::default();
    let start = Instant::now();
    assert!(
        feed.poll(start).is_none(),
        "nothing recorded, nothing to emit"
    );

    for step in 0..100 {
        feed.record("id-0", ChildStatus::Running, &format!("step {step}"));
    }
    // The first poll after an emission is what the window gates.
    let first = feed.poll(start).expect("the first window emits");
    assert_eq!(text_of(&first), "[id-0] running: step 99\n");

    for step in 100..150 {
        feed.record("id-0", ChildStatus::Running, &format!("step {step}"));
    }
    assert!(
        feed.poll(start + PROGRESS_WINDOW - Duration::from_millis(1))
            .is_none(),
        "inside the window nothing is emitted"
    );
    let second = feed
        .poll(start + PROGRESS_WINDOW)
        .expect("the window elapsed");
    assert_eq!(
        text_of(&second),
        "[id-0] running: step 149\n",
        "only the latest line survives the window"
    );
    let children = second
        .metadata
        .get("children")
        .and_then(serde_json::Value::as_array)
        .expect("the payload carries the fields");
    assert_eq!(children.len(), 1);
    assert_eq!(children[0]["child_id"], "id-0");
    assert_eq!(children[0]["status"], "running");
    assert_eq!(children[0]["activity"], "step 149");
}

#[test]
fn two_children_share_one_output_in_first_seen_order() {
    let feed = L1Feed::default();
    let start = Instant::now();
    feed.record("id-1", ChildStatus::Running, "reading guard.rs");
    feed.record("id-2", ChildStatus::Running, "tracing approvals");
    feed.record("id-1", ChildStatus::Running, "writing notes");

    let output = feed.poll(start).expect("a pending window emits");
    assert_eq!(
        text_of(&output),
        "[id-1] running: writing notes\n[id-2] running: tracing approvals\n"
    );
    assert_eq!(
        output.metadata["children"].as_array().map(Vec::len),
        Some(2)
    );
}

#[test]
fn flush_ignores_the_window() {
    let feed = L1Feed::default();
    let start = Instant::now();
    assert!(feed.flush(start).is_none(), "nothing pending");
    feed.record("id-0", ChildStatus::Idle, "finished");
    let output = feed.flush(start).expect("the terminal summary emits");
    assert_eq!(text_of(&output), "[id-0] idle: finished\n");
    assert!(
        feed.flush(start).is_none(),
        "an emission drains the pending set"
    );
}

/// A child that reports once per millisecond for a minute must not
/// cost the parent's event sink anything like one event per update.
///
/// The live sink caps at 1024 buffered events (`uva_harness`'s
/// `DEFAULT_EVENT_CAPACITY`); the window is what keeps a long child
/// run far below it.
#[test]
fn a_long_high_frequency_child_stays_far_below_the_sink_cap() {
    const SINK_CAP: usize = 1024;
    let feed = L1Feed::default();
    let start = Instant::now();
    let updates = 60_000u64; // one minute at 1 kHz
    let mut emitted = 0usize;
    let mut previous: Option<Instant> = None;

    for tick in 0..updates {
        let now = start + Duration::from_millis(tick);
        feed.record("id-0", ChildStatus::Running, &format!("step {tick}"));
        if feed.poll(now).is_some() {
            emitted += 1;
            if let Some(previous) = previous {
                assert!(
                    now.saturating_duration_since(previous) >= PROGRESS_WINDOW,
                    "two emissions inside one window"
                );
            }
            previous = Some(now);
        }
    }

    let windows = updates.div_ceil(PROGRESS_WINDOW.as_millis() as u64);
    assert!(
        emitted as u64 <= windows + 1,
        "the window bounds the stream: {emitted} chunks for {updates} \
         updates"
    );
    assert!(
        emitted < SINK_CAP,
        "a minute of child activity must stay under the event cap: \
         {emitted}"
    );
}

#[test]
fn an_llm_step_names_step_and_total() {
    let feed = std::sync::Arc::new(L1Feed::default());
    let mut observer = L1Observer::new(std::sync::Arc::clone(&feed), "id-0");
    observer.observe(&step("LLM pass 1", 1, 5));
    let output = feed.flush(Instant::now()).expect("pending");
    assert_eq!(text_of(&output), "[id-0] running: LLM pass 1 (1/5)\n");

    observer.observe(&step("Executing tool read", 0, 5));
    let output = feed.flush(Instant::now()).expect("pending");
    assert_eq!(text_of(&output), "[id-0] running: Executing tool read\n");
}

#[test]
fn a_turn_reports_its_first_line_once() {
    let feed = std::sync::Arc::new(L1Feed::default());
    let mut observer = L1Observer::new(std::sync::Arc::clone(&feed), "id-0");
    observer.observe(&text_delta("\n\n  Reading guard.rs  "));
    observer.observe(&text_delta("Reading guard.rs:444"));
    let output = feed.flush(Instant::now()).expect("pending");
    assert_eq!(
        text_of(&output),
        "[id-0] running: writing · Reading guard.rs\n",
        "later deltas of the same turn do not churn the line"
    );
}

#[test]
fn tool_calls_and_outcomes_are_reported() {
    let feed = std::sync::Arc::new(L1Feed::default());
    let mut observer = L1Observer::new(std::sync::Arc::clone(&feed), "id-0");
    observer.observe(&tool_use("read"));
    let output = feed.flush(Instant::now()).expect("pending");
    assert_eq!(text_of(&output), "[id-0] running: calling read\n");

    let mut done = SamplingResult::default();
    done.tool_calls.push(ToolUse {
        id: "call-2".to_string(),
        name: "shell".to_string(),
        input: serde_json::Value::Null,
    });
    observer.observe(&AgentEvent::ModelDone(done));
    let output = feed.flush(Instant::now()).expect("pending");
    assert_eq!(text_of(&output), "[id-0] running: calling shell\n");

    observer.finish(RoundOutcome::Completed);
    let output = feed.flush(Instant::now()).expect("pending");
    assert_eq!(text_of(&output), "[id-0] idle: finished\n");
}

#[test]
fn a_repeated_state_is_not_re_recorded() {
    let feed = std::sync::Arc::new(L1Feed::default());
    let mut observer = L1Observer::new(std::sync::Arc::clone(&feed), "id-0");
    let _ = feed.flush(Instant::now());
    observer.observe(&AgentEvent::Model(ContentPart::Reasoning(
        ReasoningContent {
            text: "hmm".to_string(),
            signature: None,
        },
    )));
    assert!(feed.flush(Instant::now()).is_some());
    observer.observe(&AgentEvent::Model(ContentPart::Reasoning(
        ReasoningContent {
            text: "still thinking".to_string(),
            signature: None,
        },
    )));
    assert!(
        feed.flush(Instant::now()).is_none(),
        "the same activity word does not re-open the window"
    );
}

#[test]
fn one_activity_line_is_bounded() {
    let long = "x".repeat(200);
    let clamped = clamp(&long);
    assert_eq!(clamped.chars().count(), super::MAX_ACTIVITY_CHARS + 1);
    assert!(clamped.ends_with('…'));
    assert_eq!(clamp("a\nb"), "a b", "control characters become spaces");
    assert_eq!(first_line("\n\n  second  \nthird"), "second");
    assert_eq!(first_line("   "), "");
}
