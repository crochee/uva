//! `RedisBus` — Redis pub/sub + `SET NX EX` leases
//! (`cluster-redis` feature).
//!
//! # Semantics
//!
//! - **Leases** are `SET {key} {node_id} NX EX {ttl}`; renewal is a
//!   Lua compare-and-expire so a node can never extend a lease it no
//!   longer owns. Expiry is the crash-recovery mechanism: a killed
//!   owner's sessions become takeover-able after `ttl`.
//! - **Frames** are Redis pub/sub: best-effort, at-most-once. The
//!   durable truth stays the shared workspace JSONL; an SSE client
//!   that missed frames resumes from its `from` cursor and learns the
//!   gap from the snapshot/cursor frames (see [`super`]).
//! - **Subscriptions**: one dedicated pub/sub connection per
//!   subscriber, forwarding into an `mpsc` so consumers await a single
//!   receiver type on both backends.

use std::time::Duration;

use async_trait::async_trait;
use redis::AsyncCommands;
use tokio::sync::mpsc;

use super::frame::{
    ClusterError,
    ControlFrame,
    FrameReceiver,
    SessionBus,
    SessionFrame,
    SessionKey,
};

/// Renewal is compare-and-expire: only the recorded owner may extend.
const RENEW_SCRIPT: &str = r"
if redis.call('GET', KEYS[1]) == ARGV[1] then
  return redis.call('EXPIRE', KEYS[1], tonumber(ARGV[2]))
end
return 0
";

/// Release is compare-and-delete.
const RELEASE_SCRIPT: &str = r"
if redis.call('GET', KEYS[1]) == ARGV[1] then
  return redis.call('DEL', KEYS[1])
end
return 0
";

#[derive(Clone)]
pub struct RedisBus {
    client: redis::Client,
    namespace: String,
}

impl std::fmt::Debug for RedisBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisBus")
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

impl RedisBus {
    /// Connect to `url` and scope every key/channel under `namespace`.
    ///
    /// # Errors
    ///
    /// [`ClusterError::Backend`] when the URL does not parse or the
    /// server is unreachable at construction time.
    pub async fn connect(
        url: &str,
        namespace: impl Into<String>,
    ) -> Result<Self, ClusterError> {
        let client = redis::Client::open(url)
            .map_err(|e| ClusterError::Backend(format!("redis url: {e}")))?;
        // Fail fast on an unusable endpoint: the bus is a boot
        // dependency in `mode = "redis"`.
        let mut conn = client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| {
                ClusterError::Backend(format!("redis connect: {e}"))
            })?;
        let _: String = redis::cmd("PING")
            .query_async(&mut conn)
            .await
            .map_err(|e| ClusterError::Backend(format!("redis ping: {e}")))?;
        Ok(Self {
            client,
            namespace: namespace.into(),
        })
    }

    fn session_channel(&self, key: &SessionKey) -> String {
        format!("{}:bus:session:{key}", self.namespace)
    }

    fn control_channel(&self) -> String {
        format!("{}:bus:control", self.namespace)
    }

    async fn connection(
        &self,
    ) -> Result<redis::aio::MultiplexedConnection, ClusterError> {
        self.client
            .get_multiplexed_async_connection()
            .await
            .map_err(|e| ClusterError::Backend(format!("redis connect: {e}")))
    }

    /// Spawn one pub/sub connection for `channel`, decoding every
    /// message with `decode` into the returned receiver.
    fn subscribe<T, F>(&self, channel: String, decode: F) -> FrameReceiver<T>
    where
        T: Send + 'static,
        F: Fn(&str) -> Option<T> + Send + 'static,
    {
        let (tx, out) = mpsc::channel(1024);
        let client = self.client.clone();
        tokio::spawn(async move {
            let Ok(mut pubsub) = client.get_async_pubsub().await else {
                return;
            };
            if pubsub.subscribe(channel.as_str()).await.is_err() {
                return;
            }
            loop {
                let message = match pubsub.on_message().next().await {
                    Some(m) => m,
                    None => break,
                };
                let payload: String = match message.get_payload() {
                    Ok(p) => p,
                    Err(_) => continue,
                };
                if let Some(value) = decode(&payload)
                    && tx.send(value).await.is_err()
                {
                    break;
                }
            }
        });
        out
    }
}

use futures::StreamExt as _;

#[async_trait]
impl SessionBus for RedisBus {
    async fn try_acquire(
        &self,
        lease: &str,
        node_id: &str,
        ttl: Duration,
    ) -> Result<bool, ClusterError> {
        let mut conn = self.connection().await?;
        let seconds = ttl.as_secs().max(1);
        let acquired: Option<String> = redis::cmd("SET")
            .arg(lease)
            .arg(node_id)
            .arg("NX")
            .arg("EX")
            .arg(seconds)
            .query_async(&mut conn)
            .await
            .map_err(|e| ClusterError::Backend(format!("redis SET NX: {e}")))?;
        if acquired.is_some() {
            return Ok(true);
        }
        // Not acquired: it may already be *ours* (re-entrant path), in
        // which case refresh and report success.
        let holder: Option<String> = conn
            .get(lease)
            .await
            .map_err(|e| ClusterError::Backend(format!("redis GET: {e}")))?;
        if holder.as_deref() == Some(node_id) {
            self.renew(lease, node_id, ttl).await?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    async fn renew(
        &self,
        lease: &str,
        node_id: &str,
        ttl: Duration,
    ) -> Result<(), ClusterError> {
        let mut conn = self.connection().await?;
        let seconds = ttl.as_secs().max(1);
        let renewed: i64 = redis::Script::new(RENEW_SCRIPT)
            .key(lease)
            .arg(node_id)
            .arg(seconds)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| ClusterError::Backend(format!("redis renew: {e}")))?;
        if renewed == 0 {
            return Err(ClusterError::Backend(format!(
                "lease '{lease}' is no longer held by node '{node_id}'"
            )));
        }
        Ok(())
    }

    async fn release(
        &self,
        lease: &str,
        node_id: &str,
    ) -> Result<(), ClusterError> {
        let mut conn = self.connection().await?;
        let _: i64 = redis::Script::new(RELEASE_SCRIPT)
            .key(lease)
            .arg(node_id)
            .invoke_async(&mut conn)
            .await
            .map_err(|e| {
                ClusterError::Backend(format!("redis release: {e}"))
            })?;
        Ok(())
    }

    async fn publish_session(
        &self,
        key: &SessionKey,
        frame: SessionFrame,
    ) -> Result<(), ClusterError> {
        let mut conn = self.connection().await?;
        let payload = serde_json::to_string(&frame)
            .map_err(|e| ClusterError::Backend(format!("encode frame: {e}")))?;
        let _: usize = conn
            .publish(self.session_channel(key), payload)
            .await
            .map_err(|e| {
            ClusterError::Backend(format!("redis publish: {e}"))
        })?;
        Ok(())
    }

    async fn subscribe_session(
        &self,
        key: &SessionKey,
    ) -> Result<FrameReceiver<SessionFrame>, ClusterError> {
        Ok(self.subscribe(self.session_channel(key), |payload| {
            serde_json::from_str(payload).ok()
        }))
    }

    async fn publish_control(
        &self,
        frame: ControlFrame,
    ) -> Result<(), ClusterError> {
        let mut conn = self.connection().await?;
        let payload = serde_json::to_string(&frame)
            .map_err(|e| ClusterError::Backend(format!("encode frame: {e}")))?;
        let _: usize = conn
            .publish(self.control_channel(), payload)
            .await
            .map_err(|e| {
                ClusterError::Backend(format!("redis publish: {e}"))
            })?;
        Ok(())
    }

    async fn subscribe_control(
        &self,
    ) -> Result<FrameReceiver<ControlFrame>, ClusterError> {
        Ok(self.subscribe(self.control_channel(), |payload| {
            serde_json::from_str(payload).ok()
        }))
    }
}
