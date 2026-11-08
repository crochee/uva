//! The structural rows a run produces travel through the session's
//! one writer: the log holds them, and every row carries the ordinal
//! the reader reproduces.

use std::sync::Arc;

use elysper_core::SystemClock;
use elysper_session::{LogWriter, SessionSink, in_memory::InMemorySessionSink};
use elysper_tool::ToolRegistry;
use futures::StreamExt;
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

/// The header names the provider the deployment configured, not the
/// handle's own name. An OpenAI-compatible handle reports its
/// *model* from `name()`, so a row built from the handle names the
/// model twice and the provider cannot be recovered from the log.
pub(super) async fn the_request_header_names_the_configured_provider() {
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
    let header = rows
        .iter()
        .find(|row| row["type"] == "request_header")
        .expect("a request header row");
    assert_eq!(
        header["data"]["provider"], "scripted",
        "the header names the configured provider id"
    );
    assert_eq!(header["data"]["model"], "fake");
    let snapshot = rows
        .iter()
        .find(|row| row["type"] == "request_snapshot")
        .expect("a request snapshot row");
    assert_eq!(
        snapshot["provider"], "scripted",
        "the snapshot names the configured provider id"
    );
}

/// A failed turn records *why* it failed.
///
/// The turn-end row names the outcome (`error`), but the message the
/// run stopped with is what a reader actually needs: the page shows
/// it as a banner, and an operator reading the log should not have to
/// guess from one word.
pub(super) async fn a_failed_turn_records_its_message() {
    use elysper_core::Error;
    use elysper_provider::{
        CompletionRequest,
        CompletionResponse,
        ModelConfig,
        ModelProvider,
        ProviderConfig,
    };

    // A provider that refuses every sample.
    struct RefusingProvider;

    #[async_trait]
    impl ModelProvider for RefusingProvider {
        async fn initialize(
            &mut self,
            _config: ProviderConfig,
        ) -> Result<(), Error> {
            Ok(())
        }

        fn name(&self) -> &str {
            "refusing"
        }

        fn model_config(&self) -> ModelConfig {
            ModelConfig {
                name: "fake".to_string(),
                provider: "refusing".to_string(),
                context_window: 128_000,
                max_output_tokens: 4096,
                supports_tools: true,
                supports_streaming: true,
                supports_reasoning: false,
            }
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, Error> {
            Err(Error::internal("the provider refused"))
        }
    }

    let sink = Arc::new(InMemorySessionSink::new("s"));
    let writer = Arc::new(LogWriter::new(sink.clone(), Arc::new(SystemClock)));
    let agent = ReActAgent::new(
        Arc::new(RefusingProvider),
        Arc::new(ToolRegistry::new()),
    )
    .with_log_writer(writer);
    let mut stream = agent
        .run(AgentInput::text("go"), Arc::new(CancellationToken::new()))
        .await;
    while stream.next().await.is_some() {}

    let rows = sink.read().await.expect("read");
    let end = rows
        .iter()
        .find(|row| row["type"] == "turn" && row["data"]["kind"] == "end")
        .expect("a turn end row");
    assert_eq!(end["data"]["reason"], "error");
    let message = end["data"]["message"]
        .as_str()
        .expect("the failed turn carries its message");
    assert!(
        message.contains("the provider refused"),
        "the message names the failure: {message}"
    );
}
