//! The session crate's read/write path, with no runtime installed.
//!
//! The dependency gate is `cargo tree -p synthia-session -e normal`
//! (no tokio). This file guards the *behaviour* behind it: a JSONL
//! sink round-trips under a bare `futures` executor, so the crate is
//! not merely free of tokio on the manifest — it does not need a
//! reactor to write a row and read it back.

use std::sync::Arc;

use synthia_core::blocking::ThreadBlocking;
use synthia_session::{SessionSink, jsonl::JsonlSessionSink};

#[test]
fn a_jsonl_sink_round_trips_without_a_runtime() {
    let dir = tempfile::TempDir::new().unwrap();
    let sink = JsonlSessionSink::new("rt", dir.path())
        .with_blocking(Arc::new(ThreadBlocking));

    futures::executor::block_on(async {
        sink.append(&serde_json::json!({"type": "Test", "data": {"n": 1}}))
            .await
            .unwrap();
        sink.append(&serde_json::json!({"type": "Test", "data": {"n": 2}}))
            .await
            .unwrap();
        let rows = sink.read().await.unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["data"]["n"], 1);
        assert_eq!(rows[1]["data"]["n"], 2);
    });
}
