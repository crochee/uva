//! The server→client notification fan-out.
//!
//! A client that follows a session gets the baseline as the
//! `sessions/follow` result and every later change as a
//! `session/event` notification. An interrupt set carries a second
//! projection beside it: one `approval/request` per held call, so a
//! transport that speaks approvals can render the suspension without
//! decoding the session projection. `approval_id` there is the UI
//! correlation id; the durable truth stays the log's
//! `approval_decided` rows and the `sessions/resume` + `ResumeToken`
//! path, never the notification.
//!
//! # The subscriber table
//!
//! One follower per session id. A second `sessions/follow` for the
//! same session replaces the first (the newer subscriber owns the
//! projection from then on); `session/unfollow` stops it. The
//! process is single-user, so one subscriber per session is the whole
//! story — two browser tabs on the same session share it.
use std::{future::Future, sync::Arc};

use dashmap::DashMap;
use tokio::task::JoinHandle;
use uva::harness::ApprovalRequest;

use super::Outbound;

/// The notification method a session's events travel on.
pub const SESSION_EVENT: &str = "session/event";
/// The notification a profile write broadcasts after it commits.
///
/// It carries no `session_id`: the gateway broadcasts an unscoped
/// notification to every live connection, so the settings page and any
/// other client see one write.
pub const PROFILE_CHANGED: &str = "profile/changed";
/// The notification method a held call is projected on.
///
/// It is a notification, not a request: the client answers with the
/// separate `approval/respond` request, which the core maps onto
/// `sessions/resume`. `approval_id` correlates the two; it is never
/// the JSON-RPC `id`.
pub const APPROVAL_REQUEST: &str = "approval/request";
/// The per-session follow subscribers.
pub struct FollowRegistry {
    outbound: Outbound,
    followers: DashMap<String, JoinHandle<()>>,
}
impl FollowRegistry {
    /// Build the registry around an outbound channel.
    pub fn new(outbound: Outbound) -> Self {
        Self {
            outbound,
            followers: DashMap::new(),
        }
    }

    /// Start the follower for a session.
    ///
    /// A follower already registered for the same session is stopped
    /// first: the newer subscription owns the projection from then
    /// on.
    pub fn start<F>(&self, session_id: String, task: F)
    where
        F: Future<Output = ()> + Send + 'static,
    {
        self.stop(&session_id);
        let handle = tokio::spawn(task);
        self.followers.insert(session_id, handle);
    }

    /// Stop the follower for a session, if there is one.
    pub fn stop(&self, session_id: &str) {
        if let Some((_, handle)) = self.followers.remove(session_id) {
            handle.abort();
        }
    }

    /// How many sessions are being followed.
    pub fn len(&self) -> usize {
        self.followers.len()
    }

    /// Whether no session is being followed.
    pub fn is_empty(&self) -> bool {
        self.followers.is_empty()
    }

    /// Emit one `session/event` notification.
    pub fn emit_session_event(
        &self,
        session_id: &str,
        payload: serde_json::Value,
    ) {
        let mut params = payload;
        if let Some(object) = params.as_object_mut() {
            object.insert(
                "session_id".to_string(),
                serde_json::Value::String(session_id.to_string()),
            );
        }
        self.outbound.notify(SESSION_EVENT, params);
    }

    /// Emit the `approval/request` projection of one held call.
    ///
    /// The payload carries what a dialog renders and what correlates
    /// the answer: the session it belongs to, the approval id, the
    /// call id, the tool, its risk, the arguments digest and the
    /// agent. It carries **no** resume token and no decision state —
    /// a reconnect re-reads the suspension from the log, and the
    /// decision's durable home is `sessions/resume`.
    pub fn emit_approval_request(
        &self,
        session_id: &str,
        request: &ApprovalRequest,
    ) {
        self.outbound.notify(
            APPROVAL_REQUEST,
            serde_json::json!({
                "session_id": session_id,
                "approval_id": request.id,
                "call_id": request.call_id,
                "tool": request.tool,
                "risk": request.risk,
                "arguments_hash": request.arguments_hash,
                "agent": request.agent,
            }),
        );
    }
}
/// The shared handle the process keeps on [`crate::state::AppState`].
pub type SharedFollowRegistry = Arc<FollowRegistry>;
#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio::sync::mpsc;

    use super::*;
    #[tokio::test]
    async fn a_started_follower_is_replaced_by_the_next_one() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let registry = FollowRegistry::new(Outbound::new(tx));
        registry.start("s1".to_string(), async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });
        assert_eq!(registry.len(), 1);
        registry.start("s1".to_string(), async {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
        });
        assert_eq!(registry.len(), 1);
        registry.stop("s1");
        assert!(registry.is_empty());
        // The stopped follower never emits.
        assert!(rx.try_recv().is_err());
    }
    #[tokio::test]
    async fn an_approval_request_carries_the_correlation_id() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let registry = FollowRegistry::new(Outbound::new(tx));
        let request = ApprovalRequest::new(
            "a1",
            "c1",
            "shell",
            "sha256:args",
            uva::tool::Risk::High,
            "default",
        );
        registry.emit_approval_request("s1", &request);
        let envelope = rx.try_recv().expect("an envelope is queued");
        assert_eq!(envelope["method"], APPROVAL_REQUEST);
        assert_eq!(
            envelope["params"],
            json!({
                "session_id": "s1",
                "approval_id": "a1",
                "call_id": "c1",
                "tool": "shell",
                "risk": "high",
                "arguments_hash": "sha256:args",
                "agent": "default",
            })
        );
    }
    #[tokio::test]
    async fn a_session_event_carries_the_session_id() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let registry = FollowRegistry::new(Outbound::new(tx));
        registry.emit_session_event("s1", json!({ "kind": "live" }));
        let envelope = rx.try_recv().expect("an envelope is queued");
        assert_eq!(envelope["method"], SESSION_EVENT);
        assert_eq!(envelope["params"]["session_id"], "s1");
        assert_eq!(envelope["params"]["kind"], "live");
    }
}
