//! Folding a run's [`AgentEvent`] stream into an Anthropic message.
//!
//! [`Turn`] is the one place that decides what a run's event stream
//! means on the Anthropic wire. Both response paths drive the same
//! accumulator — the streaming handler forwards every [`Frame`] it
//! returns, the non-streaming handler ignores the frames and serialises
//! [`Turn::message`] — so the JSON body and the concatenated stream can
//! never disagree.
//!
//! # What becomes content
//!
//! | event | response |
//! |---|---|
//! | `Model(ContentPart::Text)` | a `text` block delta |
//! | `Model(ContentPart::Reasoning)` | a `thinking` block delta |
//! | `Model(ContentPart::ToolUse)` | buffered; see below |
//! | `Model(ContentPart::ToolResult)` | nothing; see below |
//! | `Model(Image / Audio / Resource)` | nothing |
//! | `System(Usage)` / `ModelDone` | token totals and `stop_reason` |
//! | `System(SessionEnded{Error})` | a terminal `error` frame |
//! | everything else | nothing |
//!
//! ## Tool visibility
//!
//! This server executes its own tools, so a `tool_use` the model asked
//! for is never something the client is expected to answer, and a
//! `ToolResult` is tool *output* rather than the model's answer. What
//! the client is shown therefore depends on who the client is, and
//! [`Turn::new`]'s last argument decides:
//!
//! - **`tool_blocks: false`** — an external Anthropic client (the SDKs,
//!   Claude Code, editor plugins). A call is buffered and dropped as
//!   soon as a `ToolResult` proves the agent executed it; it is only
//!   surfaced as a `tool_use` block when the run ends with a call nobody
//!   ran (the loop stopping right after the model asked for one) — the
//!   case where the call *is* the assistant's final answer. A result
//!   produces nothing at all.
//! - **`tool_blocks: true`** — a request that opted in through
//!   `metadata.elysper_session_id`, i.e. Elysper's own web client, which
//!   renders a collapsible `工具 · <name>` block with 请求 / 结果 halves.
//!   Every call and every result is surfaced as it arrives, so the two
//!   halves can be paired by id.
//!
//! ### The opted-in block shapes
//!
//! A call uses the protocol's own block — `content_block_start` seeded
//! with `{"type":"tool_use","id":…,"name":…,"input":{}}`, one
//! `input_json_delta` carrying the whole input, `content_block_stop` —
//! which is exactly the shape [`Turn::surface_tool_use`] already emits
//! for a never-executed call. The corresponding result becomes its own
//! block:
//!
//! ```text
//! {"type":"tool_result","tool_use_id":…,"name":…,"content":[…],"is_error":…}
//! ```
//!
//! Anthropic has no assistant `tool_result` block (the protocol carries
//! one inside a *user* message), so this is the endpoint's one
//! documented extension to the protocol. It lives *inside* the
//! assistant's content because the whole point is that the call and its
//! result are observable in one turn — the client cannot answer a call
//! it was never handed, and a separate `user` message would be the
//! endpoint talking to itself. Two deliberate deviations from
//! Anthropic's `tool_result` spelling: `name` is included (a result the
//! runtime labels, so a client can render 工具 · `name` even when it
//! never saw the call), and `is_error` is always present as a boolean
//! rather than an optional value, so a failure is never indistinguishable
//! from a success. The result's content is complete when the event
//! arrives, so it rides the `content_block_start` and only
//! `content_block_stop` follows — no deltas.
//!
//! Everything else is identical between the two modes: the block
//! indices, the text/thinking folding, the usage totals, and — for a run
//! with no tool activity — the bytes.
//!
//! ## Thinking signatures
//!
//! A streamed reasoning block usually carries no signature: the harness
//! publishes each reasoning part as it arrives, and stamps the
//! `signature_delta` value onto the assembled turn only afterwards
//! (`elysper::provider::BlockAssembler`). The signature is
//! therefore attached to the block when the part itself carries one,
//! and left empty otherwise.

use elysper::{
    harness::{AgentEvent, SessionEndReason, SystemEvent},
    provider::{ContentPart, ImageContent, ToolResult, ToolUse},
};
use serde::Serialize;
use serde_json::Value;

use super::wire::{
    ASSISTANT_ROLE,
    CONTENT_BLOCK_DELTA,
    CONTENT_BLOCK_START,
    CONTENT_BLOCK_STOP,
    ContentBlockDelta,
    ContentBlockStart,
    ContentBlockStop,
    Delta,
    ERROR,
    ErrorEnvelope,
    MESSAGE_DELTA,
    MESSAGE_KIND,
    MESSAGE_START,
    MESSAGE_STOP,
    MessageDelta,
    MessageDeltaBody,
    MessageEnvelope,
    MessageStart,
    MessageStop,
    ResponseBlock,
    ResultBlock,
    ResultImageSource,
    Usage,
    error_type,
};

/// One frame of an Anthropic event stream.
///
/// The handler turns each frame into the `(event name, payload)` pair
/// the wire carries with [`Frame::event`].
#[derive(Debug)]
pub(super) enum Frame {
    MessageStart(MessageEnvelope),
    ContentBlockStart {
        index: usize,
        content_block: ResponseBlock,
    },
    ContentBlockDelta {
        index: usize,
        delta: Delta,
    },
    ContentBlockStop {
        index: usize,
    },
    MessageDelta {
        delta: MessageDelta,
        usage: Usage,
    },
    MessageStop,
    Error(ErrorEnvelope),
}
impl Frame {
    /// This frame's event name and its payload.
    ///
    /// The name is the protocol's own event name (`message_start`,
    /// `content_block_delta`, …) and the payload is the object that
    /// event carries — the two halves the SSE `event:` / `data:` pair
    /// used to spell.
    pub(super) fn event(&self) -> (&'static str, Value) {
        match self {
            Self::MessageStart(message) => frame(
                MESSAGE_START,
                &MessageStart {
                    kind: MESSAGE_START,
                    message: message.clone(),
                },
            ),
            Self::ContentBlockStart {
                index,
                content_block,
            } => frame(
                CONTENT_BLOCK_START,
                &ContentBlockStart {
                    kind: CONTENT_BLOCK_START,
                    index: *index,
                    content_block: content_block.clone(),
                },
            ),
            Self::ContentBlockDelta { index, delta } => frame(
                CONTENT_BLOCK_DELTA,
                &ContentBlockDelta {
                    kind: CONTENT_BLOCK_DELTA,
                    index: *index,
                    delta: clone_delta(delta),
                },
            ),
            Self::ContentBlockStop { index } => frame(
                CONTENT_BLOCK_STOP,
                &ContentBlockStop {
                    kind: CONTENT_BLOCK_STOP,
                    index: *index,
                },
            ),
            Self::MessageDelta { delta, usage } => frame(
                MESSAGE_DELTA,
                &MessageDeltaBody {
                    kind: MESSAGE_DELTA,
                    delta: MessageDelta {
                        stop_reason: delta.stop_reason.clone(),
                        stop_sequence: delta.stop_sequence.clone(),
                    },
                    usage: usage.clone(),
                },
            ),
            Self::MessageStop => {
                frame(MESSAGE_STOP, &MessageStop { kind: MESSAGE_STOP })
            }
            Self::Error(envelope) => (ERROR, envelope.value()),
        }
    }
}
/// One event: its name and the object it carries.
fn frame<T: Serialize>(name: &'static str, body: &T) -> (&'static str, Value) {
    let payload =
        serde_json::to_value(body).unwrap_or_else(|_| serde_json::json!({}));
    (name, payload)
}
/// `Delta` is not `Clone` on purpose (it carries owned strings that are
/// moved into the frame); rendering clones through this helper keeps
/// the frame type `Debug`-only.
fn clone_delta(delta: &Delta) -> Delta {
    match delta {
        Delta::Text { text } => Delta::Text { text: text.clone() },
        Delta::InputJson { partial_json } => Delta::InputJson {
            partial_json: partial_json.clone(),
        },
        Delta::Thinking { thinking } => Delta::Thinking {
            thinking: thinking.clone(),
        },
        Delta::Signature { signature } => Delta::Signature {
            signature: signature.clone(),
        },
    }
}
/// The block kinds that absorb streaming deltas.
#[derive(Clone, Copy, PartialEq, Eq)]
enum DeltaKind {
    Text,
    Thinking,
}
/// One Anthropic message being assembled from a run's events.
pub(super) struct Turn {
    id: String,
    model: String,
    /// Whether a tool call and its result become observable blocks.
    /// `false` for an external Anthropic client, `true` for a request
    /// that opted in through `metadata.elysper_session_id` — see the
    /// module docs.
    tool_blocks: bool,
    /// Blocks that have been closed (and so appear in the response).
    blocks: Vec<ResponseBlock>,
    /// The one block currently open, if any. Its response index is
    /// `blocks.len()`.
    open: Option<ResponseBlock>,
    /// Tool calls the model asked for but that no `ToolResult` has
    /// proven executed yet.
    pending_tool_calls: Vec<ToolUse>,
    /// Set once a buffered call is surfaced, so the terminal
    /// `stop_reason` can say `tool_use`.
    surfaced_tool_use: bool,
    usage: Usage,
    /// `true` once a `SystemEvent::Usage` has been folded, which makes
    /// it the usage source for the whole run.
    system_usage_seen: bool,
    /// The provider's own stop reason, mapped to Anthropic's
    /// vocabulary, from the last `ModelDone`.
    stop_reason: Option<String>,
    /// How the run ended, set by the terminal event.
    end: Option<SessionEndReason>,
}
impl Turn {
    pub(super) fn new(
        id: impl Into<String>,
        model: impl Into<String>,
        tool_blocks: bool,
    ) -> Self {
        Self {
            id: id.into(),
            model: model.into(),
            tool_blocks,
            blocks: Vec::new(),
            open: None,
            pending_tool_calls: Vec::new(),
            surfaced_tool_use: false,
            usage: Usage::default(),
            system_usage_seen: false,
            stop_reason: None,
            end: None,
        }
    }

    /// The `message_start` frame: the message before any content, with
    /// an empty usage block.
    ///
    /// Token counts cannot be known yet — the run has not been
    /// sampled — so the head reports zeroes and the real totals ride
    /// the terminal `message_delta`, whose `usage` object carries
    /// `input_tokens` alongside `output_tokens` for exactly this
    /// reason.
    pub(super) fn start(&self) -> Frame {
        Frame::MessageStart(self.envelope(Vec::new(), None))
    }

    /// Fold one event, returning the frames it produces.
    pub(super) fn on_event(&mut self, event: &AgentEvent) -> Vec<Frame> {
        match event {
            AgentEvent::Model(part) => self.on_part(part),
            AgentEvent::ModelDone(result) => {
                self.on_model_done(result);
                Vec::new()
            }
            AgentEvent::System(SystemEvent::Usage {
                input_tokens,
                output_tokens,
                cache_read_tokens,
                cache_creation_tokens,
            }) => {
                self.on_usage(
                    *input_tokens,
                    *output_tokens,
                    *cache_read_tokens,
                    *cache_creation_tokens,
                );
                Vec::new()
            }
            AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
                self.end = Some(reason.clone());
                Vec::new()
            }
            // Progress / warnings / steering notices are diagnostics,
            // and an `Agent(..)` event is a *subagent's* trace: neither
            // is part of this message.
            AgentEvent::System(_) | AgentEvent::Agent(..) => Vec::new(),
        }
    }

    /// Close the message: close the open block, surface any tool call
    /// the run never executed, and emit the terminal frames.
    ///
    /// A run that ended in an error terminates with an `error` frame
    /// instead of `message_delta` + `message_stop`, so a client never
    /// sees a successful terminal frame for a failed run.
    pub(super) fn finish(&mut self) -> Vec<Frame> {
        let mut frames = self.close_open();
        let tool_calls = std::mem::take(&mut self.pending_tool_calls);
        self.surfaced_tool_use = !tool_calls.is_empty();
        for tool_use in tool_calls {
            frames.extend(self.surface_tool_use(tool_use));
        }
        match &self.end {
            Some(SessionEndReason::Error(message)) => {
                frames.push(Frame::Error(ErrorEnvelope::new(
                    error_type::API,
                    message.clone(),
                )));
            }
            // A suspension is not a successful stop. `/v1/messages`
            // has no approval frame, so the stream terminates with the
            // protocol's `error` event rather than a `message_stop`
            // the client would read as a finished answer.
            Some(SessionEndReason::Suspended) => {
                frames.push(Frame::Error(ErrorEnvelope::new(
                    error_type::INVALID_REQUEST,
                    super::APPROVAL_REQUIRED,
                )));
            }
            _ => {
                frames.push(Frame::MessageDelta {
                    delta: MessageDelta {
                        stop_reason: self.stop_reason(),
                        stop_sequence: None,
                    },
                    usage: self.usage.clone(),
                });
                frames.push(Frame::MessageStop);
            }
        }
        frames
    }

    /// The complete message, for the non-streaming body.
    pub(super) fn message(&self) -> MessageEnvelope {
        self.envelope(self.blocks.clone(), Some(self.stop_reason()))
    }

    /// How the run ended, once the terminal event has been folded.
    pub(super) fn end_reason(&self) -> Option<&SessionEndReason> {
        self.end.as_ref()
    }

    // -- folding ----------------------------------------------------------
    fn on_part(&mut self, part: &ContentPart) -> Vec<Frame> {
        match part {
            ContentPart::Text(text) => {
                let mut frames = self.ensure_open(DeltaKind::Text);
                if let Some(ResponseBlock::Text { text: open }) =
                    self.open.as_mut()
                {
                    open.push_str(&text.text);
                }
                frames.push(self.block_delta(Delta::Text {
                    text: text.text.clone(),
                }));
                frames
            }
            ContentPart::Reasoning(reasoning) => {
                let mut frames = self.ensure_open(DeltaKind::Thinking);
                if let Some(ResponseBlock::Thinking {
                    thinking,
                    signature,
                }) = self.open.as_mut()
                {
                    thinking.push_str(&reasoning.text);
                    if let Some(value) = &reasoning.signature {
                        *signature = value.clone();
                    }
                }
                frames.push(self.block_delta(Delta::Thinking {
                    thinking: reasoning.text.clone(),
                }));
                if let Some(signature) = &reasoning.signature {
                    frames.push(self.block_delta(Delta::Signature {
                        signature: signature.clone(),
                    }));
                }
                frames
            }
            // Opted-in clients see both halves of a tool execution (the
            // module docs explain why they are separate blocks);
            // external clients see neither, unless the run never
            // executed the call at all.
            ContentPart::ToolUse(tool_use) => {
                if self.tool_blocks {
                    return self.surface_tool_use(tool_use.clone());
                }
                self.pending_tool_calls.push(tool_use.clone());
                Vec::new()
            }
            ContentPart::ToolResult(result) => {
                if self.tool_blocks {
                    return self.surface_tool_result(result);
                }
                // Proof the agent ran the batch — drop the buffered calls.
                self.pending_tool_calls.clear();
                Vec::new()
            }
            ContentPart::Image(_)
            | ContentPart::Audio(_)
            | ContentPart::Resource(_) => Vec::new(),
        }
    }

    fn on_model_done(&mut self, result: &elysper::provider::SamplingResult) {
        if let Some(raw) = &result.stop_reason {
            self.stop_reason = Some(anthropic_stop_reason(raw).to_string());
        }
        // Strategy-driven runs (best-of-n, CoT) publish a
        // `SamplingResult` without going through the chunk sink, so no
        // `SystemEvent::Usage` is emitted for the pass.
        if !self.system_usage_seen {
            self.usage.input_tokens += result.usage.prompt_tokens as u64;
            self.usage.output_tokens += result.usage.completion_tokens as u64;
        }
    }

    fn on_usage(
        &mut self,
        input_tokens: usize,
        output_tokens: usize,
        cache_read_tokens: Option<usize>,
        cache_creation_tokens: Option<usize>,
    ) {
        self.system_usage_seen = true;
        self.usage.input_tokens += input_tokens as u64;
        self.usage.output_tokens += output_tokens as u64;
        add_optional(
            &mut self.usage.cache_read_input_tokens,
            cache_read_tokens,
        );
        add_optional(
            &mut self.usage.cache_creation_input_tokens,
            cache_creation_tokens,
        );
    }

    /// Surface one tool call as a complete `tool_use` block.
    ///
    /// Called for a never-executed call in every mode, and for *every*
    /// call when the client opted in.
    ///
    /// The protocol streams tool input as partial JSON; this server
    /// holds the whole call already, so it emits the input as a single
    /// `input_json_delta` — the same wire shape a non-streaming
    /// provider produces under the streaming protocol.
    fn surface_tool_use(&mut self, tool_use: ToolUse) -> Vec<Frame> {
        // A tool call ends the text (or thinking) block that led up to
        // it: the open block must be closed *before* this one claims an
        // index, or the two would share one — a client cannot tell two
        // blocks with the same index apart.
        let mut frames = self.close_open();
        let index = self.blocks.len();
        let partial_json = serde_json::to_string(&tool_use.input)
            .unwrap_or_else(|_| "{}".to_string());
        frames.extend([
            Frame::ContentBlockStart {
                index,
                content_block: ResponseBlock::ToolUse {
                    id: tool_use.id.clone(),
                    name: tool_use.name.clone(),
                    input: serde_json::json!({}),
                },
            },
            Frame::ContentBlockDelta {
                index,
                delta: Delta::InputJson { partial_json },
            },
            Frame::ContentBlockStop { index },
        ]);
        self.blocks.push(ResponseBlock::ToolUse {
            id: tool_use.id,
            name: tool_use.name,
            input: tool_use.input,
        });
        frames
    }

    /// Surface one executed tool's output as a `tool_result` block — the
    /// opted-in client's 结果 half, paired to its call by `tool_use_id`.
    ///
    /// The result's content is whole when the event arrives, so the block
    /// rides the `content_block_start` and only the `content_block_stop`
    /// follows: there is nothing to stream in pieces and nothing for a
    /// client to accumulate.
    fn surface_tool_result(&mut self, result: &ToolResult) -> Vec<Frame> {
        let mut frames = self.close_open();
        let index = self.blocks.len();
        let block = ResponseBlock::ToolResult {
            tool_use_id: result.tool_use_id.clone(),
            name: result.tool_name.clone(),
            content: result.content.iter().map(result_block).collect(),
            is_error: result.is_error.unwrap_or(false),
        };
        self.blocks.push(block.clone());
        frames.extend([
            Frame::ContentBlockStart {
                index,
                content_block: block,
            },
            Frame::ContentBlockStop { index },
        ]);
        frames
    }

    // -- block bookkeeping ------------------------------------------------
    /// Close the open block if it is not already `kind`, then keep
    /// `kind` open — emitting the `content_block_*` frames the
    /// transition needs.
    fn ensure_open(&mut self, kind: DeltaKind) -> Vec<Frame> {
        let already = matches!(
            (&self.open, kind),
            (Some(ResponseBlock::Text { .. }), DeltaKind::Text)
                | (Some(ResponseBlock::Thinking { .. }), DeltaKind::Thinking)
        );
        if already {
            return Vec::new();
        }
        let mut frames = self.close_open();
        let seed = match kind {
            DeltaKind::Text => ResponseBlock::empty_text(),
            DeltaKind::Thinking => ResponseBlock::empty_thinking(),
        };
        frames.push(Frame::ContentBlockStart {
            index: self.blocks.len(),
            content_block: seed.clone(),
        });
        self.open = Some(seed);
        frames
    }

    /// Close the open block, if any, returning its stop frame.
    fn close_open(&mut self) -> Vec<Frame> {
        let Some(block) = self.open.take() else {
            return Vec::new();
        };
        let index = self.blocks.len();
        self.blocks.push(block);
        vec![Frame::ContentBlockStop { index }]
    }

    /// A delta frame for the open block.
    fn block_delta(&self, delta: Delta) -> Frame {
        Frame::ContentBlockDelta {
            index: self.blocks.len(),
            delta,
        }
    }

    fn stop_reason(&self) -> String {
        if self.surfaced_tool_use {
            return "tool_use".to_string();
        }
        if matches!(self.end, Some(SessionEndReason::MaxIterations)) {
            return "max_tokens".to_string();
        }
        self.stop_reason
            .clone()
            .unwrap_or_else(|| "end_turn".to_string())
    }

    fn envelope(
        &self,
        content: Vec<ResponseBlock>,
        stop_reason: Option<String>,
    ) -> MessageEnvelope {
        MessageEnvelope {
            id: self.id.clone(),
            kind: MESSAGE_KIND,
            role: ASSISTANT_ROLE,
            model: self.model.clone(),
            content,
            stop_reason,
            stop_sequence: None,
            usage: self.usage.clone(),
        }
    }
}
/// Project one part of a tool result's content onto the protocol's
/// `tool_result` block vocabulary.
///
/// `text` and `image` are the two spellings the protocol defines for a
/// result's content. Every other part — audio, a nested call, a resource
/// link — has none, and a result the client cannot read is worse than one
/// rendered as its JSON, so those become text.
fn result_block(part: &ContentPart) -> ResultBlock {
    match part {
        ContentPart::Text(text) => ResultBlock::Text {
            text: text.text.clone(),
        },
        ContentPart::Image(image) => ResultBlock::Image {
            source: result_image_source(image),
        },
        other => ResultBlock::Text {
            text: serde_json::to_string(other).unwrap_or_default(),
        },
    }
}
/// The `image.source` spelling for a canonical image.
///
/// [`super::convert::image_part`] keeps a remote image's URL *in*
/// `data`, so classifying it back is what keeps a round trip lossless.
fn result_image_source(image: &ImageContent) -> ResultImageSource {
    if image.data.starts_with("http://") || image.data.starts_with("https://") {
        ResultImageSource::Url {
            url: image.data.clone(),
        }
    } else {
        ResultImageSource::Base64 {
            media_type: image.mime_type.clone(),
            data: image.data.clone(),
        }
    }
}
/// Map a provider stop reason onto Anthropic's vocabulary.
fn anthropic_stop_reason(raw: &str) -> &'static str {
    match raw {
        "tool_use" | "tool_calls" | "function_call" => "tool_use",
        "max_tokens" | "length" => "max_tokens",
        "stop_sequence" => "stop_sequence",
        "refusal" => "refusal",
        // `end_turn`, OpenAI's `stop`, and anything unrecognised: the
        // model finished its turn.
        _ => "end_turn",
    }
}
fn add_optional(slot: &mut Option<u64>, value: Option<usize>) {
    if let Some(value) = value {
        *slot.get_or_insert(0) += value as u64;
    }
}
#[cfg(test)]
mod tests;
