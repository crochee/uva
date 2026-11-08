//! Keyless deterministic replay of recorded model-call scripts.
//!
//! Ports the `deepseek-harness` `llm-replay` design to the
//! single-provider shape elysper's tests need: a
//! [`ReplayProvider`] is constructed from per-run scripts
//! (`Vec<Vec<StreamChunk>>`, one entry per model call) or from a
//! recorded session `events.jsonl` fixture, then hands one
//! script to each `complete` / `complete_with_stream` call.
//!
//! Two properties make it a test tool rather than a mock:
//!
//! - **No silent underruns.** A call with no script left is a
//!   hard error, and [`ReplayProvider::assert_consumed`] turns
//!   "the scenario made fewer calls than were recorded" into a
//!   crisp teardown diagnostic, including calls that were bound
//!   but left undelivered (e.g. the agent cancelled mid-stream).
//! - **Optional pacing.** [`ReplayProvider::with_pacing`] sleeps
//!   before each delivered chunk so downstream consumers observe
//!   genuinely incremental delivery. Correctness never depends on
//!   it.
//!
//! # Example
//!
//! ```rust
//! use elysper_provider::{
//!     CompletionRequest,
//!     ContentPart,
//!     ModelProvider,
//!     SamplingResult,
//!     StreamChunk,
//!     TextContent,
//! };
//! use elysper_test_support::ReplayProvider;
//!
//! # async fn example() -> Result<(), elysper_core::Error> {
//! let provider = ReplayProvider::new(vec![vec![
//!     StreamChunk::Content(ContentPart::Text(TextContent {
//!         text: "hello".to_string(),
//!         cache_control: None,
//!     })),
//!     StreamChunk::IsDone {
//!         result: Box::new(SamplingResult {
//!             text: "hello".to_string(),
//!             stop_reason: Some("end_turn".to_string()),
//!             ..Default::default()
//!         }),
//!     },
//! ]]);
//!
//! let response = provider.complete(CompletionRequest::default()).await?;
//! assert_eq!(response.stop_reason.as_deref(), Some("end_turn"));
//! provider
//!     .assert_consumed()
//!     .expect("scenario used every script");
//! # Ok(())
//! # }
//! ```

use std::{collections::VecDeque, sync::Arc, time::Duration};

use async_trait::async_trait;
use elysper_core::{CancelToken, Error};
use elysper_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    SamplingResult,
    StreamChunk,
    TextContent,
    ToolUse,
};
use parking_lot::Mutex;
use serde_json::Value;
use thiserror::Error;

/// Errors raised while building or checking a [`ReplayProvider`].
#[derive(Debug, Error)]
pub enum ReplayError {
    /// A session-events fixture could not be turned into call
    /// scripts.
    #[error("replay fixture error: {0}")]
    Fixture(String),
    /// [`ReplayProvider::assert_consumed`] found scripts that were
    /// never bound to a call, or bound calls that did not deliver
    /// every recorded chunk.
    #[error("replay not fully consumed: {0}")]
    NotConsumed(String),
}

/// Bookkeeping for one bound call: chunks not yet delivered.
#[derive(Debug)]
struct BoundCall {
    remaining: usize,
}

/// Shared replay cursor state.
#[derive(Debug, Default)]
struct ReplayState {
    /// Scripts not yet bound to a call, in recorded order.
    pending: VecDeque<Vec<StreamChunk>>,
    /// One entry per bound call, in call order.
    bound: Vec<BoundCall>,
    /// Number of `complete*` calls issued so far.
    calls: usize,
    /// Total scripts the provider was constructed with.
    recorded: usize,
}

/// Deterministic, keyless [`ModelProvider`] that replays recorded
/// model-call scripts in order.
///
/// Each `complete` / `complete_with_stream` call binds the next
/// unbound script and advances a cursor through its chunks. The
/// provider never touches the network and never reads the clock
/// (except for the optional pacing sleeps).
pub struct ReplayProvider {
    state: Arc<Mutex<ReplayState>>,
    pace: Option<Duration>,
}

impl std::fmt::Debug for ReplayProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let state = self.state.lock();
        f.debug_struct("ReplayProvider")
            .field("recorded", &state.recorded)
            .field("calls", &state.calls)
            .field("pending", &state.pending.len())
            .field("pace", &self.pace)
            .finish()
    }
}

impl ReplayProvider {
    /// New provider replaying `scripts` in order — one chunk list
    /// per model call.
    ///
    /// Each script should end with `StreamChunk::IsDone` (the
    /// canonical terminal marker); scripts without one still fold
    /// into a response from their content chunks.
    #[must_use]
    pub fn new(scripts: Vec<Vec<StreamChunk>>) -> Self {
        Self {
            state: Arc::new(Mutex::new(ReplayState {
                recorded: scripts.len(),
                pending: scripts.into(),
                ..ReplayState::default()
            })),
            pace: None,
        }
    }

    /// Build a provider from a session `events.jsonl` fixture.
    ///
    /// The fixture is the durable event-log format written by
    /// `elysper-session`: one JSON record per line, where
    /// `assistant_chunk` records carry a text delta under
    /// `data.delta` and the terminal record of a model call
    /// carries `finish_reason`. Non-chunk records (the session
    /// header, user/assistant messages, tool results, …) are
    /// skipped. Each call becomes one script terminated by
    /// `Stop(finish_reason)` + `IsDone`.
    ///
    /// # Errors
    ///
    /// Returns [`ReplayError::Fixture`] when a line is not valid
    /// JSON, or when the log ends with chunks that never reached
    /// a `finish_reason` (a thrown/aborted stream cannot be
    /// reconstructed from the log alone — the scenario must pass
    /// an explicit script instead).
    pub fn from_events_jsonl(contents: &str) -> Result<Self, ReplayError> {
        Ok(Self::new(derive_scripts(contents)?))
    }

    /// Sleep `per_chunk` before delivering each chunk.
    ///
    /// Realism knob only: correctness must never depend on it.
    /// The default (no pacing) delivers chunks as fast as the
    /// consumer accepts them.
    #[must_use]
    pub fn with_pacing(mut self, per_chunk: Duration) -> Self {
        self.pace = Some(per_chunk);
        self
    }

    /// Whether the provider was constructed with at least one
    /// script (a fixture without model calls yields `false`).
    #[must_use]
    pub fn has_scripts(&self) -> bool {
        self.state.lock().recorded > 0
    }

    /// Assert the scenario consumed every recorded script in
    /// full.
    ///
    /// # Errors
    ///
    /// Returns [`ReplayError::NotConsumed`] listing the number of
    /// scripts that were never bound to a call and every bound
    /// call that left chunks undelivered (e.g. cancellation
    /// mid-stream). Call this at scenario teardown.
    pub fn assert_consumed(&self) -> Result<(), ReplayError> {
        let state = self.state.lock();
        let unbound = state.pending.len();
        let undelivered: Vec<String> = state
            .bound
            .iter()
            .enumerate()
            .filter(|(_, call)| call.remaining > 0)
            .map(|(idx, call)| {
                format!(
                    "call {} left {} chunk(s) undelivered",
                    idx + 1,
                    call.remaining
                )
            })
            .collect();
        if unbound == 0 && undelivered.is_empty() {
            return Ok(());
        }
        let mut problems = Vec::new();
        if unbound > 0 {
            problems.push(format!(
                "{unbound} script(s) were never bound to a call"
            ));
        }
        if !undelivered.is_empty() {
            problems.push(undelivered.join("; "));
        }
        Err(ReplayError::NotConsumed(problems.join("; ")))
    }

    /// Bind the next unbound script to a call, returning the call
    /// ordinal (1-based) and a copy of the script.
    fn bind_next_script(&self) -> Result<(usize, Vec<StreamChunk>), Error> {
        let mut state = self.state.lock();
        state.calls += 1;
        let call_no = state.calls;
        let Some(chunks) = state.pending.pop_front() else {
            return Err(Error::provider(format!(
                "replay provider exhausted: call {call_no} requested \
                 but no script remains ({} of {} script(s) already \
                 bound)",
                state.bound.len(),
                state.recorded,
            )));
        };
        state.bound.push(BoundCall {
            remaining: chunks.len(),
        });
        Ok((call_no, chunks))
    }

    /// Note that one chunk of `call_no` reached the consumer.
    fn note_delivered(&self, call_no: usize) {
        let mut state = self.state.lock();
        if let Some(call) = state.bound.get_mut(call_no - 1) {
            call.remaining = call.remaining.saturating_sub(1);
        }
    }

    /// Mark every chunk of `call_no` delivered (the non-streaming
    /// path aggregates the script itself).
    fn mark_all_delivered(&self, call_no: usize) {
        let mut state = self.state.lock();
        if let Some(call) = state.bound.get_mut(call_no - 1) {
            call.remaining = 0;
        }
    }

    /// Chunks of `call_no` not yet delivered.
    fn remaining_chunks(&self, call_no: usize) -> usize {
        self.state
            .lock()
            .bound
            .get(call_no - 1)
            .map(|call| call.remaining)
            .unwrap_or(0)
    }

    /// Deliver `chunks` in order through `on_delta`, honouring
    /// optional pacing and cancellation.
    ///
    /// Cancellation mid-replay leaves the remaining chunks
    /// undelivered, which [`Self::assert_consumed`] reports.
    async fn replay_script(
        &self,
        call_no: usize,
        chunks: &[StreamChunk],
        cancel: Option<&Arc<dyn CancelToken>>,
        mut on_delta: impl FnMut(StreamChunk),
    ) -> Result<(), Error> {
        for chunk in chunks {
            if let Some(token) = cancel
                && token.is_cancelled()
            {
                return Err(Error::StreamAborted {
                    message: format!(
                        "replay call {call_no} cancelled with {} \
                         chunk(s) undelivered",
                        self.remaining_chunks(call_no),
                    ),
                });
            }
            if let Some(pace) = self.pace {
                tokio::time::sleep(pace).await;
            }
            on_delta(chunk.clone());
            self.note_delivered(call_no);
        }
        Ok(())
    }
}

#[async_trait]
impl ModelProvider for ReplayProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "replay"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "replay-model".to_string(),
            provider: "replay".to_string(),
            context_window: 128_000,
            max_output_tokens: 4_096,
            supports_tools: true,
            supports_streaming: true,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        let (call_no, chunks) = self.bind_next_script()?;
        let sampling = fold_chunks(&chunks);
        self.mark_all_delivered(call_no);
        Ok(response_for(call_no, &sampling))
    }

    async fn complete_with_stream(
        &self,
        _request: CompletionRequest,
        cancel_token: Option<Arc<dyn CancelToken>>,
        mut on_delta: Box<dyn FnMut(StreamChunk) + Send>,
    ) -> Result<CompletionResponse, Error> {
        let (call_no, chunks) = self.bind_next_script()?;
        self.replay_script(call_no, &chunks, cancel_token.as_ref(), |chunk| {
            on_delta(chunk);
        })
        .await?;
        let sampling = fold_chunks(&chunks);
        Ok(response_for(call_no, &sampling))
    }
}

/// Derive one script per model call from a session `events.jsonl`
/// fixture.
fn derive_scripts(
    contents: &str,
) -> Result<Vec<Vec<StreamChunk>>, ReplayError> {
    let mut scripts = Vec::new();
    let mut current: Vec<StreamChunk> = Vec::new();
    let mut current_text = String::new();

    for (line_no, raw) in contents.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        let record: Value = serde_json::from_str(line).map_err(|e| {
            ReplayError::Fixture(format!(
                "line {}: invalid JSON: {e}",
                line_no + 1
            ))
        })?;
        if record.get("type").and_then(Value::as_str) != Some("assistant_chunk")
        {
            continue;
        }
        let closed =
            push_chunk_record(&record, &mut current, &mut current_text);
        if closed {
            scripts.push(std::mem::take(&mut current));
            current_text.clear();
        }
    }

    if !current.is_empty() {
        return Err(ReplayError::Fixture(
            "model call ended without a finish_reason chunk; a thrown or \
             aborted stream cannot be derived from the log — pass an \
             explicit script instead"
                .to_string(),
        ));
    }
    Ok(scripts)
}

/// Project one `assistant_chunk` record onto script chunks.
/// Returns `true` when the record carried a `finish_reason` and
/// therefore closes the current script.
fn push_chunk_record(
    record: &Value,
    current: &mut Vec<StreamChunk>,
    current_text: &mut String,
) -> bool {
    let delta = record
        .get("data")
        .and_then(|data| data.get("delta"))
        .and_then(Value::as_str)
        .unwrap_or("");
    if !delta.is_empty() {
        current_text.push_str(delta);
        current.push(text_chunk(delta));
    }
    let Some(reason) = record.get("finish_reason").and_then(Value::as_str)
    else {
        return false;
    };
    current.push(StreamChunk::Stop(reason.to_string()));
    current.push(StreamChunk::IsDone {
        result: Box::new(SamplingResult {
            text: current_text.clone(),
            stop_reason: Some(reason.to_string()),
            ..SamplingResult::default()
        }),
    });
    true
}

fn text_chunk(text: &str) -> StreamChunk {
    StreamChunk::Content(ContentPart::Text(TextContent {
        text: text.to_string(),
        cache_control: None,
    }))
}

/// Fold a script into the final sampling result it represents.
///
/// A terminal `IsDone` is authoritative (last one wins — mirrors
/// how `re_act` treats streamed chunks versus the final result).
/// Without one, content/usage/stop chunks are accumulated, and
/// tool-call deltas are materialised into `ToolUse` entries the
/// way the agent's stream ingest does.
fn fold_chunks(chunks: &[StreamChunk]) -> SamplingResult {
    let mut folded = SamplingResult::default();
    let mut buffers: Vec<(String, String, String)> = Vec::new();
    let mut done: Option<SamplingResult> = None;

    for chunk in chunks {
        match chunk {
            StreamChunk::Content(ContentPart::Text(t)) => {
                folded.text.push_str(&t.text);
            }
            StreamChunk::Content(ContentPart::Reasoning(r)) => {
                folded.reasoning.push_str(&r.text);
                if r.signature.is_some() {
                    folded.reasoning_signature = r.signature.clone();
                }
            }
            StreamChunk::Content(ContentPart::ToolUse(tool)) => {
                folded.tool_calls.push(tool.clone());
            }
            StreamChunk::Content(_) => {}
            StreamChunk::Usage(usage) => folded.usage = usage.clone(),
            StreamChunk::Stop(reason) => {
                folded.stop_reason = Some(reason.clone());
            }
            StreamChunk::ToolCallStart {
                id,
                name,
                arguments,
            } => {
                buffers.push((
                    id.clone(),
                    name.clone(),
                    initial_args(arguments),
                ));
            }
            StreamChunk::ToolCallDelta {
                id,
                arguments_delta,
            } => {
                if let Some(buf) = buffers.iter_mut().find(|buf| buf.0 == *id) {
                    buf.2.push_str(arguments_delta);
                }
            }
            StreamChunk::ToolCallEnd { id } => {
                if let Some((call_id, name, raw)) =
                    take_buffer(&mut buffers, id)
                {
                    folded.tool_calls.push(ToolUse {
                        id: call_id,
                        name,
                        input: parse_tool_input(&raw),
                    });
                }
            }
            StreamChunk::IsDone { result } => {
                done = Some((**result).clone());
            }
        }
    }

    // Streams that end mid-tool-call still surface the partial
    // buffer, matching the agent's drain-on-termination behavior.
    for (id, name, raw) in buffers {
        folded.tool_calls.push(ToolUse {
            id,
            name,
            input: parse_tool_input(&raw),
        });
    }

    done.unwrap_or(folded)
}

/// Remove and return the buffered tool call for `id`, if any.
fn take_buffer(
    buffers: &mut Vec<(String, String, String)>,
    id: &str,
) -> Option<(String, String, String)> {
    let pos = buffers.iter().position(|buf| buf.0 == id)?;
    Some(buffers.remove(pos))
}

/// Initial raw argument text for a `ToolCallStart` chunk.
fn initial_args(arguments: &Value) -> String {
    match arguments {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// Parse a tool input JSON string; empty input yields `{}`,
/// unparseable input yields the raw string (mirrors
/// `re_act::parse_tool_input`).
fn parse_tool_input(raw: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Value::Object(serde_json::Map::new());
    }
    serde_json::from_str(trimmed)
        .unwrap_or_else(|_| Value::String(raw.to_string()))
}

/// Build the `CompletionResponse` for one replayed call.
fn response_for(
    call_no: usize,
    sampling: &SamplingResult,
) -> CompletionResponse {
    let mut parts: Vec<ContentPart> = Vec::new();
    if !sampling.text.is_empty() || sampling.tool_calls.is_empty() {
        parts.push(ContentPart::Text(TextContent {
            text: sampling.text.clone(),
            cache_control: None,
        }));
    }
    for call in &sampling.tool_calls {
        parts.push(ContentPart::ToolUse(call.clone()));
    }
    let content = if parts.len() == 1 {
        Content::Single(parts.swap_remove(0))
    } else {
        Content::Multi(parts)
    };
    CompletionResponse {
        id: format!("replay-{call_no}"),
        model: "replay-model".to_string(),
        content,
        usage: sampling.usage.clone(),
        cached: false,
        stop_reason: sampling.stop_reason.clone(),
        replay_state: None,
    }
}

#[cfg(test)]
mod tests;
