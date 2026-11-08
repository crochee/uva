//! The plugin-event drain's place in the controller loop.
//!
//! A continuously non-empty plugin queue must not stop the loop from
//! dispatching ops, observing run completion, or keeping the lease
//! tick alive — and the queue must still drain while op traffic is
//! continuous. The loop's shape (a bounded pre-select drain, plugin
//! arm last in the biased select) is what carries both halves, and
//! the contract is asserted from the outside, through a real
//! controller.

use std::{sync::Arc, time::Duration};

use elysper::harness::{SessionEndReason, SystemEvent};
use parking_lot::Mutex;

use super::{
    super::{AgentEvent, RunStreamFactory, SessionOp, SessionState},
    support::{
        RecordedCall,
        VecFactory,
        make_manager_and_controller,
        wait_for_runs,
    },
};

/// How many events the test stuffs the queue with: far past one drain
/// batch, so the queue stays non-empty across the whole op below.
const FLOOD: usize = 200;

/// An op submitted while the plugin queue holds more events than the
/// loop can drain in many batches must still be dispatched and its
/// run completed — ops, run completion and the lease tick outrank the
/// drain — and the queue must still catch up afterwards, so neither
/// side starves the other.
#[tokio::test]
async fn ops_dispatch_and_complete_while_the_plugin_queue_is_stuffed() {
    let events = vec![AgentEvent::System(SystemEvent::SessionEnded {
        reason: SessionEndReason::Completed,
    })];
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn RunStreamFactory> =
        Arc::new(VecFactory::new(events, Arc::clone(&calls)));
    let (controller, manager, _temp) =
        make_manager_and_controller(Duration::from_secs(60), factory).await;

    // Stuff the queue through the public seam — exactly what a
    // flooding plugin's sink does — before the op exists, so the loop
    // is already behind when the op arrives.
    let sink = controller
        .plugin_event_sink()
        .expect("the owning controller exposes a session-bound sink");
    for i in 0..FLOOD {
        sink.publish(elysper::plugin_host::PublishedEvent {
            kind: format!("flood-{i}"),
            payload: serde_json::json!({ "i": i }),
        });
    }

    // The op must be dispatched anyway: the drain is bounded, so the
    // loop returns to the select — and the op arm outranks the
    // plugin arm — long before the queue is empty.
    controller
        .submit(SessionOp::Prompt {
            content: "while flooded".to_string(),
            priority: 1,
        })
        .await
        .unwrap();
    wait_for_runs(&calls, 1).await;

    // And the run's completion must still be observed: the state
    // returns to `Idle` while the queue is still draining.
    tokio::time::timeout(Duration::from_secs(5), async {
        while controller.state() != SessionState::Idle {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the run must complete while the plugin queue is busy");

    // The drain must catch up: every flooded event becomes exactly
    // one durable `plugin` row.
    let sink = manager.sink("alice", "s1");
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let rows = sink.read().await.expect("the session log reads");
            if rows.iter().filter(|row| row["type"] == "plugin").count()
                == FLOOD
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the plugin queue must drain to durable rows");
}

/// A sink whose batch write sleeps: the "slow disk" the plugin
/// drain's fsyncs stand in for.
struct SlowSink {
    inner: elysper::session::in_memory::InMemorySessionSink,
    delay: Duration,
}

impl SlowSink {
    fn new(id: &str, delay: Duration) -> Self {
        Self {
            inner: elysper::session::in_memory::InMemorySessionSink::new(id),
            delay,
        }
    }
}

#[async_trait::async_trait]
impl elysper::session::SessionSink for SlowSink {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn alloc_seq(&self) -> u64 {
        self.inner.alloc_seq()
    }

    async fn append(
        &self,
        event: &serde_json::Value,
    ) -> Result<(), elysper::session::SessionError> {
        tokio::time::sleep(self.delay).await;
        self.inner.append(event).await
    }

    async fn append_many(
        &self,
        events: &[serde_json::Value],
    ) -> Result<(), elysper::session::SessionError> {
        tokio::time::sleep(self.delay).await;
        self.inner.append_many(events).await
    }

    async fn read(
        &self,
    ) -> Result<Vec<serde_json::Value>, elysper::session::SessionError> {
        self.inner.read().await
    }

    async fn read_from(
        &self,
        from: u64,
    ) -> Result<Vec<serde_json::Value>, elysper::session::SessionError> {
        self.inner.read_from(from).await
    }

    async fn snapshot(
        &self,
    ) -> Result<elysper::session::SessionSnapshot, elysper::session::SessionError>
    {
        self.inner.snapshot().await
    }

    async fn close(
        &self,
        reason: elysper::session::SessionEndReason,
    ) -> Result<(), elysper::session::SessionError> {
        self.inner.close(reason).await
    }
}

/// A flooding plugin must not cost the session its lease.
///
/// The drain's fsyncs used to sit between the renewal deadline and
/// the loop's `select!`: the loop only learned that a renewal was due
/// when it reached the select, and a batch of eight fsyncs on a slow
/// disk is a real distance from `lease_ttl / 3`. The lease then
/// expired mid-flood — and an expired lease hands the session to
/// another node while this one still runs it.
///
/// The deadline is read *before* the batch (and between its events),
/// so the renewal is at most one event late.
#[tokio::test]
async fn a_flooding_plugin_does_not_cost_the_session_its_lease() {
    use crate::cluster::{
        ClusterRuntime,
        LocalBus,
        SessionBus,
        SessionKey,
        session_lease,
    };

    // A short TTL and a slow disk: one drain batch (eight events)
    // must not be able to outlast the whole lease, while a single
    // event's write must stay well inside it — the margin is what
    // keeps the test from flaking on a loaded machine.
    const LEASE_TTL: Duration = Duration::from_millis(600);
    const DISK: Duration = Duration::from_millis(100);

    let bus = Arc::new(LocalBus::new());
    let cluster = Arc::new(ClusterRuntime {
        bus: bus.clone(),
        node_id: "node-a".to_string(),
        namespace: "elysper".to_string(),
        lease_ttl: LEASE_TTL,
        clustered: false,
    });
    let lease = session_lease("elysper", &SessionKey::new("alice", "s1"));
    assert!(
        bus.try_acquire(&lease, "node-a", LEASE_TTL)
            .await
            .expect("the lease is acquired"),
        "the node owns the session's lease",
    );

    let sink = Arc::new(SlowSink::new("s1", DISK));
    let writer = Arc::new(elysper::session::LogWriter::new(
        Arc::clone(&sink) as Arc<dyn elysper::session::SessionSink>,
        Arc::new(elysper::core::SystemClock),
    ));
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn RunStreamFactory> =
        Arc::new(VecFactory::new(vec![], Arc::clone(&calls)));
    let controller = super::super::SessionController::spawn_with_lease(
        "alice",
        "s1",
        Arc::clone(&sink) as Arc<dyn elysper::session::SessionSink>,
        writer,
        super::support::test_deps().with_cluster(cluster),
        Duration::from_secs(60),
        factory,
        lease.clone(),
    );

    // Flood the plugin queue through the public seam.
    let plugin_sink = controller
        .plugin_event_sink()
        .expect("the owning controller exposes a session-bound sink");
    for i in 0..FLOOD {
        plugin_sink.publish(elysper::plugin_host::PublishedEvent {
            kind: format!("flood-{i}"),
            payload: serde_json::json!({ "i": i }),
        });
    }

    // Let the flood run for longer than the lease's whole TTL: the
    // renewal must have kept it alive throughout.
    tokio::time::sleep(LEASE_TTL * 2).await;

    assert!(
        controller.is_alive(),
        "the controller must not have lost its lease to the plugin flood",
    );
    assert!(
        !bus.try_acquire(&lease, "node-b", LEASE_TTL)
            .await
            .expect("the lease probe answers"),
        "the lease must still be held by this node: an expired lease \
         hands the session to another node",
    );
}
