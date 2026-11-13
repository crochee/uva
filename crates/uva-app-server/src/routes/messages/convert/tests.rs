use anyhow::Context as _;
use uva::provider::Role;

use super::*;
use crate::routes::messages::MessagesOutcome;

fn parse(body: serde_json::Value) -> Result<CanonicalRequest> {
    let request: AnthropicRequest =
        serde_json::from_value(body).context("test body must parse")?;
    to_canonical(&request)
}

fn body(messages: serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "model": "claude-test",
        "max_tokens": 64,
        "messages": messages,
    })
}

/// A `content: "<string>"` user turn is the request's whole payload:
/// no history, one text part, no system, no tools.
#[test]
fn string_content_becomes_the_turn() {
    let canonical = parse(body(serde_json::json!([
        { "role": "user", "content": "hi" }
    ])))
    .unwrap();
    assert!(canonical.system.is_none());
    assert!(canonical.history.is_empty());
    assert!(canonical.tools.is_empty());
    assert_eq!(canonical.turn.len(), 1);
    assert_eq!(canonical.turn[0].text(), Some("hi"));
}

/// Multi-turn input: every message but the last lands in `history`
/// with its own role, and the last becomes the turn.
#[test]
fn earlier_turns_become_history_with_their_roles() {
    let canonical = parse(body(serde_json::json!([
        { "role": "user", "content": "one" },
        { "role": "assistant", "content": "two" },
        { "role": "user", "content": "three" }
    ])))
    .unwrap();
    let roles: Vec<Role> = canonical.history.iter().map(|m| m.role).collect();
    assert_eq!(roles, vec![Role::User, Role::Assistant]);
    assert_eq!(
        canonical.history[1].content.extract_text().as_deref(),
        Some("two")
    );
    assert_eq!(canonical.turn[0].text(), Some("three"));
}

/// The `system` field's two spellings flatten to the same text, and
/// an all-empty block list flattens to `None`.
#[test]
fn system_flattens_from_string_and_blocks() {
    let string_form = parse(serde_json::json!({
        "model": "m",
        "max_tokens": 8,
        "system": "be brief",
        "messages": [{ "role": "user", "content": "hi" }],
    }))
    .unwrap();
    assert_eq!(string_form.system.as_deref(), Some("be brief"));

    let block_form = parse(serde_json::json!({
        "model": "m",
        "max_tokens": 8,
        "system": [{ "type": "text", "text": "be brief" }],
        "messages": [{ "role": "user", "content": "hi" }],
    }))
    .unwrap();
    assert_eq!(block_form.system.as_deref(), Some("be brief"));

    let empty = parse(serde_json::json!({
        "model": "m",
        "max_tokens": 8,
        "system": [{ "type": "text", "text": "  " }],
        "messages": [{ "role": "user", "content": "hi" }],
    }))
    .unwrap();
    assert!(empty.system.is_none());
}

/// An inline image keeps its bytes and media type; a URL image
/// stores the URL in `data` with a media type derived from its
/// extension.
#[test]
fn image_sources_round_trip_into_image_parts() {
    let canonical = parse(body(serde_json::json!([{
        "role": "user",
        "content": [
            { "type": "text", "text": "look" },
            { "type": "image", "source": {
                "type": "base64", "media_type": "image/png", "data": "AAAB"
            }},
            { "type": "image", "source": {
                "type": "url", "url": "https://example.test/a.jpg"
            }},
        ],
    }])))
    .unwrap();
    let images: Vec<&ImageContent> = canonical
        .turn
        .iter()
        .filter_map(|part| match part {
            ContentPart::Image(image) => Some(image),
            _ => None,
        })
        .collect();
    assert_eq!(images.len(), 2);
    assert_eq!(images[0].data, "AAAB");
    assert_eq!(images[0].mime_type, "image/png");
    assert_eq!(images[1].data, "https://example.test/a.jpg");
    assert_eq!(images[1].mime_type, "image/jpeg");
}

/// A URL with no recognised extension still yields an image part
/// with the neutral `image/*` type.
#[test]
fn url_image_without_extension_uses_neutral_media_type() {
    assert_eq!(super::url_mime_type("https://x.test/i"), "image/*");
    assert_eq!(
        super::url_mime_type("https://x.test/i.png?w=2"),
        "image/png"
    );
}

/// A `document` block rides as the `ResourceLink` the old
/// `/api/v1/chat/*` wire's file attachment produced: inline bytes
/// become the historical `data:<media_type>;base64,<payload>` URI
/// with the media type beside them, and a URL source keeps the URL.
#[test]
fn document_sources_become_resource_parts() {
    let canonical = parse(body(serde_json::json!([{
        "role": "user",
        "content": [
            { "type": "document", "source": {
                "type": "base64",
                "media_type": "application/pdf",
                "data": "JVBERi0=",
            }},
            { "type": "document", "source": {
                "type": "url",
                "url": "https://example.test/spec.md",
            }},
        ],
    }])))
    .unwrap();
    let links: Vec<&ResourceLink> = canonical
        .turn
        .iter()
        .filter_map(|part| match part {
            ContentPart::Resource(link) => Some(link),
            _ => None,
        })
        .collect();
    assert_eq!(links.len(), 2, "both documents must convert: {links:?}");
    assert_eq!(links[0].uri, "data:application/pdf;base64,JVBERi0=");
    assert_eq!(links[0].mime_type.as_deref(), Some("application/pdf"));
    assert_eq!(links[1].uri, "https://example.test/spec.md");
    assert_eq!(
        links[1].mime_type, None,
        "a `url` source names no media type, so none may be invented"
    );
}

/// A `document` the endpoint cannot use is refused with a sentence
/// naming what is wrong — the unknown `source.type`, the missing
/// `source`, or the field a spelling requires — never converted to
/// an empty part and never silently dropped.
///
/// The sentences come from serde and reach the handler intact
/// because [`WireContent`] deserializes block lists directly; under
/// an untagged wrapper every one of them would collapse into the
/// single "data did not match any variant" sentence and the client
/// would be told nothing about what to fix.
#[test]
fn unusable_documents_are_named() {
    for (label, block, named) in [
        (
            "unknown source type",
            serde_json::json!({
                "type": "document",
                "source": { "type": "file", "file_id": "f1" },
            }),
            "unknown variant `file`",
        ),
        (
            "no source at all",
            serde_json::json!({ "type": "document" }),
            "missing field `source`",
        ),
        (
            "base64 source without its payload",
            serde_json::json!({
                "type": "document",
                "source": {
                    "type": "base64",
                    "media_type": "application/pdf",
                },
            }),
            "missing field `data`",
        ),
        (
            "url source without a url",
            serde_json::json!({
                "type": "document",
                "source": { "type": "url" },
            }),
            "missing field `url`",
        ),
    ] {
        let error = parse(body(serde_json::json!([{
            "role": "user",
            "content": [block],
        }])))
        .err()
        .unwrap_or_else(|| panic!("{label} must be rejected"));
        let reason = format!("{error:#}");
        assert!(
            reason.contains(named),
            "{label} must name the problem ({named:?}): {reason}"
        );
        assert!(
            !reason.contains("untagged"),
            "{label} must not answer with serde's collapsed sentence: {reason}"
        );
    }
}

/// A `document` inside a `tool_result` is refused at its own
/// position: a result may carry `text` and `image` blocks only, and
/// the client must learn which block and which nesting was the
/// problem rather than seeing the bytes vanish.
#[test]
fn a_document_inside_a_tool_result_is_named_by_position() {
    let error = parse(body(serde_json::json!([
        { "role": "user", "content": "run it" },
        { "role": "assistant", "content": [
            { "type": "tool_use", "id": "toolu_1", "name": "bash",
              "input": { "cmd": "ls" } }
        ]},
        { "role": "user", "content": [
            { "type": "tool_result", "tool_use_id": "toolu_1",
              "content": [
                { "type": "text", "text": "output" },
                { "type": "document", "source": {
                    "type": "base64",
                    "media_type": "application/pdf",
                    "data": "JVBERi0=",
                }},
              ]},
        ]},
        { "role": "user", "content": "go on" }
    ])))
    .expect_err("a document-backed tool result must be rejected");
    let reason = format!("{error:#}");
    assert!(
        reason.contains("messages[2].content[0].content[1]"),
        "the inner block's position must be named: {reason}"
    );
    assert!(
        reason.contains("tool results support only `text` and `image`"),
        "the reason must say what a result may carry: {reason}"
    );
}

/// `tool_result` blocks become their own `Role::Tool` messages
/// (canonical shape) while the text around them stays a user
/// message, so a replayed transcript matches a recorded one.
#[test]
fn tool_results_split_into_tool_role_messages() {
    let canonical = parse(body(serde_json::json!([
        { "role": "user", "content": "run it" },
        { "role": "assistant", "content": [
            { "type": "tool_use", "id": "toolu_1", "name": "bash",
              "input": { "cmd": "ls" } }
        ]},
        { "role": "user", "content": [
            { "type": "text", "text": "and the output was:" },
            { "type": "tool_result", "tool_use_id": "toolu_1",
              "content": "a.txt", "is_error": false }
        ]},
        { "role": "user", "content": "thanks" }
    ])))
    .unwrap();
    let roles: Vec<Role> = canonical.history.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        vec![Role::User, Role::Assistant, Role::User, Role::Tool]
    );
    match &canonical.history[1].content {
        Content::Single(ContentPart::ToolUse(tool_use)) => {
            assert_eq!(tool_use.id, "toolu_1");
            assert_eq!(tool_use.name, "bash");
            assert_eq!(tool_use.input["cmd"], "ls");
        }
        other => {
            panic!("assistant turn must carry the tool_use: {other:?}")
        }
    }
    let result = &canonical.history[3];
    assert_eq!(result.tool_call_id.as_deref(), Some("toolu_1"));
    match &result.content {
        Content::Single(ContentPart::ToolResult(result)) => {
            assert_eq!(result.is_error, Some(false));
            assert_eq!(
                result.content[0].text(),
                Some("a.txt"),
                "the result's text must survive"
            );
        }
        other => panic!("expected a tool result: {other:?}"),
    }
    assert_eq!(canonical.turn[0].text(), Some("thanks"));
}

/// A thinking block keeps its text and signature.
#[test]
fn thinking_blocks_keep_their_signature() {
    let canonical = parse(body(serde_json::json!([
        { "role": "assistant", "content": [
            { "type": "thinking", "thinking": "hmm", "signature": "sig" }
        ]},
        { "role": "user", "content": "go on" }
    ])))
    .unwrap();
    match &canonical.history[0].content {
        Content::Single(ContentPart::Reasoning(reasoning)) => {
            assert_eq!(reasoning.text, "hmm");
            assert_eq!(reasoning.signature.as_deref(), Some("sig"));
        }
        other => panic!("expected a reasoning part: {other:?}"),
    }
}

/// `tools` project onto canonical definitions, with a missing
/// description defaulting to empty.
#[test]
fn tools_become_definitions() {
    let canonical = parse(serde_json::json!({
        "model": "m",
        "max_tokens": 8,
        "messages": [{ "role": "user", "content": "hi" }],
        "tools": [{
            "name": "get_weather",
            "description": "Look it up",
            "input_schema": { "type": "object", "properties": {} },
        }, {
            "name": "no_description",
            "input_schema": { "type": "object" },
        }],
    }))
    .unwrap();
    assert_eq!(canonical.tools.len(), 2);
    assert_eq!(canonical.tools[0].name, "get_weather");
    assert_eq!(canonical.tools[0].description, "Look it up");
    assert_eq!(
        canonical.tools[0].input_schema["type"],
        serde_json::json!("object")
    );
    assert_eq!(canonical.tools[1].description, "");
}

/// Unknown extra fields are ignored — the protocol grows optional
/// fields and a client that sends one must not be rejected.
#[test]
fn unknown_request_fields_are_ignored() {
    let canonical = parse(serde_json::json!({
        "model": "m",
        "max_tokens": 8,
        "temperature": 0.7,
        "stop_sequences": ["\n\n"],
        "metadata": { "user_id": "u1" },
        "messages": [{
            "role": "user",
            "content": [{ "type": "text", "text": "hi",
                          "cache_control": { "type": "ephemeral" } }],
        }],
    }))
    .unwrap();
    assert_eq!(canonical.turn[0].text(), Some("hi"));
}

/// Unsupported and malformed shapes are rejected with a sentence
/// naming the offending field.
#[test]
fn unusable_requests_are_rejected() {
    for (label, payload) in [
        (
            "no messages",
            serde_json::json!({
                "model": "m", "max_tokens": 8, "messages": [],
            }),
        ),
        (
            "final role is assistant",
            body(serde_json::json!([
                { "role": "assistant", "content": "hi" }
            ])),
        ),
        (
            "empty content list",
            body(serde_json::json!([
                { "role": "user", "content": [] }
            ])),
        ),
        (
            "zero max_tokens",
            serde_json::json!({
                "model": "m", "max_tokens": 0,
                "messages": [{ "role": "user", "content": "hi" }],
            }),
        ),
        (
            "unknown role",
            body(serde_json::json!([
                { "role": "system", "content": "hi" }
            ])),
        ),
        (
            "unknown image source",
            body(serde_json::json!([{
                "role": "user",
                "content": [{ "type": "image",
                              "source": { "type": "file" } }],
            }])),
        ),
    ] {
        let error = parse(payload)
            .err()
            .unwrap_or_else(|| panic!("{label} must be rejected"));
        assert!(!error.to_string().is_empty(), "{label} must carry a reason");
    }
}
/// A document's `title` is the field a client sends the file name in,
/// so it must survive as the resource's `name`. Without this a
/// reloaded transcript can only show a bare media type, and the user
/// cannot tell which file the turn carried.
#[test]
fn a_documented_title_becomes_the_resource_name() {
    let canonical = parse(body(serde_json::json!([
        { "role": "user", "content": [
            { "type": "document", "title": "quarterly-report.pdf",
              "source": { "type": "base64",
                          "media_type": "application/pdf",
                          "data": "JVBERi0=" } }
        ]}
    ])))
    .expect("a titled document is valid");
    let links: Vec<&ResourceLink> = canonical
        .turn
        .iter()
        .filter_map(|part| match part {
            ContentPart::Resource(link) => Some(link),
            _ => None,
        })
        .collect();
    assert_eq!(links.len(), 1, "got {links:?}");
    assert_eq!(links[0].name, "quarterly-report.pdf");
    assert_eq!(links[0].mime_type.as_deref(), Some("application/pdf"));
}

/// A document with no title leaves the name empty rather than
/// inventing one — the renderer owns the fallback label, because only
/// it knows what reads well for a given client.
#[test]
fn an_untitled_document_leaves_the_name_empty() {
    let canonical = parse(body(serde_json::json!([
        { "role": "user", "content": [
            { "type": "document",
              "source": { "type": "url", "url": "https://x/a.pdf" } }
        ]}
    ])))
    .expect("an untitled document is valid");
    let links: Vec<&ResourceLink> = canonical
        .turn
        .iter()
        .filter_map(|part| match part {
            ContentPart::Resource(link) => Some(link),
            _ => None,
        })
        .collect();
    assert_eq!(links.len(), 1, "got {links:?}");
    assert!(links[0].name.is_empty());
    assert_eq!(links[0].uri, "https://x/a.pdf");
}

/// A non-streaming request answers the protocol's own response body —
/// the same object the HTTP surface answered with, field for field.
///
/// The request goes through the whole handler: the body is projected
/// onto the canonical types, the turn is dispatched, and the run is
/// folded back into the message the protocol spells.
#[tokio::test]
async fn a_non_streaming_request_answers_the_protocol_body() {
    let fixture = super::super::tests::Fixture::new().await;
    let outcome = fixture
        .call(super::super::tests::request(false))
        .await
        .expect("the run completes");
    let MessagesOutcome::Body(body) = outcome else {
        panic!("a non-streaming request must answer the body")
    };
    assert_eq!(body["type"], "message");
    assert_eq!(body["role"], "assistant");
    assert_eq!(body["model"], super::super::tests::MODEL);
    assert_eq!(body["content"][0]["type"], "text");
    assert_eq!(body["content"][0]["text"], "hello");
    assert_eq!(body["stop_reason"], "end_turn");
    assert!(body["stop_sequence"].is_null());
    assert_eq!(body["usage"]["input_tokens"], 0);
}

/// A request the protocol refuses is refused in the protocol's own
/// vocabulary: the machine code is the Anthropic error type, not the
/// application's.
#[tokio::test]
async fn a_refused_request_carries_the_anthropic_error_type() {
    let fixture = super::super::tests::Fixture::new().await;
    let request: AnthropicRequest = serde_json::from_value(serde_json::json!({
        "model": super::super::tests::MODEL,
        "max_tokens": 64,
        "messages": [
            { "role": "assistant", "content": "hi" }
        ],
    }))
    .expect("the request body must parse");
    let error = fixture
        .call(request)
        .await
        .expect_err("an assistant prefill is refused");
    assert_eq!(error.code, "invalid_request_error");
}
