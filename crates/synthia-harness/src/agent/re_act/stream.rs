//! Provider `StreamChunk` -> [`AgentEvent`] translator.
//! The only module that knows chunk shapes.
use std::sync::Arc;

#[cfg(test)]
use futures::channel::mpsc;
use synthia_provider::{BlockAssembler, ContentPart, StreamChunk};

use super::state::SampleOutcome;
use crate::{agent::sink::EventSink, events::AgentEvent};

/// `Arc<Mutex>` because the provider's `'static` callback outlives the borrow scope; `parking_lot` (not
/// `std`) because a poisoned mutex would propagate a panic on a path with no `SessionEnded` to report it with.
#[derive(Debug, Default, Clone)]
struct ChunkState(Arc<parking_lot::Mutex<ChunkStateInner>>);

#[derive(Debug, Default)]
struct ChunkStateInner {
    assembler: BlockAssembler,
    published: usize,
    /// Monotonic ordinal stamped on every `SessionEvent::AssistantChunk` so
    /// the durable log preserves delta order and `assemble_chunks` rebuilds
    /// the text deterministically.
    chunk_seq: u64,
}

/// `Clone` shares the pass-local fold state (an `Arc`) so the provider's
/// `'static` callback can own one next to a sink clone.
#[derive(Debug, Default, Clone)]
pub(super) struct ChunkAssembler {
    state: ChunkState,
}

impl ChunkAssembler {
    pub(super) fn new() -> Self {
        Self::default()
    }

    pub(super) fn ingest(&self, sink: &EventSink, chunk: StreamChunk) {
        let mut guard = self.state.0.lock();
        // Preserve the raw delta durably BEFORE folding it into the
        // assembled turn. Only emitted when a typed sink is wired,
        // so the in-memory test harness pays nothing.
        if sink.has_typed() {
            let chunk_seq = guard.chunk_seq;
            guard.chunk_seq += 1;
            // `describe_chunk` performs the single delta clone;
            // the single-key `data` object is built by hand so
            // the cloned String moves straight into the durable
            // record — byte-identical to the `json!` wire shape.
            let (delta, finish_reason) = describe_chunk(&chunk);
            let mut data = serde_json::Map::with_capacity(1);
            data.insert(
                String::from("delta"),
                serde_json::Value::String(delta),
            );
            sink.emit_typed(synthia_session::SessionEvent::AssistantChunk {
                seq: 0,
                ts: String::new(),
                chunk_seq,
                finish_reason,
                data: serde_json::Value::Object(data),
            });
        }
        handle_chunk(chunk, &mut guard, sink);
    }

    /// Safe to call before `IsDone`: the assembler drains any tool buffer the
    /// stream never closed (network cut-off, missing `ToolCallEnd`), so a
    /// partial tool call reaches the wire and history once.
    pub(super) fn finalize_outcome(&self, sink: &EventSink) -> SampleOutcome {
        let mut guard = self.state.0.lock();
        let (result, parts, _incomplete) = guard.assembler.finalize_assistant();
        // The drain above can append tool calls the stream never
        // closed; publish whatever is new before returning.
        publish_parts(&parts, &mut guard.published, sink);
        SampleOutcome {
            result: Arc::new(result),
            parts,
        }
    }
}

// ---------------------------------------------------------------------------
// Free helpers — kept because they operate on plain data, no state.
// ---------------------------------------------------------------------------

fn describe_chunk(chunk: &StreamChunk) -> (String, Option<String>) {
    match chunk {
        StreamChunk::Content(ContentPart::Text(t)) => (t.text.clone(), None),
        StreamChunk::Stop(reason) => (String::new(), Some(reason.clone())),
        StreamChunk::IsDone { result } => {
            (String::new(), result.stop_reason.clone())
        }
        _ => (String::new(), None),
    }
}

fn publish_parts(
    parts: &[ContentPart],
    published: &mut usize,
    sink: &EventSink,
) {
    while *published < parts.len() {
        sink.emit(AgentEvent::Model(parts[*published].clone()));
        *published += 1;
    }
}

fn handle_chunk(
    chunk: StreamChunk,
    state: &mut ChunkStateInner,
    sink: &EventSink,
) {
    let done = match &chunk {
        StreamChunk::IsDone { result } => Some((**result).clone()),
        _ => None,
    };
    state.assembler.push(chunk);
    {
        // Split the borrow so the slice read and the cursor write do
        // not both go through `state`.
        let ChunkStateInner {
            assembler,
            published,
            ..
        } = state;
        publish_parts(assembler.parts(), published, sink);
    }
    let Some(result) = done else {
        return;
    };
    let usage = &result.usage;
    sink.emit(AgentEvent::usage(
        usage.prompt_tokens,
        usage.completion_tokens,
        usage.cache_read_tokens,
        usage.cache_write_tokens,
    ));
    sink.emit(AgentEvent::ModelDone(result));
}

#[cfg(test)]
mod tests {
    //! Publish-cursor tests.
    //!
    //! The fold itself is [`synthia_provider::BlockAssembler`]'s
    //! contract and is tested there. What is only true *here* is the
    //! publishing rule: every part reaches the wire exactly once, in
    //! order, no matter which half of the stream produced it. These
    //! tests drive real chunks through [`ChunkAssembler`] so the
    //! cursor is exercised through its production entry points.

    use super::*;

    fn sink() -> (EventSink, mpsc::UnboundedReceiver<AgentEvent>) {
        let (tx, rx) = mpsc::unbounded();
        (EventSink::new(Arc::new(tx), None, None), rx)
    }

    fn model_parts(
        rx: &mut mpsc::UnboundedReceiver<AgentEvent>,
    ) -> Vec<String> {
        let mut seen = Vec::new();
        while let Ok(event) = rx.try_recv() {
            if let AgentEvent::Model(part) = event {
                seen.push(match part {
                    ContentPart::Text(t) => format!("text:{}", t.text),
                    ContentPart::Reasoning(r) => format!("reason:{}", r.text),
                    ContentPart::ToolUse(tu) => format!("call:{}", tu.id),
                    other => format!("other:{other:?}"),
                });
            }
        }
        seen
    }

    #[test]
    fn publishes_every_part_once_in_wire_order() {
        let (sink, mut rx) = sink();
        let assembler = ChunkAssembler::new();

        for chunk in [
            StreamChunk::Content(ContentPart::Text(
                synthia_provider::TextContent {
                    text: "hello".to_string(),
                    cache_control: None,
                },
            )),
            StreamChunk::Content(ContentPart::Reasoning(
                synthia_provider::ReasoningContent {
                    text: "thinking".to_string(),
                    signature: Some("sig".to_string()),
                },
            )),
            StreamChunk::ToolCallStart {
                id: "c1".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({}),
            },
            StreamChunk::ToolCallDelta {
                id: "c1".to_string(),
                arguments_delta: "{}".to_string(),
            },
            StreamChunk::ToolCallEnd {
                id: "c1".to_string(),
            },
        ] {
            assembler.ingest(&sink, chunk);
        }

        assert_eq!(
            model_parts(&mut rx),
            vec![
                "text:hello".to_string(),
                "reason:thinking".to_string(),
                "call:c1".to_string(),
            ],
            "each part must be published once, in wire order"
        );
    }

    /// A tool call the stream never closed still publishes — once — at finalize.
    /// The cursor makes it true: the drain appends the straggler to the same
    /// parts vec the cursor walks, so it neither double-publishes nor skips.
    #[test]
    fn publishes_unclosed_tool_call_once_at_finalize() {
        let (sink, mut rx) = sink();
        let assembler = ChunkAssembler::new();

        assembler.ingest(
            &sink,
            StreamChunk::ToolCallStart {
                id: "partial".to_string(),
                name: "echo".to_string(),
                arguments: serde_json::json!({}),
            },
        );
        assembler.ingest(
            &sink,
            StreamChunk::ToolCallDelta {
                id: "partial".to_string(),
                arguments_delta: "{}".to_string(),
            },
        );
        // No `ToolCallEnd`: the stream is cut off mid-call.
        let outcome = assembler.finalize_outcome(&sink);

        assert_eq!(
            model_parts(&mut rx),
            vec!["call:partial".to_string()],
            "the straggler must reach the wire once"
        );
        assert_eq!(
            outcome
                .result
                .tool_calls
                .iter()
                .map(|t| t.id.as_str())
                .collect::<Vec<_>>(),
            vec!["partial"],
            "and once in the assembled turn"
        );
    }

    #[test]
    fn finalize_is_idempotent_on_the_wire() {
        let (sink, mut rx) = sink();
        let assembler = ChunkAssembler::new();

        assembler.ingest(
            &sink,
            StreamChunk::Content(ContentPart::Text(
                synthia_provider::TextContent {
                    text: "answer".to_string(),
                    cache_control: None,
                },
            )),
        );
        let first = assembler.finalize_outcome(&sink);
        let _ = model_parts(&mut rx);

        let second = assembler.finalize_outcome(&sink);

        assert_eq!(first.result.text, "answer");
        assert!(
            model_parts(&mut rx).is_empty(),
            "a second finalize must not republish the turn"
        );
        assert!(
            second.parts.is_empty(),
            "and must not hand back a turn it already handed back"
        );
    }
}
