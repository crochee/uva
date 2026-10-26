//! The run's event publication seam: [`EventSink`] is the single sink type every part of the harness publishes through, so an event reaches the consumer the same way no matter which layer emitted it.
use std::{
    fmt,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use futures::channel::mpsc;
use synthia_core::CancelToken;
use synthia_provider::{Content, ContentPart, Message, Role, ToolDefinition};
use synthia_session::{LogWriter, SessionError, SessionEvent};

use crate::{
    agent::{caps::AgentCaps, re_act::projection::tool_names_hash},
    events::{AgentEvent, SessionEndReason, SystemEvent, WarningKind},
};

/// The channel is `futures::channel::mpsc`, not a runtime's: an
/// out-of-tree [`ReasoningStrategy`](crate::agent::ReasoningStrategy)
/// that builds its own sink must not have to depend on tokio to
/// publish events, and the loop that receives them is
/// runtime-neutral too.
#[derive(Clone)]
pub struct EventSink {
    tx: Arc<mpsc::UnboundedSender<AgentEvent>>,
    // The run's log writer. Structural rows go through it when it
    // is wired: it stamps the row's ordinal and buffers it until
    // the step boundary flushes it.
    log_writer: Option<Arc<LogWriter>>,
    /// Shared-across-clones flag enforcing one `SessionEnded` per run.
    ended: Arc<AtomicBool>,
}

impl EventSink {
    /// Wrap the run's sender and its log writer.
    #[must_use]
    pub fn new(
        tx: Arc<mpsc::UnboundedSender<AgentEvent>>,
        log_writer: Option<Arc<LogWriter>>,
    ) -> Self {
        Self {
            tx,
            log_writer,
            ended: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Whether a log writer is wired.
    pub(crate) fn has_writer(&self) -> bool {
        self.log_writer.is_some()
    }

    /// Publish one event.
    pub fn emit(&self, event: AgentEvent) {
        let _ = self.tx.unbounded_send(event);
    }

    /// Publish one structural event to the run's log (iteration /
    /// step / usage boundaries).
    ///
    /// The row goes through the session's writer: it is stamped with
    /// the ordinal the sink reserved and buffered until the loop
    /// flushes at the step boundary. Without a writer the row goes
    /// nowhere — a structural row is a log fact, and a run with no
    /// writer has no log.
    ///
    /// The path is the writer's synchronous one: the loop must not
    /// block on a queue, and a row that does not fit waits in the
    /// writer's spill instead of being dropped.
    pub fn emit_typed(&self, event: synthia_session::SessionEvent) {
        let _ = self.append_typed(event);
    }

    /// Append one typed row and return the ordinal it was given (`0`
    /// without a writer). The sync barrier path needs the ordinal to
    /// build a resume token from the log tail.
    pub(crate) fn append_typed(
        &self,
        event: synthia_session::SessionEvent,
    ) -> u64 {
        self.log_writer
            .as_ref()
            .map(|writer| writer.append_event_nowait(&event))
            .unwrap_or(0)
    }

    /// Publish one system event.
    pub fn system(&self, event: SystemEvent) {
        self.emit(AgentEvent::System(event));
    }

    /// Flush the session's writer: the promise that every row the
    /// run has written so far is on disk.
    ///
    /// A strategy calls this immediately before each provider call,
    /// so the run's prompt — and every row queued before it — is
    /// durable before the model can see it. A run with no writer has
    /// nothing to flush, and a caller that does not flush simply
    /// leaves its rows for the next boundary.
    ///
    /// # Errors
    ///
    /// Returns the sink's error when the batch could not be written.
    pub async fn flush_log(&self) -> Result<(), SessionError> {
        let Some(writer) = &self.log_writer else {
            return Ok(());
        };
        writer.flush().await
    }

    /// Stamp the run's request header: the row that names the
    /// request envelope the model's facts that follow were produced
    /// under.
    ///
    /// The header is the run's epoch marker — one row per run, with
    /// the provider that ran, the model it ran and the hash of
    /// `offered`: the tool set the model was offered for this run (an
    /// empty slice for a strategy that sends no tools). The writer
    /// remembers the last envelope, so the row says whether the
    /// request configuration drifted since the previous run.
    ///
    /// Every strategy calls this where its run begins: the header is
    /// a fact about the run, and a run that left it out would write
    /// its model's facts into a log that names no envelope.
    ///
    /// Without a writer the row goes nowhere — a run with no writer
    /// has no log.
    pub fn stamp_request_header(
        &self,
        caps: &AgentCaps,
        offered: &[ToolDefinition],
    ) {
        let Some(writer) = &self.log_writer else {
            return;
        };
        writer.append_request_header(
            caps.model.provider.name(),
            &caps.model.provider.model_config().name,
            &format!("{:016x}", tool_names_hash(offered)),
        );
    }

    /// Write the model's turn as the session's one `assistant_message`
    /// row.
    ///
    /// The turn's parts — its text and its tool calls together —
    /// become exactly one row, written through the session's writer:
    /// the row carries the ordinal the writer stamped, and the
    /// deltas that produced it stay on the event stream only.
    ///
    /// Every strategy that produces model output calls this where it
    /// publishes the model's text. The row is the durable fact: a
    /// strategy that published its answer on the wire but never
    /// called this would leave the session's log without the run's
    /// answer, and the reader would see a prompt with no reply.
    ///
    /// Without a writer the row goes nowhere — a run with no writer
    /// has no log.
    pub fn write_assistant_turn(&self, parts: &[ContentPart]) {
        let Some(writer) = &self.log_writer else {
            return;
        };
        if parts.is_empty() {
            return;
        }
        let message =
            Message::new(Role::Assistant, Content::Multi(parts.to_vec()));
        writer.append_event_nowait(&SessionEvent::AssistantMessage {
            seq: 0,
            ts: String::new(),
            data: serde_json::to_value(message)
                .expect("assistant message serializes"),
            surface_op: None,
        });
    }

    /// Publish one `Warning` system event — the shared shape
    /// every panic-isolated seam (hooks, guards, inbox,
    /// tracker, compaction) reports through.
    pub fn warn(
        &self,
        kind: WarningKind,
        iteration: Option<usize>,
        message: String,
    ) {
        self.system(SystemEvent::Warning {
            kind,
            message,
            iteration,
        });
    }

    /// Publish `SessionStarted`, or end the run immediately as `Cancelled` when the token already fired.
    pub fn begin(&self, cancel: &Arc<dyn CancelToken>) -> bool {
        self.emit(AgentEvent::System(SystemEvent::SessionStarted {
            session_id: String::new(),
        }));
        if cancel.is_cancelled() {
            self.end(SessionEndReason::Cancelled);
            return false;
        }
        true
    }

    /// Publish `SessionEnded` with `reason`. The single spelling of
    /// a terminal event: every path that ends a run goes through
    /// here, so a consumer's `SessionStarted`/`SessionEnded`
    /// pairing does not depend on which code path produced it.
    pub fn end(&self, reason: SessionEndReason) {
        if self.ended.swap(true, Ordering::SeqCst) {
            return;
        }
        self.emit(AgentEvent::System(SystemEvent::SessionEnded { reason }));
    }

    /// End the run — as `Cancelled` when the token fired during
    /// it, `Completed` otherwise.
    pub fn finish(&self, cancel: &Arc<dyn CancelToken>) {
        self.end(if cancel.is_cancelled() {
            SessionEndReason::Cancelled
        } else {
            SessionEndReason::Completed
        });
    }

    /// End the run as a fatal `Error` carrying `message`.
    pub fn fail(&self, message: String) {
        self.end(SessionEndReason::Error(message));
    }
}

impl fmt::Debug for EventSink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventSink")
            .field("closed", &self.tx.is_closed())
            .field("log_writer", &self.log_writer.is_some())
            .finish()
    }
}
