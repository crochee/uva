//! Durable streaming cadence: a long tool call's output reaches the log
//! while the tool runs, not only when it ends.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uva_core::SystemClock;
use uva_provider::ToolUse;
use uva_session::{LogWriter, SessionLog, in_memory::InMemorySessionSink};
use uva_tool::{Context, StreamOutput, Tool, ToolOutput, ToolRegistry};

use super::super::{support::*, *};

/// A long streamed call is durable *while* it runs.
///
/// The dispatch loop flushes every `TOOL_OUTPUT_FLUSH_CHUNKS` chunks,
/// so a process that dies mid-tool loses at most the chunks since the
/// last boundary instead of the call's whole progress. The probe
/// reads the log from inside the stream — through the same writer the
/// run writes through — and sees a `tool_output` row before the tool
/// has produced its result.
pub(super) async fn a_long_stream_is_durable_before_the_tool_finishes() {
    const CHUNKS: usize =
        crate::agent::re_act::r#loop::step::TOOL_OUTPUT_FLUSH_CHUNKS * 2;

    struct Probe {
        log: Arc<dyn SessionLog>,
        seen_mid_stream: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Tool for Probe {
        fn name(&self) -> &str {
            "probe"
        }

        fn description(&self) -> &str {
            "streams more chunks than one flush window"
        }

        fn parameters(&self) -> serde_json::Value {
            json!({"type": "object"})
        }

        async fn call(
            &self,
            _input: serde_json::Value,
            _ctx: &Context,
        ) -> ToolOutput {
            ToolOutput::text("done")
        }

        fn stream<'a>(
            &'a self,
            _input: serde_json::Value,
            _ctx: &'a Context,
        ) -> std::pin::Pin<
            Box<dyn futures::Stream<Item = StreamOutput> + Send + 'a>,
        > {
            let log = Arc::clone(&self.log);
            let seen = Arc::clone(&self.seen_mid_stream);
            Box::pin(futures::stream::unfold(0usize, move |index| {
                let log = Arc::clone(&log);
                let seen = Arc::clone(&seen);
                async move {
                    if index > CHUNKS {
                        return None;
                    }
                    if index == CHUNKS {
                        return Some((
                            StreamOutput::Result(ToolOutput::text("done")),
                            index + 1,
                        ));
                    }
                    if index + 1 == CHUNKS {
                        // The last chunk is yielded after the second
                        // flush boundary; whatever the log holds now is
                        // what a crash at this instant keeps.
                        let rows = log.read().await.expect("read the log");
                        let stored = rows
                            .iter()
                            .filter(|row| row["type"] == "tool_output")
                            .count();
                        seen.store(stored, Ordering::SeqCst);
                    }
                    Some((
                        StreamOutput::Progress(ToolOutput::text("x")),
                        index + 1,
                    ))
                }
            }))
        }
    }

    let sink = Arc::new(InMemorySessionSink::new("log"));
    let writer = Arc::new(LogWriter::new(sink, Arc::new(SystemClock)));
    let seen = Arc::new(AtomicUsize::new(0));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(uva_tool::ToolEntry::new(Arc::new(Probe {
        log: Arc::clone(&writer) as Arc<dyn SessionLog>,
        seen_mid_stream: Arc::clone(&seen),
    })));
    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "call_probe".to_string(),
            name: "probe".to_string(),
            input: json!({}),
        }]),
        done_chunk("done"),
    ]));
    let agent = ReActAgent::new(
        Arc::clone(&provider) as Arc<dyn uva_provider::ModelProvider>,
        registry,
    )
    .with_log_writer(Arc::clone(&writer) as Arc<dyn SessionLog>);
    let mut stream = agent
        .run(
            AgentInput::text("probe"),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while stream.next().await.is_some() {}

    assert_eq!(
        seen.load(Ordering::SeqCst),
        1,
        "the first flush boundary landed a row before the tool finished"
    );

    writer.flush().await.expect("flush the run's rows");
    let rows = writer.read().await.expect("read the session log");
    let chunks = rows
        .iter()
        .filter(|row| row["type"] == "tool_output")
        .count();
    assert_eq!(
        chunks, 2,
        "one row per flush window, not one per chunk: {chunks}"
    );
}
