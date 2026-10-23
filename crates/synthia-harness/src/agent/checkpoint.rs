//! Durability seam for in-flight session snapshots. The loop loads `<thread_id>:<agent_name>` at setup and persists on every non-`Completed` end. Cross-restart durability is the host's job.
use std::{collections::HashMap, sync::Arc};

use parking_lot::RwLock;

/// Per-session serialised snapshot, persisted to a [`Checkpointer`].
#[derive(Debug, Clone)]
pub struct CheckpointSnapshot {
    /// Thread identifier; resumption keys on `thread_id`.
    pub thread_id: String,
    /// Agent identifier (the `AgentDescriptor::name`).
    pub agent_id: String,
    /// Iteration index when the snapshot was taken; `0` for a pre-loop snapshot.
    pub iteration: usize,
    /// The transcript at the moment of the snapshot. Shared behind an
    /// `Arc` so persisting a snapshot of a live transcript is a refcount
    /// bump, not a full deep copy.
    pub messages: Arc<Vec<synthia_provider::Message>>,
    /// Wall-clock snapshot time (UTC).
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl CheckpointSnapshot {
    /// Convenience constructor: the caller supplies `now`, so this stays free of `chrono::Utc::now()` and `make check-clock` stays
    /// clean. Production code passes `self.runtime.clock.now()`; tests a `FixedClock`.
    #[must_use]
    pub(crate) fn at(
        thread_id: impl Into<String>,
        agent_id: impl Into<String>,
        iteration: usize,
        messages: impl Into<Arc<Vec<synthia_provider::Message>>>,
        now: chrono::DateTime<chrono::Utc>,
    ) -> Self {
        Self {
            thread_id: thread_id.into(),
            agent_id: agent_id.into(),
            iteration,
            messages: messages.into(),
            created_at: now,
        }
    }
}

/// Errors produced by a [`Checkpointer`] backend.
#[derive(Debug, thiserror::Error)]
pub enum CheckpointError {
    #[error("checkpoint backend I/O error: {0}")]
    Io(String),
    #[error("checkpoint snapshot corrupted: {0}")]
    Corrupted(String),
}

/// The persistence seam the loop uses for durable HITL / pause /
/// resume. All four methods are synchronous by design.
pub trait Checkpointer: Send + Sync {
    /// Persist `snapshot` under `key`, overwriting any existing value.
    fn save(
        &self,
        key: &str,
        snapshot: &CheckpointSnapshot,
    ) -> Result<(), CheckpointError>;

    fn load(
        &self,
        key: &str,
    ) -> Result<Option<CheckpointSnapshot>, CheckpointError>;

    /// Delete the snapshot under `key`; a missing key is a no-op.
    fn delete(&self, key: &str) -> Result<(), CheckpointError>;
}

/// Process-local [`Checkpointer`]: a `HashMap` under `parking_lot::RwLock`. Cross-restart durability requires a host-provided
/// backend; `Clone` is Arc-backed, so the handle can be cloned into the loop without moving data.
#[derive(Default)]
pub struct InMemoryCheckpointer {
    inner: Arc<RwLock<HashMap<String, CheckpointSnapshot>>>,
}

impl InMemoryCheckpointer {
    /// Keys of every stored snapshot, sorted.
    #[must_use]
    pub fn list_keys(&self) -> Vec<String> {
        let mut keys = self.inner.read().keys().cloned().collect::<Vec<_>>();
        keys.sort();
        keys
    }
}

impl Clone for InMemoryCheckpointer {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl Checkpointer for InMemoryCheckpointer {
    fn save(
        &self,
        key: &str,
        snapshot: &CheckpointSnapshot,
    ) -> Result<(), CheckpointError> {
        self.inner.write().insert(key.to_string(), snapshot.clone());
        Ok(())
    }

    fn load(
        &self,
        key: &str,
    ) -> Result<Option<CheckpointSnapshot>, CheckpointError> {
        Ok(self.inner.read().get(key).cloned())
    }

    fn delete(&self, key: &str) -> Result<(), CheckpointError> {
        self.inner.write().remove(key);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use synthia_provider::{Message, Role};

    use super::*;

    fn snap(thread: &str, msgs: Vec<Message>) -> CheckpointSnapshot {
        CheckpointSnapshot::at(
            thread,
            "agent_a",
            0,
            msgs,
            chrono::DateTime::from_timestamp(0, 0).unwrap(),
        )
    }

    #[test]
    fn in_memory_round_trip_returns_same_snapshot() {
        let cp = InMemoryCheckpointer::default();
        let s = snap(
            "thread-1",
            vec![Message::system("hi"), Message::tool("ok", "c1")],
        );
        cp.save("t1", &s).unwrap();
        let loaded = cp.load("t1").unwrap().expect("present");
        assert_eq!(loaded.thread_id, "thread-1");
        assert_eq!(loaded.agent_id, "agent_a");
        assert_eq!(loaded.messages.len(), 2);
        // The clone carried the data verbatim.
        assert_eq!(loaded.messages[0].role, Role::System);
        assert_eq!(loaded.messages[1].role, Role::Tool);
    }

    #[test]
    fn delete_removes_key_and_list_keys_drops_it() {
        let cp = InMemoryCheckpointer::default();
        cp.save("k1", &snap("t", vec![])).unwrap();
        cp.save("k2", &snap("t", vec![])).unwrap();
        assert_eq!(cp.list_keys().len(), 2);
        cp.delete("k1").unwrap();
        assert!(cp.load("k1").unwrap().is_none());
        cp.delete("never-existed").unwrap();
        let keys = cp.list_keys();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0], "k2");
    }

    #[test]
    fn missing_key_load_returns_none_not_error() {
        let cp = InMemoryCheckpointer::default();
        let loaded = cp.load("nope").unwrap();
        assert!(loaded.is_none());
    }

    #[test]
    fn empty_list_keys_on_fresh_checkpointer() {
        let cp = InMemoryCheckpointer::default();
        assert!(cp.list_keys().is_empty());
    }

    #[test]
    fn clone_of_checkpointer_shares_state() {
        let cp = InMemoryCheckpointer::default();
        let handle = cp.clone();
        cp.save("k", &snap("t", vec![])).unwrap();
        assert!(handle.load("k").unwrap().is_some());
        // Mutating through the clone is also visible to the
        // original.
        handle.delete("k").unwrap();
        assert!(cp.load("k").unwrap().is_none());
    }

    #[test]
    fn save_overwrites_existing_key() {
        let cp = InMemoryCheckpointer::default();
        let mut a = snap("t", vec![Message::system("a")]);
        let b = snap("t", vec![Message::system("b")]);
        a.created_at =
            chrono::DateTime::parse_from_rfc3339("2026-01-01T00:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc);
        cp.save("k", &a).unwrap();
        cp.save("k", &b).unwrap();
        let loaded = cp.load("k").unwrap().unwrap();
        assert_eq!(loaded.messages.len(), 1);
        let text = loaded.messages[0]
            .content
            .extract_text()
            .unwrap_or_default();
        assert_eq!(text, "b");
    }
}
