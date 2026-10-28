//! Multi-node cluster bus (see [`frame`] for the delivery contract).
//!
//! `mode = "local"` (the default) uses [`LocalBus`]: single-process
//! semantics, zero external dependencies. `mode = "redis"` (feature
//! `cluster-redis`) uses `RedisBus`: leases + pub/sub shared by every
//! node over the same workspace filesystem.

pub mod frame;
mod local;
#[cfg(feature = "cluster-redis")]
mod redis_bus;

pub use frame::{
    ClusterError,
    ControlFrame,
    FrameReceiver,
    NodeId,
    SessionBus,
    SessionFrame,
    SessionKey,
    SessionOpWire,
    ToolMutation,
    scheduler_leader_lease,
    session_lease,
};
pub use local::LocalBus;
#[cfg(feature = "cluster-redis")]
pub use redis_bus::RedisBus;

use crate::config::server::{ClusterConfig, ClusterMode};

/// Everything a request path needs to know about this node's bus.
#[derive(Clone)]
pub struct ClusterRuntime {
    pub bus: std::sync::Arc<dyn SessionBus>,
    pub node_id: NodeId,
    pub namespace: String,
    /// `[cluster].lease_ttl_secs` as a duration; renewals run at a
    /// third of it.
    pub lease_ttl: std::time::Duration,
    /// `true` when the bus is shared across nodes (`mode = "redis"`).
    /// A local bus never has another node to publish to, so the
    /// controller skips its session-frame publish entirely in the
    /// default deployment.
    pub clustered: bool,
}

impl std::fmt::Debug for ClusterRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ClusterRuntime")
            .field("node_id", &self.node_id)
            .field("namespace", &self.namespace)
            .field("lease_ttl", &self.lease_ttl)
            .finish_non_exhaustive()
    }
}

impl ClusterRuntime {
    /// The single-process default: in-memory bus, `"local"` node id.
    #[must_use]
    pub fn local() -> Self {
        Self {
            bus: std::sync::Arc::new(LocalBus::new()),
            node_id: "local".to_string(),
            namespace: "elysper".to_string(),
            lease_ttl: std::time::Duration::from_secs(15),
            clustered: false,
        }
    }

    /// Build from the `[cluster]` section. `node_id` defaults to a
    /// fresh ULID so two nodes started without config still differ.
    ///
    /// # Errors
    ///
    /// Whatever [`build_bus`] returns — an unreachable Redis in
    /// `mode = "redis"` is a boot failure.
    pub async fn from_config(
        config: &ClusterConfig,
    ) -> Result<Self, ClusterError> {
        let bus = build_bus(config).await?;
        let node_id = config
            .node_id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        Ok(Self {
            bus,
            node_id,
            namespace: config.namespace.clone(),
            lease_ttl: std::time::Duration::from_secs(
                config.lease_ttl_secs.max(1),
            ),
            clustered: config.mode == ClusterMode::Redis,
        })
    }

    /// The lease name for one session.
    #[must_use]
    pub fn session_lease(&self, key: &SessionKey) -> String {
        session_lease(&self.namespace, key)
    }
}

/// Build the bus the config asks for.
///
/// # Errors
///
/// [`ClusterError::Unavailable`] when `mode = "redis"` was requested
/// without the `cluster-redis` feature; [`ClusterError::Backend`] when
/// Redis is configured but unreachable.
pub async fn build_bus(
    config: &ClusterConfig,
) -> Result<std::sync::Arc<dyn SessionBus>, ClusterError> {
    match config.mode {
        ClusterMode::Local => Ok(std::sync::Arc::new(LocalBus::new())),
        ClusterMode::Redis => {
            #[cfg(feature = "cluster-redis")]
            {
                let url = config.redis_url.as_deref().ok_or_else(|| {
                    ClusterError::Backend(
                        "[cluster].redis_url is required when mode = \"redis\""
                            .to_string(),
                    )
                })?;
                let bus =
                    RedisBus::connect(url, config.namespace.clone()).await?;
                Ok(std::sync::Arc::new(bus))
            }
            #[cfg(not(feature = "cluster-redis"))]
            {
                Err(ClusterError::Unavailable(
                    "[cluster].mode = \"redis\" needs the `cluster-redis` \
                     feature at build time"
                        .to_string(),
                ))
            }
        }
    }
}
