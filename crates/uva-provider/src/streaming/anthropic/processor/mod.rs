//! The [`StreamProcessor`] used by `complete_with_stream`.
//!
//! Decode only: it turns Anthropic wire events into [`StreamChunk`]s
//! and keeps nothing about the turn itself. The fold
//! ([`crate::assembler::BlockAssembler`]) owns the finished turn —
//! text, reasoning, tool calls, usage and the stop reason all come
//! from the chunk sequence.
//!
//! Emits incremental
//! - `Content(Text)` / `Content(Reasoning)` for deltas,
//! - `ToolCallStart { id, name, arguments }`,
//!   `ToolCallDelta { id, arguments_delta }` and
//!   `ToolCallEnd { id }` for tool blocks,
//! - `Usage(...)` whenever the wire reports usage,
//! - `ReasoningSignature(...)` on `signature_delta`,
//! - `Stop(reason)` when the terminal delta reports the stop reason.
//!
//! The only state left is decode scratch: the wire index → tool
//! buffer map, the think extractor, the usage frames merged so far
//! (the wire splits one report across two frames), and the guard that
//! stops a second `Stop` from being emitted.

use std::collections::HashMap;

use tracing::warn;

use super::events::{AnthropicStreamEvent, AnthropicStreamUsage};
use crate::{
    streaming::{ThinkExtractor, ToolUseBuffer},
    types::{ContentPart, ReasoningContent, StreamChunk},
};

/// Stream processor that turns Anthropic SSE events into chunks.
pub struct StreamProcessor {
    /// Wire `index` → the tool block being accumulated. Kept only so
    /// a `content_block_stop` can name the call it closes and so a
    /// truncated stream can warn about the buffer it never saw close.
    tool_buffers: HashMap<usize, ToolUseBuffer>,
    /// The usage frames seen so far.
    ///
    /// Anthropic splits one report across the turn: `message_start`
    /// has the prompt-side counts, the terminal `message_delta` the
    /// final output count. Each emitted `Usage` chunk carries the
    /// merge so far, because the fold keeps the last one.
    usage: AnthropicStreamUsage,
    /// Per-turn extractor that splits `<think>…</think>` markers out
    /// of `text_delta` deltas. Anthropic's native `thinking_delta`
    /// events are routed through `ContentPart::Reasoning` directly,
    /// but a few upstreams (notably MiniMax-M2.7 reached via the
    /// Anthropic-compatible endpoint) embed reasoning inline in the
    /// text block; this extractor handles that case so the frontend
    /// still sees a clean separation.
    think: ThinkExtractor,
    /// Whether the terminal `Stop` chunk has been emitted. A stream
    /// reports its stop reason once; `message_stop` only supplies the
    /// `end_turn` default when the terminal delta did not carry one.
    saw_stop: bool,
}

impl StreamProcessor {
    pub fn new() -> Self {
        Self {
            tool_buffers: HashMap::with_capacity(4),
            usage: AnthropicStreamUsage::default(),
            think: ThinkExtractor::new(),
            saw_stop: false,
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
            "message_start" => self.handle_message_start(event, &mut chunks),
            "message_delta" => self.handle_message_delta(event, &mut chunks),
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
                    chunks.push(StreamChunk::Content(ContentPart::Reasoning(
                        ReasoningContent {
                            text: thinking.clone(),
                            signature: None,
                        },
                    )));
                }
            }
            "redacted_thinking" => {
                chunks.push(StreamChunk::Content(ContentPart::Reasoning(
                    ReasoningContent {
                        text: "[Redacted by safety filter]".to_string(),
                        signature: None,
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
                    chunks.append(&mut self.think.process_text(text));
                }
            }
            "thinking_delta" => {
                if let Some(thinking) = &delta.thinking {
                    chunks.push(StreamChunk::Content(ContentPart::Reasoning(
                        ReasoningContent {
                            text: thinking.clone(),
                            signature: None,
                        },
                    )));
                }
            }
            "signature_delta" => {
                // The signature signs the reasoning block that just
                // ended. It cannot ride the Reasoning chunks (it
                // arrives after them), so it is its own chunk and the
                // fold stamps it onto the trailing reasoning run.
                if let Some(signature) = &delta.signature {
                    chunks.push(StreamChunk::ReasoningSignature(
                        signature.clone(),
                    ));
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
            chunks.push(StreamChunk::ToolCallEnd { id: buffer.id });
        }
    }

    /// The opening frame reports the prompt-side usage (and the output
    /// count so far). Report it as a usage chunk.
    fn handle_message_start(
        &mut self,
        event: &AnthropicStreamEvent,
        chunks: &mut Vec<StreamChunk>,
    ) {
        let Some(usage) = event.message.as_ref().and_then(|m| m.usage.as_ref())
        else {
            return;
        };
        self.absorb_usage(usage, chunks);
    }

    /// The terminal frame carries the turn's stop reason and the final
    /// output count. Both the usage object and the stop reason are
    /// optional and are only reported when present.
    fn handle_message_delta(
        &mut self,
        event: &AnthropicStreamEvent,
        chunks: &mut Vec<StreamChunk>,
    ) {
        if let Some(usage) = event.usage.as_ref() {
            self.absorb_usage(usage, chunks);
        }
        // The real wire nests the stop reason inside `delta`; the
        // top-level field is only ever set by synthetic events (tests,
        // gateways that hoist it), so it is the fallback rather than
        // the primary.
        if !self.saw_stop
            && let Some(reason) = event
                .delta
                .as_ref()
                .and_then(|d| d.stop_reason.as_ref())
                .or(event.stop_reason.as_ref())
        {
            self.saw_stop = true;
            // Release the extractor's carry before the terminal chunk:
            // a consumer that stops at `Stop` must still see the tail
            // this delta just made final.
            chunks.append(&mut self.think.flush());
            chunks.push(StreamChunk::Stop(reason.clone()));
        }
    }

    /// Merge one usage frame into the report so far and emit it as a
    /// usage chunk. The fold keeps the last usage chunk, so each
    /// emission carries the merge-so-far, never just the frame that
    /// arrived.
    ///
    /// The opening frame's output count is provisional (typically `1`,
    /// the opening token), so a stream cut before the terminal
    /// `message_delta` would report that provisional count. That is
    /// acceptable because the adapters now reject a truncated stream
    /// outright, so no consumer ever sees the provisional turn.
    fn absorb_usage(
        &mut self,
        usage: &AnthropicStreamUsage,
        chunks: &mut Vec<StreamChunk>,
    ) {
        self.usage.merge(usage);
        chunks.push(StreamChunk::Usage(self.usage.to_token_usage()));
    }

    /// Close the decode: drain the think extractor's carry as content
    /// chunks, warn about buffers the stream never closed, and emit the
    /// terminal stop chunk when the wire never reported a reason.
    ///
    /// The carry has to travel as content: it is the tail the extractor
    /// withheld while it waited for a marker to complete, and folding it
    /// into a terminal result is what used to drop it whenever text had
    /// already streamed.
    fn handle_message_stop(&mut self, chunks: &mut Vec<StreamChunk>) {
        self.warn_orphan_buffers();
        chunks.append(&mut self.think.flush());
        if !self.saw_stop {
            self.saw_stop = true;
            chunks.push(StreamChunk::Stop("end_turn".to_string()));
        }
    }

    /// Warn about tool buffers the stream never closed, without
    /// touching them: the decoder does not own the turn, so the fold
    /// is the one that drains them when it finalizes.
    fn warn_orphan_buffers(&self) {
        if self.tool_buffers.is_empty() {
            return;
        }
        let orphaned: Vec<String> = self
            .tool_buffers
            .values()
            .map(|b| format!("{}:{}", b.id, b.name))
            .collect();
        warn!(
            target: "uva.provider",
            count = orphaned.len(),
            tools = ?orphaned,
            "anthropic stream ended with orphan tool_use buffers; \
             the matching content_block_stop events were missing \
             (truncated SSE, network drop, or malformed upstream)"
        );
    }
}

impl Default for StreamProcessor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
