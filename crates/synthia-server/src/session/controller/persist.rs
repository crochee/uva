//! What a run's events owe the outside world: durable append, the
//! broadcast fan-out, and the classification each drives on.
//! Method continuation of the inner state's `ControllerInner`.

use std::sync::atomic::Ordering;

use anyhow::{Context as _, Result};
use synthia::harness::{AgentEvent, SystemEvent};

use super::{inner::ControllerInner, run_log::RunLog};
use crate::state::UsageMetrics;

impl ControllerInner {
    pub(super) async fn persist_and_broadcast(
        &self,
        event: &AgentEvent,
        log: &mut RunLog,
    ) -> Result<()> {
        // A wrapped event is a delegated sub-agent's trace. It never
        // joins the parent's durable log or SSE: the router gives
        // the child its own session (its own log, its own live
        // stream), and the parent keeps only the lightweight
        // enter/exit markers the router writes through `log`.
        if let AgentEvent::Agent(meta, inner) = event {
            Self::record_usage_tree(&self.usage, inner);
            if let Some(router) = self.subagent_router.as_ref() {
                router
                    .route(&self.user_id, &self.session_id, meta, inner, log)
                    .await;
            }
            return Ok(());
        }
        let outer_kind = event.kind();
        let system_kind = Self::classify_system_kind(event);
        let (byte_size, event_value) = Self::encode_event(event)?;
        let event_type = event.kind();
        tracing::debug!(
            session_id = %self.session_id,
            event_kind = outer_kind,
            system_kind = system_kind.unwrap_or("-"),
            event_type,
            payload_bytes = byte_size,
            subscribers = self.broadcaster.subscriber_count(),
            "persist_and_broadcast: entering"
        );
        Self::record_usage(&self.usage, event);
        // The log is the authority for the paused state (`session_state`
        // derives it); this flag only spares the list view a second read
        // of a log it already holds. It is cleared when the next run
        // starts.
        if matches!(event, AgentEvent::System(SystemEvent::Suspended(_))) {
            self.suspended.store(true, Ordering::SeqCst);
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
        self.broadcast_event(event, outer_kind, system_kind);
        self.publish_session_frame(None, &event_value).await;
        Ok(())
    }

    /// Fan one run event out on the cluster bus (best-effort).
    ///
    /// A publish failure only costs a remote subscriber a live
    /// frame; its `from` cursor re-reads the row from disk. The
    /// event stream carries no durable rows any more — the log's
    /// rows are written by the writers, not this tap — so the
    /// frame's cursor is always `None` and is never filtered
    /// against a resume cursor.
    async fn publish_session_frame(
        &self,
        cursor: Option<u64>,
        payload: &serde_json::Value,
    ) {
        // A single-node deployment never has another subscriber; the
        // local broadcaster already served every SSE client. Skipping
        // the publish keeps the default hot path free of an extra
        // payload clone + channel send per event.
        if !self.cluster.clustered {
            return;
        }
        let frame = crate::cluster::SessionFrame {
            origin: self.cluster.node_id.clone(),
            cursor,
            payload: payload.clone(),
        };
        let key = crate::cluster::SessionKey::new(
            self.user_id.clone(),
            self.session_id.clone(),
        );
        if let Err(e) = self.cluster.bus.publish_session(&key, frame).await {
            tracing::debug!(
                target: "synthia.session",
                session_id = %self.session_id,
                error = %e,
                "cluster: session frame publish failed"
            );
        }
    }

    /// [`Self::record_usage`] over a whole sub-agent trace: the
    /// child's usage and turn totals are real consumption of the
    /// deployment's tokens, so they belong in `/api/v1/chat/usage`
    /// even though the child's events now land in their own session.
    fn record_usage_tree(usage: &UsageMetrics, event: &AgentEvent) {
        Self::record_usage(usage, event);
        if let AgentEvent::Agent(_, inner) = event {
            Self::record_usage_tree(usage, inner);
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

    /// Single serialization pass: `to_value` builds the tree the
    /// sink append and the cluster frame share, and the byte
    /// count comes from formatting that same tree — the previous
    /// shape ran `to_vec` + `from_slice`, paying a full
    /// deserialization half-trip per event.
    pub(super) fn encode_event(
        event: &AgentEvent,
    ) -> Result<(usize, serde_json::Value)> {
        let event_value: serde_json::Value = serde_json::to_value(event)
            .context("failed to serialize agent event")?;
        let byte_size = serde_json::to_vec(&event_value)
            .context("failed to measure event payload")?
            .len();
        Ok((byte_size, event_value))
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

    /// Fan the event out to the live SSE/WebSocket
    /// subscribers. A send with no subscribers is a debug
    /// log, not an error — the durable rows a subscriber may
    /// resume from are the writers', flushed before the
    /// corresponding event reaches this fan-out.
    pub(super) fn broadcast_event(
        &self,
        event: &AgentEvent,
        outer_kind: &'static str,
        system_kind: Option<&'static str>,
    ) {
        tracing::debug!(
            target: "synthia.session",
            session_id = %self.session_id,
            event_kind = outer_kind,
            system_kind = system_kind.unwrap_or("-"),
            "persist_and_broadcast: appended to event store; broadcasting"
        );
        if let Err(e) = self.broadcaster.send(event.clone()) {
            tracing::debug!(
                target: "synthia.session",
                session_id = %self.session_id,
                event_kind = outer_kind,
                system_kind = system_kind.unwrap_or("-"),
                error = %e,
                "No subscribers to broadcast event to"
            );
        }
    }
}
