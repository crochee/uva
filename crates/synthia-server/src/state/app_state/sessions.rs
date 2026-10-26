//! The per-`(user, session)` controller cache on `AppState`.

use std::sync::{Arc, Weak};

use dashmap::mapref::entry::Entry;
use synthia::harness::AgentEvent;

use super::AppState;
use crate::session::controller::{
    AgentRunStreamFactory,
    RunDependencies,
    SessionController,
    SpawnGate,
};

/// How far back a takeover starts looking for a prompt the previous
/// owner parked and never ran.
///
/// A starting point, not a bound: the read widens (×4) until it reaches
/// the log's start or a consumption frontier — a `park_consumed` row,
/// or a legacy `UserInput` one — because a prompt queued behind a busy
/// run sits before every row that run writes, and 200 rows is ~40
/// iterations of a live session. A `parked_prompt` row is only spent by
/// the `park_consumed` row naming it, so a park that reaches the window
/// is answered exactly: its consumer is always written after it.
pub(crate) const PARKED_PROMPT_WINDOW: usize = 200;

/// How long a cold open waits for its session's gate, in total.
///
/// The holder is inside a filesystem read, a cluster lease acquisition
/// and the log rebuild. None of those has a timeout of its own, so a
/// wedged session would otherwise block every request for it for the
/// life of the process. A waiter that cannot get in within this budget
/// fails with a diagnostic naming the session: the request is
/// retryable, and the log says which session is wedged. The work
/// *inside* the gate is not preempted — timing out a lease acquisition
/// could leave half an acquisition behind — so a wedged holder is fixed
/// by restarting the node, not by a waiter.
pub(crate) const COLD_OPEN_GATE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(30);

/// The error a waiter that could not take the gate reports.
fn cold_open_timeout(
    key: &(String, String),
    waited: std::time::Duration,
) -> synthia::core::Error {
    synthia::core::Error::internal(format!(
        "session '{}/{}' could not be opened: another request has held its \
         open gate for over {waited:?}; the session is wedged or the \
         filesystem it lives on is unresponsive",
        key.0, key.1
    ))
}

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
        // The cold path is a read-modify-write of `active_sessions`, and
        // it now also rebuilds from the log and re-queues parked work.
        // Two concurrent requests for one session would both spawn, both
        // rebuild, and both start the recovered turn — the duplicate this
        // feature exists to prevent. The gate serialises the decision:
        // a request that finds the session already open returns the
        // controller it finds, behind the same gate.
        //
        // The wait is bounded: the holder is inside a filesystem read and
        // a cluster lease acquisition, and one wedged session must not
        // block every request for it for the life of the process. The
        // budget covers the whole loop below, not one attempt, so a gate
        // evicted under us cannot extend the wait.
        let deadline = tokio::time::Instant::now() + self.spawn_gate_timeout;
        loop {
            let gate = self.session_spawn_gate(&key);
            let registered = Arc::downgrade(&gate);
            let remaining =
                deadline.saturating_duration_since(tokio::time::Instant::now());
            let acquired = if remaining.is_zero() {
                None
            } else {
                // The guard owns its own handle on the lock, so the
                // cold open can release its own before it checks
                // whether anyone else still owns the gate.
                tokio::time::timeout(remaining, Arc::clone(&gate).lock_owned())
                    .await
                    .ok()
            };
            let Some(guard) = acquired else {
                // The wait is over. The gate this call held is released
                // with it, and an entry nobody owns goes with it.
                drop(gate);
                self.drop_unowned_gate(&key, &registered);
                return Err(cold_open_timeout(&key, self.spawn_gate_timeout));
            };
            // The gate belongs to the session's controller, so it leaves
            // when that controller does — which may have happened while
            // this waiter queued behind it. Taking a detached lock is
            // the one thing that must not happen: the next caller would
            // take a fresh gate and spawn a second controller beside
            // this one. Re-check under the lock and, if the map no
            // longer holds *this* gate, queue on the new one.
            if self.spawn_gates.get(&key).is_some_and(|current| {
                Weak::ptr_eq(current.value(), &registered)
            }) {
                let opened = self.open_session_under_gate(&key, &gate).await;
                drop(guard);
                drop(gate);
                // The controller that was opened owns the gate now; if
                // nobody took it, its entry is dead weight.
                self.drop_unowned_gate(&key, &registered);
                return opened;
            }
            tracing::debug!(
                target: "synthia.session",
                user_id = %key.0,
                session_id = %key.1,
                "cold open: the session's gate was evicted while waiting;                  retrying on the current one"
            );
            drop(guard);
            drop(gate);
        }
    }

    /// The cold path, with the session's spawn gate held.
    ///
    /// The gate is handed to the controller this path ends with: the
    /// gate belongs to the controller, so the map's entry lives exactly
    /// as long as the controller does.
    async fn open_session_under_gate(
        &self,
        key: &(String, String),
        gate: &Arc<tokio::sync::Mutex<()>>,
    ) -> synthia::core::Result<Arc<SessionController>> {
        let (user_id, session_id) = (key.0.as_str(), key.1.as_str());
        if let Some(entry) = self.active_sessions.get(key) {
            if !entry.is_remote() {
                // Another request won the race while this one waited:
                // its controller is the session's owner now.
                entry.attach_spawn_gate(self.spawn_gate_owner(key, gate));
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
                    // The controller alone: this task still holds the
                    // gate, and is about to spawn its replacement under
                    // it (`forget_session` would evict the gate too).
                    self.active_sessions.remove(key);
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
            // A forwarder takes no writer: it writes no row, and it
            // must not touch the session's files — the owner owns
            // that side too.
            let remote = SessionController::spawn_remote(
                cluster_key.user_id.clone(),
                cluster_key.session_id.clone(),
                Arc::clone(&self.cluster),
            );
            remote.attach_spawn_gate(self.spawn_gate_owner(key, gate));
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

        let queue = self.input_queue.clone();
        let session_sink = self.session_manager.sink(user_id, session_id);
        // The session's one writer: it stamps the rows' ordinals and
        // batches their flush, so the session's log has one ordinal
        // source.
        let writer = self.session_manager.writer(user_id, session_id);
        // Crash repair (R4 Phase A): close any open tail turn the
        // previous process left behind so the resumed run starts
        // from a balanced log. Best-effort — a repair failure
        // logs and continues (the fold will surface the
        // corruption when the history is projected).
        match synthia::context::repair_session(Arc::clone(&writer)).await {
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
        .with_plugin_host(Some(self.plugin_host.clone()))
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
        .with_cluster(Arc::clone(&self.cluster))
        // The one place multimodal payload bytes live: a parked
        // prompt stores them here and its row carries the hash.
        .with_attachment_store(Arc::clone(&self.attachment_store));

        // Owner path: spawn the controller with the lease it holds.
        let restore_sink = Arc::clone(&session_sink);
        let controller = SessionController::spawn_with_lease(
            user_id,
            session_id,
            queue,
            session_sink,
            writer,
            deps,
            crate::session::controller::DEFAULT_IDLE_TIMEOUT,
            Arc::new(AgentRunStreamFactory),
            lease,
        );

        controller.attach_spawn_gate(self.spawn_gate_owner(key, gate));
        self.active_sessions.insert(
            (cluster_key.user_id, cluster_key.session_id),
            Arc::clone(&controller),
        );
        // This node may have just taken the session over from one that
        // died, so nothing the previous owner held in memory exists
        // here. The log does: the repair above closed its open tail,
        // the state is derived from it, and a prompt parked before the
        // crash is still queued. Rebuilding is the takeover path.
        self.rebuild_from_log(&controller, &restore_sink, user_id, session_id)
            .await;
        Ok(controller)
    }

    /// Rebuild a controller's state from the session's log.
    ///
    /// A node that acquires the session lease inherits no memory from
    /// the node that lost it, so everything the controller reports is
    /// re-derived here: `repair` closed the open tail before the
    /// controller was spawned, the fold decides whether the session is
    /// suspended, and a parked prompt is put back in the queue with its
    /// attachment bytes fetched from the store.
    ///
    /// The resume a client had in flight is deliberately **not**
    /// rebuilt (§5.11): the decisions are the client's intent, and the
    /// token derived from the log makes a re-send idempotent, so the
    /// client re-sends rather than the server inventing a second source
    /// of truth.
    async fn rebuild_from_log(
        &self,
        controller: &Arc<SessionController>,
        sink: &Arc<dyn synthia::session::SessionSink>,
        user_id: &str,
        session_id: &str,
    ) {
        let state = controller.session_state().await;
        controller.adopt_state(state);

        // The queue is whatever follows the last consumed park, and a
        // park can sit behind a long run — 200 rows is ~40 iterations of
        // a busy session. So widen until the window either reaches the
        // log's start or holds the consumption frontier (the newest
        // `UserInput` row): everything after that row is the queue, and
        // nothing before it can still be waiting.
        let mut window = PARKED_PROMPT_WINDOW;
        let rows = loop {
            let rows = match sink.read_tail(window).await {
                Ok(rows) => rows,
                Err(error) => {
                    tracing::warn!(
                        target: "synthia.session",
                        user_id,
                        session_id,
                        %error,
                        "takeover: the parked-prompt window could not be read"
                    );
                    return;
                }
            };
            let whole_log = rows.len() < window;
            if whole_log || rows.iter().any(is_consumption_marker) {
                if window > PARKED_PROMPT_WINDOW {
                    tracing::debug!(
                        target: "synthia.session",
                        user_id,
                        session_id,
                        window,
                        "takeover: widened the parked-prompt read to reach \
                         the consumption frontier"
                    );
                }
                break rows;
            }
            window = window.saturating_mul(4);
        };
        let recovered = synthia::session::parked_prompts(&rows);
        if recovered.is_empty() {
            return;
        }
        let mut restored = 0;
        for prompt in &recovered {
            let parts = self.parts_from_parked(prompt);
            if controller.restore_parked_prompt(
                parts,
                prompt.park_id.clone(),
                prompt.agent_name.clone(),
                prompt.rerun,
            ) {
                restored += 1;
            }
        }
        tracing::info!(
            target: "synthia.session",
            user_id,
            session_id,
            restored,
            state = ?state,
            "takeover: re-queued the prompts a previous owner parked"
        );
        // The loop only starts a run for an operation it receives, and
        // the prompts are already queued: this op names no input, it
        // only lets the recovered turn begin.
        if restored > 0
            && let Err(error) = controller
                .submit(crate::session::controller::SessionOp::StartPending)
                .await
        {
            tracing::warn!(
                target: "synthia.session",
                user_id,
                session_id,
                %error,
                "takeover: the recovered prompt could not be started"
            );
        }
    }

    /// The per-session gate a cold open takes.
    ///
    /// Keyed by `(user_id, session_id)`, and **one entry per live
    /// controller**: the map holds a weak reference to the lock and the
    /// controller holds the strong one, so the entry is created with
    /// the controller and leaves with it — through every path that
    /// drops it, not only the one that evicts a session explicitly.
    /// Keeping it after the spawn is deliberate — a session that goes
    /// cold again (idle shutdown, a takeover) must queue on the same
    /// lock as a request already waiting. Losing it is safe because
    /// every waiter re-checks, under the lock, that the map still holds
    /// the gate it took (see
    /// `get_or_create_session_controller_with_parent`); a waiter whose
    /// gate was evicted retries on the new one instead of proceeding on a
    /// detached lock.
    fn session_spawn_gate(
        &self,
        key: &(String, String),
    ) -> Arc<tokio::sync::Mutex<()>> {
        // The entry API holds the map's lock across the whole
        // decision: two cold opens that arrive together must find the
        // *same* gate, or they would serialise on two locks and spawn
        // two controllers beside each other.
        match self.spawn_gates.entry(key.clone()) {
            Entry::Occupied(mut entry) => {
                if let Some(gate) = entry.get().upgrade() {
                    return gate;
                }
                // The entry's gate is gone — its controller died — so
                // the session gets a fresh lock.
                let gate = Arc::new(tokio::sync::Mutex::new(()));
                entry.insert(Arc::downgrade(&gate));
                gate
            }
            Entry::Vacant(entry) => {
                let gate = Arc::new(tokio::sync::Mutex::new(()));
                entry.insert(Arc::downgrade(&gate));
                gate
            }
        }
    }

    /// The owner a controller takes for its gate.
    ///
    /// The owner holds the map, the session's key and the lock: when
    /// the controller dies, the owner drops the lock and takes the
    /// map's entry with it.
    fn spawn_gate_owner(
        &self,
        key: &(String, String),
        gate: &Arc<tokio::sync::Mutex<()>>,
    ) -> SpawnGate {
        SpawnGate::new(
            Arc::clone(&self.spawn_gates),
            key.clone(),
            Arc::clone(gate),
        )
    }

    /// Drop the map entry of a gate nobody owns any more.
    ///
    /// The map holds a weak reference, so an entry whose gate is gone
    /// is dead weight: it would sit in the map for the life of the
    /// process. Only *this* gate's entry goes — a session that opened
    /// again in the meantime has its own.
    fn drop_unowned_gate(
        &self,
        key: &(String, String),
        gate: &Weak<tokio::sync::Mutex<()>>,
    ) {
        if gate.upgrade().is_some() {
            return;
        }
        self.spawn_gates
            .remove_if(key, |_, current| Weak::ptr_eq(current, gate));
    }

    /// Whether this session still has a cold-open gate entry.
    ///
    /// Test-only: the map is a spawn detail, not server state.
    #[cfg(test)]
    pub(crate) fn has_spawn_gate(&self, key: &(String, String)) -> bool {
        self.spawn_gates
            .get(key)
            .is_some_and(|entry| entry.value().upgrade().is_some())
    }

    /// How many cold-open gates the map holds.
    ///
    /// Test-only: the map is a spawn detail, not server state. The
    /// count is the growth proof — a map that keeps one entry per
    /// session this process opened grows with the sessions, not with
    /// the live ones.
    #[cfg(test)]
    pub(crate) fn spawn_gate_count(&self) -> usize {
        self.spawn_gates.len()
    }

    /// Take this session's cold-open gate and keep the guard.
    ///
    /// Test-only: a wedged open is a held lock, and a test cannot
    /// produce one through the filesystem without a fixture that hangs.
    #[cfg(test)]
    pub(crate) async fn hold_spawn_gate(
        &self,
        key: &(String, String),
    ) -> tokio::sync::OwnedMutexGuard<()> {
        self.session_spawn_gate(key).lock_owned().await
    }

    /// Forget a session's controller.
    ///
    /// The cold-open gate needs no eviction of its own: it belongs to
    /// the controller, so it leaves with the controller — through this
    /// path or through any other that drops it.
    pub(crate) fn forget_session(&self, key: &(String, String)) {
        self.active_sessions.remove(key);
    }

    /// Rebuild a parked prompt's parts from its durable row.
    ///
    /// The bytes come back from the attachment store by hash; a ref
    /// whose bytes are gone (an evicted store, a ref a different
    /// deployment wrote) is dropped with a warning rather than failing
    /// the whole prompt — the text is the turn.
    fn parts_from_parked(
        &self,
        prompt: &synthia::session::ParkedPromptRecovery,
    ) -> Vec<synthia::provider::ContentPart> {
        use base64::Engine as _;
        use synthia::provider::{
            ContentPart,
            ImageContent,
            ImageDetail,
            ResourceLink,
            TextContent,
            types::AudioContent,
        };

        let mut parts = Vec::new();
        if !prompt.text.is_empty() {
            parts.push(ContentPart::Text(TextContent {
                text: prompt.text.clone(),
                cache_control: None,
            }));
        }
        for attachment in &prompt.attachments {
            let Some(hash) = attachment.hash.as_deref() else {
                continue;
            };
            let data = match self.attachment_store.read_base64_raw(hash) {
                // The wire form the parts carry is base64.
                Ok(bytes) => {
                    base64::engine::general_purpose::STANDARD.encode(&bytes)
                }
                Err(error) => {
                    tracing::warn!(
                        target: "synthia.session",
                        %error,
                        hash,
                        "takeover: a parked attachment no longer resolves"
                    );
                    continue;
                }
            };
            let mime_type = attachment.mime_type.clone().unwrap_or_default();
            parts.push(match attachment.kind.as_str() {
                "image" => ContentPart::Image(ImageContent {
                    data,
                    mime_type,
                    detail: Some(ImageDetail::Auto),
                }),
                "audio" => ContentPart::Audio(AudioContent {
                    data,
                    mime_type,
                    format: None,
                }),
                _ => ContentPart::Resource(ResourceLink {
                    uri: format!("data:{mime_type};base64,{data}"),
                    name: attachment.name.clone().unwrap_or_default(),
                    title: None,
                    description: None,
                    mime_type: Some(mime_type),
                }),
            });
        }
        parts
    }
}

/// The row that consumes a parked prompt: the run's own record that a
/// queued turn reached the model.
fn is_consumption_marker(row: &serde_json::Value) -> bool {
    matches!(
        row.get("type").and_then(serde_json::Value::as_str),
        // The link this build writes …
        Some("park_consumed")
            // … and the one a log written before it relied on (see
            // `repair::parked_prompts`).
            | Some("UserInput")
    )
}
