//! Per-session input queue: the bridge between
//! `submit(Prompt | Steer)` and the run loop.
//!
//! The queue is the server's, not the session crate's. A session's
//! durable log is `synthia-session`'s business; the text queue exists
//! only because this application multiplexes several submissions onto
//! one running agent, so it lives next to the controller that drains
//! it. (It used to sit in `synthia-session::manager` and reach callers
//! through the facade as `synthia::session::manager::InputQueue` — a
//! server concern in a runtime-free crate.)
//!
//! Backed by [`futures::lock::Mutex`] rather than
//! `tokio::sync::Mutex`: the lock is held across a `.await` boundary
//! only for the map operation itself, and the runtime-free mutex
//! keeps the type usable from any executor (the server's own tests
//! drive it on a tokio runtime; nothing about the type requires
//! tokio).

use std::{collections::HashMap, sync::Arc};

use futures::lock::Mutex;
use serde_json::Value;
use synthia::session::SessionError;

/// In-memory per-session input queue. The steering channel is owned
/// by the agent loop, but the controller still needs a small queue to
/// bridge between `submit(Prompt | Steer)` calls and the run loop.
///
/// Cloneable and cheap: every clone shares one map, so a caller that
/// holds a handle observes what the queued operations did.
#[derive(Clone, Default)]
pub struct InputQueue {
    inner: Arc<Mutex<HashMap<String, Vec<PendingEntry>>>>,
}

impl InputQueue {
    /// Push a new pending entry onto the session's queue. Returns the
    /// entries that were *also* queued at the same time (an artifact
    /// of the previous `VecDeque`-based API; preserved so the call
    /// site keeps compiling).
    ///
    /// Async-friendly: awaits the lock without blocking the runtime
    /// worker.
    pub async fn push(
        &self,
        _user_id: &str,
        session_id: &str,
        value: Value,
        _priority: Option<()>,
    ) -> Result<Vec<PendingEntry>, SessionError> {
        let entry = PendingEntry::from_value(value);
        let mut guard = self.inner.lock().await;
        let bucket = guard.entry(session_id.to_string()).or_default();
        bucket.push(entry.clone());
        Ok(vec![entry])
    }

    /// Whether the session has any pending input. The controller uses
    /// this to decide whether to start a run.
    pub async fn has_pending(&self, _user_id: &str, session_id: &str) -> bool {
        self.inner
            .lock()
            .await
            .get(session_id)
            .is_some_and(|v| !v.is_empty())
    }

    /// Drain all pending entries for the session. After the call the
    /// queue is empty.
    pub async fn drain_pending(
        &self,
        _user_id: &str,
        session_id: &str,
    ) -> Result<Vec<PendingEntry>, SessionError> {
        let mut guard = self.inner.lock().await;
        Ok(guard.remove(session_id).unwrap_or_default())
    }
}

/// One entry in the input queue. The controller folds `content` into
/// the next user message.
#[derive(Clone, Debug)]
pub struct PendingEntry {
    pub content: String,
}

impl PendingEntry {
    fn from_value(value: Value) -> Self {
        let content = match value {
            Value::String(s) => s,
            other => other.to_string(),
        };
        Self { content }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn input_queue_round_trips() {
        let q = InputQueue::default();
        q.push("alice", "s1", Value::String("hello".into()), None)
            .await
            .unwrap();
        assert!(q.has_pending("alice", "s1").await);
        let drained = q.drain_pending("alice", "s1").await.unwrap();
        assert_eq!(drained.len(), 1);
        assert_eq!(drained[0].content, "hello");
        assert!(!q.has_pending("alice", "s1").await);
    }

    /// The queue is per session: one session's pending input never
    /// answers for another's.
    #[tokio::test]
    async fn sessions_do_not_see_each_others_input() {
        let q = InputQueue::default();
        q.push("alice", "s1", Value::String("one".into()), None)
            .await
            .unwrap();
        assert!(!q.has_pending("bob", "s2").await);
        let drained = q.drain_pending("bob", "s2").await.unwrap();
        assert!(drained.is_empty());
        assert!(q.has_pending("alice", "s1").await);
    }
}
