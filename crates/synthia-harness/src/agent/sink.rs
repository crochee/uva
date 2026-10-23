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

use crate::{
    events::{AgentEvent, SessionEndReason, SystemEvent, WarningKind},
    log::{LogWriter, StampedRow},
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
    /// The fallback channel for a host that drives the log itself.
    typed_sink: Option<synthia_session::TypedEventSink>,
    /// Shared-across-clones flag enforcing one `SessionEnded` per run.
    ended: Arc<AtomicBool>,
}

impl EventSink {
    /// Wrap the run's sender, its log writer, and its typed sink.
    #[must_use]
    pub fn new(
        tx: Arc<mpsc::UnboundedSender<AgentEvent>>,
        log_writer: Option<Arc<LogWriter>>,
        typed_sink: Option<synthia_session::TypedEventSink>,
    ) -> Self {
        Self {
            tx,
            log_writer,
            typed_sink,
            ended: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Whether a typed sink is wired.
    pub(crate) fn has_typed(&self) -> bool {
        self.log_writer.is_some() || self.typed_sink.is_some()
    }

    /// Publish one event.
    pub fn emit(&self, event: AgentEvent) {
        let _ = self.tx.unbounded_send(event);
    }

    /// Publish one structural event to the run's log (iteration /
    /// step / usage boundaries).
    ///
    /// With a log writer wired, the row is stamped and buffered
    /// here; the loop flushes the buffer at the step boundary.
    /// Without one, the row goes to the typed sink the host wired.
    pub fn emit_typed(&self, event: synthia_session::SessionEvent) {
        if let Some(writer) = &self.log_writer {
            let seq = writer.stamp();
            let value = writer.row_value(&event, seq);
            writer.append_nowait(StampedRow { seq, value });
            return;
        }
        if let Some(sink) = &self.typed_sink {
            sink.record(synthia_session::TypedEventRecord::new(event));
        }
    }

    /// Publish one system event.
    pub fn system(&self, event: SystemEvent) {
        self.emit(AgentEvent::System(event));
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
            .field("typed", &self.typed_sink.is_some())
            .finish()
    }
}
