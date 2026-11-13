//! The in-flight request table: one cancel token and deadline per
//! answered frame id.
//!
//! The stdio pump registers a request before it dispatches it and
//! removes it when the handler finishes. A `$/cancelRequest` for that
//! id cancels the token and removes the entry; the request's own task
//! wakes on the token, sees that it is no longer current, and writes
//! no response. A request that runs past its deadline is answered
//! with a `timeout` error and removed the same way.
//!
//! Notifications (frames without an `id`) are not registered: they
//! have no response to cancel.
use std::{
    collections::HashMap,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
    time::{Duration, Instant},
};

use serde_json::Value;
use uva::core::{AtomicCancelToken, CancelToken};

/// How long a request may run, and how to stop it early.
pub struct Inflight {
    entries: Mutex<HashMap<Value, Entry>>,
    timeout: Option<Duration>,
}

struct Entry {
    token: Arc<dyn CancelToken>,
    deadline: Option<Instant>,
}

impl Inflight {
    /// Build a table whose entries expire after `timeout`.
    ///
    /// `None` means a request waits indefinitely; cancellation is
    /// still available.
    #[must_use]
    pub fn new(timeout: Option<Duration>) -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
            timeout,
        }
    }

    /// Record one request and hand back its cancel token.
    ///
    /// The deadline the entry was registered with is available from
    /// [`Self::deadline`]. Ids are expected to be unique while in
    /// flight (the JSON-RPC contract); a duplicate replaces the
    /// earlier entry.
    pub fn register(&self, id: &Value) -> Arc<dyn CancelToken> {
        let token = AtomicCancelToken::shared();
        let deadline = self
            .timeout
            .and_then(|timeout| Instant::now().checked_add(timeout));
        self.entries().insert(
            id.clone(),
            Entry {
                token: Arc::clone(&token),
                deadline,
            },
        );
        token
    }

    /// The deadline registered for `id`, or `None` when the request
    /// has no deadline (or is no longer in flight).
    #[must_use]
    pub fn deadline(&self, id: &Value) -> Option<Instant> {
        self.entries().get(id).and_then(|entry| entry.deadline)
    }

    /// Cancel one in-flight request and forget it.
    ///
    /// Returns `false` when the id is unknown — the caller can answer
    /// a cancel *request* with `not_found`, and must not affect other
    /// requests.
    pub fn cancel(&self, id: &Value) -> bool {
        let Some(entry) = self.entries().remove(id) else {
            return false;
        };
        entry.token.cancel();
        true
    }

    /// Forget one request, reporting whether it was still current.
    ///
    /// A `false` answer means a cancel (or a deadline) already
    /// removed the entry, so the caller must not write a response.
    pub fn finish(&self, id: &Value) -> bool {
        self.entries().remove(id).is_some()
    }

    /// How many requests are in flight.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries().len()
    }

    /// Whether no request is in flight.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries().is_empty()
    }

    fn entries(&self) -> MutexGuard<'_, HashMap<Value, Entry>> {
        // No user code runs under the lock, so poisoning would be a
        // bug in this module; recover rather than let one panic take
        // the transport down.
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registers_a_deadline_and_a_token() {
        let inflight = Inflight::new(Some(Duration::from_millis(50)));
        let id = Value::from(7);
        let token = inflight.register(&id);
        assert!(!token.is_cancelled());
        assert!(inflight.deadline(&id).is_some());
        assert_eq!(inflight.len(), 1);
        assert!(!inflight.is_empty());
    }

    #[test]
    fn without_a_timeout_there_is_no_deadline() {
        let inflight = Inflight::new(None);
        let id = Value::from("c1");
        inflight.register(&id);
        assert!(inflight.deadline(&id).is_none());
    }

    #[test]
    fn the_deadline_leaves_with_the_entry() {
        let inflight = Inflight::new(Some(Duration::from_secs(30)));
        let id = Value::from(1);
        inflight.register(&id);
        assert!(inflight.finish(&id));
        assert!(inflight.deadline(&id).is_none());
    }

    #[test]
    fn cancel_removes_the_entry_and_signals_the_token() {
        let inflight = Inflight::new(None);
        let id = Value::from("c1");
        let token = inflight.register(&id);
        assert!(inflight.cancel(&id));
        assert!(token.is_cancelled());
        assert!(inflight.is_empty());
        assert!(!inflight.cancel(&id), "the entry is already gone");
    }

    #[test]
    fn cancelling_an_unknown_id_is_false() {
        let inflight = Inflight::new(None);
        assert!(!inflight.cancel(&Value::from(99)));
    }

    #[test]
    fn finish_reports_whether_the_request_was_still_current() {
        let inflight = Inflight::new(None);
        let id = Value::from(1);
        inflight.register(&id);
        assert!(inflight.finish(&id));
        assert!(!inflight.finish(&id), "a cancelled id is not current");
        let cancelled = Value::from(2);
        inflight.register(&cancelled);
        assert!(inflight.cancel(&cancelled));
        assert!(!inflight.finish(&cancelled));
    }
}
