//! The log writer's step-boundary flush: a run's structural rows
//! are durable when the run ends, and each row carries the ordinal
//! the log holds it under.

use std::sync::Arc;

use futures::StreamExt;
use synthia_session::{SessionSink, in_memory::InMemorySessionSink};
use synthia_tool::ToolRegistry;
use tokio_util::sync::CancellationToken;

use super::super::{support::*, *};
use crate::{Durability, LogWriter};

/// A finished run leaves no row in the writer's buffer: the step
/// boundary and the final flush wrote them all, and each row's own
/// `seq` is the ordinal the reader reproduces.
pub(super) async fn a_completed_run_flushes_its_structural_rows() {
    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer =
        Arc::new(LogWriter::new(sink.clone(), Durability::StepBoundary));
    let provider =
        Arc::new(ScriptedStreamProvider::new(vec![done_chunk("all done")]));

    let agent = ReActAgent::new(provider, Arc::new(ToolRegistry::new()))
        .with_log_writer(writer);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read");
    assert!(
        !rows.is_empty(),
        "the run's structural rows must reach the sink"
    );
    let seqs: Vec<u64> = rows
        .iter()
        .map(|row| row["seq"].as_u64().expect("stamped ordinal"))
        .collect();
    assert_eq!(
        seqs,
        (1..=seqs.len() as u64).collect::<Vec<u64>>(),
        "the ordinals are contiguous and the rows carry their own"
    );
}
