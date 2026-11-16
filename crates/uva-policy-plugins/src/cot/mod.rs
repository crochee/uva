//! [`ChainOfThoughtStrategy`] — one tool-free pass with a
//! step-format instruction appended to the system prompt.
use std::sync::Arc;

use async_trait::async_trait;
use uva_harness::{
    AgentEvent,
    AgentInput,
    EventSink,
    ReasoningStrategy,
    WarningKind,
    agent::strategy::{AgentRuntime, single_shot_request},
};
use uva_provider::{
    CompletionRequest,
    Content,
    ContentPart,
    Message,
    Role,
    StreamChunk,
    TextContent,
};
use uva_session::{step_end, step_start};

/// How many reasoning steps the instruction asks for, when the caller
/// does not say.
pub(crate) const DEFAULT_COT_STEPS: usize = 5;

#[derive(Debug, Clone, Copy)]
pub struct ChainOfThoughtStrategy {
    max_steps: usize,
}

impl Default for ChainOfThoughtStrategy {
    fn default() -> Self {
        Self::new(DEFAULT_COT_STEPS)
    }
}

impl ChainOfThoughtStrategy {
    #[must_use]
    pub fn new(max_steps: usize) -> Self {
        Self {
            max_steps: max_steps.max(1),
        }
    }

    #[must_use]
    pub fn instruction(&self) -> String {
        format!(
            "Reason before you answer. Work through at most {} numbered \
             steps, each on its own line and labelled `Step N:`. Then \
             write a final line starting with `Answer:` that contains \
             only the answer itself. Do not call tools; you have none.",
            self.max_steps
        )
    }

    /// The labelled reasoning steps in `text`, in order.
    ///
    /// Recognises `Step N:` / `Thought N:` at the start of a line
    /// (case-insensitive, after optional Markdown list markers and
    /// bold/italic emphasis).
    #[must_use]
    pub fn parse_steps(text: &str) -> Vec<String> {
        text.lines()
            .filter_map(strip_step_label)
            .map(str::to_string)
            .collect()
    }

    /// The single streaming pass, with deltas published as they arrive
    /// so a consumer renders the chain live.
    async fn stream_once(
        &self,
        runtime: &AgentRuntime,
        request: CompletionRequest,
        sink: &EventSink,
    ) -> (Option<String>, Vec<ContentPart>, Result<(), String>) {
        let attempt = Arc::new(parking_lot::Mutex::new(None));
        let slot = Arc::clone(&attempt);
        // The turn's parts, kept for the durable row: the deltas
        // live on the event stream, and the row the run leaves behind
        // is the same turn.
        let parts =
            Arc::new(parking_lot::Mutex::new(Vec::<ContentPart>::new()));
        let parts_slot = Arc::clone(&parts);
        // A provider that answers in one batch streams no text chunk:
        // the answer is the terminal result's own text.
        let answer = Arc::new(parking_lot::Mutex::new(String::new()));
        let answer_slot = Arc::clone(&answer);
        let stream_sink = sink.clone();
        // The prompt (and everything queued before it) lands before
        // the model sees it.
        if let Err(error) = sink.flush_log().await {
            return (None, Vec::new(), Err(error.to_string()));
        }
        let outcome = runtime
            .caps
            .model
            .provider
            .clone()
            .complete_with_stream(
                request,
                Some(runtime.cancel.clone()),
                Box::new(move |chunk| match chunk {
                    StreamChunk::Content(part) => {
                        // The cot offers no tools: a tool call in
                        // the answer is ignored (and warned about),
                        // and it must not reach the log, where it
                        // would be an unanswered call.
                        if !matches!(part, ContentPart::ToolUse(_)) {
                            parts_slot.lock().push(part.clone());
                        }
                        stream_sink.emit(AgentEvent::Model(part));
                    }
                    StreamChunk::Usage(usage) => {
                        stream_sink.emit(AgentEvent::usage(
                            usage.prompt_tokens,
                            usage.completion_tokens,
                            usage.cache_read_tokens,
                            usage.cache_write_tokens,
                        ));
                    }
                    StreamChunk::IsDone { result } => {
                        if let Some(first) = result.tool_calls.first() {
                            *slot.lock() = Some(first.name.clone());
                        }
                        *answer_slot.lock() = result.text.clone();
                        stream_sink.emit(AgentEvent::ModelDone(*result));
                    }
                    StreamChunk::Stop(_)
                    | StreamChunk::ToolCallStart { .. }
                    | StreamChunk::ToolCallDelta { .. }
                    | StreamChunk::ToolCallEnd { .. } => {}
                }),
            )
            .await;
        let attempted = attempt.lock().clone();
        let mut parts = std::mem::take(&mut *parts.lock());
        if !parts.iter().any(|part| {
            matches!(part, ContentPart::Text(text) if !text.text.is_empty())
        }) {
            let text = answer.lock().clone();
            if !text.is_empty() {
                parts.push(ContentPart::Text(TextContent {
                    text,
                    cache_control: None,
                }));
            }
        }
        (
            attempted,
            parts,
            outcome.map(|_| ()).map_err(|err| err.to_string()),
        )
    }
}

#[async_trait]
impl ReasoningStrategy for ChainOfThoughtStrategy {
    fn name(&self) -> &str {
        "chain-of-thought"
    }

    async fn run(
        &self,
        runtime: AgentRuntime,
        input: AgentInput,
        sink: &EventSink,
    ) {
        let begin_token = runtime.cancel.clone();
        if !sink.begin(&begin_token) {
            return;
        }
        // The run's epoch marker: one header naming the request
        // envelope this run's facts are produced under.
        sink.stamp_request_header(&runtime.caps, &[]);

        let instruction = self.instruction();
        // The prompt is on disk before the context manager's
        // summariser — a provider call — not only before the sample.
        if let Err(error) = sink.flush_log().await {
            sink.fail(format!("log flush failed: {error}"));
            return;
        }
        let request =
            single_shot_request(&runtime.caps, &input, Some(&instruction))
                .await;

        sink.emit_typed(step_start(0, 1));
        let (attempted, parts, outcome) =
            self.stream_once(&runtime, request, sink).await;

        sink.emit_typed(step_end(0, 1, "final_answer"));

        if let Some(name) = attempted {
            sink.warn(
                WarningKind::Loop,
                Some(1),
                format!(
                    "chain-of-thought offers no tools, but the model called \
                     `{name}`; the call was ignored. Use ReActStrategy for \
                     tool work."
                ),
            );
        }

        let finish_token = runtime.cancel.clone();
        match outcome {
            Ok(()) => {
                // The model's answer is the run's durable fact: one
                // `assistant_message` row through the session's
                // writer. Only a turn that *completed* is a fact — a
                // stream that failed mid-answer leaves its deltas on
                // the wire, because a truncated row would read back
                // as a finished assistant turn.
                sink.write_assistant_turn(&Message::new(
                    Role::Assistant,
                    Content::parts(parts),
                ));
                sink.finish(&finish_token);
            }
            Err(message) => sink.fail(message),
        }
    }
}

#[cfg(test)]
mod tests;

/// `Some(rest)` when `line` opens with a step label, `None` otherwise.
fn strip_step_label(line: &str) -> Option<&str> {
    let mut rest = line.trim_start();
    for prefix in ["- ", "* ", "+ ", "# ", "> "] {
        if let Some(stripped) = rest.strip_prefix(prefix) {
            rest = stripped.trim_start();
        }
    }
    rest = rest.trim_start_matches(['*', '_', '`']);

    for label in ["step", "thought"] {
        let Some(after) = rest.get(..label.len()) else {
            continue;
        };
        if !after.eq_ignore_ascii_case(label) {
            continue;
        }
        let tail = &rest[label.len()..];
        let tail = tail
            .trim_start()
            .trim_start_matches(|c: char| c.is_ascii_digit());
        if let Some(body) = tail.strip_prefix(':') {
            return Some(body.trim_start_matches(['*', '_', '`']).trim());
        }
    }
    None
}

/// The README, **compiled**.
///
/// The `rust` fences in [`README.md`](../README.md) are doctests, so
/// a wiring recipe that stops compiling fails
/// `cargo test -p uva-policy-plugins --all-features`.
#[cfg(doctest)]
#[doc = include_str!("README.md")]
pub struct Readme;
