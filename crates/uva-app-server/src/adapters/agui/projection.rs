//! The projection from the core's stream into AG-UI events.
//!
//! The core publishes one event stream — sessions, turns, tool calls,
//! permissions, cancellation. The web front-end renders a run as a
//! stream of AG-UI events, so something has to translate. This
//! module is that translation, and it is the *only* thing that
//! knows AG-UI: the core never learns which protocol is watching it.
//!
//! # The mapping
//!
//! | the core | AG-UI |
//! |---|---|
//! | the turn's first event | `RUN_STARTED` |
//! | an assistant text delta | `TEXT_MESSAGE_START` + `TEXT_MESSAGE_CONTENT` |
//! | a reasoning delta | `REASONING_MESSAGE_CONTENT` |
//! | a model tool call | `TOOL_CALL_START` + `TOOL_CALL_ARGS` |
//! | a tool result | `TOOL_CALL_RESULT` |
//! | the model's response ended | `TEXT_MESSAGE_END` |
//! | the turn ended | `RUN_FINISHED` |
//! | the run failed | `RUN_ERROR` |
//!
//! # The state it keeps
//!
//! A projection is stateful per session: which message is open, which
//! reasoning message is open, which run is live. The state is what
//! makes the events *pairs* — a content chunk needs the start that
//! opened it — and it is why the projection is an object rather
//! than a function.

use uuid::Uuid;
use uva::harness::{AgentEvent, SystemEvent};

use crate::{adapters::agui::event::AguiEvent, event_stream::BroadcastEvent};

/// The projection of one session's core stream into AG-UI events.
pub struct AguiProjection {
    /// The conversation the events belong to.
    thread_id: String,
    /// The run that is live, when one is.
    run_id: Option<String>,
    /// The assistant message that is open, when one is.
    message_id: Option<String>,
    /// The reasoning message that is open, when one is.
    reasoning_id: Option<String>,
}

impl AguiProjection {
    /// A projection over one session's stream.
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            thread_id: session_id.into(),
            run_id: None,
            message_id: None,
            reasoning_id: None,
        }
    }

    /// The events one core item projects into.
    ///
    /// An item that has no AG-UI shape projects into nothing: the
    /// durable rows keep the fact, and the protocol has no place for
    /// it.
    pub fn project(&mut self, item: &BroadcastEvent) -> Vec<AguiEvent> {
        match item {
            BroadcastEvent::Run { turn_id, event } => {
                self.project_run(turn_id.as_deref(), event)
            }
            BroadcastEvent::TurnEnded { turn_id, .. } => {
                self.project_turn_end(turn_id.as_deref())
            }
        }
    }

    /// The events one run event projects into.
    fn project_run(
        &mut self,
        turn_id: Option<&str>,
        event: &AgentEvent,
    ) -> Vec<AguiEvent> {
        let mut events = Vec::new();
        if self.run_id.is_none() {
            let run_id = turn_id
                .map(str::to_string)
                .unwrap_or_else(|| Uuid::new_v4().to_string());
            self.run_id = Some(run_id.clone());
            events.push(AguiEvent::RunStarted {
                thread_id: self.thread_id.clone(),
                run_id,
            });
        }
        match event {
            AgentEvent::Model(uva::provider::ContentPart::Text(text)) => {
                let message_id = self.open_message();
                events.push(AguiEvent::TextMessageContent {
                    message_id,
                    delta: text.text.clone(),
                });
            }
            AgentEvent::Model(uva::provider::ContentPart::Reasoning(
                reasoning,
            )) => {
                let message_id = self.open_reasoning();
                events.push(AguiEvent::ReasoningMessageContent {
                    message_id,
                    delta: reasoning.text.clone(),
                });
            }
            AgentEvent::Model(uva::provider::ContentPart::ToolUse(call)) => {
                events.push(AguiEvent::ToolCallStart {
                    tool_call_id: call.id.clone(),
                    tool_call_name: call.name.clone(),
                    parent_message_id: self.message_id.clone(),
                });
                events.push(AguiEvent::ToolCallArgs {
                    tool_call_id: call.id.clone(),
                    delta: call.input.to_string(),
                });
                events.push(AguiEvent::ToolCallEnd {
                    tool_call_id: call.id.clone(),
                });
            }
            AgentEvent::Model(uva::provider::ContentPart::ToolResult(
                result,
            )) => {
                events.push(AguiEvent::ToolCallResult {
                    message_id: self
                        .message_id
                        .clone()
                        .unwrap_or_else(|| "tool".to_string()),
                    tool_call_id: result.tool_use_id.clone(),
                    content: tool_result_text(result),
                    role: "tool".to_string(),
                });
            }
            // The model's response is over: the message it was
            // building is complete, and so is the reasoning beside
            // it.
            AgentEvent::ModelDone(_) => {
                events.extend(self.close_messages());
            }
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: uva::harness::SessionEndReason::Error(message),
            }) => {
                events.extend(self.close_messages());
                events.push(AguiEvent::RunError {
                    message: message.clone(),
                    code: None,
                });
            }
            _ => {}
        }
        events
    }

    /// The events the turn's end projects into.
    fn project_turn_end(&mut self, turn_id: Option<&str>) -> Vec<AguiEvent> {
        let mut events = self.close_messages();
        let run_id = turn_id
            .map(str::to_string)
            .or_else(|| self.run_id.clone())
            .unwrap_or_else(|| Uuid::new_v4().to_string());
        self.run_id = None;
        events.push(AguiEvent::RunFinished {
            thread_id: self.thread_id.clone(),
            run_id,
        });
        events
    }

    /// The message id the next text chunk belongs to, opening one
    /// when none is open.
    fn open_message(&mut self) -> String {
        if let Some(id) = &self.message_id {
            return id.clone();
        }
        let id = Uuid::new_v4().to_string();
        self.message_id = Some(id.clone());
        id
    }

    /// The message id the next reasoning chunk belongs to, opening
    /// one when none is open.
    fn open_reasoning(&mut self) -> String {
        if let Some(id) = &self.reasoning_id {
            return id.clone();
        }
        let id = Uuid::new_v4().to_string();
        self.reasoning_id = Some(id.clone());
        id
    }

    /// The events that close the open messages.
    fn close_messages(&mut self) -> Vec<AguiEvent> {
        let mut events = Vec::new();
        if let Some(id) = self.message_id.take() {
            events.push(AguiEvent::TextMessageEnd { message_id: id });
        }
        if self.reasoning_id.take().is_some() {
            events.push(AguiEvent::ReasoningEnd);
        }
        events
    }
}

/// The text one tool result carries.
///
/// The protocol's result content is a string or a list of parts; the
/// projection sends the text the tool produced, which is what a
/// browser renders.
fn tool_result_text(result: &uva::provider::ToolResult) -> String {
    let text = result
        .content
        .iter()
        .filter_map(|part| part.text().map(str::to_string))
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        match result.is_error.unwrap_or(false) {
            true => "the call failed".to_string(),
            false => String::new(),
        }
    } else {
        text
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uva::{
        harness::AgentEvent,
        provider::{ContentPart, TextContent, ToolUse},
    };

    use super::*;

    /// The wire shape of one event.
    fn wire(event: &AguiEvent) -> serde_json::Value {
        serde_json::to_value(event).expect("the event serialises")
    }

    /// The run's first event opens the run.
    #[test]
    fn the_first_event_opens_the_run() {
        let mut projection = AguiProjection::new("s1");
        let events = projection.project(&BroadcastEvent::Run {
            turn_id: Some("t1".to_string()),
            event: Box::new(AgentEvent::ModelDone(
                uva::provider::SamplingResult {
                    text: String::new(),
                    tool_calls: Vec::new(),
                    reasoning: String::new(),
                    reasoning_signature: None,
                    usage: Default::default(),
                    stop_reason: None,
                },
            )),
        });
        assert_eq!(events.len(), 1);
        assert_eq!(
            wire(&events[0]),
            json!({"type": "RUN_STARTED", "threadId": "s1", "runId": "t1"})
        );
    }

    /// A text delta opens a message once and streams into it.
    #[test]
    fn a_text_delta_opens_a_message_and_streams_into_it() {
        let mut projection = AguiProjection::new("s1");
        let text = |delta: &str| BroadcastEvent::Run {
            turn_id: Some("t1".to_string()),
            event: Box::new(AgentEvent::Model(ContentPart::Text(
                TextContent {
                    text: delta.to_string(),
                    cache_control: None,
                },
            ))),
        };
        let first = projection.project(&text("hello"));
        assert_eq!(first.len(), 2);
        assert_eq!(wire(&first[0])["type"], "RUN_STARTED");
        assert_eq!(wire(&first[1])["type"], "TEXT_MESSAGE_CONTENT");
        assert_eq!(wire(&first[1])["delta"], "hello");
        let second = projection.project(&text(" world"));
        // The second chunk belongs to the message the first opened.
        assert_eq!(second.len(), 1);
        assert_eq!(wire(&second[0])["messageId"], wire(&first[1])["messageId"]);
        assert_eq!(wire(&second[0])["delta"], " world");
    }

    /// A tool call is a start, its arguments, and its end.
    #[test]
    fn a_tool_call_is_start_args_and_end() {
        let mut projection = AguiProjection::new("s1");
        let events = projection.project(&BroadcastEvent::Run {
            turn_id: Some("t1".to_string()),
            event: Box::new(AgentEvent::Model(ContentPart::ToolUse(ToolUse {
                id: "c1".to_string(),
                name: "read".to_string(),
                input: json!({"path": "a.txt"}),
            }))),
        });
        let kinds: Vec<String> = events
            .iter()
            .map(|event| wire(event)["type"].as_str().unwrap_or("").to_string())
            .collect();
        assert_eq!(
            kinds,
            vec![
                "RUN_STARTED",
                "TOOL_CALL_START",
                "TOOL_CALL_ARGS",
                "TOOL_CALL_END"
            ]
        );
        assert_eq!(wire(&events[1])["toolCallId"], "c1");
        assert_eq!(wire(&events[1])["toolCallName"], "read");
    }

    /// The turn's end closes the run.
    #[test]
    fn the_turns_end_finishes_the_run() {
        let mut projection = AguiProjection::new("s1");
        let _ = projection.project(&BroadcastEvent::Run {
            turn_id: Some("t1".to_string()),
            event: Box::new(AgentEvent::Model(ContentPart::Text(
                TextContent {
                    text: "hi".to_string(),
                    cache_control: None,
                },
            ))),
        });
        let events = projection.project(&BroadcastEvent::TurnEnded {
            turn_id: Some("t1".to_string()),
            reason: "completed".to_string(),
        });
        let last = wire(events.last().expect("the run finishes"));
        assert_eq!(last["type"], "RUN_FINISHED");
        assert_eq!(last["runId"], "t1");
        assert_eq!(last["threadId"], "s1");
    }
}
