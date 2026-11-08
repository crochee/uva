//! The run's event publication seam: [`EventSink`] is the single sink type every part of the harness publishes through, so an event reaches the consumer the same way no matter which layer emitted it.
use std::{
    fmt,
    pin::Pin,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    task::{Context, Poll},
};

use elysper_core::CancelToken;
use elysper_provider::{Content, ContentPart, Message, Role, ToolDefinition};
use elysper_session::{SessionError, SessionEvent, SessionLog};
use futures::{Stream, channel::mpsc};

use crate::{
    agent::{caps::AgentCaps, re_act::projection::tool_names_hash},
    events::{AgentEvent, SessionEndReason, SystemEvent, WarningKind},
};

/// How many live events a run buffers before it starts dropping them.
pub const DEFAULT_EVENT_CAPACITY: usize = 1024;

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
    log_writer: Option<Arc<dyn SessionLog>>,
    // Shared-across-clones flag enforcing one `SessionEnded` per run.
    ended: Arc<AtomicBool>,
    // How many live events are buffered but not yet consumed.
    live: Arc<AtomicUsize>,
    // The soft cap: above it, live events are dropped and counted.
    cap: usize,
    // Live events dropped because the buffer was full.
    dropped: Arc<AtomicUsize>,
}

/// The run's event stream.
///
/// A thin wrapper over the channel's receiver that accounts for the
/// live-event cap: every item taken out frees a slot, so a slow
/// consumer applies backpressure to the cap rather than to memory.
pub struct EventStream {
    rx: mpsc::UnboundedReceiver<AgentEvent>,
    live: Arc<AtomicUsize>,
}

impl EventStream {
    /// Wrap the channel's receiver.
    #[must_use]
    pub fn new(
        rx: mpsc::UnboundedReceiver<AgentEvent>,
        live: Arc<AtomicUsize>,
    ) -> Self {
        Self { rx, live }
    }
}

impl Stream for EventStream {
    type Item = AgentEvent;

    fn poll_next(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match Pin::new(&mut this.rx).poll_next(cx) {
            Poll::Ready(Some(item)) => {
                // Saturating: a terminal event bypasses the counter.
                let _ = this.live.try_update(
                    Ordering::Relaxed,
                    Ordering::Relaxed,
                    |n| Some(n.saturating_sub(1)),
                );
                Poll::Ready(Some(item))
            }
            other => other,
        }
    }
}

impl fmt::Debug for EventStream {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EventStream")
            .field("live", &self.live.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl EventSink {
    /// Wrap the run's sender and its log writer.
    #[must_use]
    pub fn new(
        tx: Arc<mpsc::UnboundedSender<AgentEvent>>,
        log_writer: Option<Arc<dyn SessionLog>>,
    ) -> Self {
        Self::with_capacity(tx, log_writer, DEFAULT_EVENT_CAPACITY)
    }

    /// Wrap the sender, the writer and a live-event cap.
    #[must_use]
    pub fn with_capacity(
        tx: Arc<mpsc::UnboundedSender<AgentEvent>>,
        log_writer: Option<Arc<dyn SessionLog>>,
        cap: usize,
    ) -> Self {
        Self {
            tx,
            log_writer,
            ended: Arc::new(AtomicBool::new(false)),
            live: Arc::new(AtomicUsize::new(0)),
            cap,
            dropped: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// The live-event counter, for the stream this sink feeds.
    #[must_use]
    pub fn live_counter(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.live)
    }

    /// How many live events were dropped under backpressure.
    #[must_use]
    pub fn dropped(&self) -> usize {
        self.dropped.load(Ordering::Relaxed)
    }

    /// Whether a log writer is wired.
    pub(crate) fn has_writer(&self) -> bool {
        self.log_writer.is_some()
    }

    /// Publish one live event.
    ///
    /// Live events may be dropped: above the sink's cap the event is
    /// counted and discarded. The durable log is the record — an
    /// event a consumer had to see is a row, not a notification.
    pub fn emit(&self, event: AgentEvent) {
        if self.cap > 0 && self.live.load(Ordering::Relaxed) >= self.cap {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        self.live.fetch_add(1, Ordering::Relaxed);
        if self.tx.unbounded_send(event).is_err() {
            let _ = self.live.try_update(
                Ordering::Relaxed,
                Ordering::Relaxed,
                |n| Some(n.saturating_sub(1)),
            );
        }
    }

    // Publish one event ahead of the cap. Only terminal events use
    // this: a consumer must see the run end.
    fn emit_unbounded(&self, event: AgentEvent) {
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
    pub fn emit_typed(&self, event: elysper_session::SessionEvent) {
        let _ = self.append_typed(event);
    }

    /// Append one typed row and return the ordinal it was given (`0`
    /// without a writer). The sync barrier path needs the ordinal to
    /// build a resume token from the log tail.
    pub(crate) fn append_typed(
        &self,
        event: elysper_session::SessionEvent,
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

    /// Append one durable plugin row: a `SessionEvent::Plugin` with the
    /// plugin's own kind and payload.
    ///
    /// The payload is bounded by the session crate on write, so an
    /// oversized payload is truncated there rather than rejected. A
    /// run with no writer has no log: the row is published as the live
    /// `SystemEvent::Plugin` notification and `0` is returned.
    ///
    /// # Errors
    ///
    /// Returns the writer's error when the row cannot be queued.
    pub async fn append_plugin(
        &self,
        kind: &str,
        payload: serde_json::Value,
    ) -> Result<u64, SessionError> {
        let Some(writer) = self.log_writer.as_ref() else {
            self.system(SystemEvent::Plugin {
                kind: kind.to_string(),
                payload,
            });
            return Ok(0);
        };
        self.system(SystemEvent::Plugin {
            kind: kind.to_string(),
            payload: payload.clone(),
        });
        writer
            .append_event(&SessionEvent::plugin(
                0,
                String::new(),
                kind,
                payload,
            ))
            .await
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
    /// its model's facts into a log that names no envelope. The row
    /// names the provider id from the provider's own
    /// [`elysper_provider::ModelConfig::provider`], falling back to
    /// its `name()` only when the id is empty.
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
        let config = caps.model.provider.model_config();
        // The provider id the deployment configured; the handle's
        // `name()` is the *model* for an OpenAI-compatible
        // implementation, so a row built from it would name the model
        // twice and the provider would be unrecoverable from the log.
        let provider = if config.provider.is_empty() {
            caps.model.provider.name()
        } else {
            config.provider.as_str()
        };
        writer.append_request_header(
            provider,
            &config.name,
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
    /// Returns the ordinal the row was given (`0` without a writer or
    /// when there is nothing to write): the loop records it beside the
    /// message it commits, so a later compaction cites the row rather
    /// than guessing it.
    ///
    /// Without a writer the row goes nowhere — a run with no writer
    /// has no log.
    pub fn write_assistant_turn(&self, parts: &[ContentPart]) -> u64 {
        let Some(writer) = &self.log_writer else {
            return 0;
        };
        if parts.is_empty() {
            return 0;
        }
        let message =
            Message::new(Role::Assistant, Content::Multi(parts.to_vec()));
        writer.append_event_nowait(&SessionEvent::AssistantMessage {
            seq: 0,
            ts: String::new(),
            data: serde_json::to_value(message)
                .expect("assistant message serializes"),
            surface_op: None,
        })
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
        let dropped = self.dropped.swap(0, Ordering::Relaxed);
        if dropped > 0 {
            self.emit_unbounded(AgentEvent::System(
                SystemEvent::EventsDropped { count: dropped },
            ));
        }
        self.emit_unbounded(AgentEvent::System(SystemEvent::SessionEnded {
            reason,
        }));
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
