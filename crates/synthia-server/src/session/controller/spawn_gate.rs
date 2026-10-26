//! The cold-open gate a session's controller owns.

use std::sync::{Arc, Weak};

use tokio::sync::Mutex;

use crate::state::SessionSpawnGates;

/// The cold-open gate of one session.
///
/// The gate map holds a **weak** reference to the lock and the
/// controller holds the strong one, so the two share a lifetime: when
/// the controller dies, the lock dies with it and the map's entry goes
/// with the lock. That is what keeps the map from growing with the
/// number of sessions the process has ever opened — the entry count
/// follows the live controllers, not the opened sessions.
///
/// A session that goes cold again — an idle shutdown, a takeover —
/// still finds the gate its controller left behind, because that
/// controller is alive; a session whose controller is gone finds a
/// fresh one.
pub(crate) struct SpawnGate {
    gates: Arc<SessionSpawnGates>,
    key: (String, String),
    lock: Option<Arc<Mutex<()>>>,
}

impl SpawnGate {
    /// Take ownership of a session's gate.
    pub(crate) fn new(
        gates: Arc<SessionSpawnGates>,
        key: (String, String),
        lock: Arc<Mutex<()>>,
    ) -> Self {
        Self {
            gates,
            key,
            lock: Some(lock),
        }
    }
}

impl Drop for SpawnGate {
    fn drop(&mut self) {
        let Some(lock) = self.lock.take() else {
            return;
        };
        let registered = Arc::downgrade(&lock);
        drop(lock);
        // Another owner may still hold the lock — a controller that
        // replaced this one owns the same gate — and then the map's
        // entry is still live and must stay.
        if registered.upgrade().is_some() {
            return;
        }
        self.gates.remove_if(&self.key, |_, current| {
            Weak::ptr_eq(current, &registered)
        });
    }
}
