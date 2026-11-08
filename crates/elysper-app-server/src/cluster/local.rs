//! `LocalBus` — the in-process [`SessionBus`] backend (the default).
//!
//! Leases live in a mutex-guarded map keyed by lease name; session and
//! control frames fan out over `tokio::sync::broadcast` and are
//! forwarded into each subscriber's `mpsc` by a small task. A single
//! process behaves exactly as it did before the bus existed: no
//! external dependency, no extra hop for the SSE tail unless the caller
//! subscribes through the bus.

use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use parking_lot::Mutex;
use tokio::sync::{broadcast, mpsc};

use super::frame::{
    ClusterError,
    ControlFrame,
    FrameReceiver,
    SessionBus,
    SessionFrame,
    SessionKey,
};

/// Broadcast capacity per channel: a full LLM burst with headroom.
const CHANNEL_CAPACITY: usize = 1024;

#[derive(Clone, Debug)]
struct Lease {
    node_id: String,
    expires_at: Instant,
}

/// In-process bus. Cheap to clone (all state is `Arc`-shared).
#[derive(Clone, Debug, Default)]
pub struct LocalBus {
    inner: Arc<Inner>,
}

#[derive(Debug, Default)]
struct Inner {
    leases: Mutex<HashMap<String, Lease>>,
    sessions: Mutex<HashMap<SessionKey, broadcast::Sender<SessionFrame>>>,
    control: Mutex<Option<broadcast::Sender<ControlFrame>>>,
}

impl LocalBus {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn session_sender(
        &self,
        key: &SessionKey,
    ) -> broadcast::Sender<SessionFrame> {
        let mut sessions = self.inner.sessions.lock();
        sessions
            .entry(key.clone())
            .or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .clone()
    }

    fn control_sender(&self) -> broadcast::Sender<ControlFrame> {
        let mut slot = self.inner.control.lock();
        slot.get_or_insert_with(|| broadcast::channel(CHANNEL_CAPACITY).0)
            .clone()
    }

    /// Drop the session channel for `key` once nobody is listening.
    /// Called by nothing yet (channels are cheap and bounded); the
    /// method exists so an idle-session sweep can be added without
    /// reaching into the map.
    pub fn forget_session(&self, key: &SessionKey) {
        let mut sessions = self.inner.sessions.lock();
        if let Some(tx) = sessions.get(key)
            && tx.receiver_count() == 0
        {
            sessions.remove(key);
        }
    }
}

/// Forward a broadcast receiver into an `mpsc` (the trait's one
/// receiver shape). `Lagged` skips the gap; `Closed` ends the task.
fn forward<T: Clone + Send + 'static>(
    mut rx: broadcast::Receiver<T>,
) -> FrameReceiver<T> {
    let (tx, out) = mpsc::channel(CHANNEL_CAPACITY);
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(frame) => {
                    if tx.send(frame).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => continue,
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
    out
}

#[async_trait::async_trait]
impl SessionBus for LocalBus {
    async fn try_acquire(
        &self,
        lease: &str,
        node_id: &str,
        ttl: Duration,
    ) -> Result<bool, ClusterError> {
        let now = Instant::now();
        let mut leases = self.inner.leases.lock();
        match leases.get(lease) {
            Some(held) if held.node_id == node_id && held.expires_at > now => {
                // Re-entrant acquisition: refresh the TTL.
                leases.insert(
                    lease.to_string(),
                    Lease {
                        node_id: node_id.to_string(),
                        expires_at: now + ttl,
                    },
                );
                Ok(true)
            }
            Some(held) if held.expires_at > now => {
                let _ = held;
                Ok(false)
            }
            _ => {
                leases.insert(
                    lease.to_string(),
                    Lease {
                        node_id: node_id.to_string(),
                        expires_at: now + ttl,
                    },
                );
                Ok(true)
            }
        }
    }

    async fn renew(
        &self,
        lease: &str,
        node_id: &str,
        ttl: Duration,
    ) -> Result<(), ClusterError> {
        let now = Instant::now();
        let mut leases = self.inner.leases.lock();
        match leases.get(lease) {
            Some(held) if held.node_id == node_id && held.expires_at > now => {
                leases.insert(
                    lease.to_string(),
                    Lease {
                        node_id: node_id.to_string(),
                        expires_at: now + ttl,
                    },
                );
                Ok(())
            }
            _ => Err(ClusterError::Backend(format!(
                "lease '{lease}' is no longer held by node '{node_id}'"
            ))),
        }
    }

    async fn release(
        &self,
        lease: &str,
        node_id: &str,
    ) -> Result<(), ClusterError> {
        let mut leases = self.inner.leases.lock();
        if leases
            .get(lease)
            .is_some_and(|held| held.node_id == node_id)
        {
            leases.remove(lease);
        }
        Ok(())
    }

    async fn publish_session(
        &self,
        key: &SessionKey,
        frame: SessionFrame,
    ) -> Result<(), ClusterError> {
        // No subscribers is not an error: the durable append already
        // happened (the same posture `EventBroadcaster::send` takes).
        let _ = self.session_sender(key).send(frame);
        Ok(())
    }

    async fn subscribe_session(
        &self,
        key: &SessionKey,
    ) -> Result<FrameReceiver<SessionFrame>, ClusterError> {
        Ok(forward(self.session_sender(key).subscribe()))
    }

    async fn publish_control(
        &self,
        frame: ControlFrame,
    ) -> Result<(), ClusterError> {
        let _ = self.control_sender().send(frame);
        Ok(())
    }

    async fn subscribe_control(
        &self,
    ) -> Result<FrameReceiver<ControlFrame>, ClusterError> {
        Ok(forward(self.control_sender().subscribe()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TTL: Duration = Duration::from_secs(15);

    #[tokio::test]
    async fn lease_is_exclusive_and_reentrant() {
        let bus = LocalBus::new();
        assert!(bus.try_acquire("l", "a", TTL).await.unwrap());
        // Same node: re-entrant.
        assert!(bus.try_acquire("l", "a", TTL).await.unwrap());
        // Another node: denied while unexpired.
        assert!(!bus.try_acquire("l", "b", TTL).await.unwrap());
        // Renewal by the holder succeeds; by a foreigner fails.
        bus.renew("l", "a", TTL).await.unwrap();
        assert!(bus.renew("l", "b", TTL).await.is_err());
        bus.release("l", "a").await.unwrap();
        // Released: anyone can take it.
        assert!(bus.try_acquire("l", "b", TTL).await.unwrap());
    }

    #[tokio::test]
    async fn expired_lease_is_takeable() {
        let bus = LocalBus::new();
        assert!(
            bus.try_acquire("l", "a", Duration::from_millis(1))
                .await
                .unwrap()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
        assert!(bus.try_acquire("l", "b", TTL).await.unwrap());
        assert!(bus.renew("l", "a", TTL).await.is_err());
    }

    #[tokio::test]
    async fn session_frames_round_trip() {
        let bus = LocalBus::new();
        let key = SessionKey::new("u", "s");
        let mut rx = bus.subscribe_session(&key).await.unwrap();
        let other_key = SessionKey::new("u", "other");
        let mut other_rx = bus.subscribe_session(&other_key).await.unwrap();
        let frame = SessionFrame {
            origin: "node-a".to_string(),
            cursor: Some(7),
            payload: serde_json::json!({"kind": "text"}),
        };
        bus.publish_session(&key, frame).await.unwrap();
        let got = rx.recv().await.expect("frame");
        assert_eq!(got.cursor, Some(7));
        assert_eq!(got.payload["kind"], "text");

        // Channel isolation: the other session's subscriber sees
        // nothing within the window.
        assert!(
            tokio::time::timeout(Duration::from_millis(100), other_rx.recv())
                .await
                .is_err(),
            "a frame must not leak to another session's channel"
        );
    }

    #[tokio::test]
    async fn control_frames_round_trip() {
        let bus = LocalBus::new();
        let mut rx = bus.subscribe_control().await.unwrap();
        bus.publish_control(ControlFrame::SetDefaultAgent {
            origin: "n".to_string(),
            name: "reviewer".to_string(),
        })
        .await
        .unwrap();
        match rx.recv().await.expect("frame") {
            ControlFrame::SetDefaultAgent { name, origin } => {
                assert_eq!(name, "reviewer");
                assert_eq!(origin, "n");
            }
            other => panic!("unexpected frame: {other:?}"),
        }
    }
}
