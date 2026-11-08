use elysper::{
    harness::SystemEvent,
    provider::{
        ContentPart,
        ReasoningContent,
        SamplingResult,
        TextContent,
        TokenUsage,
        ToolResult,
        ToolUse,
    },
};

use super::*;
use crate::routes::messages::{MESSAGES_EVENT, MessagesOutcome};

fn text_event(text: &str) -> AgentEvent {
    AgentEvent::Model(ContentPart::Text(TextContent {
        text: text.to_string(),
        cache_control: None,
    }))
}

fn reasoning_event(text: &str, signature: Option<&str>) -> AgentEvent {
    AgentEvent::Model(ContentPart::Reasoning(ReasoningContent {
        text: text.to_string(),
        signature: signature.map(str::to_string),
    }))
}

fn tool_use_event(id: &str) -> AgentEvent {
    AgentEvent::Model(ContentPart::ToolUse(ToolUse {
        id: id.to_string(),
        name: "bash".to_string(),
        input: serde_json::json!({ "cmd": "ls" }),
    }))
}

fn tool_result_event(id: &str) -> AgentEvent {
    let mut result = ToolResult::new(id, "a.txt");
    // The runtime labels every result with the tool that produced it
    // (`commit_tool_result`), which is what lets a client render
    // 工具 · <name> even for a call it never saw.
    result.tool_name = Some("bash".to_string());
    AgentEvent::Model(ContentPart::ToolResult(result))
}

/// A result the runtime left unnamed — a replayed transcript row can
/// be, and the block must then omit `name` rather than invent one.
fn unnamed_tool_result_event(id: &str) -> AgentEvent {
    AgentEvent::Model(ContentPart::ToolResult(ToolResult::new(id, "a.txt")))
}

fn failed_tool_result_event(id: &str) -> AgentEvent {
    AgentEvent::Model(ContentPart::ToolResult(ToolResult::error(id, "boom")))
}

fn usage_event(input: usize, output: usize) -> AgentEvent {
    AgentEvent::System(SystemEvent::Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: None,
        cache_creation_tokens: None,
    })
}

fn done_event(stop_reason: Option<&str>) -> AgentEvent {
    AgentEvent::ModelDone(SamplingResult {
        stop_reason: stop_reason.map(str::to_string),
        usage: TokenUsage {
            prompt_tokens: 5,
            completion_tokens: 7,
            total_tokens: 12,
            ..TokenUsage::default()
        },
        ..SamplingResult::default()
    })
}

fn ended_event(reason: SessionEndReason) -> AgentEvent {
    AgentEvent::System(SystemEvent::SessionEnded { reason })
}

/// A turn that folds `events` and returns the accumulator plus every
/// frame `on_event` produced, in order.
fn fold(events: &[AgentEvent]) -> (Turn, Vec<Frame>) {
    fold_in(events, false)
}

/// The same, for a client that opted into tool blocks.
fn fold_with_tools(events: &[AgentEvent]) -> (Turn, Vec<Frame>) {
    fold_in(events, true)
}

fn fold_in(events: &[AgentEvent], tool_blocks: bool) -> (Turn, Vec<Frame>) {
    let mut turn = Turn::new("msg_1", "claude-test", tool_blocks);
    let mut frames = Vec::new();
    for event in events {
        frames.extend(turn.on_event(event));
    }
    (turn, frames)
}

/// The `content_block` a start frame carries, as it appears on the
/// wire — the shape a client actually parses.
fn block_json(frame: &Frame) -> serde_json::Value {
    match frame {
        Frame::ContentBlockStart { content_block, .. } => {
            serde_json::to_value(content_block).unwrap()
        }
        other => panic!("{other:?} is not a block start"),
    }
}

fn names(frames: &[Frame]) -> Vec<&'static str> {
    frames.iter().map(frame_name).collect()
}

fn frame_name(frame: &Frame) -> &'static str {
    match frame {
        Frame::MessageStart(_) => MESSAGE_START,
        Frame::ContentBlockStart { .. } => CONTENT_BLOCK_START,
        Frame::ContentBlockDelta { .. } => CONTENT_BLOCK_DELTA,
        Frame::ContentBlockStop { .. } => CONTENT_BLOCK_STOP,
        Frame::MessageDelta { .. } => MESSAGE_DELTA,
        Frame::MessageStop => MESSAGE_STOP,
        Frame::Error(_) => ERROR,
    }
}

/// The response index a block frame addresses.
fn index(frame: &Frame) -> usize {
    match frame {
        Frame::ContentBlockStart { index, .. }
        | Frame::ContentBlockDelta { index, .. }
        | Frame::ContentBlockStop { index } => *index,
        other => panic!("{other:?} is not a block frame"),
    }
}

/// Two text deltas stream as one `text` block: start, one delta per
/// part, one stop — and the assembled block holds the
/// concatenation.
#[test]
fn text_deltas_share_one_block() {
    let (mut turn, frames) = fold(&[text_event("hel"), text_event("lo")]);
    assert_eq!(
        names(&frames),
        vec![
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_DELTA,
            CONTENT_BLOCK_DELTA
        ]
    );
    assert_eq!(frames.iter().map(index).collect::<Vec<_>>(), vec![0, 0, 0]);

    let tail = turn.finish();
    assert_eq!(
        names(&tail),
        vec![CONTENT_BLOCK_STOP, MESSAGE_DELTA, MESSAGE_STOP]
    );
    assert_eq!(index(&tail[0]), 0);
    let message = turn.message();
    assert_eq!(message.content.len(), 1);
    match &message.content[0] {
        ResponseBlock::Text { text } => assert_eq!(text, "hello"),
        other => panic!("expected one text block: {other:?}"),
    }
}

/// Thinking then text produces two blocks with sequential indices,
/// and the thinking block keeps the signature it was given.
#[test]
fn thinking_and_text_are_separate_blocks() {
    let (mut turn, frames) =
        fold(&[reasoning_event("why", Some("sig")), text_event("answer")]);
    assert_eq!(
        names(&frames),
        vec![
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_DELTA,
            CONTENT_BLOCK_DELTA,
            CONTENT_BLOCK_STOP,
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_DELTA
        ]
    );
    let indices: Vec<usize> = frames.iter().map(index).collect();
    assert_eq!(indices, vec![0, 0, 0, 0, 1, 1]);
    turn.finish();
    let message = turn.message();
    assert_eq!(message.content.len(), 2);
    match &message.content[0] {
        ResponseBlock::Thinking {
            thinking,
            signature,
        } => {
            assert_eq!(thinking, "why");
            assert_eq!(signature, "sig");
        }
        other => panic!("first block must be thinking: {other:?}"),
    }
}

/// An executed tool call never reaches the client: the
/// `tool_use` part is dropped once its `tool_result` proves the
/// agent ran it, and the response is just the final text.
#[test]
fn executed_tool_calls_are_not_surfaced() {
    let (mut turn, frames) = fold(&[
        tool_use_event("toolu_1"),
        tool_result_event("toolu_1"),
        text_event("done"),
    ]);
    assert_eq!(
        names(&frames),
        vec![CONTENT_BLOCK_START, CONTENT_BLOCK_DELTA],
        "a call the agent executed must produce no block"
    );
    turn.finish();
    let message = turn.message();
    assert_eq!(message.content.len(), 1);
    assert_eq!(message.stop_reason.as_deref(), Some("end_turn"));
}

/// Text emitted *before* a call closes its block first: every block
/// gets its own index, in the order the model produced them.
#[test]
fn a_call_closes_the_text_block_that_led_up_to_it() {
    let (mut turn, frames) = fold_with_tools(&[
        text_event("working"),
        tool_use_event("toolu_1"),
        tool_result_event("toolu_1"),
        text_event("done"),
    ]);
    assert_eq!(
        names(&frames),
        vec![
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_DELTA,
            CONTENT_BLOCK_STOP,
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_DELTA,
            CONTENT_BLOCK_STOP,
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_STOP,
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_DELTA,
        ]
    );
    assert_eq!(
        frames.iter().map(index).collect::<Vec<_>>(),
        vec![0, 0, 0, 1, 1, 1, 2, 2, 3, 3],
        "four blocks, one index each — a client cannot tell two \
         blocks apart if they share an index"
    );
    // The text deltas land in the blocks that enclose them, so the
    // `("working")` delta precedes the call and `("done")` follows
    // the result.
    turn.finish();
    let message = turn.message();
    let kinds: Vec<&str> = message
        .content
        .iter()
        .map(|block| match block {
            ResponseBlock::Text { .. } => "text",
            ResponseBlock::ToolUse { .. } => "tool_use",
            ResponseBlock::ToolResult { .. } => "tool_result",
            ResponseBlock::Thinking { .. } => "thinking",
        })
        .collect();
    assert_eq!(kinds, vec!["text", "tool_use", "tool_result", "text"]);
}

/// An opted-in client sees the call *and* its result as two blocks,
/// paired by the call id, with the call's input on the
/// `input_json_delta` and the result's content inline — the two
/// halves the web UI renders as 请求 / 结果.
#[test]
fn opted_in_clients_see_a_call_and_its_result_paired_by_id() {
    let (mut turn, frames) = fold_with_tools(&[
        tool_use_event("toolu_1"),
        tool_result_event("toolu_1"),
        text_event("done"),
    ]);
    assert_eq!(
        names(&frames),
        vec![
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_DELTA,
            CONTENT_BLOCK_STOP,
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_STOP,
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_DELTA
        ]
    );
    assert_eq!(
        frames.iter().map(index).collect::<Vec<_>>(),
        vec![0, 0, 0, 1, 1, 2, 2],
        "the call, its result and the answer are three blocks"
    );

    assert_eq!(
        block_json(&frames[0]),
        serde_json::json!({
            "type": "tool_use",
            "id": "toolu_1",
            "name": "bash",
            "input": {},
        }),
        "the call opens as the protocol's own tool_use block"
    );
    assert_eq!(
        block_json(&frames[3]),
        serde_json::json!({
            "type": "tool_result",
            "tool_use_id": "toolu_1",
            "name": "bash",
            "content": [{ "type": "text", "text": "a.txt" }],
            "is_error": false,
        }),
        "the result names the call it answers, and says it succeeded"
    );

    // The non-streaming body agrees with the frames, value for value.
    turn.finish();
    let message = turn.message();
    assert_eq!(
        serde_json::to_value(&message.content).unwrap(),
        serde_json::json!([
            {
                "type": "tool_use",
                "id": "toolu_1",
                "name": "bash",
                "input": { "cmd": "ls" },
            },
            {
                "type": "tool_result",
                "tool_use_id": "toolu_1",
                "name": "bash",
                "content": [{ "type": "text", "text": "a.txt" }],
                "is_error": false,
            },
            { "type": "text", "text": "done" },
        ])
    );
    assert_eq!(
        message.stop_reason.as_deref(),
        Some("end_turn"),
        "an opted-in client is never asked to answer a call it saw run"
    );
}

/// A tool that failed is distinguishable from one that succeeded
/// without reading its output, and an unnamed result omits `name`
/// rather than inventing one.
#[test]
fn opted_in_results_flag_failure_and_success() {
    let (_, frames) = fold_with_tools(&[failed_tool_result_event("toolu_err")]);
    let block = block_json(&frames[0]);
    assert_eq!(block["is_error"], true);
    assert_eq!(block["tool_use_id"], "toolu_err");
    assert_eq!(block["content"][0]["text"], "boom");

    let (_, frames) = fold_with_tools(&[tool_result_event("toolu_ok")]);
    assert_eq!(block_json(&frames[0])["is_error"], false);
    assert_eq!(block_json(&frames[0])["name"], "bash");

    let (_, frames) = fold_with_tools(&[unnamed_tool_result_event("t")]);
    assert!(
        block_json(&frames[0]).get("name").is_none(),
        "an unnamed result must omit the field, not send null: {:?}",
        block_json(&frames[0])
    );
}

/// An unexecuted call is surfaced exactly once in either mode, and
/// an opted-in run reports the run's own stop reason rather than
/// `tool_use`.
#[test]
fn opted_in_runs_keep_the_runs_own_stop_reason() {
    let (mut turn, frames) =
        fold_with_tools(&[tool_use_event("toolu_9"), text_event("hi")]);
    assert_eq!(
        names(&frames),
        vec![
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_DELTA,
            CONTENT_BLOCK_STOP,
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_DELTA
        ],
        "an executed-or-not call is surfaced as it arrives, never twice"
    );
    turn.finish();
    assert_eq!(turn.message().stop_reason.as_deref(), Some("end_turn"));
}

/// The same event stream under both modes differs only by the tool
/// blocks: an external client's frames, content and stop reason are
/// what they were before the opt-in existed.
#[test]
fn the_modes_differ_only_by_the_tool_blocks() {
    let events = [
        tool_use_event("toolu_1"),
        tool_result_event("toolu_1"),
        text_event("answer"),
        usage_event(3, 4),
        done_event(Some("end_turn")),
        ended_event(SessionEndReason::Completed),
    ];
    let (mut plain, plain_frames) = fold(&events);
    let (mut opted_in, opted_frames) = fold_with_tools(&events);

    assert_eq!(
        names(&plain_frames),
        vec![CONTENT_BLOCK_START, CONTENT_BLOCK_DELTA],
        "an external client sees neither half of an executed call"
    );
    assert_eq!(
        names(&opted_frames),
        vec![
            // The call: the protocol's own three frames...
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_DELTA,
            CONTENT_BLOCK_STOP,
            // ...its result: start (whole payload) + stop...
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_STOP,
            // ...and then the answer's text block.
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_DELTA,
        ],
        "the opt-in adds exactly the two blocks, nothing else"
    );
    assert_eq!(
        opted_frames.len(),
        plain_frames.len() + 5,
        "three frames for the call, two for its result"
    );

    plain.finish();
    opted_in.finish();
    let plain = plain.message();
    let opted_in = opted_in.message();
    assert_eq!(
        serde_json::to_value(&plain.content).unwrap(),
        serde_json::json!([{ "type": "text", "text": "answer" }]),
        "the unopted-in content is the run's text alone"
    );
    assert_eq!(plain.stop_reason, opted_in.stop_reason);
    assert_eq!(plain.usage.input_tokens, opted_in.usage.input_tokens);
    assert_eq!(plain.usage.output_tokens, opted_in.usage.output_tokens);
    assert_eq!(plain.id, opted_in.id);
    assert_eq!(plain.model, opted_in.model);
}

/// A call the run never executed IS the assistant's answer, so it
/// is surfaced as a `tool_use` block with the input as one
/// `input_json_delta`, and the stop reason says `tool_use`.
#[test]
fn unexecuted_tool_calls_become_tool_use_blocks() {
    let (mut turn, _) = fold(&[tool_use_event("toolu_9")]);
    let tail = turn.finish();
    assert_eq!(
        names(&tail),
        vec![
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_DELTA,
            CONTENT_BLOCK_STOP,
            MESSAGE_DELTA,
            MESSAGE_STOP
        ]
    );
    let message = turn.message();
    match &message.content[0] {
        ResponseBlock::ToolUse { id, name, input } => {
            assert_eq!(id, "toolu_9");
            assert_eq!(name, "bash");
            assert_eq!(input["cmd"], "ls");
        }
        other => panic!("expected a tool_use block: {other:?}"),
    }
    assert_eq!(message.stop_reason.as_deref(), Some("tool_use"));
}

/// Usage accumulates over the run's sampling passes, and the
/// provider's stop reason is mapped into Anthropic's vocabulary.
#[test]
fn usage_and_stop_reason_come_from_the_run() {
    let (mut turn, _) = fold(&[
        usage_event(10, 2),
        done_event(Some("stop")),
        usage_event(4, 3),
        done_event(Some("end_turn")),
    ]);
    turn.finish();
    let message = turn.message();
    assert_eq!(message.usage.input_tokens, 14);
    assert_eq!(message.usage.output_tokens, 5);
    assert_eq!(message.stop_reason.as_deref(), Some("end_turn"));
}

/// Without a `SystemEvent::Usage` (strategy-driven runs) the
/// `SamplingResult`'s own counters are used instead.
#[test]
fn sampling_result_usage_is_the_fallback() {
    let (mut turn, _) = fold(&[done_event(None)]);
    turn.finish();
    let message = turn.message();
    assert_eq!(message.usage.input_tokens, 5);
    assert_eq!(message.usage.output_tokens, 7);
}

/// A run that ended in an error terminates with an `error` frame —
/// no `message_delta`, no `message_stop`.
#[test]
fn failed_runs_terminate_with_an_error_frame() {
    let (mut turn, _) = fold(&[
        text_event("partial"),
        ended_event(SessionEndReason::Error("provider exploded".to_string())),
    ]);
    let tail = turn.finish();
    assert_eq!(
        names(&tail),
        vec![CONTENT_BLOCK_STOP, ERROR],
        "an error must replace the success terminal frames"
    );
    let Some(Frame::Error(envelope)) = tail.last() else {
        panic!("the last frame must be the error")
    };
    let json = envelope.value();
    assert_eq!(json["type"], "error");
    assert_eq!(json["error"]["type"], "api_error");
    assert_eq!(json["error"]["message"], "provider exploded");
    assert_eq!(
        turn.end_reason(),
        Some(&SessionEndReason::Error("provider exploded".to_string()))
    );
}

/// Hitting the iteration cap reports `max_tokens`, the closest
/// Anthropic stop reason for "the model was cut off".
#[test]
fn iteration_cap_reports_max_tokens() {
    let (mut turn, _) = fold(&[
        text_event("x"),
        ended_event(SessionEndReason::MaxIterations),
    ]);
    turn.finish();
    assert_eq!(turn.message().stop_reason.as_deref(), Some("max_tokens"));
}

/// `message_start` is a message with no content and no stop reason,
/// which is what the protocol's first frame carries.
#[test]
fn message_start_is_an_empty_assistant_message() {
    let turn = Turn::new("msg_1", "claude-test", false);
    let Frame::MessageStart(message) = turn.start() else {
        panic!("start must be a message_start frame")
    };
    assert_eq!(message.kind, "message");
    assert_eq!(message.role, "assistant");
    assert_eq!(message.id, "msg_1");
    assert_eq!(message.model, "claude-test");
    assert!(message.content.is_empty());
    assert!(message.stop_reason.is_none());
    assert!(message.stop_sequence.is_none());
    assert_eq!(message.usage.input_tokens, 0);
}

/// A subagent's wrapped trace is not this message's content.
#[test]
fn subagent_events_are_ignored() {
    let inner = text_event("child answer");
    let wrapped = AgentEvent::Agent(
        elysper::harness::AgentMeta::new("parent", "child", 1),
        Box::new(inner),
    );
    let (mut turn, frames) = fold(&[wrapped]);
    assert!(frames.is_empty(), "a child trace must produce no frames");
    turn.finish();
    assert!(turn.message().content.is_empty());
}

/// A streamed run publishes the protocol's event sequence on the
/// process's outbound channel — the same names, the same payloads and
/// the same order the SSE stream used to carry.
///
/// The fixture's own channel has no reader (see
/// [`crate::state::AppState::for_test`]), so the fixture replaces the
/// detached RPC runtime with a live one and keeps the receiving half;
/// that is what these notifications arrive on.
#[tokio::test]
async fn a_streamed_run_publishes_the_event_sequence() {
    let mut fixture = super::super::tests::Fixture::new().await;
    let outcome = fixture
        .call(super::super::tests::request(true))
        .await
        .expect("the run starts");
    assert!(
        matches!(outcome, MessagesOutcome::Streaming { accepted: true }),
        "a streaming request answers the acceptance, not the body"
    );

    // Read the notifications up to the terminal event.
    let mut events: Vec<(String, serde_json::Value)> = Vec::new();
    loop {
        let envelope = fixture.next_envelope().await;
        assert_eq!(
            envelope["method"], MESSAGES_EVENT,
            "a streamed run publishes on the anthropic event method"
        );
        let event = envelope["params"]["event"]
            .as_str()
            .expect("the event name is a string")
            .to_string();
        let terminal = event == MESSAGE_STOP;
        events.push((event, envelope["params"]["data"].clone()));
        if terminal {
            break;
        }
    }

    let names: Vec<&str> =
        events.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            MESSAGE_START,
            CONTENT_BLOCK_START,
            CONTENT_BLOCK_DELTA,
            CONTENT_BLOCK_STOP,
            MESSAGE_DELTA,
            MESSAGE_STOP,
        ],
        "the frames travel in the order the protocol sends them"
    );

    // The payloads are the protocol's own objects.
    assert_eq!(events[0].1["type"], MESSAGE_START);
    assert_eq!(events[0].1["message"]["role"], ASSISTANT_ROLE);
    assert_eq!(events[1].1["type"], CONTENT_BLOCK_START);
    assert_eq!(events[1].1["index"], 0);
    assert_eq!(events[1].1["content_block"]["type"], "text");
    assert_eq!(events[2].1["type"], CONTENT_BLOCK_DELTA);
    assert_eq!(events[2].1["index"], 0);
    assert_eq!(events[2].1["delta"]["type"], "text_delta");
    assert_eq!(events[2].1["delta"]["text"], "hello");
    assert_eq!(events[3].1["type"], CONTENT_BLOCK_STOP);
    assert_eq!(events[4].1["type"], MESSAGE_DELTA);
    assert_eq!(events[4].1["delta"]["stop_reason"], "end_turn");
    assert_eq!(events[5].1["type"], MESSAGE_STOP);
}
