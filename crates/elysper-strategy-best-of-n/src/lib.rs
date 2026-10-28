//! [`BestOfNStrategy`] — N parallel samples, scored, the
//! winner published via [`EventSink`].
use std::{sync::Arc, time::Instant};

use async_trait::async_trait;
use futures::{StreamExt as _, channel::mpsc};
use elysper_core::CancelToken;
use elysper_harness::{
    AgentEvent,
    AgentInput,
    EventSink,
    ReasoningStrategy,
    SystemEvent,
    agent::strategy::{
        AgentRuntime,
        prepare_messages_for_shot,
        single_shot_request_shared,
    },
};
use elysper_provider::{
    CompletionRequest,
    ContentPart,
    ModelProvider,
    SamplingResult,
    TextContent,
    completion_to_sampling,
};
use elysper_session::{step_end, step_start};

pub mod judge;

/// Scores one candidate answer; **higher wins**.
///
/// Implementations run on the strategy's own spawned task; an
/// LLM-as-judge scorer may make a provider call inside `score`.
/// The score is reported in progress events, so it should be
/// meaningful to an operator — not a secret rank.
#[async_trait]
pub trait CandidateScorer: Send + Sync + 'static {
    /// Score `candidate` (the answer text, never empty — empty
    /// completions are rejected as failures before scoring).
    async fn score(&self, candidate: &str) -> f64;

    /// Label for logs, e.g. `"longest-answer"`.
    fn name(&self) -> &str {
        "candidate-scorer"
    }
}

/// The default scorer: prefer the longest answer — a crude proxy
/// for "most complete". Replace it with anything that has a real
/// signal.
#[derive(Debug, Clone, Copy, Default)]
pub struct LongestAnswer;

#[async_trait]
impl CandidateScorer for LongestAnswer {
    async fn score(&self, candidate: &str) -> f64 {
        // Characters, not bytes: a CJK answer must not outrank
        // an ASCII one of the same length for byte-count
        // reasons.
        candidate.chars().count() as f64
    }

    fn name(&self) -> &str {
        "longest-answer"
    }
}

pub use judge::{DEFAULT_JUDGE_PROMPT, LlmJudgeScorer};

/// How many candidates the strategy samples by default.
pub(crate) const DEFAULT_CANDIDATES: usize = 3;

/// One scored candidate: the sample index it was drawn from, its score,
/// and its text.
type Scored = (usize, f64, String);

/// Sample one candidate. Streams (not `complete`) so the
/// candidate honours cancellation; deltas are discarded —
/// alternatives are not a live answer.
async fn sample_candidate(
    provider: Arc<dyn ModelProvider>,
    cancel: Arc<dyn CancelToken>,
    request: CompletionRequest,
    index: usize,
    tx: mpsc::UnboundedSender<(usize, Result<String, String>)>,
) {
    let result = provider
        .complete_with_stream(request, Some(cancel), Box::new(|_chunk| {}))
        .await
        .map(|response| completion_to_sampling(&response).text)
        .map_err(|err| err.to_string());
    let _ = tx.unbounded_send((index, result));
}

/// Sample `candidates` answers concurrently, score them, publish the
/// winner.
pub struct BestOfNStrategy {
    candidates: usize,
    scorer: Arc<dyn CandidateScorer>,
}

impl std::fmt::Debug for BestOfNStrategy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BestOfNStrategy")
            .field("candidates", &self.candidates)
            .field("scorer", &self.scorer.name())
            .finish()
    }
}

impl Default for BestOfNStrategy {
    fn default() -> Self {
        Self::new(DEFAULT_CANDIDATES, Arc::new(LongestAnswer))
    }
}

impl BestOfNStrategy {
    /// Sample `candidates` answers and keep the highest-scoring one.
    /// `candidates` clamps to at least 1.
    #[must_use]
    pub fn new(candidates: usize, scorer: Arc<dyn CandidateScorer>) -> Self {
        Self {
            candidates: candidates.max(1),
            scorer,
        }
    }

    fn fan_out(
        &self,
        runtime: &AgentRuntime,
        request: &CompletionRequest,
    ) -> mpsc::UnboundedReceiver<(usize, Result<String, String>)> {
        let (tx, rx) = mpsc::unbounded::<(usize, Result<String, String>)>();
        for index in 0..self.candidates {
            let provider = Arc::clone(&runtime.caps.model.provider);
            let cancel = Arc::clone(&runtime.cancel);
            runtime.caps.env.spawner.spawn(Box::pin(sample_candidate(
                provider,
                cancel,
                request.clone(),
                index,
                tx.clone(),
            )));
        }
        drop(tx);
        rx
    }

    /// Ask the model `candidates` times concurrently, then score
    /// every usable answer concurrently (an LLM-judge scorer's N
    /// provider calls overlap instead of queueing). Returns the
    /// scored answers plus one failure message per candidate that
    /// produced nothing usable.
    async fn sample_and_score(
        &self,
        runtime: &AgentRuntime,
        request: &CompletionRequest,
        sink: &EventSink,
    ) -> (Vec<Scored>, Vec<String>) {
        // The prompt (and everything queued before it) lands before
        // any candidate samples.
        if let Err(error) = sink.flush_log().await {
            return (Vec::new(), vec![format!("log flush failed: {error}")]);
        }
        let mut answers = self.fan_out(runtime, request);
        let mut arrivals: Vec<(usize, String)> = Vec::new();
        let mut failures: Vec<String> = Vec::new();
        while let Some((index, result)) = answers.next().await {
            match result {
                Ok(text) if !text.trim().is_empty() => {
                    arrivals.push((index, text));
                }
                Ok(_) => failures.push(format!(
                    "candidate {index} produced an empty answer"
                )),
                Err(message) => {
                    failures
                        .push(format!("candidate {index} failed: {message}"));
                }
            }
        }
        let scorer = Arc::clone(&self.scorer);
        let scored = futures::future::join_all(arrivals.into_iter().map(
            |(index, text)| {
                let scorer = Arc::clone(&scorer);
                async move {
                    let score = scorer.score(&text).await;
                    (index, score, text)
                }
            },
        ))
        .await;
        for (step, (index, score, _)) in scored.iter().enumerate() {
            sink.emit(AgentEvent::System(SystemEvent::Progress {
                message: format!(
                    "candidate (index {index}) scored {score:.3} via {}",
                    self.scorer.name(),
                ),
                step: step + 1,
                total: self.candidates,
            }));
        }
        (scored, failures)
    }

    /// Publish the winner, or a typed failure when nothing was usable.
    fn publish(
        &self,
        runtime: &AgentRuntime,
        sink: &EventSink,
        started: Instant,
        scored: Vec<Scored>,
        failures: Vec<String>,
    ) {
        // Highest score wins; a tie goes to the lowest index so a run
        // with several equally good answers is reproducible.
        let scored_count = scored.len();
        let winner = {
            let mut ranked = scored;
            ranked.sort_by(|a, b| {
                b.1.partial_cmp(&a.1)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.0.cmp(&b.0))
            });
            ranked.into_iter().next()
        };

        let Some((index, score, text)) = winner else {
            // Every candidate failed. The first failure explains
            // why; the rest are logged, not concatenated, so a
            // quota error is not buried under four timeouts.
            for failure in failures.iter().skip(1) {
                tracing::warn!(%failure, "best-of-n candidate failed");
            }
            let message = failures
                .first()
                .cloned()
                .unwrap_or_else(|| "no candidates were sampled".to_string());
            sink.fail(format!(
                "best-of-n produced no usable answer: {message}"
            ));
            return;
        };

        sink.emit(AgentEvent::System(SystemEvent::Progress {
            message: format!(
                "selected candidate {index} (score {score:.3}) of \
                 {scored_count} in {:?}",
                started.elapsed()
            ),
            step: self.candidates,
            total: self.candidates,
        }));
        sink.emit(AgentEvent::Model(ContentPart::Text(TextContent {
            text: text.clone(),
            cache_control: None,
        })));
        // The winner is the run's durable fact: one
        // `assistant_message` row through the session's writer. The
        // alternatives were never published, so they leave no row.
        sink.write_assistant_turn(&[ContentPart::Text(TextContent {
            text: text.clone(),
            cache_control: None,
        })]);
        sink.emit(AgentEvent::ModelDone(SamplingResult {
            text,
            usage: Default::default(),
            ..Default::default()
        }));
        let finish_token = Arc::clone(&runtime.cancel);
        sink.finish(&finish_token);
    }
}

#[async_trait]
impl ReasoningStrategy for BestOfNStrategy {
    fn name(&self) -> &str {
        "best-of-n"
    }

    async fn run(
        &self,
        runtime: AgentRuntime,
        input: AgentInput,
        sink: &EventSink,
    ) {
        let started = Instant::now();
        let begin_token = Arc::clone(&runtime.cancel);
        if !sink.begin(&begin_token) {
            return;
        }
        // The run's epoch marker: one header naming the request
        // envelope this run's facts are produced under.
        sink.stamp_request_header(&runtime.caps, &[]);

        // The prompt lands before the context manager's summariser —
        // a provider call — not only before the candidates sample.
        if let Err(error) = sink.flush_log().await {
            sink.fail(format!("log flush failed: {error}"));
            return;
        }
        let prepared =
            prepare_messages_for_shot(&runtime.caps, &input, None).await;
        let request =
            single_shot_request_shared(&runtime.caps, prepared.clone());

        sink.emit_typed(step_start(0, 1));
        let (scored, failures) =
            self.sample_and_score(&runtime, &request, sink).await;
        sink.emit_typed(step_end(0, 1, "final_answer"));

        self.publish(&runtime, sink, started, scored, failures);
    }
}

#[cfg(test)]
mod judge_tests;
#[cfg(test)]
mod tests;

/// The README, **compiled**.
///
/// The `rust` fences in [`README.md`](../README.md) are doctests, so
/// a wiring recipe that stops compiling fails
/// `cargo test -p elysper-strategy-best-of-n`.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct Readme;
