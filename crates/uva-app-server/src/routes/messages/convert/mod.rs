//! Inbound Anthropic request → canonical provider types.
//!
//! [`to_canonical`] is the whole of the request-side projection: it
//! validates the body and returns the exact values the agent runtime
//! consumes ([`uva::provider::Message`] /
//! [`uva::provider::ContentPart`] / [`ToolDefinition`]), split into
//! the transcript to replay and the turn to answer.
//!
//! Two structural decisions are worth stating, because neither is
//! forced by the wire format:
//!
//! 1. **`tool_result` blocks become their own [`Role::Tool`] messages.**
//!    Anthropic carries a tool result as a block inside a `user`
//!    message; the canonical model has a dedicated `Role::Tool`
//!    message, and that is what the runtime's own runs write into the
//!    session log (`uva::session::fold_log_surface` maps a
//!    `tool_result` row onto `Role::Tool`). Projecting onto the
//!    runtime's own shape keeps a replayed transcript identical to a
//!    recorded one, and it is the only spelling every provider adapter
//!    renders correctly.
//!
//! 2. **The client's `system` stays separate.** It cannot become a
//!    `Role::System` message: the harness assembles the agent's own
//!    system prompt ahead of `input.history`
//!    (`uva::harness` loop `prepare` step) and the
//!    Anthropic adapter promotes only the *first* `Role::System`
//!    message to the request's top-level `system` field while
//!    filtering the rest out
//!    (`uva::provider::anthropic::transform`) — a seeded
//!    `Role::System` message would therefore be silently dropped for
//!    exactly the provider this endpoint mirrors. It is returned
//!    separately and delivered as a leading user turn instead (see
//!    [`super::seed_history`]).
//!
//! `tools` are converted faithfully but are **not** injected into the
//! run: the run's tool surface comes from the server's own registry
//! and configuration, and the session-controller seam has no
//! per-dispatch tool-injection point. See the module docs on [`super`].
/// One inbound request, projected onto the canonical types the agent
/// runtime consumes.
use anyhow::{Result, bail};
use uva::provider::{
    Content,
    ContentPart,
    ImageContent,
    Message,
    ReasoningContent,
    ResourceLink,
    TextContent,
    ToolDefinition,
    ToolResult,
    ToolUse,
};

use super::wire::{
    AnthropicRequest,
    DocumentSource,
    ImageSource,
    SystemPrompt,
    WireBlock,
    WireContent,
    WireMessage,
};

#[derive(Debug)]
pub(super) struct CanonicalRequest {
    /// The client's `system`, flattened to plain text.
    pub(super) system: Option<String>,
    /// Every message except the final user turn, in order.
    pub(super) history: Vec<Message>,
    /// The final user turn's parts — what the run is asked to answer.
    pub(super) turn: Vec<ContentPart>,
    /// The request's tool definitions, canonical.
    pub(super) tools: Vec<ToolDefinition>,
}
/// Validate `request` and project it onto canonical types.
///
/// # Errors
///
/// Returns a human-readable reason for every rejected body; the
/// handler maps it onto the protocol's `invalid_request_error`.
pub(super) fn to_canonical(
    request: &AnthropicRequest,
) -> Result<CanonicalRequest> {
    if request.max_tokens == 0 {
        bail!("max_tokens must be at least 1");
    }
    let system = request.system.as_ref().and_then(flatten_system);
    let mut messages = canonical_messages(&request.messages)?;
    let tools = request
        .tools
        .iter()
        .flatten()
        .map(|tool| {
            ToolDefinition::new(
                tool.name.clone(),
                tool.description.clone().unwrap_or_default(),
                tool.input_schema.clone(),
            )
        })
        .collect();
    let turn = split_final_turn(&mut messages)?;
    Ok(CanonicalRequest {
        system,
        history: messages,
        turn,
        tools,
    })
}
/// Project every wire message onto canonical messages, in order.
///
/// A message whose content contains `tool_result` blocks expands to
/// the message's other parts followed by one `Role::Tool` message per
/// result — see the module docs.
fn canonical_messages(wire: &[WireMessage]) -> Result<Vec<Message>> {
    let mut messages = Vec::with_capacity(wire.len());
    for (index, message) in wire.iter().enumerate() {
        let role = message.role.canonical();
        let parts = match &message.content {
            WireContent::Text(text) => vec![text_part(text)],
            WireContent::Blocks(blocks) => {
                canonical_blocks(blocks, &format!("messages[{index}]"))?
            }
        };
        if parts.is_empty() {
            bail!("messages[{index}] must have non-empty content");
        }
        push_parts(&mut messages, role, parts);
    }
    Ok(messages)
}
/// Split `parts` into at most one role-bearing message plus one
/// [`Role::Tool`] message per tool result, preserving order.
fn push_parts(
    messages: &mut Vec<Message>,
    role: uva::provider::Role,
    parts: Vec<ContentPart>,
) {
    let mut plain = Vec::new();
    for part in parts {
        let result = match part {
            ContentPart::ToolResult(result) => result,
            other => {
                plain.push(other);
                continue;
            }
        };
        if !plain.is_empty() {
            messages.push(message_of(role, std::mem::take(&mut plain)));
        }
        let tool_use_id = result.tool_use_id.clone();
        messages.push(Message::tool(
            Content::Single(ContentPart::ToolResult(result)),
            tool_use_id,
        ));
    }
    if !plain.is_empty() {
        messages.push(message_of(role, plain));
    }
}
fn message_of(role: uva::provider::Role, parts: Vec<ContentPart>) -> Message {
    Message {
        role,
        content: Content::parts(parts),
        ..Message::default()
    }
}
/// Project one block list onto canonical parts.
fn canonical_blocks(
    blocks: &[WireBlock],
    where_: &str,
) -> Result<Vec<ContentPart>> {
    let mut parts = Vec::with_capacity(blocks.len());
    for (index, block) in blocks.iter().enumerate() {
        let part = match block {
            WireBlock::Text { text } => text_part(text),
            WireBlock::Image { source } => image_part(source),
            WireBlock::ToolUse { id, name, input } => {
                ContentPart::ToolUse(ToolUse {
                    id: id.clone(),
                    name: name.clone(),
                    input: input.clone(),
                })
            }
            WireBlock::ToolResult {
                tool_use_id,
                content,
                is_error,
            } => ContentPart::ToolResult(ToolResult {
                tool_use_id: tool_use_id.clone(),
                tool_name: None,
                content: tool_result_parts(content, where_, index)?,
                structured_content: None,
                is_error: *is_error,
                metadata: serde_json::Map::new(),
                truncated_by: None,
            }),
            WireBlock::Thinking {
                thinking,
                signature,
            } => ContentPart::Reasoning(ReasoningContent {
                text: thinking.clone(),
                signature: signature.clone(),
            }),
            WireBlock::Document { source, title } => {
                document_part(source, title.as_deref())
            }
        };
        parts.push(part);
    }
    Ok(parts)
}
/// Project a `tool_result`'s inner content.
///
/// The protocol allows `string | [text|image]`; anything else is
/// rejected rather than silently flattened, so a client that sends a
/// document-backed result learns why instead of losing the bytes.
fn tool_result_parts(
    content: &Option<WireContent>,
    where_: &str,
    index: usize,
) -> Result<Vec<ContentPart>> {
    let Some(content) = content else {
        return Ok(Vec::new());
    };
    let blocks = match content {
        WireContent::Text(text) => return Ok(vec![text_part(text)]),
        WireContent::Blocks(blocks) => blocks,
    };
    let mut parts = Vec::with_capacity(blocks.len());
    for (inner, block) in blocks.iter().enumerate() {
        let part = match block {
            WireBlock::Text { text } => text_part(text),
            WireBlock::Image { source } => image_part(source),
            _ => bail!(
                "{where_}.content[{index}].content[{inner}]: tool results support only `text` and `image` blocks"
            ),
        };
        parts.push(part);
    }
    Ok(parts)
}
/// Pop the final user turn off `messages`, returning its parts.
fn split_final_turn(messages: &mut Vec<Message>) -> Result<Vec<ContentPart>> {
    let Some(last) = messages.pop() else {
        bail!("messages must contain at least one message");
    };
    if last.role != uva::provider::Role::User {
        bail!(
            "the final message must have role `user` (assistant prefills are not supported)"
        );
    }
    let parts: Vec<ContentPart> = last.content.into_iter().collect();
    if parts.is_empty() {
        bail!("the final message must have non-empty content");
    }
    Ok(parts)
}
/// Flatten a `system` field to plain text, dropping empty blocks.
fn flatten_system(prompt: &SystemPrompt) -> Option<String> {
    let joined = match prompt {
        SystemPrompt::Text(text) => text.trim().to_string(),
        SystemPrompt::Blocks(blocks) => blocks
            .iter()
            .map(|block| block.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n\n"),
    };
    (!joined.is_empty()).then_some(joined)
}
fn text_part(text: &str) -> ContentPart {
    ContentPart::Text(TextContent {
        text: text.to_string(),
        cache_control: None,
    })
}
/// Project an Anthropic image source onto [`ImageContent`].
///
/// The wire already says which spelling this is, so no `data:`-URL
/// classification is needed here: a `base64` source's payload is
/// stored verbatim (the canonical type accepts bare base64 as well as
/// a `data:` URL) and a `url` source stores the URL in `data`, which
/// is how the adapters recognise a remote image.
fn image_part(source: &ImageSource) -> ContentPart {
    let image = match source {
        ImageSource::Base64 { media_type, data } => ImageContent {
            data: data.clone(),
            mime_type: media_type.clone(),
            detail: None,
        },
        ImageSource::Url { url } => ImageContent {
            data: url.clone(),
            mime_type: url_mime_type(url).to_string(),
            detail: None,
        },
    };
    ContentPart::Image(image)
}
/// Project an Anthropic `document` source onto
/// [`ContentPart::Resource`].
///
/// The canonical model has no document part, so a file rides as the
/// same generic [`ResourceLink`] the old `/api/v1/chat/*` wire's
/// `kind: "file"` attachment produced (see `routes::chat::file_part`) —
/// which is what makes a transcript replayed through either endpoint
/// project identically:
///
/// - an inline source becomes the historical
///   `data:<media_type>;base64,<payload>` URI, with the media type
///   repeated in `mime_type`. The URI has to carry the bytes because
///   the [`ResourceLink`] is what the run persists and replays; the
///   field the adapters actually render is the URI (the OpenAI adapter
///   as `[Resource: <uri> - <name>]`, the Anthropic one as a
///   `[ResourceLink]` placeholder, that provider having no resource
///   block);
/// - a URL source keeps the URL as its `uri` and carries no media type
///   at all: the protocol's `url` spelling names no type, and guessing
///   one from the path extension would invent a fact the client did
///   not send.
///
/// `name` carries the block's optional `title`, which is the field a
/// client sends the file name in. It is what lets a reloaded transcript
/// show which file a turn carried instead of falling back to a bare
/// media type; a block without a title leaves it empty rather than
/// inventing one from the URI.
///
/// A malformed source never reaches here: [`DocumentSource`] is a
/// tagged enum whose own deserialization refuses an unknown `type` and
/// a missing field, and that sentence reaches the client because
/// [`WireContent`] deserializes block lists directly (see `wire`).
fn document_part(source: &DocumentSource, title: Option<&str>) -> ContentPart {
    let name = title.unwrap_or_default().to_string();
    let link = match source {
        DocumentSource::Base64 { media_type, data } => ResourceLink {
            uri: format!("data:{media_type};base64,{data}"),
            mime_type: Some(media_type.clone()),
            name: name.clone(),
            title: title.map(str::to_string),
            description: None,
        },
        DocumentSource::Url { url } => ResourceLink {
            uri: url.clone(),
            mime_type: None,
            name: name.clone(),
            title: title.map(str::to_string),
            description: None,
        },
    };
    ContentPart::Resource(link)
}
/// Best-effort media type for a `{type:"url"}` image source.
///
/// The canonical part requires a `mime_type` but the URL source has
/// none, so it is derived from the URL's path extension. Both provider
/// adapters ignore the field for a remote source (they pass the URL
/// through unchanged), and `image/*` is the honest answer for "an
/// image whose type the URL does not name".
fn url_mime_type(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let extension = path
        .rsplit_once('.')
        .map(|(_, extension)| extension.to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "image/*",
    }
}
#[cfg(test)]
mod tests;
