//! The L1 progress summary a `task` call streams into its parent's
//! tool block.
//!
//! One line per child — `[child_id] status: activity` — carrying the
//! three facts the parent page needs: which child, what state, and the
//! one-line answer to "what is it doing right now". The child id is
//! also the L2 handle: the summary names it, the reader clicks it.
//!
//! # Why a merge window
//!
//! A child's event stream is high-frequency: every model delta, every
//! step progress notice, every tool call. Forwarding each one as
//! `StreamOutput::Progress` would hand the parent's `EventSink`
//! thousands of live events for one child run, and that sink drops
//! above its 1024-event soft cap. [`L1Feed`] keeps the **latest**
//! activity per child and hands it out at most once per
//! [`PROGRESS_WINDOW`] (150 ms, the reference implementations' own
//! window), so the parent's buffer is bounded by time, not by how
//! chatty the child is.
//!
//! # The payload
//!
//! The progress output's text is the rendered line(s); its
//! `children` metadata carries the same facts as
//! `[{child_id, status, activity}]` for a consumer that wants the
//! fields rather than the sentence.

use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use uva_events::{AgentEvent, SystemEvent};
use uva_provider::ContentPart;
use uva_tool::ToolOutput;

use crate::control::{ChildStatus, RoundOutcome};

/// How often a child's merged activity may reach the parent tool
/// block.
pub(crate) const PROGRESS_WINDOW: Duration = Duration::from_millis(150);

/// The longest activity line, in characters; the rest is elided.
const MAX_ACTIVITY_CHARS: usize = 80;

/// The merge window one `task` call's progress passes through.
///
/// [`L1Feed::record`] is cheap and may be called per child event;
/// [`L1Feed::poll`] is what the tool stream calls between dispatch
/// polls, and returns at most one merged output per
/// [`PROGRESS_WINDOW`].
#[derive(Default)]
pub(crate) struct L1Feed {
    inner: Mutex<Feed>,
}

#[derive(Default)]
struct Feed {
    /// One entry per child with something new since the last emission,
    /// in first-seen order. A later `record` for the same child
    /// replaces the entry instead of appending: the window keeps the
    /// latest activity, never a queue.
    pending: Vec<PendingLine>,
    /// When the last merged output was handed out.
    last_emit: Option<Instant>,
}

struct PendingLine {
    child_id: String,
    status: ChildStatus,
    activity: String,
}

impl L1Feed {
    /// Record the child's current activity.
    ///
    /// A second record for the same child before the next emission
    /// replaces the first — the parent sees the latest line, not every
    /// line.
    pub(crate) fn record(
        &self,
        child_id: &str,
        status: ChildStatus,
        activity: &str,
    ) {
        let mut feed = self.inner.lock();
        match feed
            .pending
            .iter_mut()
            .find(|line| line.child_id == child_id)
        {
            Some(line) => {
                line.status = status;
                line.activity.clear();
                line.activity.push_str(activity);
            }
            None => feed.pending.push(PendingLine {
                child_id: child_id.to_string(),
                status,
                activity: activity.to_string(),
            }),
        }
    }

    /// The merged lines due at `now`, or `None` when nothing has
    /// changed or the last emission is younger than
    /// [`PROGRESS_WINDOW`].
    pub(crate) fn poll(&self, now: Instant) -> Option<ToolOutput> {
        let mut feed = self.inner.lock();
        if feed.pending.is_empty() {
            return None;
        }
        if let Some(last) = feed.last_emit
            && now.saturating_duration_since(last) < PROGRESS_WINDOW
        {
            return None;
        }
        Some(feed.emit(now))
    }

    /// Emit whatever is pending regardless of the window.
    ///
    /// The stream's terminal step calls this so a child that finishes
    /// inside one window still leaves a summary line before its
    /// result, instead of none at all.
    pub(crate) fn flush(&self, now: Instant) -> Option<ToolOutput> {
        let mut feed = self.inner.lock();
        if feed.pending.is_empty() {
            return None;
        }
        Some(feed.emit(now))
    }
}

impl Feed {
    fn emit(&mut self, now: Instant) -> ToolOutput {
        self.last_emit = Some(now);
        render(&std::mem::take(&mut self.pending))
    }
}

/// Render one window's lines as the progress output.
fn render(lines: &[PendingLine]) -> ToolOutput {
    let mut text = String::new();
    let mut children = Vec::with_capacity(lines.len());
    for line in lines {
        text.push('[');
        text.push_str(&line.child_id);
        text.push_str("] ");
        text.push_str(line.status.as_str());
        text.push_str(": ");
        text.push_str(&line.activity);
        text.push('\n');
        children.push(serde_json::json!({
            "child_id": line.child_id,
            "status": line.status.as_str(),
            "activity": line.activity,
        }));
    }
    ToolOutput::text(text)
        .with_metadata("children", serde_json::Value::Array(children))
}

/// Follows one child's run and records its activity into the call's
/// feed.
///
/// The observer lives for one child run. [`Self::observe`] is called
/// for every event the child streams; it turns the events that mean
/// "a new step started" into one activity line and ignores the rest,
/// so the feed never churns on per-token deltas.
pub(crate) struct L1Observer {
    feed: Arc<L1Feed>,
    child_id: String,
    /// The last activity reported, so a repeated state is not
    /// re-recorded.
    last: String,
    /// Whether the current model turn's text has been reported. A
    /// turn streams many deltas; the first non-empty one names it.
    writing: bool,
}

impl L1Observer {
    /// Start reporting for `child_id`.
    pub(crate) fn new(feed: Arc<L1Feed>, child_id: &str) -> Self {
        let mut observer = Self {
            feed,
            child_id: child_id.to_string(),
            last: String::new(),
            writing: false,
        };
        observer.report("starting".to_string());
        observer
    }

    /// Record what `event` says the child is doing, if anything.
    pub(crate) fn observe(&mut self, event: &AgentEvent) {
        match event {
            AgentEvent::Model(ContentPart::Text(text)) => {
                if self.writing {
                    return;
                }
                let line = first_line(&text.text);
                if !line.is_empty() {
                    self.writing = true;
                    self.report(format!("writing · {line}"));
                }
            }
            AgentEvent::Model(ContentPart::Reasoning(_)) => {
                self.writing = false;
                self.report("thinking".to_string());
            }
            AgentEvent::Model(ContentPart::ToolUse(tool)) => {
                self.writing = false;
                self.report(format!("calling {}", clamp(&tool.name)));
            }
            AgentEvent::Model(ContentPart::ToolResult(_)) => {
                // A tool result starts no new visible step; the next
                // model turn or tool call will.
                self.writing = false;
            }
            AgentEvent::Model(_) => {}
            AgentEvent::ModelDone(done) => {
                self.writing = false;
                if done.tool_calls.is_empty() {
                    if !done.text.trim().is_empty() {
                        self.report("composing the answer".to_string());
                    }
                } else {
                    let names = done
                        .tool_calls
                        .iter()
                        .map(|call| clamp(&call.name))
                        .collect::<Vec<_>>()
                        .join(", ");
                    self.report(format!("calling {names}"));
                }
            }
            AgentEvent::System(event) => self.observe_system(event),
        }
    }

    fn observe_system(&mut self, event: &SystemEvent) {
        let activity = match event {
            SystemEvent::Progress {
                message,
                step,
                total,
            } => {
                if *step > 0 && *total > 0 {
                    format!("{} ({step}/{total})", clamp(message))
                } else {
                    clamp(message)
                }
            }
            SystemEvent::SessionStarted { .. } => "running".to_string(),
            SystemEvent::SessionInterrupted { reason } => {
                format!("interrupted: {}", clamp(reason))
            }
            SystemEvent::Warning { message, .. } => {
                format!("warning: {}", clamp(message))
            }
            _ => return,
        };
        self.writing = false;
        self.report(activity);
    }

    /// The child's round ended: report the idle state and how it went.
    pub(crate) fn finish(&mut self, outcome: RoundOutcome) {
        self.writing = false;
        let activity = match outcome {
            RoundOutcome::Completed => "finished",
            RoundOutcome::Cancelled => "cancelled",
            RoundOutcome::Failed => "failed",
        };
        self.feed
            .record(&self.child_id, ChildStatus::Idle, activity);
    }

    fn report(&mut self, activity: String) {
        if activity == self.last {
            return;
        }
        self.feed
            .record(&self.child_id, ChildStatus::Running, &activity);
        self.last = activity;
    }
}

/// The first non-empty line of `text`, clamped to one line.
fn first_line(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(clamp)
        .unwrap_or_default()
}

/// One line, at most [`MAX_ACTIVITY_CHARS`] characters: control
/// characters become spaces, the tail becomes an ellipsis.
fn clamp(text: &str) -> String {
    let mut out = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index == MAX_ACTIVITY_CHARS {
            out.push('…');
            break;
        }
        out.push(if ch.is_control() { ' ' } else { ch });
    }
    out
}

#[cfg(test)]
mod tests;
