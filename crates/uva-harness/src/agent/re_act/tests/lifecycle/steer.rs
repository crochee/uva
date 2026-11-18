//! A steer is a durable user row appended while a run is live, plus
//! the same message handed to the run's inbox. The row is the fact; the
//! inbox is how the live run learns about it without re-reading the
//! log. The step boundary drains the inbox *after* the tool batch the
//! current step already dispatched, so the request in flight never
//! changes and the next one sees the steer exactly once.

use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use uva_core::SystemClock;
use uva_provider::{ContentPart, Message, Role, ToolUse};
use uva_session::{LogWriter, in_memory::InMemorySessionSink, user_message};
use uva_tool::{Context, Tool, ToolEntry, ToolOutput, ToolRegistry};

use super::super::{support::*, *};
use crate::input::ChannelInbox;

// The steer text this test injects.
const STEER: &str = "steer: focus on the tests";

// A tool that does what the controller's steer path does: append a
// user row carrying `source: "steer"` through the session's writer,
// flush it, then hand the same message to the run's inbox. It runs
// after the tool batch was dispatched and before the next step
// boundary, which is exactly the window a live steer lands in.
struct SteerTool {
    writer: Arc<LogWriter>,
    inbox: futures::channel::mpsc::UnboundedSender<Message>,
}

#[async_trait]
impl Tool for SteerTool {
    fn name(&self) -> &str {
        "steer_probe"
    }

    fn description(&self) -> &str {
        "Appends a durable steer row, as the controller does for a live run."
    }

    fn parameters(&self) -> serde_json::Value {
        json!({"type": "object", "properties": {}})
    }

    async fn call(
        &self,
        _input: serde_json::Value,
        _context: &Context,
    ) -> ToolOutput {
        let row = user_message(
            serde_json::to_value(Message::user(STEER))
                .expect("a user message serialises"),
            Some("steer"),
        );
        self.writer
            .append_event(&row)
            .await
            .expect("append the steer row");
        self.writer.flush().await.expect("the steer row is durable");
        self.inbox
            .unbounded_send(Message::user(STEER))
            .expect("the live run is still draining its inbox");
        ToolOutput::text("steered")
    }
}

fn logged_turn(writer: &Arc<LogWriter>, text: &str) {
    let row = user_message(
        serde_json::to_value(Message::user(text))
            .expect("a user message serialises"),
        None,
    );
    writer.append_event_nowait(&row);
}

// The tool calls a request offered the model.
fn tool_calls(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .flat_map(|message| message.content.iter())
        .filter_map(|part| match part {
            ContentPart::ToolUse(call) => Some(call.name.clone()),
            _ => None,
        })
        .collect()
}

// Every user turn in a request, by text. The loop appends the
// runtime-context snapshot as a user-role message too; it is not a
// user turn.
fn user_turns(messages: &[Message]) -> Vec<String> {
    messages
        .iter()
        .filter(|message| message.role == Role::User)
        .filter_map(|message| message.content.extract_text())
        .filter(|text| !text.starts_with("Current runtime context"))
        .collect()
}

// The index of the first message with the given role.
fn index_of_role(messages: &[Message], role: Role) -> Option<usize> {
    messages.iter().position(|message| message.role == role)
}

// The index of the user turn carrying exactly `text`.
fn index_of_user_turn(messages: &[Message], text: &str) -> Option<usize> {
    messages.iter().position(|message| {
        message.role == Role::User
            && message.content.extract_text().as_deref() == Some(text)
    })
}

// A steer row appended after the tool batch was dispatched reaches
// the NEXT request and never rewrites the batch in flight.
//
// A standalone test rather than a `lifecycle::mod` table fixture:
// the behaviour is end-to-end (log → next request), and naming it
// lets a reviewer run exactly it.
#[tokio::test]
async fn a_steer_row_is_in_the_next_request_but_not_the_current_tool_batch() {
    let sink = Arc::new(InMemorySessionSink::new("steer"));
    let writer = Arc::new(LogWriter::new(sink, Arc::new(SystemClock)));
    logged_turn(&writer, "start");
    writer.flush().await.expect("the host row is durable");

    let (inbox, steer_sender) = ChannelInbox::channel();
    let registry = Arc::new(ToolRegistry::new());
    registry.register_entry(ToolEntry::new(Arc::new(SteerTool {
        writer: Arc::clone(&writer),
        inbox: steer_sender,
    })));

    let provider = Arc::new(CapturingProvider::new(vec![
        tool_call_response(vec![ToolUse {
            id: "call-1".to_string(),
            name: "steer_probe".to_string(),
            input: json!({}),
        }]),
        done_chunk("ok"),
    ]));
    let agent = ReActAgent::new(
        Arc::clone(&provider) as Arc<dyn uva_provider::traits::ModelProvider>,
        registry,
    )
    .with_log_writer(writer.clone());
    let mut stream = agent
        .run(
            AgentInput::text("start").with_inbox(inbox),
            Arc::new(CancellationToken::new()),
        )
        .await;
    while stream.next().await.is_some() {}

    let captured = provider.captured.lock().await;
    assert_eq!(
        captured.len(),
        2,
        "the tool call earns the run a second request"
    );
    let first = &captured[0];
    assert!(
        !user_turns(first).iter().any(|text| text == STEER),
        "a steer appended mid-step must not rewrite the request in \
         flight: {:?}",
        user_turns(first)
    );

    let second = &captured[1];
    assert_eq!(
        tool_calls(second),
        vec!["steer_probe".to_string()],
        "the batch the run dispatched is still one call: the steer is a \
         user turn beside it, not a rewrite of it"
    );
    let second_turns = user_turns(second);
    let seen = second_turns
        .iter()
        .filter(|text| text.as_str() == STEER)
        .count();
    assert_eq!(
        seen, 1,
        "the next request carries the steer exactly once: {second_turns:?}"
    );
    // The steer must not sit between the assistant's tool call and
    // the result that answers it: every provider rejects that
    // transcript, and the steer would break the run it steers.
    let tool_at = index_of_role(second, Role::Tool);
    let steer_at = index_of_user_turn(second, STEER);
    assert!(tool_at.is_some(), "the tool result is in the request");
    assert!(steer_at.is_some(), "the steer is in the request");
    assert!(
        tool_at < steer_at,
        "the steer follows the tool result it interrupted: {second:?}"
    );
}
