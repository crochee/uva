//! The [`StreamProcessor`] used by `complete_with_stream`.
//! Emits incremental
//! `ToolCallStart { id, name, arguments }`,
//! `ToolCallDelta { id, arguments_delta }`, `ToolCallEnd { id }`,
//! and a terminal `IsDone { result: SamplingResult }` on
//! `message_stop`.

use std::collections::HashMap;

use tracing::warn;

use super::events::AnthropicStreamEvent;
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

/// Stream processor that emits the chunk model required by
/// `complete_with_stream`:
/// - `Content(Text)` for `text_delta`
/// - `Content(Reasoning)` for `thinking_delta`
/// - `ToolCallStart { id, name, arguments }` on `content_block_start` (tool_use)
/// - `ToolCallDelta { id, arguments_delta }` on `input_json_delta`
/// - `ToolCallEnd { id }` on `content_block_stop`
/// - `Usage(...)` and `IsDone { result }` on `message_delta` / `message_stop`
pub struct StreamProcessor {
    tool_buffers: HashMap<usize, ToolUseBuffer>,
    /// Per-turn accumulators for the final `SamplingResult`.
    text: String,
    reasoning: String,
    /// Latest Anthropic `signature_delta` observed during this turn
    /// (if any). Folded into the final `SamplingResult.reasoning_signature`
    /// so the agent layer can preserve reasoning continuity across turns.
    last_reasoning_signature: Option<String>,
    tool_calls: Vec<ToolUse>,
    /// Usage is optional; Anthropic emits it on a `message_delta` event.
    usage: Option<TokenUsage>,
    /// Stop reason captured from `message_delta.stop_reason` or
    /// `message_stop`.
    stop_reason: Option<String>,
    /// Per-turn extractor that splits `<think>…</think>` markers out
    /// of `text_delta` deltas. Anthropic's native `thinking_delta`
    /// events are routed through `ContentPart::Reasoning` directly,
    /// but a few upstreams (notably MiniMax-M2.7 reached via the
    /// Anthropic-compatible endpoint) embed reasoning inline in the
    /// text block; this extractor handles that case so the frontend
    /// still sees a clean separation.
    think: ThinkExtractor,
}

impl StreamProcessor {
    pub fn new() -> Self {
        Self {
            tool_buffers: HashMap::with_capacity(4),
            text: String::new(),
            reasoning: String::new(),
            last_reasoning_signature: None,
            tool_calls: Vec::new(),
            usage: None,
            stop_reason: None,
            think: ThinkExtractor::new(),
        }
    }

    pub fn process_event(
        &mut self,
        event: &AnthropicStreamEvent,
    ) -> Vec<StreamChunk> {
        let mut chunks = Vec::with_capacity(8);

        match event.r#type.as_str() {
            "content_block_start" => {
                self.handle_content_block_start(event, &mut chunks)
            }
            "content_block_delta" => {
                self.handle_content_block_delta(event, &mut chunks)
            }
            "content_block_stop" => {
                self.handle_content_block_stop(event, &mut chunks)
            }
            "message_delta" => self.handle_message_delta(event),
            "message_stop" => self.handle_message_stop(&mut chunks),
            _ => {}
        }

        chunks
    }

    fn handle_content_block_start(
        &mut self,
        event: &AnthropicStreamEvent,
        chunks: &mut Vec<StreamChunk>,
    ) {
        let (Some(block), Some(index)) = (&event.content_block, event.index)
        else {
            return;
        };

        match block.r#type.as_str() {
            "tool_use" => {
                let (Some(id), Some(name)) = (&block.id, &block.name) else {
                    return;
                };
                // The buffer accumulates the arguments as TEXT (the
                // continuation arrives as `input_json_delta` fragments), so
                // an object-form `input` becomes its own JSON text and a
                // string form is taken verbatim.
                let input_str = match block.input.as_ref() {
                    None | Some(serde_json::Value::Null) => String::new(),
                    Some(serde_json::Value::String(text)) => text.clone(),
                    // An EMPTY object is the streaming protocol's
                    // placeholder (`"input": {}`): the arguments arrive as
                    // `input_json_delta` fragments, so seeding the buffer
                    // with "{}" would corrupt the accumulation into
                    // `{}{"command":…}`. A NON-empty object is real
                    // prefill and is kept.
                    Some(serde_json::Value::Object(map)) if map.is_empty() => {
                        String::new()
                    }
                    Some(value) => value.to_string(),
                };
                self.tool_buffers.insert(
                    index,
                    ToolUseBuffer {
                        id: id.clone(),
                        name: name.clone(),
                        input: input_str.clone(),
                        salvaged: false,
                    },
                );
                chunks.push(StreamChunk::ToolCallStart {
                    id: id.clone(),
                    name: name.clone(),
                    arguments: serde_json::Value::String(input_str),
                });
            }
            "thinking" => {
                if let Some(thinking) = &block.thinking {
                    self.reasoning.push_str(thinking);
                    chunks.push(StreamChunk::Content(ContentPart::Reasoning(
                        ReasoningContent {
                            text: thinking.clone(),
                            signature: self.last_reasoning_signature.clone(),
                        },
                    )));
                }
            }
            "redacted_thinking" => {
                let marker = "[Redacted by safety filter]".to_string();
                self.reasoning.push_str(&marker);
                chunks.push(StreamChunk::Content(ContentPart::Reasoning(
                    ReasoningContent {
                        text: marker,
                        signature: self.last_reasoning_signature.clone(),
                    },
                )));
            }
            "server_tool_use" => {
                warn!("server_tool_use not supported");
            }
            _ => {}
        }
    }

    fn handle_content_block_delta(
        &mut self,
        event: &AnthropicStreamEvent,
        chunks: &mut Vec<StreamChunk>,
    ) {
        let (Some(delta), Some(index)) = (&event.delta, event.index) else {
            return;
        };

        match delta.r#type.as_str() {
            "text_delta" => {
                if let Some(text) = &delta.text {
                    // Route text deltas through the shared
                    // ThinkExtractor so `<think>…</think>`
                    // markers are split into separate
                    // reasoning chunks. Anthropic-native
                    // thinking blocks already arrive as
                    // `thinking_delta` events and bypass
                    // this path; this handles upstreams
                    // (e.g. MiniMax-M2.7) that embed
                    // reasoning inside the text block.
                    let mut extracted = self.think.process_text(text);
                    for chunk in &extracted {
                        if let StreamChunk::Content(ContentPart::Text(t)) =
                            chunk
                        {
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
            }
            "thinking_delta" => {
                if let Some(thinking) = &delta.thinking {
                    self.reasoning.push_str(thinking);
                    chunks.push(StreamChunk::Content(ContentPart::Reasoning(
                        ReasoningContent {
                            text: thinking.clone(),
                            signature: self.last_reasoning_signature.clone(),
                        },
                    )));
                }
            }
            "signature_delta" => {
                // The signature folds into the most recent
                // reasoning block on finalize; we do not emit a
                // stream chunk here because the agent will
                // read the signature off `SamplingResult`.
                if let Some(signature) = &delta.signature {
                    self.last_reasoning_signature = Some(signature.clone());
                }
            }
            "input_json_delta" => {
                if let (Some(partial), Some(buffer)) = (
                    delta.partial_json.as_ref(),
                    self.tool_buffers.get_mut(&index),
                ) {
                    buffer.input.push_str(partial);
                    chunks.push(StreamChunk::ToolCallDelta {
                        id: buffer.id.clone(),
                        arguments_delta: partial.clone(),
                    });
                }
            }
            _ => {}
        }
    }

    fn handle_content_block_stop(
        &mut self,
        event: &AnthropicStreamEvent,
        chunks: &mut Vec<StreamChunk>,
    ) {
        if let Some(index) = event.index
            && let Some(buffer) = self.tool_buffers.remove(&index)
        {
            // Parse the accumulated JSON once, here, so callers
            // don't have to re-parse the raw string at the end.
            // The shared finalizer repairs the malformations
            // providers actually emit and logs a salvage.
            let parsed = parse_tool_input_logged(&buffer.input, &buffer.name);
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
    }

    fn handle_message_delta(&mut self, event: &AnthropicStreamEvent) {
        // Note: `message_delta` carries the turn-level
        // `usage` block, but `AnthropicStreamDelta` (the
        // streaming delta struct) doesn't model it. Usage
        // tracking is deferred to the PR2 provider-level
        // work; for now only the stop reason is propagated.
        //
        // The real wire nests it in `delta.stop_reason`; the top-level
        // field is only ever set by synthetic events (tests, gateways that
        // hoist it), so it is the fallback rather than the primary.
        if let Some(reason) = event
            .delta
            .as_ref()
            .and_then(|d| d.stop_reason.as_ref())
            .or(event.stop_reason.as_ref())
        {
            self.stop_reason = Some(reason.clone());
        }
    }

    fn handle_message_stop(&mut self, chunks: &mut Vec<StreamChunk>) {
        let _ = self.drain_orphan_buffers();
        self.flush_think_extractor(chunks);
        let result = self.build_sampling_result();
        let stop = result.stop_reason.clone().unwrap_or_default();
        tracing::debug!(
            target: "elysper_provider::anthropic::stream",
            stop_reason = %stop,
            text_len = result.text.len(),
            tool_calls = result.tool_calls.len(),
            "anthropic stream complete"
        );
        chunks.push(StreamChunk::IsDone {
            result: Box::new(result),
        });
    }

    fn drain_orphan_buffers(&self) -> Vec<String> {
        let orphaned: Vec<String> = self
            .tool_buffers
            .values()
            .map(|b| format!("{}:{}", b.id, b.name))
            .collect();
        if !orphaned.is_empty() {
            warn!(
                target: "elysper.provider",
                count = orphaned.len(),
                tools = ?orphaned,
                "anthropic stream ended with orphan tool_use buffers; \
                 the matching content_block_stop events were missing \
                 (truncated SSE, network drop, or malformed upstream)"
            );
        }
        orphaned
    }

    fn flush_think_extractor(&mut self, _chunks: &mut Vec<StreamChunk>) {
        // The drained chunks update the per-turn accumulators
        // (self.text / self.reasoning) so the aggregated
        // `SamplingResult` matches the streaming output, but
        // they are intentionally NOT pushed into the outgoing
        // chunk vector — only the terminal `IsDone` is emitted
        // on `message_stop`.
        let drained = self.think.flush();
        for chunk in &drained {
            if let StreamChunk::Content(ContentPart::Text(t)) = chunk {
                self.text.push_str(&t.text);
            } else if let StreamChunk::Content(ContentPart::Reasoning(r)) =
                chunk
            {
                self.reasoning.push_str(&r.text);
            }
        }
    }

    fn build_sampling_result(&mut self) -> SamplingResult {
        let usage = self.usage.clone().unwrap_or_default();
        let stop = self
            .stop_reason
            .clone()
            .unwrap_or_else(|| "end_turn".to_string());
        let reasoning = std::mem::take(&mut self.reasoning);
        let text = std::mem::take(&mut self.text);
        let tool_calls = std::mem::take(&mut self.tool_calls);
        let reasoning_signature =
            std::mem::take(&mut self.last_reasoning_signature);
        SamplingResult {
            text,
            tool_calls,
            reasoning,
            reasoning_signature,
            usage,
            stop_reason: Some(stop),
        }
    }
}

impl Default for StreamProcessor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
