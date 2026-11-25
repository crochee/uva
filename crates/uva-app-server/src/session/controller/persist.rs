//! What a run's events owe the outside world: durable append, the
//! broadcast fan-out, and the classification each drives on.
//! Method continuation of the inner state's `ControllerInner`.

use std::sync::atomic::Ordering;

use anyhow::Result;
use uva::harness::{AgentEvent, SystemEvent};

use super::inner::ControllerInner;
use crate::{event_stream::BroadcastEvent, state::UsageMetrics};

impl ControllerInner {
    /// The live funnel every run event passes through.
    ///
    /// `turn_id` names the turn the event belongs to — the id the
    /// turn's `parked_prompt` row carries, so a client correlates
    /// the live stream and the transcript with one id. `None` for a
    /// run no prompt started (a resumed turn, a compaction pass).
    pub(super) async fn persist_and_broadcast(
        &self,
        turn_id: Option<&str>,
        event: &AgentEvent,
    ) -> Result<()> {
        let outer_kind = event.kind();
        let system_kind = Self::classify_system_kind(event);
        let event_type = event.kind();
        tracing::debug!(
            session_id = %self.session_id,
            event_kind = outer_kind,
            system_kind = system_kind.unwrap_or("-"),
            event_type,
            subscribers = self.broadcaster.subscriber_count(),
            "persist_and_broadcast: entering"
        );
        Self::record_usage(&self.usage, event);
        // The log is the authority for the paused state (`session_state`
        // derives it); the controller state's private record only
        // carries the fact to this run's settle. It is cleared when
        // the next run starts.
        if matches!(event, AgentEvent::System(SystemEvent::Suspended(_))) {
            self.state.note_suspension();
        }
        // The live projection is what a `follow` client renders until
        // the durable row for this turn lands. It is fed from the same
        // funnel as the broadcast, so the two cannot disagree.
        self.tap_live(event);
        // The event stream is live-only: no `AgentEvent` is
        // durable any more, so there is no event-shaped row to
        // append here. The log's rows come from the writers —
        // the run's own (its structural rows and the harness's
        // model facts, flushed before the corresponding event is
        // emitted) and the op loop's out-of-band rows. This path
        // only broadcasts what already happened durably.
        self.fan_out_live(turn_id, event, outer_kind, system_kind)
            .await;
        Ok(())
    }

    /// Encode-free fan-out tail every live event shares: broadcast to
    /// this node's subscribers, then the cluster bus frame.
    async fn fan_out_live(
        &self,
        turn_id: Option<&str>,
        event: &AgentEvent,
        outer_kind: &'static str,
        system_kind: Option<&'static str>,
    ) {
        let item = BroadcastEvent::Run {
            turn_id: turn_id.map(str::to_string),
            event: Box::new(event.clone()),
        };
        self.broadcast_event(&item, outer_kind, system_kind);
        // The cluster frame carries the same item the local
        // subscribers get, so a remote node's live stream is the
        // local one — same events, same turn ids.
        self.publish_session_frame(None, &item).await;
    }

    /// Fan one live item out on the cluster bus (best-effort).
    ///
    /// A publish failure only costs a remote subscriber a live
    /// frame; its `from` cursor re-reads the row from disk. The
    /// event stream carries no durable rows any more — the log's
    /// rows are written by the writers, not by this tap — so the
    /// frame's cursor is always `None` and is never filtered
    /// against a resume cursor.
    async fn publish_session_frame(
        &self,
        cursor: Option<u64>,
        item: &BroadcastEvent,
    ) {
        // A single-node deployment never has another subscriber; the
        // local broadcaster already served every SSE client. Skipping
        // the publish keeps the default hot path free of an extra
        // payload clone + channel send per event.
        if !self.cluster.clustered {
            return;
        }
        let payload = match serde_json::to_value(item) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(
                    target: "uva.session",
                    session_id = %self.session_id,
                    error = %error,
                    "cluster: could not encode the live event"
                );
                return;
            }
        };
        let frame = crate::cluster::SessionFrame {
            origin: self.cluster.node_id.clone(),
            cursor,
            payload,
        };
        let key = crate::cluster::SessionKey::new(
            self.user_id.clone(),
            self.session_id.clone(),
        );
        if let Err(e) = self.cluster.bus.publish_session(&key, frame).await {
            tracing::debug!(
                target: "uva.session",
                session_id = %self.session_id,
                error = %e,
                "cluster: session frame publish failed"
            );
        }
    }

    /// Fold one event into the process-wide usage counters — the
    /// **only** write site for `/api/v1/chat/usage`.
    ///
    /// `Relaxed` is deliberate: the endpoint reports a monotone
    /// running total, so there is no other memory to order these
    /// against and a lost update race would still be a bounded,
    /// self-correcting skew on a counter nobody reads causally.
    fn record_usage(usage: &UsageMetrics, event: &AgentEvent) {
        match event {
            AgentEvent::System(SystemEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            }) => {
                usage
                    .tokens_in
                    .fetch_add(*input_tokens as u64, Ordering::Relaxed);
                usage
                    .tokens_out
                    .fetch_add(*output_tokens as u64, Ordering::Relaxed);
            }
            // Terminal event of every run, whatever the reason: a
            // cancelled or errored run consumed a turn too.
            AgentEvent::System(SystemEvent::SessionEnded { .. }) => {
                usage.turns.fetch_add(1, Ordering::Relaxed);
            }
            _ => {}
        }
    }

    /// Publish the turn's end on the live channel.
    ///
    /// The turn-scoped subscriber waits for exactly this frame: it
    /// is the signal that the turn's rows are on disk and no further
    /// event of that turn will arrive.
    pub(super) async fn broadcast_turn_ended(
        &self,
        turn_id: Option<&str>,
        reason: &str,
    ) {
        let item = BroadcastEvent::TurnEnded {
            turn_id: turn_id.map(str::to_string),
            reason: reason.to_string(),
        };
        self.broadcast_event(&item, "TurnEnded", None);
        self.publish_session_frame(None, &item).await;
    }

    /// Resolved kind for the structural log line, if `event`
    /// is a `System` event; `None` otherwise.
    pub(super) fn classify_system_kind(
        event: &AgentEvent,
    ) -> Option<&'static str> {
        match event {
            AgentEvent::System(sys) => Some(sys.kind()),
            _ => None,
        }
    }

    /// Fan the item out to the live SSE/WebSocket
    /// subscribers. A send with no subscribers is a debug
    /// log, not an error — the durable rows a subscriber may
    /// resume from are the writers', flushed before the
    /// corresponding event reaches this fan-out.
    pub(super) fn broadcast_event(
        &self,
        item: &BroadcastEvent,
        outer_kind: &'static str,
        system_kind: Option<&'static str>,
    ) {
        // The clone is skipped entirely when nobody listens: a
        // session with no live client must not pay an event clone
        // per persisted event on top of the serialization it
        // already paid.
        if self.broadcaster.subscriber_count() == 0 {
            tracing::debug!(
                target: "uva.session",
                session_id = %self.session_id,
                event_kind = outer_kind,
                system_kind = system_kind.unwrap_or("-"),
                "broadcast_event: no subscribers, skipping the clone"
            );
            return;
        }
        tracing::debug!(
            target: "uva.session",
            session_id = %self.session_id,
            event_kind = outer_kind,
            system_kind = system_kind.unwrap_or("-"),
            "broadcast_event: fanning the live event out"
        );
        if let Err(e) = self.broadcaster.send(item.clone()) {
            tracing::debug!(
                target: "uva.session",
                session_id = %self.session_id,
                event_kind = outer_kind,
                system_kind = system_kind.unwrap_or("-"),
                error = %e,
                "No subscribers to broadcast event to"
            );
        }
    }
}
