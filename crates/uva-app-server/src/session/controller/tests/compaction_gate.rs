//! The compaction gate as the handle offers it: a promise about run
//! starts, not a lock.
//!
//! The gate's shape belongs to the controller's inner; this suite
//! pins what the handle promises, and what it promises for a handle
//! that owns no run at all.

use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use crate::{
    cluster::{ClusterRuntime, LocalBus},
    session::controller::SessionController,
};

/// A remote handle owns no run and no gate: there is nothing to
/// serialise a compaction pass against, so the work runs as it would
/// anywhere else.
#[tokio::test]
async fn a_remote_handle_runs_the_work_without_a_gate() {
    let cluster = Arc::new(ClusterRuntime {
        bus: Arc::new(LocalBus::new()),
        node_id: "node".to_string(),
        namespace: "test".to_string(),
        lease_ttl: Duration::from_secs(30),
        clustered: true,
    });
    let remote = SessionController::spawn_remote("u", "s", cluster);
    assert!(remote.is_remote(), "the handle forwards, it does not own");

    let ran = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&ran);
    remote
        .with_compaction_gate(async move {
            flag.store(true, Ordering::SeqCst);
        })
        .await;

    assert!(
        ran.load(Ordering::SeqCst),
        "a remote handle runs the work; there is no gate to hold"
    );
}
