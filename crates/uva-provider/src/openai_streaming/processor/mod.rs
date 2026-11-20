use std::collections::HashMap;

use super::types::OpenAIDeltaResponse;
use crate::{
    streaming::{ThinkExtractor, ToolUseBuffer},
    types::{ContentPart, ReasoningContent, StreamChunk, TokenUsage},
};

/// Decode only: turns OpenAI-compatible SSE lines into [`StreamChunk`]s
/// and keeps nothing about the turn itself. The fold
/// ([`crate::assembler::BlockAssembler`]) owns the finished turn.
pub struct OpenAIStreamProcessor {
    /// Wire tool-call `index` → the call being accumulated. Kept only
    /// so a later argument delta can name its call and a seal can close
    /// the calls it saw.
    tool_buffers: HashMap<usize, ToolUseBuffer>,
    /// `true` once the turn has been sealed by a `finish_reason`.
    /// Frames after the seal are still decoded for their usage block,
    /// because some providers report usage after the finish marker, but
    /// they add no content and no second terminal chunk.
    terminal_emitted: bool,
    /// `true` after the `[DONE]` sentinel. Nothing after the
    /// end-of-stream marker belongs to this turn.
    saw_done: bool,
    /// Per-turn extractor that splits `<think>…</think>` markers out
    /// of plain-text deltas. Providers like MiniMax-M2.7 emit reasoning
    /// inline in `delta.content`; without this extractor the reasoning
    /// ends up concatenated with the visible answer on the frontend.
    think: ThinkExtractor,
}

impl OpenAIStreamProcessor {
    pub fn new() -> Self {
        Self {
            tool_buffers: HashMap::with_capacity(4),
            terminal_emitted: false,
            saw_done: false,
            think: ThinkExtractor::new(),
        }
    }

    pub fn process_line(&mut self, data: &str) -> Vec<StreamChunk> {
        // Past the end-of-stream marker nothing belongs to this turn.
        if self.saw_done {
            return vec![];
        }
        let data = data.trim();
        if data == "[DONE]" {
            self.saw_done = true;
            if self.terminal_emitted {
                return vec![];
            }
            // A stream that ends at the sentinel without a finish
            // marker is truncated. It has no stop reason to report, so
            // it emits no terminal chunk; the fold still drains any
            // tool buffer the stream never closed.
            self.terminal_emitted = true;
            return self.think.flush();
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
            chunks.push(usage_chunk(usage));
        }

        // OpenAI streaming: when a request is sent with `n > 1`,
        // each SSE message carries an array of N choices (one per
        // sample). Our agent layer consumes one stream per turn, so
        // we process only the FIRST choice and silently drop the
        // rest.
        let Some(choice) = delta_resp.choices.first() else {
            return chunks;
        };

        // Reasoning is emitted as a dedicated Content(Reasoning) chunk
        // without any text sniffing — the upstream already separates
        // reasoning from text into its own field.
        //
        // Only while the turn is open: once the finish marker sealed
        // it, a late frame can still carry this turn's usage block (the
        // `include_usage` terminator) but no part of its content.
        if !self.terminal_emitted
            && let Some(delta) = &choice.delta
        {
            if let Some(reasoning) = &delta.reasoning_content
                && !reasoning.is_empty()
            {
                chunks.push(StreamChunk::Content(ContentPart::Reasoning(
                    ReasoningContent {
                        text: reasoning.clone(),
                        signature: None,
                    },
                )));
            }
            if let Some(content) = &delta.content
                && !content.is_empty()
            {
                // Route text deltas through the shared ThinkExtractor
                // so `<think>…</think>` markers get split into separate
                // reasoning chunks.
                chunks.append(&mut self.think.process_text(content));
            }
            if let Some(tool_calls) = &delta.tool_calls {
                chunks.append(&mut self.decode_tool_calls(tool_calls));
            }
        }

        // Some gateways nest the usage block inside the choice instead of
        // the chunk. Honour it too — but only when the top level did not
        // already provide one, so the authoritative value is never
        // overwritten by a stale duplicate. This runs after the seal as
        // well: a late usage frame is still this turn's.
        if !chunks.iter().any(|c| matches!(c, StreamChunk::Usage(_)))
            && let Some(usage) = &choice.usage
        {
            chunks.push(usage_chunk(usage.clone()));
        }

        // A finish marker after the seal is a duplicate, not a second
        // turn.
        if self.terminal_emitted {
            return chunks;
        }

        if let Some(finish_reason) = &choice.finish_reason {
            self.terminal_emitted = true;
            // The extractor's carry is the tail of the text it withheld
            // while waiting for a marker to complete. It has to leave as
            // content, or it is lost whenever text already streamed.
            chunks.append(&mut self.think.flush());
            // Flush all open tool calls (this is the OpenAI-style "end
            // of stream" for tool calls).
            for (_, buffer) in std::mem::take(&mut self.tool_buffers) {
                chunks.push(StreamChunk::ToolCallEnd { id: buffer.id });
            }
            chunks.push(StreamChunk::Stop(finish_reason.clone()));
        }

        chunks
    }

    /// Decode one frame's tool-call deltas. The first delta for an index
    /// announces the call with `ToolCallStart`; later ones append to the
    /// buffered arguments as `ToolCallDelta`.
    fn decode_tool_calls(
        &mut self,
        tool_calls: &[super::types::OpenAIDeltaToolUse],
    ) -> Vec<StreamChunk> {
        let mut chunks = Vec::with_capacity(tool_calls.len());
        for tc in tool_calls {
            let index = tc.index.unwrap_or(0) as usize;
            let tc_id =
                tc.id.clone().unwrap_or_else(|| format!("call_{index}"));

            // First time we see this index: emit ToolCallStart.
            match self.tool_buffers.entry(index) {
                std::collections::hash_map::Entry::Vacant(vacant) => {
                    let name = tc
                        .function
                        .name
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string());
                    // The first delta may already include a partial
                    // argument string; treat that as part of the input
                    // buffer and emit only the start chunk.
                    let initial =
                        tc.function.arguments.clone().unwrap_or_default();
                    chunks.push(StreamChunk::ToolCallStart {
                        id: tc_id.clone(),
                        name: name.clone(),
                        arguments: serde_json::Value::String(initial.clone()),
                    });
                    vacant.insert(ToolUseBuffer {
                        id: tc_id,
                        name,
                        input: initial,
                        salvaged: false,
                    });
                }
                std::collections::hash_map::Entry::Occupied(mut occ) => {
                    let buffer = occ.get_mut();
                    // A later fragment can complete the call's name
                    // (the wire splits `id` from `function.name`); the
                    // fold owns the turn, so hand it the update rather
                    // than patching state the fold cannot see.
                    if let Some(name) = &tc.function.name {
                        buffer.name = name.clone();
                        chunks.push(StreamChunk::ToolCallName {
                            id: buffer.id.clone(),
                            name: name.clone(),
                        });
                    }
                    if let Some(args) = tc.function.arguments.as_ref()
                        && !args.is_empty()
                    {
                        buffer.input.push_str(args);
                        chunks.push(StreamChunk::ToolCallDelta {
                            id: buffer.id.clone(),
                            arguments_delta: args.clone(),
                        });
                    }
                }
            }
        }
        chunks
    }
}

/// Map one wire usage block onto the canonical buckets.
///
/// A usage block is a complete report, so the fold's last-usage-wins
/// rule keeps the most recent one — including one that lands after the
/// finish marker.
fn usage_chunk(usage: super::types::OpenAIDeltaUsage) -> StreamChunk {
    StreamChunk::Usage(TokenUsage {
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
    })
}

impl Default for OpenAIStreamProcessor {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests;
