//! [`BlockAssembler`] — incremental chunk-to-message builder.
//!
//! dsh `packages/llm/llm/src/assembler.ts` parity (R6-1).
//!
//! ## Purpose
//!
//! Elysper's `ModelProvider::complete_with_stream` returns a
//! `Stream<Result<StreamChunk, Error>>`. The agent loop needs to
//! fold that stream into a final [`SamplingResult`] (text +
//! tool_calls + usage + stop reason) **and** a canonical list of
//! [`ContentPart`]s to feed the prompt builder. The previous
//! version of this folding logic lived inline in
//! `ReActLoop::sample_once` and `handle_chunk` (≈200 lines of
//! stream-state plumbing).
//!
//! [`BlockAssembler`] pulls that logic into a single
//! testable unit. Producers feed chunks; the loop reads
//! [`BlockAssembler::finalize_assistant`] once the stream ends.
//!
//! ## Why not just use `SamplingResult` from `IsDone`?
//!
//! Two reasons:
//!
//! 1. **Emission**: the agent loop wants to surface streamed
//!    chunks to the SSE event stream as they arrive
//!    (`AgentEvent::Model`). `BlockAssembler` keeps the
//!    accumulated parts so the loop can ship them without
//!    re-running the fold.
//! 2. **Stragglers**: `IsDone` re-lists every tool call the
//!    provider finalised, but the streamed `ToolCallEnd` chunks
//!    may have already pushed them into the parts vec. Without
//!    dedup the model-visible history ends up with duplicate
//!    tool calls. [`BlockAssembler::finalize_assistant`] pins
//!    the dedup contract.

use std::collections::{HashMap, HashSet};

use crate::{
    json_repair::parse_tool_input_logged,
    types::{
        ContentPart,
        ReasoningContent,
        SamplingResult,
        StreamChunk,
        TextContent,
        TokenUsage,
        ToolUse,
    },
};

/// One in-progress content block. Lives in [`BlockAssembler::partials`]
/// keyed by `StreamChunk::block_index()` (when the chunk carries
/// one) or the tool-call id (for the legacy `ToolCallStart` shape).
///
/// Open blocks carry their accumulated state (text + tool
/// [`BlockAssembler`] freezes the block
/// on `block-end` or `ToolCallEnd` so a straggler delta cannot
/// mutate it.
#[derive(Clone, Debug)]
enum PartialBlock {
    /// Open tool-use block — provider sends
    /// `ToolCallStart` / `ToolCallDelta` / `ToolCallEnd`.
    ToolUse {
        id: String,
        name: String,
        arguments: String,
        closed: bool,
    },
}

/// Fold a single provider stream into a final assistant
/// [`SamplingResult`] + canonical [`ContentPart`] list.
///
/// Construct one per LLM call. Feed every [`StreamChunk`] via
/// [`BlockAssembler::push`]; once the stream ends call
/// [`BlockAssembler::finalize_assistant`]. The
/// [`BlockAssembler`] is single-use: drop it after
/// `finalize_assistant` to free the partial buffer.
#[derive(Debug, Default)]
pub struct BlockAssembler {
    /// In-progress blocks keyed by either the chunk's
    /// `block_index()` (a numeric id the provider can stamp on
    /// each chunk) or the tool-call id for the legacy
    /// `ToolCallStart` / `ToolCallDelta` / `ToolCallEnd` shape.
    partials: HashMap<String, PartialBlock>,
    /// Stable insertion order so `finalize_assistant` reproduces
    /// the on-wire order even after dedup.
    order: Vec<String>,
    /// Accumulated text (concatenated `Content(Text)` deltas).
    text: String,
    /// Accumulated reasoning text (concatenated
    /// `Content(Reasoning)` deltas).
    reasoning: String,
    /// Whether at least one streamed text delta arrived. Used by
    /// the `IsDone` branch to decide whether to fold
    /// `result.text` in (only when no streamed text existed —
    /// non-streaming providers batch everything into `IsDone`).
    saw_streamed_text: bool,
    /// Same logic for reasoning.
    saw_streamed_reasoning: bool,
    /// The most recent `signature_delta` for the reasoning block.
    ///
    /// It arrives **after** the `thinking_delta`s that carried the text
    /// (that is the Anthropic event order), so the streamed
    /// [`ContentPart::Reasoning`] chunks cannot carry it — see
    /// `streaming::anthropic::processor`, which stamps them with
    /// whatever it has at that moment, i.e. `None`. It therefore reaches
    /// us on the terminal `IsDone` and is applied to the already-built
    /// parts in [`BlockAssembler::finalize_assistant`], where the parts
    /// are still ours to patch.
    ///
    /// Dropping it would silently break interleaved extended thinking:
    /// Anthropic rejects a re-sent thinking block without its signature,
    /// so the turn cannot be continued.
    reasoning_signature: Option<String>,
    /// `tool_use_id`s already pushed into `parts` from streamed
    /// `ToolCallEnd` chunks. The `IsDone` `result.tool_calls`
    /// re-listing is deduped against this set so we never push
    /// the same tool call twice.
    seen_tool_ids: HashSet<String>,
    /// Final usage from the `Usage` chunk; only the last one
    /// wins (Anthropic emits multiple; the terminal `message.usage`
    /// is authoritative).
    usage: Option<TokenUsage>,
    /// Provider-reported stop reason.
    stop_reason: Option<String>,
    /// Has `IsDone` arrived? After `finalize_assistant` returns,
    /// the loop expects this to be `true`.
    is_done: bool,
    /// Final assembled content parts in wire order.
    parts: Vec<ContentPart>,
}

impl BlockAssembler {
    /// Build a fresh assembler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one chunk from the provider stream. Safe to call
    /// before, between, or after `IsDone` — `finalize_assistant`
    /// pins the ordering invariants.
    pub fn push(&mut self, chunk: StreamChunk) {
        match chunk {
            StreamChunk::Content(part) => match part {
                ContentPart::Text(tc) => {
                    self.saw_streamed_text = true;
                    self.text.push_str(&tc.text);
                    self.append_part(ContentPart::Text(tc));
                }
                ContentPart::Reasoning(rc) => {
                    self.saw_streamed_reasoning = true;
                    self.reasoning.push_str(&rc.text);
                    self.append_part(ContentPart::Reasoning(rc));
                }
                other => self.append_part(other),
            },
            StreamChunk::Usage(usage) => {
                // Anthropic emits intermediate `message_delta.usage`
                // before the terminal one. We let the last writer
                // win so the assembled result carries the
                // authoritative count.
                self.usage = Some(usage);
            }
            StreamChunk::Stop(reason) => {
                if self.stop_reason.is_none() {
                    self.stop_reason = Some(reason);
                }
            }
            StreamChunk::ToolCallStart {
                id,
                name,
                arguments,
            } => {
                let initial_args = match arguments {
                    serde_json::Value::String(s) => s,
                    other => other.to_string(),
                };
                if !self.partials.contains_key(&id) {
                    self.order.push(id.clone());
                }
                self.partials.insert(
                    id.clone(),
                    PartialBlock::ToolUse {
                        id,
                        name,
                        arguments: initial_args,
                        closed: false,
                    },
                );
            }
            StreamChunk::ToolCallDelta {
                id,
                arguments_delta,
            } => {
                if let Some(PartialBlock::ToolUse { arguments, .. }) =
                    self.partials.get_mut(&id)
                {
                    arguments.push_str(&arguments_delta);
                }
            }
            StreamChunk::ToolCallEnd { id } => {
                if let Some(PartialBlock::ToolUse { closed, .. }) =
                    self.partials.get_mut(&id)
                {
                    *closed = true;
                }
                if let Some(PartialBlock::ToolUse {
                    id: tool_id,
                    name,
                    arguments,
                    ..
                }) = self.partials.get(&id).cloned()
                {
                    let parsed = parse_tool_input_logged(&arguments, &name);
                    let tool_use = ToolUse {
                        id: tool_id.clone(),
                        name,
                        input: parsed,
                    };
                    if !self.seen_tool_ids.contains(&tool_use.id) {
                        self.seen_tool_ids.insert(tool_use.id.clone());
                        self.append_part(ContentPart::ToolUse(tool_use));
                    }
                }
            }
            StreamChunk::IsDone { result } => {
                self.is_done = true;
                self.fold_is_done(*result);
            }
        }
    }

    /// True once [`StreamChunk::IsDone`] has been pushed.
    #[must_use]
    pub fn is_done(&self) -> bool {
        self.is_done
    }

    /// Read-only access to the assembled content parts. Useful
    /// for the agent loop's mid-stream event emission, which
    /// pushes the current view to the SSE stream on every chunk.
    #[must_use]
    pub fn parts(&self) -> &[ContentPart] {
        &self.parts
    }

    /// Accumulated text so far. Used by the loop to publish a
    /// "partial assistant text" event without re-walking
    /// `parts`.
    #[must_use]
    pub fn text_so_far(&self) -> &str {
        &self.text
    }

    /// Accumulated reasoning text so far.
    #[must_use]
    pub fn reasoning_so_far(&self) -> &str {
        &self.reasoning
    }

    /// Read-only view of the most recent `Usage` chunk.
    #[must_use]
    pub fn usage(&self) -> Option<&TokenUsage> {
        self.usage.as_ref()
    }

    /// Read-only view of the provider-reported stop reason.
    #[must_use]
    pub fn stop_reason(&self) -> Option<&str> {
        self.stop_reason.as_deref()
    }

    /// Finish the fold. Returns the assembled
    /// [`SamplingResult`] (consumed by the loop to publish
    /// `AgentEvent::System(Usage)` / `ModelDone`) **and** a
    /// canonical [`ContentPart`] vec (consumed by the history
    /// builder).
    ///
    /// Calling `finalize_assistant` before `IsDone` is allowed;
    /// the assembler returns the partial state it has so the
    /// loop can publish a cancellation message. The
    /// `incomplete` flag on the returned tuple signals the
    /// caller that the provider stream ended without a clean
    /// `IsDone`.
    #[must_use]
    pub fn finalize_assistant(
        &mut self,
    ) -> (SamplingResult, Vec<ContentPart>, bool) {
        // Drain any tool buffers the stream did not explicitly
        // close (e.g. a `ToolCallStart` followed by a stream
        // error). Elysper's previous code already handled this in
        // `finalize_buffers` — folded into the assembler so the
        // loop has one fewer helper to call.
        let drained_ids: Vec<String> = self
            .partials
            .iter()
            .filter_map(|(id, block)| match block {
                PartialBlock::ToolUse { closed: false, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        for id in drained_ids {
            if let Some(PartialBlock::ToolUse {
                id: tool_id,
                name,
                arguments,
                ..
            }) = self.partials.get(&id).cloned()
            {
                let parsed = parse_tool_input_logged(&arguments, &name);
                let tool_use = ToolUse {
                    id: tool_id.clone(),
                    name,
                    input: parsed,
                };
                if !self.seen_tool_ids.contains(&tool_use.id) {
                    self.seen_tool_ids.insert(tool_use.id.clone());
                    self.append_part(ContentPart::ToolUse(tool_use));
                }
            }
        }

        // Stamp the deferred signature onto the reasoning parts that
        // were streamed without one.
        //
        // One thinking block produces *several* parts: a
        // `content_block_start` carrying the opening text plus one per
        // `thinking_delta`, and none of them can see the signature,
        // which arrives last.
        //
        // Only the **trailing contiguous** reasoning run is stamped.
        // That bound is load-bearing: on the streaming path *every*
        // reasoning part is un-signed (see `reasoning_signature`), so a
        // walk that kept going past a signed part — or past any
        // non-reasoning part — would hand the one latest signature to
        // every earlier block too, forging a signature onto text it was
        // never computed for. A signature is bound to the block it
        // signed.
        if let Some(signature) = self.reasoning_signature.clone()
            && let Some(last) = self
                .parts
                .iter()
                .rposition(|p| matches!(p, ContentPart::Reasoning(_)))
        {
            for part in self.parts[..=last].iter_mut().rev() {
                match part {
                    ContentPart::Reasoning(rc) if rc.signature.is_none() => {
                        rc.signature = Some(signature.clone());
                    }
                    // An already-signed reasoning part ends the run, as
                    // does anything that is not reasoning.
                    _ => break,
                }
            }
        }

        let tool_uses: Vec<ToolUse> = self
            .parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::ToolUse(tu) => Some(tu.clone()),
                _ => None,
            })
            .collect();

        let result = SamplingResult {
            text: self.text.clone(),
            tool_calls: tool_uses,
            reasoning: self.reasoning.clone(),
            reasoning_signature: self.reasoning_signature.clone(),
            usage: self.usage.clone().unwrap_or_default(),
            stop_reason: self.stop_reason.clone(),
        };
        let parts = std::mem::take(&mut self.parts);
        let incomplete = !self.is_done;
        (result, parts, incomplete)
    }

    // -- private helpers --------------------------------------------------

    /// Append a content part while preserving insertion order.
    /// Text/Reasoning already mutated `self.text` /
    /// `self.reasoning` in `push`; non-text parts go straight
    /// onto the parts vec.
    fn append_part(&mut self, part: ContentPart) {
        self.parts.push(part);
    }

    /// Fold an `IsDone` chunk's terminal `SamplingResult` into
    /// the accumulated state. Dedupes tool calls against the
    /// streamed `ToolCallEnd` ids; folds `result.text` only when
    /// no streamed text arrived (non-streaming providers batch
    /// everything into `IsDone`).
    fn fold_is_done(&mut self, result: SamplingResult) {
        if !self.saw_streamed_text && !result.text.is_empty() {
            self.text.push_str(&result.text);
            self.append_part(ContentPart::Text(TextContent {
                text: result.text.clone(),
                cache_control: None,
            }));
        }
        // The signature may arrive with reasoning text that was already
        // streamed (the usual shape) or with no text at all, so record
        // it regardless of whether the text branch below runs.
        if let Some(signature) = result.reasoning_signature.clone() {
            self.reasoning_signature = Some(signature);
        }
        if !self.saw_streamed_reasoning && !result.reasoning.is_empty() {
            let reasoning_text = result.reasoning.clone();
            self.reasoning.push_str(&reasoning_text);
            self.append_part(ContentPart::Reasoning(ReasoningContent {
                text: reasoning_text,
                signature: self.reasoning_signature.clone(),
            }));
        }
        for call in &result.tool_calls {
            if self.seen_tool_ids.contains(&call.id) {
                continue;
            }
            self.seen_tool_ids.insert(call.id.clone());
            self.append_part(ContentPart::ToolUse(call.clone()));
        }
        // `IsDone` carries the authoritative usage; let it
        // overwrite any intermediate `Usage` chunks (Anthropic
        // emits multiple).
        self.usage = Some(result.usage);
        if self.stop_reason.is_none() {
            self.stop_reason = result.stop_reason;
        }
    }
}

#[cfg(test)]
mod tests;
