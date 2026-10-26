//! The structural rows a run produces travel through the session's
//! one writer: the log holds them, and every row carries the ordinal
//! the reader reproduces.

use std::sync::Arc;

use futures::StreamExt;
use synthia_core::SystemClock;
use synthia_session::{LogWriter, SessionSink, in_memory::InMemorySessionSink};
use synthia_tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use super::super::{support::*, *};

/// A run's structural rows — the step boundary and the usage frame —
/// reach the log through the writer the agent was given, and each row
/// carries the ordinal it is written under.
pub(super) async fn structural_rows_go_through_the_writer() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let provider =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("all done")]));

    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
        .with_log_writer(writer);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read");
    let kinds: Vec<&str> =
        rows.iter().filter_map(|row| row["type"].as_str()).collect();
    assert!(
        kinds.contains(&"step"),
        "the step boundary must reach the log: {kinds:?}"
    );
    assert!(
        kinds.contains(&"usage"),
        "the usage frame must reach the log: {kinds:?}"
    );
    // One run leaves one request header: the harness stamps the
    // run's epoch marker, and nothing else stamps a second one.
    assert_eq!(
        kinds
            .iter()
            .filter(|kind| **kind == "request_header")
            .count(),
        1,
        "one request header per run: {kinds:?}"
    );
    // `seq: 0` is the typed builder's default — the unstamped shape —
    // so a non-zero ordinal is what proves the writer stamped the
    // row, and contiguous ordinals prove they came from one counter.
    let seqs: Vec<u64> =
        rows.iter()
            .map(|row| {
                row["seq"].as_u64().filter(|seq| *seq > 0).unwrap_or_else(
                    || panic!("every row carries a stamped ordinal: {row}"),
                )
            })
            .collect();
    assert_eq!(
        seqs,
        (1..=seqs.len() as u64).collect::<Vec<u64>>(),
        "the ordinals are non-zero and contiguous"
    );
}
