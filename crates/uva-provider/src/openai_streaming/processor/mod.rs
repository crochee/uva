use std::collections::HashMap;

use super::types::OpenAIDeltaResponse;
use crate::{
    json_repair::parse_tool_input_logged,
    streaming::{ThinkExtractor, ToolUseBuffer},
    types::{
        ContentPart,
        ReasoningContent,
        SamplingResult,
        StreamChunk,
        TokenUsage,
        ToolUse,
    },
};

pub struct OpenAIStreamProcessor {
    tool_buffers: HashMap<usize, ToolUseBuffer>,
    /// Per-turn accumulators for the final `SamplingResult`.
    text: String,
    reasoning: String,
    tool_calls: Vec<ToolUse>,
    usage: Option<TokenUsage>,
    /// Canonical stop reason captured from the last
    /// `finish_reason` chunk in this turn. Propagated to
    /// the final `SamplingResult.stop_reason` so the
    /// agent layer can distinguish `stop`, `tool_calls`,
    /// `length`, etc. without parsing the SSE itself.
    stop_reason: Option<String>,
    /// `true` after `IsDone` has been emitted; further events are
    /// ignored.
    finished: bool,
    /// Per-turn extractor that splits `<think>…</think>` markers out
    /// of plain-text deltas. Providers like MiniMax-M2.7 emit reasoning
    /// inline in `delta.content`; without this extractor the reasoning
    /// ends up concatenated with the visible answer on the frontend.
    think: ThinkExtractor,
}

impl OpenAIStreamProcessor {
    /// Fold a wire usage block into the processor's sticky `usage` slot
    /// and return the [`StreamChunk::Usage`] the caller should emit.
    ///
    /// Sticky because the terminal `IsDone` result must carry the same
    /// counts the live `Usage` chunk reported, and a later terminal
    /// chunk may repeat them.
    fn record_usage(
        &mut self,
        usage: super::types::OpenAIDeltaUsage,
    ) -> StreamChunk {
        let u = TokenUsage {
            prompt_tokens: usage.prompt_tokens,
            completion_tokens: usage.completion_tokens,
            total_tokens: usage.total_tokens,
            cached_prompt_tokens: usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens),
            cache_read_tokens: usage
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens),
            cache_write_tokens: None,
            reasoning_tokens: usage
                .completion_tokens_details
                .as_ref()
                .and_then(|d| d.reasoning_tokens),
        };
        self.usage = Some(u.clone());
        StreamChunk::Usage(u)
    }

    pub fn new() -> Self {
        Self {
            tool_buffers: HashMap::with_capacity(4),
            text: String::new(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            usage: None,
            stop_reason: None,
            finished: false,
            think: ThinkExtractor::new(),
        }
    }

    pub fn process_line(&mut self, data: &str) -> Vec<StreamChunk> {
        if self.finished {
            return vec![];
        }
        let data = data.trim();
        if data == "[DONE]" {
            return vec![self.emit_is_done(None)];
        }

        let Ok(delta_resp) = serde_json::from_str::<OpenAIDeltaResponse>(data)
        else {
            return vec![];
        };

        let mut chunks: Vec<StreamChunk> = Vec::with_capacity(8);

        // The authoritative usage for this turn lives at the chunk's top
        // level (see `OpenAIDeltaResponse::usage`). Capture it before the
        // choice lookup: the documented final frame carries an EMPTY
        // `choices` array, so a `choices.first()` that bailed out first
        // would discard the only place the token counts appear.
        if let Some(usage) = delta_resp.usage {
            chunks.push(self.record_usage(usage));
        }

        // OpenAI streaming: when a request is sent with `n > 1`,
        // each SSE message carries an array of N choices (one per
        // sample). Our agent layer consumes one stream per turn, so
        // we process only the FIRST choice and silently drop the
        // rest. Clients that need multi-sample parallel streams
        // should send `n = 1` per request. Any caller sending `n > 1`
        // gets a single canonical stream — the dropped choices are
        // not recoverable from this processor.
        let Some(choice) = delta_resp.choices.first() else {
            return chunks;
        };

        // Reasoning is emitted as a dedicated Content(Reasoning) chunk
        // without any text sniffing — the upstream already separates
        // reasoning from text into its own field.
        if let Some(ref delta) = choice.delta {
            if let Some(ref reasoning) = delta.reasoning_content
                && !reasoning.is_empty()
            {
                self.reasoning.push_str(reasoning);
                chunks.push(StreamChunk::Content(ContentPart::Reasoning(
                    ReasoningContent {
                        text: reasoning.clone(),
                        signature: None,
                    },
                )));
            }
            if let Some(ref content) = delta.content
                && !content.is_empty()
            {
                // Route text deltas through the shared ThinkExtractor
                // so `<think>…</think>` markers get split into separate
                // reasoning chunks. The extractor also folds the safe
                // (non-marker) prefix into `self.text` so the final
                // `SamplingResult` matches what the streaming chunks
                // emitted.
                let mut extracted = self.think.process_text(content);
                for chunk in &extracted {
                    if let StreamChunk::Content(ContentPart::Text(t)) = chunk {
                        self.text.push_str(&t.text);
                    } else if let StreamChunk::Content(
                        ContentPart::Reasoning(r),
                    ) = chunk
                    {
                        self.reasoning.push_str(&r.text);
                    }
                }
                chunks.append(&mut extracted);
            }
            if let Some(ref tool_calls) = delta.tool_calls {
                for tc in tool_calls {
                    let index = tc.index.unwrap_or(0) as usize;
                    let tc_id = tc
                        .id
                        .clone()
                        .unwrap_or_else(|| format!("call_{index}"));

                    // First time we see this index: emit ToolCallStart.
                    match self.tool_buffers.entry(index) {
                        std::collections::hash_map::Entry::Vacant(vacant) => {
                            let name = tc
                                .function
                                .name
                                .clone()
                                .unwrap_or_else(|| "unknown".to_string());
                            // The first delta may already include a partial
                            // argument string; treat that as part of the
                            // input buffer and emit only the start chunk.
                            let initial = tc
                                .function
                                .arguments
                                .clone()
                                .unwrap_or_default();
                            chunks.push(StreamChunk::ToolCallStart {
                                id: tc_id.clone(),
                                name: name.clone(),
                                arguments: serde_json::Value::String(
                                    initial.clone(),
                                ),
                            });
                            vacant.insert(ToolUseBuffer {
                                id: tc_id,
                                name,
                                input: initial,
                                salvaged: false,
                            });
                        }
                        std::collections::hash_map::Entry::Occupied(
                            mut occ,
                        ) => {
                            // Subsequent delta: patch name (rare) and
                            // append argument delta.
                            if let Some(ref name) = tc.function.name {
                                occ.get_mut().name = name.clone();
                            }
                            if let Some(args) = tc.function.arguments.as_ref()
                                && !args.is_empty()
                            {
                                let buffer = occ.get_mut();
                                buffer.input.push_str(args);
                                chunks.push(StreamChunk::ToolCallDelta {
                                    id: buffer.id.clone(),
                                    arguments_delta: args.clone(),
                                });
                            }
                        }
                    }
                }
            }
        }

        // Some gateways nest the usage block inside the choice instead of
        // the chunk. Honour it too — but only when the top level did not
        // already provide one, so the authoritative value is never
        // overwritten by a stale duplicate.
        if !chunks.iter().any(|c| matches!(c, StreamChunk::Usage(_)))
            && let Some(usage) = &choice.usage
        {
            chunks.push(self.record_usage(usage.clone()));
        }

        if let Some(ref finish_reason) = choice.finish_reason {
            let reason = finish_reason.clone();
            // Flush all open tool calls (this is the OpenAI-style "end
            // of stream" for tool calls).
            for (_, buffer) in
                std::mem::take(&mut self.tool_buffers).into_iter()
            {
                let parsed =
                    parse_tool_input_logged(&buffer.input, &buffer.name);
                let tool_use = ToolUse {
                    id: buffer.id.clone(),
                    name: buffer.name.clone(),
                    input: parsed,
                };
                self.tool_calls.push(tool_use);
                chunks.push(StreamChunk::ToolCallEnd {
                    id: buffer.id.clone(),
                });
            }
            let done = self.emit_is_done(Some(reason));
            chunks.push(done);
        }

        chunks
    }

    fn emit_is_done(&mut self, finish_reason: Option<String>) -> StreamChunk {
        self.finished = true;
        // Capture the canonical stop reason so it
        // propagates into the final `SamplingResult`.
        // When the stream terminates via `[DONE]` without
        // a preceding `finish_reason` (malformed
        // upstream), the stop reason is unknown — leave
        // it as `None` so consumers can distinguish
        // "missing" from "explicit stop".
        if let Some(ref reason) = finish_reason {
            self.stop_reason = Some(reason.clone());
        }
        // If `finish_reason` was Some, the `process_line`
        // path already drained `tool_buffers` and pushed
        // `ToolCallEnd` chunks BEFORE calling `emit_is_done`.
        // But if the stream ended via `[DONE]` with no
        // preceding `finish_reason` (malformed upstream,
        // truncation), `tool_buffers` may still hold live
        // entries that the caller will never see emitted.
        // Flush them here so the final `SamplingResult`
        // is consistent with the buffered calls: every
        // `ToolUse` in `tool_calls` was at least seen by
        // the processor (whether it was fully closed is a
        // separate question).
        //
        // We do NOT synthesize `ToolCallEnd` chunks here
        // because downstream consumers already key off
        // the `IsDone.tool_calls` vec; emitting `ToolCallEnd`
        // from this fallback path would cause double-emit
        // for the normal `finish_reason`-driven path. The
        // drain-then-aggregate is the same approach used
        // by the anthropic processor (see
        // `streaming/anthropic/processor.rs::message_stop`).
        if finish_reason.is_none() {
            for (_, buffer) in
                std::mem::take(&mut self.tool_buffers).into_iter()
            {
                let parsed =
                    parse_tool_input_logged(&buffer.input, &buffer.name);
                self.tool_calls.push(ToolUse {
                    id: buffer.id,
                    name: buffer.name,
                    input: parsed,
                });
            }
        }
        let usage = self.usage.clone().unwrap_or_default();
        // Drain any residual text or reasoning the ThinkExtractor
        // was still holding as carry. These would otherwise be lost
        // (the carry is only released when the next delta arrives,
        // but the next delta after this point is `[DONE]` or a
        // terminal `finish_reason` chunk with empty content).
        let mut drained = self.think.flush();
        for chunk in &drained {
            if let StreamChunk::Content(ContentPart::Text(t)) = chunk {
                self.text.push_str(&t.text);
            } else if let StreamChunk::Content(ContentPart::Reasoning(r)) =
                chunk
            {
                self.reasoning.push_str(&r.text);
            }
        }
        drained.clear();
        let text = std::mem::take(&mut self.text);
        let reasoning = std::mem::take(&mut self.reasoning);
        let tool_calls = std::mem::take(&mut self.tool_calls);
        let result = SamplingResult {
            text,
            tool_calls,
            reasoning,
            reasoning_signature: None,
            usage,
            stop_reason: std::mem::take(&mut self.stop_reason),
        };
        tracing::debug!(
            target: "uva_provider::openai::processor",
            text_len = result.text.len(),
            tool_calls = result.tool_calls.len(),
            "openai stream complete"
        );
        StreamChunk::IsDone {
            result: Box::new(result),
        }
    }
}

impl Default for OpenAIStreamProcessor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
