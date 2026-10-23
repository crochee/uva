//! The per-`(user, session)` controller cache on `AppState`.

use std::sync::Arc;

use synthia::harness::AgentEvent;

use super::AppState;
use crate::session::controller::{
    AgentRunStreamFactory,
    RunDependencies,
    SessionController,
};

impl AppState {
    /// Gets or creates a [`SessionController`] for `(user_id, session_id)`,
    /// restoring the session from the on-disk store if it is not already
    /// loaded in memory.
    pub async fn get_or_create_session_controller(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> synthia::core::Result<Arc<SessionController>> {
        self.get_or_create_session_controller_with_parent(
            user_id, session_id, None, None,
        )
        .await
    }

    /// Gets or creates a [`SessionController`] for `(user_id, session_id)`
    /// with an optional parent spawn depth.
    ///
    /// When `parent_depth` is `Some(d)`, the controller is created
    /// under a derived depth so future nested spawns could enforce
    /// `max_depth`. With subagent factories removed, the depth is
    /// currently informational; the controller is always treated as
    /// a root session at runtime.
    pub async fn get_or_create_session_controller_with_parent(
        &self,
        user_id: &str,
        session_id: &str,
        _parent_event_sender: Option<tokio::sync::mpsc::Sender<AgentEvent>>,
        _parent_depth: Option<usize>,
    ) -> synthia::core::Result<Arc<SessionController>> {
        let key = (user_id.to_string(), session_id.to_string());
        if let Some(entry) = self.active_sessions.get(&key) {
            if !entry.is_remote() {
                return Ok(entry.clone());
            }
            // A cached remote handle must re-check ownership: its
            // owner may have died since, and a remote handle can
            // never "upgrade" itself into a controller. If the lease
            // is free now (expired TTL or released), drop the stale
            // handle and fall through to the owner path; otherwise
            // keep forwarding.
            let cluster_key =
                crate::cluster::SessionKey::new(user_id, session_id);
            let lease = self.cluster.session_lease(&cluster_key);
            match self
                .cluster
                .bus
                .try_acquire(
                    &lease,
                    &self.cluster.node_id,
                    self.cluster.lease_ttl,
                )
                .await
            {
                Ok(true) => {
                    drop(entry);
                    self.active_sessions.remove(&key);
                }
                Ok(false) => return Ok(entry.clone()),
                Err(e) => {
                    return Err(synthia::core::Error::internal(format!(
                        "cluster lease for '{cluster_key}': {e}"
                    )));
                }
            }
        }

        // Cluster ownership first: the node that holds the session
        // lease runs the controller. A node that loses the race gets a
        // remote handle that forwards ops and subscribes to frames —
        // it must not create/repair the shared session files (the
        // owner owns that side too).
        let cluster_key = crate::cluster::SessionKey::new(user_id, session_id);
        let lease = self.cluster.session_lease(&cluster_key);
        let acquired = self
            .cluster
            .bus
            .try_acquire(&lease, &self.cluster.node_id, self.cluster.lease_ttl)
            .await
            .map_err(|e| {
                synthia::core::Error::internal(format!(
                    "cluster lease for '{cluster_key}': {e}"
                ))
            })?;
        if !acquired {
            let remote = SessionController::spawn_remote(
                cluster_key.user_id.clone(),
                cluster_key.session_id.clone(),
                Arc::clone(&self.cluster),
            );
            self.active_sessions.insert(
                (cluster_key.user_id, cluster_key.session_id),
                Arc::clone(&remote),
            );
            return Ok(remote);
        }

        if self.session_manager.get(session_id).await.is_none() {
            // client may supply a fresh task_id without a
            // pre-existing session. Create one eagerly so we never
            // have to round-trip through the metadata restore path.
            self.session_manager
                .create_with_user(session_id.to_string(), user_id.to_string())
                .await
                .map_err(|e| {
                    // 404 semantics (the requested session could
                    // not be established). The underlying cause
                    // crosses the wire as structured data.
                    synthia::core::Error::not_found(format!(
                        "session '{session_id}' (create failed: {e})"
                    ))
                })?;
        }

        let queue = self.session_manager.input_queue();
        let session_sink = self.session_manager.sink(user_id, session_id);
        // Crash repair (R4 Phase A): close any open tail turn the
        // previous process left behind so the resumed run starts
        // from a balanced log. Best-effort — a repair failure
        // logs and continues (the fold will surface the
        // corruption when the history is projected).
        match synthia::context::repair_session(Arc::clone(&session_sink)).await
        {
            Ok(0) => {}
            Ok(n) => tracing::info!(
                target: "synthia.session",
                user_id,
                session_id,
                closers = n,
                "repaired interrupted session tail"
            ),
            Err(e) => tracing::warn!(
                target: "synthia.session",
                user_id,
                session_id,
                error = %e,
                "session tail repair failed; continuing"
            ),
        }
        let deps = RunDependencies::new(
            Arc::clone(&self.default_provider),
            Arc::clone(&self.tool_registry),
            self.workspace_root.clone(),
            self.system_prompt.clone(),
        )
        .with_prompt_context(self.prompt_context.clone())
        .with_steering(Arc::clone(&self.steering))
        .with_max_iterations(self.default_max_iterations)
        .with_optional_compaction(self.default_compaction)
        // R34: every run agent advertises the configured surface
        // (`max_visible` is request-side, so the registry alone cannot
        // carry it).
        .with_optional_tool_surface(self.tool_surface.policy())
        // R50: the deployment's reasoning loop reaches every run
        // through the same hop as the surface policy.
        .with_optional_strategy(self.default_strategy.clone())
        // R58: the agent's allow/deny list reaches every run the same
        // way the reasoning loop does.
        .with_optional_tool_restriction(
            self.default_tool_restriction.clone(),
        )
        .with_clock(self.clock.clone())
        .with_agent_registry(
            Arc::clone(&self.agent_registry),
            Arc::new(parking_lot::RwLock::new(
                self.default_agent_name.read().clone(),
            )),
        )
        // Usage counters are process-wide: every controller shares the
        // one `Arc` the boot created, so the totals outlive any single
        // session. The sub-agent router is likewise the process-wide
        // one: child sessions land in the same registry the sessions
        // API lists.
        .with_usage_metrics(Arc::clone(&self.usage_metrics))
        .with_subagent_router(Some(Arc::clone(&self.subagent_router)))
        .with_cluster(Arc::clone(&self.cluster));

        // Owner path: spawn the controller with the lease it holds.
        let controller = SessionController::spawn_with_lease(
            user_id,
            session_id,
            queue,
            session_sink,
            deps,
            crate::session::controller::DEFAULT_IDLE_TIMEOUT,
            Arc::new(AgentRunStreamFactory),
            lease,
        );

        self.active_sessions.insert(
            (cluster_key.user_id, cluster_key.session_id),
            Arc::clone(&controller),
        );
        Ok(controller)
    }
}
