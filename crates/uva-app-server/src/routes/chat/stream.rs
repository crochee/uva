//! `chat/message/stream` — send one message and stream its answer.
//!
//! The headless core's native streaming door. `chat/message/send`
//! answers `{queued: true}` and leaves the client to reconstruct the
//! answer from the follow stream's cumulative snapshots; this method
//! answers the id of the turn it started and publishes that turn's
//! deltas as they happen, one notification per model chunk, until the
//! turn ends. A client that wants the whole transcript follows the
//! session (`sessions/follow`); a client that wants *the answer to
//! this message* reads this stream and nothing else.
//!
//! # The frames
//!
//! `chat/turn/event`, one JSON-RPC notification per frame, each
//! carrying `{session_id, turn_id, seq, kind, …}`:
//!
//! | `kind` | carries | when |
//! |---|---|---|
//! | `text` | `text` | one model text chunk |
//! | `reasoning` | `text` | one reasoning chunk |
//! | `tool_call` | `call_id`, `name`, `arguments` | the model asked for a tool |
//! | `tool_progress` | `call_id`, `tool`, `content` | a running tool emitted a chunk |
//! | `tool_result` | `call_id`, `tool`, `content`, `is_error` | a tool call settled |
//! | `usage` | `input_tokens`, `output_tokens` | the sampling accounting |
//! | `warning` | `message` | the loop warned |
//! | `gap` | `dropped` | live frames were dropped — the text has a hole; the durable rows do not |
//! | `end` | `reason` | the turn ended (`completed` / `cancelled` / `suspended`) |
//!
//! `seq` numbers the frames of one turn from `1`, so a client can
//! detect a lost frame by its own arithmetic — a gap in the sequence
//! is a dropped notification, and the `gap` frame is the server
//! admitting the same fact from its side.
//!
//! # Correlation
//!
//! `turn_id` is the answer's second field. It is the same id the
//! turn's durable `parked_prompt` row carries, so a client that reads
//! the stream and a client that reads the transcript agree on which
//! turn is which. The gateway routes the frames by `session_id` — a
//! client connected through it follows the session, as it does for
//! `session/event`.

use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use uuid::Uuid;
use uva::{
    core::Error,
    harness::AgentEvent,
    provider::ContentPart,
    tool::FileDiff,
};

use super::SendMessageRequest;
use crate::{
    event_stream::BroadcastEvent,
    rpc::{Outbound, RpcError},
    session::controller::{PinnedProvider, SessionOp},
    state::AppState,
};

/// The notification method the turn's frames travel on.
pub const TURN_EVENT: &str = "chat/turn/event";

/// What `chat/message/stream` answers.
#[derive(Debug, Serialize)]
pub struct StreamMessageResponse {
    /// The stream is open; the answer follows as [`TURN_EVENT`]
    /// notifications.
    pub accepted: bool,
    /// The turn this request started. Every frame of the answer
    /// carries it, and so does the turn's `parked_prompt` row — one
    /// id for the live stream and the transcript.
    pub turn_id: String,
}

/// `chat/message/stream` — `chat/message/send`'s request, the turn's
/// deltas back.
///
/// # Errors
///
/// The same refusals `chat/message/send` reports: a `model` the
/// deployment cannot serve, an empty message, attachments the mounted
/// services cannot admit. A `delivery: "steer"` request is refused —
/// a steer injects into a run that is already live, so there is no
/// new turn of its own to stream.
pub async fn stream_message(
    state: Arc<AppState>,
    session_id: String,
    req: SendMessageRequest,
) -> Result<StreamMessageResponse, RpcError> {
    // The model selection is resolved before anything is queued,
    // exactly as `send_message` does: a selection the server cannot
    // honour must not leave the client watching a turn that runs a
    // different model.
    let pinned = match req.model.as_deref() {
        Some(selection) => {
            let provider = state
                .resolve_provider(Some(selection))
                .map_err(super::selection_error)?;
            let id = state.provider_id(Some(selection)).unwrap_or_default();
            Some(PinnedProvider::new(provider).with_id(id))
        }
        None => None,
    };
    if req.text.trim().is_empty() && req.attachments.is_empty() {
        let message =
            "a message needs text or at least one attachment".to_string();
        return Err(RpcError::from(Error::validation(message.clone()))
            .with_code("empty_message")
            .with_message(message));
    }
    if req.delivery == super::Delivery::Steer {
        let message = "a streamed turn cannot steer a live run; send it \
                       with chat/message/send instead"
            .to_string();
        return Err(RpcError::from(Error::validation(message.clone()))
            .with_code("invalid_steer")
            .with_message(message));
    }
    let attachments = state.attachments();
    super::admit_attachments(&req, attachments.is_some()).map_err(
        |message| {
            RpcError::from(Error::validation(message.to_string()))
                .with_code("attachments_unavailable")
                .with_message(message)
        },
    )?;
    let parts = super::build_parts(&req, attachments.as_deref());
    // The op ladder is `send_message`'s: a payload with attachments or
    // an agent override parks a `PromptMulti`, everything else a plain
    // `Prompt`.
    let explicit_agent = req.agent_name.is_some();
    let agent_name = state.resolve_agent_name_for(req.agent_name.as_deref());
    let op = if parts.len() > 1
        || parts.iter().any(|p| !matches!(p, ContentPart::Text(_)))
        || explicit_agent
    {
        SessionOp::PromptMulti {
            parts,
            agent_name,
            priority: 1,
        }
    } else {
        let text = match parts.into_iter().next() {
            Some(ContentPart::Text(t)) => t.text,
            _ => String::new(),
        };
        SessionOp::Prompt {
            content: text,
            priority: 1,
        }
    };
    crate::api::validate_resource_name(&session_id)?;
    let user_id = super::resolve_user_id(&state);
    let controller = state
        .create_on_demand_session_controller(&user_id, &session_id)
        .await?;
    // The turn's id is minted here, handed to the controller, and
    // answered to the client: the client and the durable row share
    // it, and the pump below matches the turn's frames by it.
    let turn_id = Uuid::new_v4().to_string();
    // Subscribe before dispatching: the channel only carries what is
    // published after the subscription, so a pump that arrived after
    // the run started would miss the opening chunks.
    let events = controller.subscribe();
    controller
        .submit_durable_turn(op, pinned, Some(turn_id.clone()))
        .await
        .map_err(|e| Error::internal(format!("{e}")))?;
    let outbound = state.rpc.outbound().clone();
    tokio::spawn(publish_turn_frames(
        outbound,
        session_id,
        turn_id.clone(),
        events,
    ));
    Ok(StreamMessageResponse {
        accepted: true,
        turn_id,
    })
}

/// Pump the turn's frames until the turn ends, publishing each one.
///
/// The pump reads the session's live channel and keeps only this
/// turn's items: a queued turn ahead of this one streams on its own
/// pump, and the tail of an earlier turn's events is ignored here.
async fn publish_turn_frames(
    outbound: Outbound,
    session_id: String,
    turn_id: String,
    mut events: broadcast::Receiver<BroadcastEvent>,
) {
    let mut seq: u64 = 0;
    let emit = |seq: &mut u64, frame: Value| {
        *seq += 1;
        outbound.notify(
            TURN_EVENT,
            json!({
                "session_id": session_id,
                "turn_id": turn_id,
                "seq": *seq,
                "frame": frame,
            }),
        );
    };
    loop {
        match events.recv().await {
            Ok(item) => {
                // Another turn's item: the pump is one turn's, and
                // the controller runs one turn at a time, so this is
                // the tail of an earlier turn queued ahead.
                if item.turn_id() != Some(turn_id.as_str()) {
                    continue;
                }
                match item {
                    BroadcastEvent::Run { event, .. } => {
                        if let Some(frame) = delta_frame(&event) {
                            emit(&mut seq, frame);
                        }
                    }
                    BroadcastEvent::TurnEnded { reason, .. } => {
                        emit(
                            &mut seq,
                            json!({"kind": "end", "reason": reason}),
                        );
                        break;
                    }
                }
            }
            // The receiver fell behind and the channel dropped its
            // oldest unread items: say so on the stream. The durable
            // rows are unaffected — a client that needs the whole
            // answer re-reads the transcript.
            Err(broadcast::error::RecvError::Lagged(dropped)) => {
                emit(&mut seq, json!({"kind": "gap", "dropped": dropped}));
            }
            Err(broadcast::error::RecvError::Closed) => break,
        }
    }
}

/// The delta frame one run event maps to, when it maps to one.
///
/// The frames are the turn's *increments* — what a client renders as
/// it happens. The fold's own completions (`ModelDone`) carry no new
/// increment: their facts land in the durable rows and the projection
/// the follow stream serves.
fn delta_frame(event: &AgentEvent) -> Option<Value> {
    match event {
        AgentEvent::Model(part) => match part {
            ContentPart::Text(text) => {
                Some(json!({"kind": "text", "text": text.text}))
            }
            ContentPart::Reasoning(reasoning) => {
                Some(json!({"kind": "reasoning", "text": reasoning.text}))
            }
            ContentPart::ToolUse(call) => Some(json!({
                "kind": "tool_call",
                "call_id": call.id,
                "name": call.name,
                "arguments": call.input,
            })),
            ContentPart::ToolResult(result) => {
                // The change the tool made rides the frame as data:
                // a headless consumer renders it as a diff. A
                // truncated report is not a diff, so it stays out.
                let diff = FileDiff::from_metadata(&result.metadata)
                    .filter(|diff| !diff.truncated);
                let mut frame = json!({
                    "kind": "tool_result",
                    "call_id": result.tool_use_id,
                    "tool": result.tool_name,
                    "content": result.content,
                    "is_error": result.is_error,
                });
                if let (Some(diff), Some(object)) =
                    (diff, frame.as_object_mut())
                {
                    object.insert(
                        "diff".to_string(),
                        serde_json::to_value(diff).unwrap_or(Value::Null),
                    );
                }
                Some(frame)
            }
            // Media and resource parts are inputs a request carried,
            // not deltas a run produced.
            _ => None,
        },
        AgentEvent::ModelDone(_) => None,
        AgentEvent::System(system) => match system {
            uva::harness::SystemEvent::Usage {
                input_tokens,
                output_tokens,
                ..
            } => Some(json!({
                "kind": "usage",
                "input_tokens": input_tokens,
                "output_tokens": output_tokens,
            })),
            uva::harness::SystemEvent::ToolProgress {
                tool_name,
                call_id,
                output,
            } => Some(json!({
                "kind": "tool_progress",
                "call_id": call_id,
                "tool": tool_name,
                "content": output.content,
            })),
            uva::harness::SystemEvent::EventsDropped { count } => {
                Some(json!({"kind": "gap", "dropped": count}))
            }
            uva::harness::SystemEvent::Warning { message, .. } => {
                Some(json!({"kind": "warning", "message": message}))
            }
            _ => None,
        },
    }
}

#[cfg(test)]
mod tests {
    use uva::{
        harness::SystemEvent,
        provider::{TextContent, ToolResult, ToolUse},
        tool::DIFF_METADATA_KEY,
    };

    use super::*;

    /// A text part maps to one `text` frame carrying exactly the
    /// chunk — the frame is the increment, never the accumulated
    /// text.
    #[test]
    fn a_text_chunk_is_one_increment() {
        let event = AgentEvent::Model(ContentPart::Text(TextContent {
            text: " world".to_string(),
            cache_control: None,
        }));
        assert_eq!(
            delta_frame(&event),
            Some(json!({"kind": "text", "text": " world"}))
        );
    }

    /// A tool call carries its id, name and arguments: a headless
    /// client renders the call from the frame alone.
    #[test]
    fn a_tool_call_carries_its_identity() {
        let event = AgentEvent::Model(ContentPart::ToolUse(ToolUse {
            id: "c1".to_string(),
            name: "read".to_string(),
            input: json!({"path": "a.txt"}),
        }));
        assert_eq!(
            delta_frame(&event),
            Some(json!({
                "kind": "tool_call",
                "call_id": "c1",
                "name": "read",
                "arguments": {"path": "a.txt"},
            }))
        );
    }

    /// A tool result that changed a file carries the change as data:
    /// a headless consumer renders it as a diff.
    #[test]
    fn a_tool_result_carries_the_change_it_made() {
        let diff = FileDiff::new("/srv/app.rs", Some("old".to_string()), "new");
        let mut metadata = serde_json::Map::new();
        metadata.insert(DIFF_METADATA_KEY.to_string(), diff.to_metadata());
        let event = AgentEvent::Model(ContentPart::ToolResult(ToolResult {
            tool_use_id: "c1".to_string(),
            tool_name: Some("edit".to_string()),
            content: vec![ContentPart::Text(TextContent {
                text: "Replaced the text on line 2.".to_string(),
                cache_control: None,
            })],
            structured_content: None,
            is_error: Some(false),
            metadata,
            truncated_by: None,
        }));
        let frame = delta_frame(&event).expect("the frame");
        assert_eq!(frame["kind"], "tool_result");
        assert_eq!(frame["diff"]["path"], "/srv/app.rs");
        assert_eq!(frame["diff"]["old_text"], "old");
        assert_eq!(frame["diff"]["new_text"], "new");
    }

    /// The dropped-events notification maps to the `gap` frame: the
    /// stream says so itself, so a client can stop trusting its own
    /// text and re-read the transcript instead.
    #[test]
    fn a_dropped_events_notice_is_a_gap_frame() {
        let event = AgentEvent::System(SystemEvent::EventsDropped { count: 3 });
        assert_eq!(
            delta_frame(&event),
            Some(json!({"kind": "gap", "dropped": 3}))
        );
    }

    /// The fold's own completion carries no increment: its facts land
    /// in the durable rows and the follow projection.
    #[test]
    fn the_folds_completion_carries_no_increment() {
        let event =
            AgentEvent::ModelDone(uva::provider::SamplingResult::default());
        assert_eq!(delta_frame(&event), None);
    }
}
