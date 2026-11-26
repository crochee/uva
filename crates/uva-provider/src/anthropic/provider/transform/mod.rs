//! The request- and message-transformation methods on
//! [`super::core::AnthropicProvider`]:
//!
//! - [`AnthropicProvider::transform_request`] — top-level
//!   dispatcher that takes a
//!   [`crate::types::CompletionRequest`] and produces an
//!   [`super::super::types::AnthropicRequest`].
//! - [`AnthropicProvider::transform_message`] — maps a
//!   [`crate::types::Message`] to an
//!   [`super::super::types::AnthropicMessage`].
//! - [`AnthropicProvider::transform_part`] — maps a
//!   [`crate::types::ContentPart`] to an
//!   [`super::super::types::AnthropicContentBlock`].
//! - [`AnthropicProvider::reorder_anthropic_messages`] —
//!   reorders assistant messages so that text blocks
//!   precede tool-use blocks.
//! - [`AnthropicProvider::sanitize_tool_id`] — replaces
//!   non-alphanumeric / non-underscore / non-hyphen chars
//!   with `_`.

use super::{
    super::types::{
        AnthropicAudioSource,
        AnthropicContentBlock,
        AnthropicImageSource,
        AnthropicMessage,
        AnthropicRequest,
        AnthropicSystem,
        AnthropicSystemBlock,
        AnthropicTool,
        AnthropicToolChoice,
        AnthropicToolResultContent,
        CacheControl,
    },
    core::AnthropicProvider,
};
use crate::{
    cache_mark::{CacheControlMark, CacheScope, CacheTtl},
    types::{CompletionRequest, Content, ContentPart, Role, ToolChoice},
};

/// Map the canonical [`ToolChoice`] onto Anthropic's wire field.
///
/// `Auto` is omitted: Anthropic's own default is `auto`, so sending
/// it would spend a field to say what the endpoint already assumes.
/// The other variants are honoured — a summary request that replays
/// tool schemas for prefix-cache alignment asks for `None`, and the
/// reply must be text, not a tool call.
fn tool_choice_from(choice: &ToolChoice) -> Option<AnthropicToolChoice> {
    match choice {
        ToolChoice::Auto => None,
        ToolChoice::None => Some(AnthropicToolChoice::None),
        ToolChoice::Required => Some(AnthropicToolChoice::Any),
        ToolChoice::Specific { name } => {
            Some(AnthropicToolChoice::Tool { name: name.clone() })
        }
    }
}

/// Map the provider-neutral [`CacheTtl`] class to an Anthropic
/// `ttl_seconds` value. `Ephemeral` defers to Anthropic's default (no
/// explicit `ttl_seconds`); `Extended` requests 5 minutes and `Long`
/// requests 1 hour.
fn ttl_seconds_from_ttl(ttl: CacheTtl) -> Option<u32> {
    match ttl {
        CacheTtl::Ephemeral => None,
        CacheTtl::Extended => Some(300),
        CacheTtl::Long => Some(3600),
    }
}

/// Translate a provider-neutral [`CacheControlMark`] to the Anthropic
/// [`CacheControl`] wire type, including the `scope.0` value as a
/// `cache_namespace` field so two different users with otherwise identical
/// prompts produce distinct `cache_control` JSON (per the cross-session
/// cache leakage prevention requirement).
///
/// The namespace is only emitted when the scope differs from
/// [`CacheScope::default()`]; this keeps the anonymous-default path
/// byte-identical to the pre-change wire format (`{"type": "ephemeral"}`).
fn cache_control_from_mark(mark: &CacheControlMark) -> CacheControl {
    let cache_namespace = if mark.scope == CacheScope::default() {
        None
    } else {
        Some(mark.scope.0.clone())
    };
    CacheControl {
        r#type: "ephemeral".to_string(),
        ttl_seconds: ttl_seconds_from_ttl(mark.ttl),
        cache_namespace,
    }
}

/// Extract a representative [`CacheControlMark`] from `request` — the first
/// mark found scanning the last tool then the last user message in reverse.
/// Used to propagate the cache scope to the system block, which is marked
/// by the provider (not by `apply_cache_policy`) and otherwise has no mark
/// of its own.
fn representative_cache_mark(
    request: &CompletionRequest,
) -> Option<&CacheControlMark> {
    if let Some(mark) = request
        .tools
        .iter()
        .rev()
        .find_map(|t| t.cache_control.as_ref())
    {
        return Some(mark);
    }
    for msg in request.messages.iter().rev() {
        for part in &msg.content {
            if let Some(mark) = part.cache_control() {
                return Some(mark);
            }
        }
    }
    None
}

impl AnthropicProvider {
    pub(in crate::anthropic) fn transform_request(
        &self,
        request: &CompletionRequest,
    ) -> AnthropicRequest {
        // Clone so we can apply policy mutably without modifying the
        // caller's request. When `cache_policy` is `None` the clone is
        // byte-identical to the original, preserving backward-compatible
        // output (Text system variant, no cache_control fields anywhere).
        let mut request = request.clone();

        // Apply cache policy (marks last tool + last user message) when
        // present. System marking is deferred to `build_anthropic_system`
        // because the system text is embedded in a `Role::System` message
        // and is only extracted during this provider-specific transform.
        // The policy is cloned out first to avoid borrowing `request`
        // immutably while we need a mutable borrow to apply the marks.
        if let Some(policy) = request.cache_policy.clone() {
            // Resolve the effective cache policy through the optional
            // deployment-supplied hook (R62, pi
            // `cache_warming_decision` parity). Identity when no hook
            // is installed.
            let effective = crate::cache_policy::resolve_cache_policy(
                self.cache_strategy_hook.as_deref(),
                &request,
                &policy,
            );
            // `apply` short-circuits (returns `true`, skips
            // `apply_cache_policy`) when `tools` / `messages` `Arc`
            // references are identical to the previous call — the
            // cache_control marks from the prior call are still present.
            // Otherwise it performs full evaluation and stores the new
            // references for the next call.
            self.cache_policy_applier
                .lock()
                .apply(&mut request, &effective);
        }

        let system_text = request
            .messages
            .iter()
            .find(|m| m.role == Role::System)
            .and_then(|m| m.content.extract_text());

        let messages: Vec<AnthropicMessage> = request
            .messages
            .iter()
            .filter(|m| m.role != Role::System)
            .filter_map(|m| self.transform_message(m))
            .collect();

        let messages = Self::reorder_anthropic_messages(messages);

        // The system block is marked by the provider (not by
        // `apply_cache_policy`); propagate the scope from a representative
        // mark so the system cache entry is namespaced identically to the
        // tool / message cache entries.
        let representative_mark = representative_cache_mark(&request);
        let system = build_anthropic_system(
            system_text,
            request.cache_policy.as_ref(),
            representative_mark,
        );

        let tools: Vec<AnthropicTool> = request
            .tools
            .iter()
            .map(|t| {
                let cache_control =
                    t.cache_control.as_ref().map(cache_control_from_mark);
                AnthropicTool {
                    name: t.name.clone(),
                    description: t.description.clone(),
                    input_schema: t.input_schema.clone(),
                    cache_control,
                }
            })
            .collect();

        AnthropicRequest {
            // Fall back to the provider-configured model name when
            // the caller leaves `request.model` empty.
            model: if request.model.is_empty() {
                self.model_config.name.clone()
            } else {
                request.model.clone()
            },
            system,
            messages,
            max_tokens: request.max_tokens.unwrap_or(4096),
            tools: if tools.is_empty() { None } else { Some(tools) },
            tool_choice: tool_choice_from(&request.tool_choice),
            temperature: request.temperature,
            stream: false,
        }
    }

    fn reorder_anthropic_messages(
        messages: Vec<AnthropicMessage>,
    ) -> Vec<AnthropicMessage> {
        let mut result: Vec<AnthropicMessage> = Vec::new();

        for msg in messages {
            if msg.role != "assistant" {
                // Consecutive tool-result turns merge into ONE user
                // message — the protocol's own documented shape for
                // answering several calls at once. The harness commits one
                // `Message::tool` per result (`commit.rs`), so a turn that
                // asked for two tools in parallel arrives as two adjacent
                // user messages.
                //
                // This is NOT a rejection guard: the API documents that
                // "consecutive `user` or `assistant` turns in your request
                // will be combined into a single turn", so the unmerged form
                // is accepted as well. Emitting the merged shape avoids
                // depending on that combining, and matches the example the
                // tool-use docs give for parallel calls.
                //
                // Only tool-result-only turns merge: a genuine user turn
                // that follows another user turn keeps its own message, so
                // the transcript's turn boundaries are not rewritten.
                let incoming_is_results = !msg.content.is_empty()
                    && msg.content.iter().all(|block| {
                        matches!(
                            block,
                            AnthropicContentBlock::ToolResult { .. }
                        )
                    });
                if incoming_is_results
                    && let Some(last) = result.last_mut()
                    && last.role == "user"
                    && last.content.iter().all(|block| {
                        matches!(
                            block,
                            AnthropicContentBlock::ToolResult { .. }
                        )
                    })
                {
                    last.content.extend(msg.content);
                    continue;
                }
                result.push(msg);
                continue;
            }

            let tool_use_blocks: Vec<_> = msg
                .content
                .iter()
                .filter(|c| matches!(c, AnthropicContentBlock::ToolUse { .. }))
                .cloned()
                .collect();

            let other_blocks: Vec<_> = msg
                .content
                .iter()
                .filter(|c| !matches!(c, AnthropicContentBlock::ToolUse { .. }))
                .cloned()
                .collect();

            if !tool_use_blocks.is_empty() && !other_blocks.is_empty() {
                // Split into two messages: text first, then tool_use
                if !other_blocks.is_empty() {
                    result.push(AnthropicMessage {
                        role: msg.role.clone(),
                        content: other_blocks,
                    });
                }
                result.push(AnthropicMessage {
                    role: msg.role.clone(),
                    content: tool_use_blocks,
                });
            } else {
                result.push(msg);
            }
        }

        result
    }

    fn sanitize_tool_id(id: &str) -> String {
        id.chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '_' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    }

    pub(in crate::anthropic) fn transform_message(
        &self,
        msg: &crate::types::Message,
    ) -> Option<AnthropicMessage> {
        let role = match msg.role {
            Role::User => "user".to_string(),
            Role::Assistant => "assistant".to_string(),
            Role::Tool => "user".to_string(),
            Role::System => return None,
        };

        let mut content: Vec<AnthropicContentBlock> = match &msg.content {
            Content::Single(part) => {
                self.transform_part(part).into_iter().collect()
            }
            Content::Multi(parts) => parts
                .iter()
                .filter_map(|p| self.transform_part(p))
                .collect(),
        };

        // A tool message whose result arrived UNWRAPPED — the shape the
        // harness commits (`Message::tool(Content::parts(tr.content),
        // tr.call_id)`) — projects to plain `text`/`image` blocks with no
        // `tool_result` anywhere, and `tool_call_id` would be dropped.
        //
        // That is not merely lossy: Anthropic requires every `tool_use`
        // block to be answered by a `tool_result` immediately after it,
        // and rejects the request with "`tool_use` ids were found without
        // `tool_result` blocks" otherwise. So the id is re-attached and
        // the parts are re-projected through the tool-result vocabulary,
        // mirroring the OpenAI adapter's
        // `msg.tool_call_id.clone().or(tool_use_id)`.
        let already_a_result = content
            .iter()
            .any(|b| matches!(b, AnthropicContentBlock::ToolResult { .. }));
        if msg.role == Role::Tool
            && !already_a_result
            && let Some(call_id) = msg.tool_call_id.as_deref()
        {
            let parts: Vec<crate::types::ContentPart> =
                msg.content.iter().cloned().collect();
            let wrapped = crate::types::ToolResult {
                tool_use_id: call_id.to_string(),
                tool_name: msg.name.clone(),
                content: parts,
                structured_content: None,
                is_error: None,
                truncated_by: None,
                metadata: serde_json::Map::new(),
            };
            content = vec![AnthropicContentBlock::ToolResult {
                tool_use_id: Self::sanitize_tool_id(call_id),
                content: tool_result_blocks(&wrapped),
                is_error: None,
                cache_control: None,
            }];
        }

        // Nothing survived the projection — the message has no wire
        // representation, and an empty `content` array is not a legal
        // Anthropic message. Mirrors the OpenAI-compatible adapter.
        if content.is_empty() {
            return None;
        }

        Some(AnthropicMessage { role, content })
    }

    /// Project one canonical part onto the Anthropic wire vocabulary, or
    /// `None` when this API has no valid slot for it.
    ///
    /// Only an **unsigned** [`ContentPart::Reasoning`] is such a part.
    /// Anthropic requires a `thinking` block to carry its `signature` —
    /// that is exactly what the signature is for, and why the provider
    /// crate documents it as "Required to preserve reasoning continuity
    /// across turns". Sending `signature: None` produces a request
    /// Anthropic rejects, so the block is dropped instead.
    ///
    /// The unsigned case is the *reachable* one: `reasoning_content`
    /// gateways and `<think>`-marker-compatible upstreams emit reasoning
    /// without a signature, whereas a real Anthropic `signature_delta`
    /// only appears once extended thinking is enabled. Dropping here
    /// means an unsigned part can never become an invalid request,
    /// whichever upstream produced it — while a signed part, the one
    /// Anthropic actually needs back, is still carried.
    ///
    /// [`ContentPart::Reasoning`]: crate::types::ContentPart::Reasoning
    pub(in crate::anthropic) fn transform_part(
        &self,
        part: &ContentPart,
    ) -> Option<AnthropicContentBlock> {
        // Translate the provider-neutral `CacheControlMark` (only present on
        // `Text` parts per MVP) to the Anthropic `CacheControl` hint,
        // propagating the mark's `scope` as a `cache_namespace` field. Other
        // variants get `cache_control: None` — the mark is only ever set on
        // user-message Text parts by `apply_cache_policy`.
        let cache_control = part.cache_control().map(cache_control_from_mark);
        // An EMPTY text block is rejected by Anthropic, in every position.
        // It is reachable from a legitimately-persisted turn: the composer
        // lets the user send attachments with no text, which writes
        // `{"text":""}` to the log, and the next turn replays it as a
        // `Text("")` part. Dropping it here — rather than at each producer
        // — keeps every caller honest, and when the drop leaves a message
        // with no parts the existing `content.is_empty()` guard in
        // `transform_message` already turns it into "nothing to send".
        if let ContentPart::Text(tc) = part
            && tc.text.is_empty()
        {
            return None;
        }
        let block = match part {
            ContentPart::Text(tc) => AnthropicContentBlock::Text {
                text: tc.text.clone(),
                cache_control,
            },
            ContentPart::Reasoning(rc) => {
                let signature = rc.signature.clone()?;
                AnthropicContentBlock::ThinkingBlock {
                    thinking: rc.text.clone(),
                    signature: Some(signature),
                }
            }
            ContentPart::Image(ic) => AnthropicContentBlock::Image {
                source: image_source(ic),
            },
            ContentPart::Audio(ac) => {
                let remote = ac.is_remote_url();
                AnthropicContentBlock::Audio {
                    source: AnthropicAudioSource {
                        r#type: if remote {
                            "url".to_string()
                        } else {
                            "base64".to_string()
                        },
                        media_type: ac.mime_type.clone(),
                        data: if remote {
                            ac.data.clone()
                        } else {
                            ac.inline_base64().unwrap_or_default().to_string()
                        },
                        format: ac.format_label().map(str::to_string),
                    },
                }
            }
            ContentPart::ToolUse(tu) => AnthropicContentBlock::ToolUse {
                id: Self::sanitize_tool_id(&tu.id),
                // `name` is `1 <= len <= 200` and `input` is
                // `map[unknown]` on this wire; both are shaped by the
                // shared helpers so the same canonical part cannot be
                // valid here and rejected by the OpenAI adapter (or the
                // reverse).
                name: tu.wire_name(),
                input: tu.wire_input(),
                cache_control: None,
            },
            ContentPart::ToolResult(tr) => AnthropicContentBlock::ToolResult {
                tool_use_id: Self::sanitize_tool_id(&tr.tool_use_id),
                content: tool_result_blocks(tr),
                is_error: tr.is_error,
                cache_control: None,
            },
            ContentPart::Resource(link) => AnthropicContentBlock::Text {
                text: link.prompt_projection(),
                cache_control: None,
            },
        };
        Some(block)
    }
}

/// Map a provider [`ImageContent`](crate::types::ImageContent) onto
/// Anthropic's `image.source`.
///
/// Remote vs inline is decided by the URL scheme alone (see
/// [`ImageContent::is_remote_url`](crate::types::ImageContent::is_remote_url)).
/// The previous implementation also
/// treated any string shorter than 1000 characters as base64, which
/// silently mis-classifies a short remote URL and — the dangerous
/// direction — classifies a real (tens-of-kilobytes) base64 payload as
/// a URL, producing an `image.source.type = "url"` block the API
/// rejects.
fn image_source(ic: &crate::types::ImageContent) -> AnthropicImageSource {
    if ic.is_remote_url() {
        AnthropicImageSource::Url {
            url: ic.data.clone(),
        }
    } else {
        AnthropicImageSource::Base64 {
            media_type: ic.mime_type.clone(),
            data: ic.inline_base64().unwrap_or_default().to_string(),
        }
    }
}

/// Map a tool result's content parts onto Anthropic's
/// `tool_result.content` blocks.
///
/// `Text` and `Image` survive as themselves — Anthropic renders an
/// image block inside a tool result, which is how a screenshot,
/// chart, or OCR tool returns something the model can see. Anything
/// else (audio, a nested tool result, a resource link) is
/// Debug-formatted into a text block, and an **error** result keeps
/// the single `Error: {:?}` text block it has always produced.
///
/// An EMPTY text part becomes a placeholder rather than being shipped
/// as-is: the API's `text` field has `minLength: 1`, so a tool that
/// returned nothing would otherwise make the whole request invalid. The
/// placeholder matches the vocabulary the built-in shell tool already
/// prints for the same situation.
fn tool_result_blocks(
    tr: &crate::types::ToolResult,
) -> Vec<AnthropicToolResultContent> {
    if tr.is_error.unwrap_or(false) {
        return vec![AnthropicToolResultContent::Text {
            text: format!("Error: {:?}", tr.content),
        }];
    }
    // A result with no parts at all is the same hole from the other
    // direction: `content: []` carries nothing for the model to read.
    if tr.content.is_empty() {
        return vec![AnthropicToolResultContent::Text {
            text: EMPTY_TOOL_RESULT.to_string(),
        }];
    }
    tr.content
        .iter()
        .map(|part| match part {
            ContentPart::Text(tc) if tc.text.is_empty() => {
                AnthropicToolResultContent::Text {
                    text: EMPTY_TOOL_RESULT.to_string(),
                }
            }
            ContentPart::Text(tc) => AnthropicToolResultContent::Text {
                text: tc.text.clone(),
            },
            ContentPart::Image(ic) => AnthropicToolResultContent::Image {
                source: image_source(ic),
            },
            other => AnthropicToolResultContent::Text {
                text: format!("{other:?}"),
            },
        })
        .collect()
}

/// What the model is told when a tool returned no text. Matches the
/// wording the built-in shell tool prints for a command with no output.
const EMPTY_TOOL_RESULT: &str = "(no output)";

/// Build the Anthropic `system` field from the extracted system text and
/// the cache policy.
///
/// When `cache_policy` is `None` (or `policy.system == false`) the `Text`
/// variant is used, which serializes as a plain JSON string — preserving
/// byte-identical backward-compatible output. When `policy.system == true`
/// the `Structured` variant is used so a `cache_control` hint can be
/// attached to the (single) system block, marking the cache prefix
/// boundary.
///
/// `representative_mark` propagates the cache scope (carried on tool /
/// message marks by `apply_cache_policy`) to the system block so its
/// `cache_control` is namespaced identically. When `None` (no marks exist,
/// e.g. empty request) [`CacheControl::default()`] is used.
fn build_anthropic_system(
    system_text: Option<String>,
    cache_policy: Option<&crate::cache_policy::CachePolicy>,
    representative_mark: Option<&CacheControlMark>,
) -> Option<AnthropicSystem> {
    // An EMPTY system prompt is treated as no system prompt at all, because
    // the protocol's `system` text carries the same `minLength: 1` as every
    // other text block — `{"type":"text","text":""}` is rejected.
    //
    // Reachable rather than theoretical: `Content::extract_text` joins the
    // text parts, so a single empty `Text` part yields `Some("")` (the vec
    // is `[""]`, not empty), and this function would otherwise pass that
    // straight through. Omitting the field is also the honest encoding —
    // an empty prompt and no prompt mean the same thing to the model.
    let text = system_text.filter(|t| !t.is_empty())?;
    let use_structured = cache_policy.map(|p| p.system).unwrap_or(false);
    if use_structured {
        let cache_control = representative_mark
            .map(cache_control_from_mark)
            .unwrap_or_default();
        Some(AnthropicSystem::Structured(vec![AnthropicSystemBlock {
            r#type: "text".to_string(),
            text,
            cache_control: Some(cache_control),
        }]))
    } else {
        Some(AnthropicSystem::Text(text))
    }
}

#[cfg(test)]
mod tests;
