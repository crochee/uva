//! `RedisBus` integration: `SET NX EX` leases and pub/sub frames
//! against a real Redis.
//!
//! Self-skipping: `ELYSPER_TEST_REDIS_URL` unset means the test prints
//! a note and returns, so CI without Redis stays green. Locally:
//!
//! ```text
//! docker run -d -p 6379:6379 redis:7
//! ELYSPER_TEST_REDIS_URL=redis://127.0.0.1:6379 \
//!   cargo test -p elysper-server --features cluster-redis --test cluster_redis_test
//! ```
#![cfg(feature = "cluster-redis")]

use std::time::Duration;

use elysper_server::cluster::{
    ControlFrame,
    RedisBus,
    SessionBus,
    SessionFrame,
    SessionKey,
};

const TTL: Duration = Duration::from_secs(15);

/// Poll-publish until the subscriber observes a frame (subscription
/// establishment is asynchronous on the Redis side, so an immediate
/// publish can race it).
async fn publish_until_received<T>(
    rx: &mut tokio::sync::mpsc::Receiver<T>,
    mut publish: impl FnMut(),
) -> Option<T> {
    for _ in 0..100 {
        publish();
        if let Ok(frame) =
            tokio::time::timeout(Duration::from_millis(20), rx.recv()).await
            && let Some(frame) = frame
        {
            return Some(frame);
        }
    }
    None
}

#[tokio::test]
async fn redis_leases_are_exclusive_and_releasable() {
    let Ok(url) = std::env::var("ELYSPER_TEST_REDIS_URL") else {
        eprintln!("SKIP: ELYSPER_TEST_REDIS_URL unset");
        return;
    };
    let namespace = format!("elysper-test-{}", std::process::id());
    let a = RedisBus::connect(&url, namespace.clone()).await.unwrap();
    let b = RedisBus::connect(&url, namespace.clone()).await.unwrap();
    let lease = format!("{namespace}:lease:session:u:s");

    assert!(a.try_acquire(&lease, "node-a", TTL).await.unwrap());
    // Re-entrant acquisition by the holder.
    assert!(a.try_acquire(&lease, "node-a", TTL).await.unwrap());
    // A foreign node is denied while the lease lives.
    assert!(!b.try_acquire(&lease, "node-b", TTL).await.unwrap());
    // Only the holder may renew.
    a.renew(&lease, "node-a", TTL).await.unwrap();
    assert!(b.renew(&lease, "node-b", TTL).await.is_err());
    // Release transfers ownership.
    a.release(&lease, "node-a").await.unwrap();
    assert!(b.try_acquire(&lease, "node-b", TTL).await.unwrap());
    b.release(&lease, "node-b").await.unwrap();
}

#[tokio::test]
async fn redis_pubsub_carries_session_and_control_frames() {
    let Ok(url) = std::env::var("ELYSPER_TEST_REDIS_URL") else {
        eprintln!("SKIP: ELYSPER_TEST_REDIS_URL unset");
        return;
    };
    let namespace = format!("elysper-test-{}", std::process::id());
    let a = RedisBus::connect(&url, namespace.clone()).await.unwrap();
    let b = RedisBus::connect(&url, namespace.clone()).await.unwrap();

    // Session frames.
    let key = SessionKey::new("u", "s");
    let mut rx = b.subscribe_session(&key).await.unwrap();
    let frame = SessionFrame {
        origin: "node-a".to_string(),
        cursor: Some(9),
        payload: serde_json::json!({"kind": "text"}),
    };
    let got = publish_until_received(&mut rx, || {
        let a = a.clone();
        let frame = frame.clone();
        let key = key.clone();
        tokio::spawn(async move {
            let _ = a.publish_session(&key, frame).await;
        });
    })
    .await
    .expect("a session frame within the retry budget");
    assert_eq!(got.cursor, Some(9));
    assert_eq!(got.payload["kind"], "text");

    // Control frames.
    let mut control = b.subscribe_control().await.unwrap();
    let control_frame = ControlFrame::SetDefaultAgent {
        origin: "node-a".to_string(),
        name: "reviewer".to_string(),
    };
    let got = publish_until_received(&mut control, || {
        let a = a.clone();
        let frame = control_frame.clone();
        tokio::spawn(async move {
            let _ = a.publish_control(frame).await;
        });
    })
    .await
    .expect("a control frame within the retry budget");
    match got {
        ControlFrame::SetDefaultAgent { name, .. } => {
            assert_eq!(name, "reviewer");
        }
        other => panic!("unexpected frame: {other:?}"),
    }
}
