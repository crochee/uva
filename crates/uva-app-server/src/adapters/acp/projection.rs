//! The projection between Uva's vocabulary and ACP's.
//!
//! Two directions:
//!
//! - **live** — a core event becomes the session updates a client
//!   renders. The mapping is total: every event either maps to
//!   updates or to none, and the ones that map to none are the
//!   events the protocol has no shape for (a usage frame, a
//!   warning).
//! - **replay** — a session's folded transcript becomes the updates
//!   `session/load` streams back, so a client that reloads a session
//!   sees the same conversation it saw before.
//!
//! The protocol's own types carry the wire shapes: the SDK's
//! [`SessionUpdate`] and its content blocks are what the client
//! decodes, so a shape cannot drift between what this agent writes
//! and what a client reads.
//!
//! The projection is one direction only: it never decides what a
//! turn means, it only decides what the wire says.

use std::path::PathBuf;

use agent_client_protocol::v1::schema::{
    ContentBlock,
    ContentChunk,
    Diff,
    EmbeddedResource,
    EmbeddedResourceResource,
    ImageContent,
    MessageId,
    ResourceLink,
    SessionUpdate,
    TextContent,
    TextResourceContents,
    ToolCall,
    ToolCallContent,
    ToolCallId,
    ToolCallStatus,
    ToolCallUpdate,
    ToolCallUpdateFields,
    ToolKind,
};
use serde_json::Value;
use uva::{
    harness::{AgentEvent, SystemEvent},
    provider::{Content, ContentPart, Message, Role, ToolResult, ToolUse},
    tool::FileDiff,
};

/// The session updates one live event maps to.
///
/// An event may map to more than one update, and may map to none:
/// the protocol has no shape for every event the loop emits.
///
/// `message_id` names the message the chunk belongs to — the caller
/// owns it, because only the caller knows where a message ends.
/// `registry` answers where a tool call will work: a client that
/// follows along ("the agent is editing foo.rs") is told the file
/// when the call **starts**, and the tool is the one party that
/// knows where its own arguments resolve.
#[must_use]
pub fn updates_for(
    event: &AgentEvent,
    message_id: Option<&str>,
    registry: &uva::tool::ToolRegistry,
    context: &uva::tool::Context,
) -> Vec<SessionUpdate> {
    match event {
        AgentEvent::Model(ContentPart::Text(text)) => {
            vec![SessionUpdate::AgentMessageChunk(chunk(
                message_id,
                ContentBlock::Text(TextContent::new(text.text.clone())),
            ))]
        }
        AgentEvent::Model(ContentPart::Reasoning(reasoning)) => {
            vec![SessionUpdate::AgentThoughtChunk(chunk(
                message_id,
                ContentBlock::Text(TextContent::new(reasoning.text.clone())),
            ))]
        }
        AgentEvent::Model(ContentPart::ToolUse(call)) => {
            vec![tool_call_update_for(call, registry, context)]
        }
        AgentEvent::Model(ContentPart::ToolResult(result)) => {
            vec![tool_result_update(result)]
        }
        AgentEvent::System(SystemEvent::ToolProgress {
            tool_name: _,
            call_id,
            output,
        }) => {
            let content = output
                .content
                .iter()
                .map(tool_call_content)
                .collect::<Vec<_>>();
            vec![SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
                ToolCallId::new(call_id.clone()),
                ToolCallUpdateFields::new()
                    .status(ToolCallStatus::InProgress)
                    .content(content),
            ))]
        }
        // Everything else has no ACP shape: a usage frame, a warning,
        // a compaction boundary, the events dropped notice. The
        // durable rows keep them; the client does not need them.
        _ => Vec::new(),
    }
}

/// The `tool_call` update one model tool use maps to.
///
/// The call starts `pending`: it has not run yet — it is either
/// waiting for the approval gate or for the tool itself. It names
/// the files it will touch when the tool can say so **before** it
/// runs: a client follows along ("the agent is editing foo.rs")
/// while the call is in flight.
fn tool_call_update_for(
    call: &ToolUse,
    registry: &uva::tool::ToolRegistry,
    context: &uva::tool::Context,
) -> SessionUpdate {
    let locations = registry
        .locations_for(&call.name, &call.input, context)
        .into_iter()
        .map(|location| {
            agent_client_protocol::v1::schema::ToolCallLocation::new(
                std::path::PathBuf::from(&location.path),
            )
        })
        .collect::<Vec<_>>();
    let mut tool_call = ToolCall::new(
        ToolCallId::new(call.id.clone()),
        title_for(&call.name, &call.input),
    )
    .name(call.name.clone())
    .kind(tool_kind(&call.name))
    .status(ToolCallStatus::Pending)
    .raw_input(call.input.clone());
    if !locations.is_empty() {
        tool_call = tool_call.locations(locations);
    }
    SessionUpdate::ToolCall(tool_call)
}

/// The `tool_call` update a **replayed** tool use maps to.
///
/// The replay reads the conversation, not the registry: a replayed
/// call carries its identity and its arguments, and the files it
/// touched are the log's to answer (the `changed` member), not the
/// projection's to guess.
fn replayed_tool_call_update_for(call: &ToolUse) -> SessionUpdate {
    SessionUpdate::ToolCall(
        ToolCall::new(
            ToolCallId::new(call.id.clone()),
            title_for(&call.name, &call.input),
        )
        .name(call.name.clone())
        .kind(tool_kind(&call.name))
        .status(ToolCallStatus::Pending)
        .raw_input(call.input.clone()),
    )
}

/// The `tool_call_update` one tool result maps to.
///
/// A failed call is `failed`, not `completed`: the client shows the
/// difference, and the model's own view of the failure is the error
/// content the tool returned.
fn tool_result_update(result: &ToolResult) -> SessionUpdate {
    let failed = result.is_error.unwrap_or(false);
    let mut content = result
        .content
        .iter()
        .map(tool_call_content)
        .collect::<Vec<_>>();
    // The change the tool made, as the protocol's diff block: an
    // editor renders it, and the model's own view of the call is
    // unchanged (the report rides the result's metadata, not its
    // content). A truncated report is not a diff — the tool's own
    // summary is the honest picture, so nothing is added.
    if let Some(diff) = FileDiff::from_metadata(&result.metadata)
        && !diff.truncated
    {
        content.push(ToolCallContent::Diff(
            Diff::new(PathBuf::from(&diff.path), diff.new_text)
                .old_text(diff.old_text),
        ));
    }
    let mut fields = ToolCallUpdateFields::new().status(if failed {
        ToolCallStatus::Failed
    } else {
        ToolCallStatus::Completed
    });
    if !content.is_empty() {
        fields = fields.content(content);
    }
    if let Some(output) = result.structured_content.clone() {
        fields = fields.raw_output(output);
    }
    SessionUpdate::ToolCallUpdate(ToolCallUpdate::new(
        ToolCallId::new(result.tool_use_id.clone()),
        fields,
    ))
}

/// The chunk one text delta travels as.
fn chunk(message_id: Option<&str>, content: ContentBlock) -> ContentChunk {
    let chunk = ContentChunk::new(content);
    match message_id {
        Some(id) => chunk.message_id(MessageId::new(id.to_string())),
        None => chunk,
    }
}

/// The ACP tool kind a tool name maps to.
///
/// The kind is a coarse presentation category: a client picks an
/// icon from it. The mapping is by the tool's own name, so a client
/// never has to guess what a call does — the tool that runs it is
/// the same value the model was shown.
#[must_use]
pub fn tool_kind(name: &str) -> ToolKind {
    match name {
        "read" | "list" | "glob" => ToolKind::Read,
        "write" | "edit" | "patch" => ToolKind::Edit,
        "search" | "grep" => ToolKind::Search,
        "shell" | "bash" | "exec" => ToolKind::Execute,
        "web_fetch" | "fetch" => ToolKind::Fetch,
        _ => ToolKind::Other,
    }
}

/// The human-readable title of a tool call.
///
/// The title says what the call is doing, not just what it is
/// called: the argument that identifies the work (the path, the
/// command, the query) is part of the title, so a client can render
/// a list of calls without decoding every argument.
#[must_use]
pub fn title_for(name: &str, input: &Value) -> String {
    let argument = ["path", "file_path", "command", "query", "pattern", "url"]
        .iter()
        .find_map(|key| {
            input.get(*key).and_then(Value::as_str).map(str::to_string)
        });
    match argument {
        Some(argument) => format!("{name}: {argument}"),
        None => name.to_string(),
    }
}

/// One provider content part as an ACP tool-call content block.
fn tool_call_content(
    part: &ContentPart,
) -> agent_client_protocol::v1::schema::ToolCallContent {
    match part {
        ContentPart::Text(text) => {
            agent_client_protocol::v1::schema::ToolCallContent::from(
                ContentBlock::Text(TextContent::new(text.text.clone())),
            )
        }
        ContentPart::Resource(link) => {
            agent_client_protocol::v1::schema::ToolCallContent::from(
                ContentBlock::ResourceLink(ResourceLink::new(
                    link.name.clone(),
                    link.uri.clone(),
                )),
            )
        }
        other => agent_client_protocol::v1::schema::ToolCallContent::from(
            ContentBlock::Text(TextContent::new(format!("{other:?}"))),
        ),
    }
}

// ---- the prompt content ------------------------------------------------

/// The provider parts one ACP prompt's content blocks map to.
///
/// The blocks a client sends are the conversation the model sees: a
/// text block is text, an image block is an image, a resource block
/// carries its text. A block the protocol allows but this agent
/// cannot represent is refused rather than silently dropped — the
/// model must not see a turn that lost part of the question.
pub fn prompt_parts(
    blocks: &[ContentBlock],
) -> Result<Vec<ContentPart>, String> {
    let mut parts = Vec::new();
    for block in blocks {
        match block {
            ContentBlock::Text(text) => {
                parts.push(ContentPart::Text(uva::provider::TextContent {
                    text: text.text.clone(),
                    cache_control: None,
                }));
            }
            ContentBlock::Image(image) => {
                parts.push(ContentPart::Image(uva::provider::ImageContent {
                    data: image.data.clone(),
                    mime_type: image.mime_type.clone(),
                    detail: None,
                }));
            }
            ContentBlock::Audio(_) => {
                return Err("this agent does not take audio".to_string());
            }
            ContentBlock::ResourceLink(link) => {
                parts.push(ContentPart::Resource(
                    uva::provider::ResourceLink {
                        uri: link.uri.clone(),
                        name: link.name.clone(),
                        title: link.title.clone(),
                        description: link.description.clone(),
                        mime_type: link.mime_type.clone(),
                    },
                ));
            }
            ContentBlock::Resource(resource) => {
                let EmbeddedResourceResource::TextResourceContents(
                    TextResourceContents { text, .. },
                ) = &resource.resource
                else {
                    return Err(
                        "this agent reads a resource's text, not its blob"
                            .to_string(),
                    );
                };
                parts.push(ContentPart::Text(uva::provider::TextContent {
                    text: text.clone(),
                    cache_control: None,
                }));
            }
            _ => {
                return Err(
                    "this agent does not take this content block".to_string()
                );
            }
        }
    }
    Ok(parts)
}

// ---- the replay --------------------------------------------------------

/// The session updates a folded transcript replays as.
///
/// `session/load` streams the conversation back before it answers:
/// the user's turns as `user_message_chunk`, the agent's turns as
/// `agent_message_chunk`, and the tool calls as `tool_call` plus the
/// `tool_call_update` that settles them.
#[must_use]
pub fn replay_updates(messages: &[Message]) -> Vec<SessionUpdate> {
    let mut updates = Vec::new();
    for message in messages {
        // One assistant message, one id: the chunks that carry it
        // belong together, and the next assistant message gets its
        // own.
        let message_id = (message.role == Role::Assistant)
            .then(|| uuid::Uuid::new_v4().to_string());
        let message_id = message_id.as_deref();
        let parts: Vec<&ContentPart> = match &message.content {
            Content::Single(part) => vec![part],
            Content::Multi(parts) => parts.iter().collect(),
        };
        for part in parts {
            match (message.role, part) {
                (Role::User, ContentPart::Text(text)) => {
                    updates.push(SessionUpdate::UserMessageChunk(
                        ContentChunk::new(ContentBlock::Text(
                            TextContent::new(text.text.clone()),
                        )),
                    ));
                }
                (Role::Assistant, ContentPart::Text(text)) => {
                    updates.push(SessionUpdate::AgentMessageChunk(chunk(
                        message_id,
                        ContentBlock::Text(TextContent::new(text.text.clone())),
                    )));
                }
                (Role::Assistant, ContentPart::ToolUse(call)) => {
                    // The replay has no registry to ask where the
                    // call worked: a replayed call shows its identity,
                    // and the files it touched live in the rows a
                    // later read serves.
                    updates.push(replayed_tool_call_update_for(call));
                }
                (Role::Tool, ContentPart::ToolResult(result)) => {
                    updates.push(tool_result_update(result));
                }
                _ => {}
            }
        }
    }
    updates
}

/// The image content block a prompt's image maps to.
///
/// The helper exists for the callers that build a block themselves;
/// the projection itself goes through the SDK's own types.
#[must_use]
pub fn image_block(
    data: impl Into<String>,
    mime_type: impl Into<String>,
) -> ContentBlock {
    ContentBlock::Image(ImageContent::new(data, mime_type))
}

/// The embedded resource block a prompt's resource maps to.
#[must_use]
pub fn resource_block(text: impl Into<String>) -> ContentBlock {
    ContentBlock::Resource(EmbeddedResource::new(
        EmbeddedResourceResource::TextResourceContents(
            TextResourceContents::new(text, ""),
        ),
    ))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use uva::{
        provider::{ReasoningContent, TextContent},
        tool::{DIFF_METADATA_KEY, MAX_DIFF_TEXT_BYTES},
    };

    use super::*;

    /// The empty registry: a projection test that does not name a
    /// tool needs no tools in it.
    fn registry() -> uva::tool::ToolRegistry {
        uva::tool::ToolRegistry::new()
    }

    /// A context for the calls the projection asks about.
    fn context() -> uva::tool::Context {
        uva::tool::Context::new(
            "s1".to_string(),
            std::path::PathBuf::from("/tmp"),
        )
    }

    /// The wire shape of one update.
    fn wire_shape(update: &SessionUpdate) -> serde_json::Value {
        serde_json::to_value(update).expect("the update serialises")
    }

    /// A text delta is one agent message chunk carrying exactly the
    /// delta — never the accumulated text.
    #[test]
    fn a_text_delta_is_one_agent_chunk() {
        let event = AgentEvent::Model(ContentPart::Text(TextContent {
            text: " world".to_string(),
            cache_control: None,
        }));
        let updates = updates_for(&event, Some("m1"), &registry(), &context());
        assert_eq!(updates.len(), 1);
        assert_eq!(
            wire_shape(&updates[0]),
            json!({
                "sessionUpdate": "agent_message_chunk",
                "messageId": "m1",
                "content": {"type": "text", "text": " world"},
            })
        );
    }

    /// A reasoning delta is a thought chunk, not an answer chunk:
    /// the client renders the two differently.
    #[test]
    fn a_reasoning_delta_is_a_thought_chunk() {
        let event =
            AgentEvent::Model(ContentPart::Reasoning(ReasoningContent {
                text: "thinking".to_string(),
                signature: None,
            }));
        let updates = updates_for(&event, Some("m1"), &registry(), &context());
        assert_eq!(updates.len(), 1);
        assert_eq!(
            wire_shape(&updates[0])["sessionUpdate"],
            "agent_thought_chunk"
        );
    }

    /// A tool call names the tool, the kind and the raw arguments,
    /// and starts pending.
    #[test]
    fn a_tool_call_carries_its_identity() {
        let event = AgentEvent::Model(ContentPart::ToolUse(ToolUse {
            id: "c1".to_string(),
            name: "read".to_string(),
            input: json!({"path": "a.txt"}),
        }));
        let updates = updates_for(&event, Some("m1"), &registry(), &context());
        assert_eq!(updates.len(), 1);
        let shape = wire_shape(&updates[0]);
        assert_eq!(shape["sessionUpdate"], "tool_call");
        assert_eq!(shape["toolCallId"], "c1");
        assert_eq!(shape["kind"], "read");
        // The protocol's default status is `pending`, and the SDK
        // omits a field that carries its default: the wire says
        // "pending" by not saying otherwise.
        assert!(
            shape.get("status").is_none(),
            "the pending status is the default: {shape}"
        );
        assert_eq!(shape["title"], "read: a.txt");
    }

    /// A failed tool result is `failed`, not `completed`.
    #[test]
    fn a_failed_tool_result_is_failed() {
        let event = AgentEvent::Model(ContentPart::ToolResult(ToolResult {
            tool_use_id: "c1".to_string(),
            tool_name: Some("shell".to_string()),
            content: vec![ContentPart::Text(TextContent {
                text: "boom".to_string(),
                cache_control: None,
            })],
            structured_content: None,
            is_error: Some(true),
            metadata: Default::default(),
            truncated_by: None,
        }));
        let updates = updates_for(&event, Some("m1"), &registry(), &context());
        let shape = wire_shape(&updates[0]);
        assert_eq!(shape["sessionUpdate"], "tool_call_update");
        assert_eq!(shape["status"], "failed");
    }

    /// A tool call names the files it will touch when the tool can
    /// say so before it runs: a client follows along while the call
    /// is in flight.
    #[test]
    fn a_tool_call_names_the_files_it_will_touch() {
        /// A tool that declares where it works.
        struct WorkingTool;

        #[async_trait::async_trait]
        impl uva::tool::Tool for WorkingTool {
            fn name(&self) -> &str {
                "edit"
            }

            fn description(&self) -> &str {
                "changes a file"
            }

            fn parameters(&self) -> serde_json::Value {
                serde_json::json!({"type": "object"})
            }

            fn locations(
                &self,
                _input: &serde_json::Value,
                _context: &uva::tool::Context,
            ) -> Vec<uva::tool::ToolLocation> {
                vec![uva::tool::ToolLocation {
                    path: "/srv/app.rs".to_string(),
                }]
            }

            async fn call(
                &self,
                _input: serde_json::Value,
                _context: &uva::tool::Context,
            ) -> uva::tool::ToolOutput {
                uva::tool::ToolOutput::text("done")
            }
        }

        let registry = uva::tool::ToolRegistry::new();
        registry.register_entry(uva::tool::ToolEntry::new(
            std::sync::Arc::new(WorkingTool),
        ));
        let event = AgentEvent::Model(ContentPart::ToolUse(ToolUse {
            id: "c1".to_string(),
            name: "edit".to_string(),
            input: json!({"file_path": "app.rs"}),
        }));
        let updates = updates_for(&event, Some("m1"), &registry, &context());
        let shape = wire_shape(&updates[0]);
        assert_eq!(shape["sessionUpdate"], "tool_call");
        assert_eq!(
            shape["locations"][0]["path"], "/srv/app.rs",
            "the call names where it will work: {shape}"
        );
    }

    /// The change a file tool made rides the settled call as the
    /// protocol's diff block: an editor renders it, and the model's
    /// own view of the call is unchanged.
    #[test]
    fn a_file_change_is_reported_as_a_diff() {
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
        let updates = updates_for(&event, Some("m1"), &registry(), &context());
        let shape = wire_shape(&updates[0]);
        assert_eq!(shape["sessionUpdate"], "tool_call_update");
        let content = shape["content"].as_array().expect("the content blocks");
        let rendered = content
            .iter()
            .find(|block| block["type"] == "diff")
            .expect("the diff block is there");
        assert_eq!(rendered["path"], "/srv/app.rs");
        assert_eq!(rendered["oldText"], "old");
        assert_eq!(rendered["newText"], "new");
    }

    /// A change too big to carry is not a diff: the client's own
    /// summary is the honest picture.
    #[test]
    fn a_truncated_change_is_not_a_diff() {
        let big = "x".repeat(MAX_DIFF_TEXT_BYTES + 1);
        let diff = FileDiff::new("/srv/app.rs", Some(big.clone()), big);
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
        let updates = updates_for(&event, Some("m1"), &registry(), &context());
        let shape = wire_shape(&updates[0]);
        let content = shape["content"].as_array().expect("the content blocks");
        assert!(
            !content.iter().any(|block| block["type"] == "diff"),
            "a truncated report is not a diff: {shape}"
        );
    }

    /// A prompt's blocks map to the parts the model sees.
    #[test]
    fn prompt_blocks_map_to_parts() {
        let parts = prompt_parts(&[
            ContentBlock::Text(
                agent_client_protocol::v1::schema::TextContent::new("hello"),
            ),
            ContentBlock::Image(ImageContent::new("AAA", "image/png")),
        ])
        .expect("the blocks map");
        assert_eq!(parts.len(), 2);
        assert!(matches!(parts[0], ContentPart::Text(_)));
        assert!(matches!(parts[1], ContentPart::Image(_)));
    }

    /// A block this agent cannot represent is refused, not dropped.
    #[test]
    fn an_unsupported_block_is_refused() {
        let error = prompt_parts(&[ContentBlock::Audio(
            agent_client_protocol::v1::schema::AudioContent::new(
                "AA".to_string(),
                "audio/wav".to_string(),
            ),
        )])
        .expect_err("audio is not supported");
        assert!(error.contains("audio"), "{error}");
    }
}
