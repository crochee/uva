//! Shared event broadcaster for a session's live channel.
//!
//! Provides a shared broadcast channel that allows multiple
//! subscribers to receive a session's live items from a single
//! source.
//!
//! Every send / subscribe / drop on this broadcaster is
//! `tracing`-instrumented so the lifetime of a live stream can be
//! reconstructed from log output. The logs are intentionally
//! structured (`event.kind`, `subscriber_count`, `session_id`-like
//! fields) so they can be filtered by observability tooling without
//! free-form regex matching.
//!
//! # What the channel carries
//!
//! [`BroadcastEvent`] — a run's event, or the turn's end. Every item
//! names the turn it belongs to, so a subscriber that asked for one
//! turn's stream can tell that turn's events from the tail of an
//! earlier one. The turn id is the *same* id the turn's durable
//! `parked_prompt` row carries, so a client correlates the live
//! stream and the transcript with one id.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use uva::harness::AgentEvent;

/// One item on a session's live channel.
///
/// The channel is the app-server's own wire shape: it crosses the
/// cluster bus between nodes, so it is serialised — and the row
/// payloads that travel on the same bus are not, which is what tells
/// the two apart on the receiving side.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum BroadcastEvent {
    /// A run's event.
    Run {
        /// The turn the event belongs to, when the run had one.
        ///
        /// `None` for a run that no prompt started — a resumed
        /// turn, a compaction pass — because those turns have no
        /// parked prompt to name.
        turn_id: Option<String>,
        /// The event itself.
        event: AgentEvent,
    },
    /// The turn's run ended.
    TurnEnded {
        /// The turn that ended, when the run had one.
        turn_id: Option<String>,
        /// Why the turn ended (`completed`, `cancelled`, …).
        reason: String,
    },
}

impl BroadcastEvent {
    /// The turn this item belongs to.
    #[must_use]
    pub fn turn_id(&self) -> Option<&str> {
        match self {
            Self::Run { turn_id, .. } | Self::TurnEnded { turn_id, .. } => {
                turn_id.as_deref()
            }
        }
    }
}

/// Default capacity for the event broadcast channel.
///
/// Sized to absorb a full LLM streaming burst for a typical agent run
/// (≈ 200 model chunks + system events). Slow SSE subscribers cause
/// the broadcaster to drop the oldest unread events (see
/// [`tokio::sync::broadcast`] semantics); the gap is logged at `warn`.
const EVENT_CHANNEL_CAPACITY: usize = 1024;

/// A shared event broadcaster that allows multiple subscribers to receive
/// a session's live items from a single source.
#[derive(Clone)]
pub struct EventBroadcaster {
    tx: Arc<broadcast::Sender<BroadcastEvent>>,
    /// Optional logical id attached to the broadcaster so logs from
    /// a controller's `persist_and_broadcast` can be correlated with
    /// the broadcaster's own send logs.
    label: Arc<String>,
}

impl EventBroadcaster {
    /// Creates a new `EventBroadcaster` with a fresh broadcast channel.
    pub fn new() -> Self {
        Self::with_label("default")
    }

    /// Creates a new `EventBroadcaster` tagged with a logical label
    /// (typically the session_id) so log lines from multiple
    /// broadcasters in a single process can be disambiguated.
    pub fn with_label(label: impl Into<String>) -> Self {
        let label = label.into();
        tracing::debug!(
            target: "uva.event_stream",
            broadcaster = %label,
            capacity = EVENT_CHANNEL_CAPACITY,
            "Creating new EventBroadcaster"
        );
        let (tx, _rx) = broadcast::channel(EVENT_CHANNEL_CAPACITY);
        Self {
            tx: Arc::new(tx),
            label: Arc::new(label),
        }
    }

    /// Returns the broadcaster's logical label (e.g. session_id).
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Subscribes to the live channel, returning a new receiver.
    ///
    /// Each subscription is logged at `debug` with the new total
    /// subscriber count so the open / close pattern of live clients
    /// is observable.
    pub fn subscribe(&self) -> broadcast::Receiver<BroadcastEvent> {
        let rx = self.tx.subscribe();
        let total = self.tx.receiver_count();
        tracing::debug!(
            target: "uva.event_stream",
            broadcaster = %self.label,
            subscriber_count = total,
            "New subscriber attached to EventBroadcaster"
        );
        rx
    }

    /// Sends one item to all subscribers.
    ///
    /// Emits a structured `tracing::debug!` line carrying the
    /// event's kind, an inner `SystemEvent::kind` when applicable,
    /// the payload byte size, and the resulting subscriber count.
    /// Errors are logged at `warn` so a dropped item is never
    /// silent.
    pub fn send(
        &self,
        item: BroadcastEvent,
    ) -> Result<usize, Box<broadcast::error::SendError<BroadcastEvent>>> {
        // Use the cheap O(payload) size helper rather than
        // re-serializing here — the controller path already has
        // a serialized copy for the disk write, and a second
        // serialize just for a log line is wasted work on every
        // streaming chunk.
        let (byte_size, outer_kind, inner_kind) = match &item {
            BroadcastEvent::Run { event, .. } => (
                event.size_hint(),
                event.kind(),
                match event {
                    AgentEvent::System(sys) => Some(sys.kind()),
                    _ => None,
                },
            ),
            BroadcastEvent::TurnEnded { .. } => (0, "TurnEnded", None),
        };
        match self.tx.send(item) {
            Ok(receivers) => {
                tracing::debug!(
                    target: "uva.event_stream",
                    broadcaster = %self.label,
                    event_kind = outer_kind,
                    system_kind = inner_kind.unwrap_or("-"),
                    payload_bytes = byte_size,
                    receivers,
                    "Broadcast session event"
                );
                Ok(receivers)
            }
            Err(e) => {
                tracing::warn!(
                    target: "uva.event_stream",
                    broadcaster = %self.label,
                    event_kind = outer_kind,
                    system_kind = inner_kind.unwrap_or("-"),
                    error = %e,
                    "Failed to broadcast session event (no subscribers)"
                );
                Err(Box::new(e))
            }
        }
    }

    /// Returns the number of active subscribers.
    ///
    /// Logs at `trace` so volume-sensitive callers (e.g. the
    /// idle-deadline loop) do not flood the log on every poll.
    pub fn subscriber_count(&self) -> usize {
        let n = self.tx.receiver_count();
        tracing::trace!(
            target: "uva.event_stream",
            broadcaster = %self.label,
            subscriber_count = n,
            "EventBroadcaster subscriber_count queried"
        );
        n
    }
}

impl Default for EventBroadcaster {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use uva::harness::SystemEvent;

    use super::*;

    /// The channel's item for a run's event, with no turn.
    fn run(event: AgentEvent) -> BroadcastEvent {
        BroadcastEvent::Run {
            turn_id: None,
            event,
        }
    }

    #[test]
    fn test_broadcaster_creation() {
        let broadcaster = EventBroadcaster::new();
        assert_eq!(broadcaster.subscriber_count(), 0);
    }

    #[tokio::test]
    async fn test_subscribe_and_send() {
        let broadcaster = EventBroadcaster::new();
        let mut rx = broadcaster.subscribe();

        let event =
            AgentEvent::System(uva::harness::SystemEvent::SessionStarted {
                session_id: "test-1".to_string(),
            });
        let sent = broadcaster.send(run(event)).unwrap();
        assert_eq!(sent, 1);

        let received = rx.recv().await.unwrap();
        assert!(matches!(
            received,
            BroadcastEvent::Run {
                event: AgentEvent::System(
                    uva::harness::SystemEvent::SessionStarted { .. }
                ),
                ..
            }
        ));
    }

    #[test]
    fn test_multiple_subscribers() {
        let broadcaster = EventBroadcaster::new();
        let _rx1 = broadcaster.subscribe();
        assert_eq!(broadcaster.subscriber_count(), 1);

        let _rx2 = broadcaster.subscribe();
        assert_eq!(broadcaster.subscriber_count(), 2);
    }

    #[test]
    fn test_clone_shares_channel() {
        let broadcaster = EventBroadcaster::new();
        let cloned = broadcaster.clone();

        let _rx = broadcaster.subscribe();
        assert_eq!(cloned.subscriber_count(), 1);
    }

    #[test]
    fn test_with_label_sets_log_label() {
        let broadcaster = EventBroadcaster::with_label("alice/s1");
        assert_eq!(broadcaster.label(), "alice/s1");
    }

    #[test]
    fn test_kind_label_on_send_path() {
        // Smoke check that AgentEvent::kind() returns the expected
        // string tokens for the two variants we log specifically
        // (Model + System).
        let text = AgentEvent::text_delta("hi");
        assert_eq!(text.kind(), "Model");

        let sys = AgentEvent::System(SystemEvent::SessionEnded {
            reason: uva::harness::SessionEndReason::Completed,
        });
        assert_eq!(sys.kind(), "System");
        assert!(matches!(
            sys,
            AgentEvent::System(_)
                | AgentEvent::Model(_)
                | AgentEvent::ModelDone(_)
        ));
    }

    /// Direct test for the broadcast contract that the
    /// SSE consumer relies on: when the receiver falls behind
    /// and the channel overwrites a ring-buffer of pending
    /// messages, `recv()` returns
    /// `RecvError::Lagged(skipped)`. The executor's fix
    /// requires `Lagged` to be a recoverable signal (the
    /// receiver must `continue`, not break), and
    /// `RecvError::Closed` to be the only terminal
    /// condition.
    #[tokio::test]
    async fn test_recv_error_lagged_reports_skipped_count() {
        let broadcaster = EventBroadcaster::new();
        let mut rx = broadcaster.subscribe();

        // Push past `EVENT_CHANNEL_CAPACITY` events WITHOUT
        // consuming the receiver. The next `recv()` MUST
        // surface a `Lagged` error.
        let event = AgentEvent::System(SystemEvent::SessionStarted {
            session_id: "lag-test".to_string(),
        });
        for _ in 0..(super::EVENT_CHANNEL_CAPACITY + 8) {
            // `send` errors once the channel is full *for
            // new senders*, but a single live receiver plus
            // active producer keeps writing into the
            // ring buffer; the receiver sees them as
            // Lagged.
            let _ = broadcaster.send(run(event.clone()));
        }

        let result = rx.recv().await;
        match result {
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                assert!(
                    skipped > 0,
                    "Lagged must report at least one skipped event, got {skipped}"
                );
            }
            other => panic!(
                "expected Err(Lagged(_)) when receiver falls behind, got {other:?}"
            ),
        }

        // After Lagged, the receiver must still receive
        // subsequent messages (i.e. it's recoverable).
        // This is the contract the executor relies on.
        let next = rx.recv().await;
        assert!(
            next.is_ok(),
            "receiver must remain connected after Lagged; got {next:?}"
        );
    }

    /// Late subscribers (those that join AFTER events have
    /// already been broadcast) MUST NOT see the historical
    /// events on the wire — the broadcast channel is a
    /// live fan-out, not a queue of pending messages.
    /// `tokio::sync::broadcast` documents this explicitly:
    /// "Receivers only see messages sent after they
    /// subscribed."
    ///
    /// This contract is what allows a fresh SSE subscriber
    /// to safely short-circuit to the persisted session
    /// snapshot when a brand-new client attaches after the
    /// active execution has cleared: the client MUST NOT
    /// see half-streamed in-flight events because the
    /// broadcast channel no longer retains them. If you
    /// ever need replay, build it from the durable session
    /// event store, not the broadcast channel.
    #[tokio::test]
    async fn test_late_subscriber_does_not_see_past_events() {
        let broadcaster = EventBroadcaster::new();
        let event = AgentEvent::System(SystemEvent::SessionStarted {
            session_id: "historic".to_string(),
        });

        // Send 5 events BEFORE any subscriber attaches.
        // `tokio::sync::broadcast::Sender::send` returns
        // `Err(SendError(value))` when there are no
        // receivers — which is exactly our setup. The
        // broadcaster layer wraps and re-emits that as
        // `Box<SendError<BroadcastEvent>>`, so we discard
        // it. The important point is: the channel does
        // not buffer undelivered events for future
        // subscribers.
        for _ in 0..5 {
            let _ = broadcaster.send(run(event.clone()));
        }

        // Late subscriber attaches AFTER the events were
        // broadcast. recv() MUST NOT see any of those
        // historic events.
        let mut rx = broadcaster.subscribe();

        // Race the recv against a short sleep so the test
        // does not hang if the channel does happen to
        // surface a phantom event.
        let outcome = tokio::time::timeout(
            std::time::Duration::from_millis(50),
            rx.recv(),
        )
        .await;
        match outcome {
            // The only acceptable outcome: timeout fires
            // because no event was sent after subscribe().
            Err(_elapsed) => {}
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                // Also acceptable: if the late subscriber
                // was attached while the ring buffer was
                // already full, the next recv can return
                // Lagged(0) and that's a recoverable
                // signal too. We treat it as "did not
                // receive a historical event".
            }
            Ok(Ok(event)) => panic!(
                "late subscriber must NOT see events sent before subscribe(); got {event:?}"
            ),
            Ok(Err(other)) => {
                panic!("unexpected recv error for late subscriber: {other:?}")
            }
        }
    }
}
