//! What a run's events owe the outside world: durable append, the
//! broadcast fan-out, and the classification each drives on.
//! Method continuation of the inner state's `ControllerInner`.

use std::sync::atomic::Ordering;

use anyhow::{Context as _, Result};
use elysper::{
    harness::{AgentEvent, SystemEvent},
    plugin_host::PublishedEvent,
};
use tokio::sync::mpsc;

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
        let event_value = Self::encode_event(event)?;
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
        self.fan_out_live(event, outer_kind, system_kind, &event_value)
            .await;
        Ok(())
    }

    /// Encode-free fan-out tail every live event shares: broadcast to
    /// this node's subscribers, then the cluster bus frame.
    ///
    /// [`Self::persist_and_broadcast`] and
    /// [`Self::handle_plugin_event`] both end here so the two fan-outs
    /// cannot drift — a run-shaped event and a plugin-published one
    /// reach the live stream through the same code.
    async fn fan_out_live(
        &self,
        event: &AgentEvent,
        outer_kind: &'static str,
        system_kind: Option<&'static str>,
        event_value: &serde_json::Value,
    ) {
        self.broadcast_event(event, outer_kind, system_kind);
        self.publish_session_frame(None, event_value).await;
    }

    /// Handle one event a plugin published to the host: append the
    /// durable `plugin` row first, then broadcast the live-only ping.
    ///
    /// The row is the record and the ping is the notification — the
    /// same pairing every run event keeps, so a subscriber that reads
    /// the log after seeing the ping finds the row. Both carry the
    /// *bounded* kind and payload ([`SessionEvent::plugin`] caps
    /// them), so the live stream can never disagree with — or
    /// outgrow — the durable row.
    ///
    /// Failures are logged, never propagated: the drain is the
    /// controller loop's, and one bad event must not stop the ops.
    /// An event whose row cannot be made durable is not pinged
    /// either — a broadcast ahead of its row would promise a
    /// subscriber a record the log does not hold.
    pub(super) async fn handle_plugin_event(&self, event: PublishedEvent) {
        // The writer stamps `seq` / `ts` (the builder's zeros never
        // reach the log) and enforces the payload bound on every
        // append; the flush lands the row before the ping goes out.
        let row = elysper::session::SessionEvent::plugin(
            0,
            String::new(),
            event.kind,
            event.payload,
        );
        if let Err(e) = self.writer.append_event(&row).await {
            tracing::warn!(
                target: "elysper.session",
                session_id = %self.session_id,
                error = %e,
                "Failed to append the plugin event row"
            );
            return;
        }
        if let Err(e) = self.writer.flush().await {
            tracing::warn!(
                target: "elysper.session",
                session_id = %self.session_id,
                error = %e,
                "Failed to flush the plugin event row; the live ping is \
                 withheld until the record is durable"
            );
            return;
        }
        let (kind, payload) = match row {
            elysper::session::SessionEvent::Plugin {
                kind, payload, ..
            } => (kind, payload),
            // `SessionEvent::plugin` built this row above; nothing
            // else can be here.
            _ => return,
        };
        let ping = AgentEvent::System(SystemEvent::Plugin { kind, payload });
        // Not `tap_live`: a plugin event is not turn content, and the
        // live projection's `observe` ignores it by design.
        let outer_kind = ping.kind();
        let system_kind = Self::classify_system_kind(&ping);
        match Self::encode_event(&ping) {
            Ok(ping_value) => {
                tracing::debug!(
                    session_id = %self.session_id,
                    event_kind = outer_kind,
                    system_kind = system_kind.unwrap_or("-"),
                    subscribers = self.broadcaster.subscriber_count(),
                    "handle_plugin_event: broadcasting"
                );
                self.fan_out_live(&ping, outer_kind, system_kind, &ping_value)
                    .await;
            }
            Err(e) => tracing::warn!(
                target: "elysper.session",
                session_id = %self.session_id,
                error = %e,
                "Failed to encode the plugin event for broadcast"
            ),
        }
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
                target: "elysper.session",
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
    /// cluster frame shares. The byte count the frame path once
    /// needed is gone with it — nothing reads it any more, so no
    /// event pays a `to_vec` of its own tree just to feed a log
    /// line.
    pub(super) fn encode_event(
        event: &AgentEvent,
    ) -> Result<serde_json::Value> {
        serde_json::to_value(event).context("failed to serialize agent event")
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
        // The clone is skipped entirely when nobody listens: a
        // session with no live client must not pay an event clone
        // per persisted event on top of the serialization it
        // already paid.
        if self.broadcaster.subscriber_count() == 0 {
            tracing::debug!(
                target: "elysper.session",
                session_id = %self.session_id,
                event_kind = outer_kind,
                system_kind = system_kind.unwrap_or("-"),
                "broadcast_event: no subscribers, skipping the clone"
            );
            return;
        }
        tracing::debug!(
            target: "elysper.session",
            session_id = %self.session_id,
            event_kind = outer_kind,
            system_kind = system_kind.unwrap_or("-"),
            "broadcast_event: fanning the live event out"
        );
        if let Err(e) = self.broadcaster.send(event.clone()) {
            tracing::debug!(
                target: "elysper.session",
                session_id = %self.session_id,
                event_kind = outer_kind,
                system_kind = system_kind.unwrap_or("-"),
                error = %e,
                "No subscribers to broadcast event to"
            );
        }
    }
}

/// The server's [`PluginEventSink`]: a plugin's published events,
/// handed to one session's controller.
///
/// The bridge is an unbounded channel: `publish` is called from the
/// plugin host's reader thread and from the install path's settle —
/// both synchronous contexts that must not block, the trait's
/// contract — so the send is non-blocking and the controller's op
/// loop drains the other end, the task that owns the session's
/// writer, so the row the event becomes keeps the session's one
/// ordinal sequence. A send to a controller that has shut down fails
/// and the event is dropped: the session is over, so there is no log
/// to append to and no subscriber to ping.
pub(super) struct SessionPluginSink {
    pub(super) tx: mpsc::UnboundedSender<PublishedEvent>,
}

impl elysper::plugin_host::PluginEventSink for SessionPluginSink {
    fn publish(&self, event: PublishedEvent) {
        // Best effort by design: the live counterpart of the row is
        // documented as droppable when there is no one to serve it.
        let _ = self.tx.send(event);
    }
}
