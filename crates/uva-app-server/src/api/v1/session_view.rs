//! The durable-row and legacy-attachment views the frontend reads.
//!
//! Two v1 responses carry JSON whose *shape* is closed but whose
//! *Rust type* is not: `SessionDetail::history` / `artifacts` and
//! `FollowSnapshot::rows` are `Vec<serde_json::Value>`, because a
//! session's log is an open row vocabulary and the legacy attachment
//! slot predates it. The frontend still reads those arrays through a
//! named shape, so the shape is named here once — a Rust type the
//! schema emitter derives from, instead of a hand-written TypeScript
//! mirror that can drift.
//!
//! The row envelope is not invented here: it is what the session sink
//! writes. `uva_session`'s writer stamps `seq` and `ts` onto the top
//! level of every durable row, and [`uva::session::SessionEvent`]
//! supplies `type` / `data` plus the optional `source`, `is_error` and
//! `surface_op` members. Every member of [`SessionTurn`] is optional
//! because a row may legitimately carry any subset: an event family
//! that does not record `source` omits it, and a non-object value is
//! wrapped as `{data, seq, ts}` with no `type` at all.
//!
//! The row's `type` is not named here either: it is
//! [`uva::session::RowTag`] — the vocabulary that owns the tag set.
//! The server **publishes current rows** ([`uva::session::publish_row`]
//! restores a dialect row at the wire boundary), so a legacy dialect
//! spelling never reaches the wire and the browser's row accounting
//! has one closed answer for every `type` it can hold.
//!
//! These types are **schema views**. They are not serialized or
//! deserialized by this crate — the routes keep writing
//! `serde_json::Value` — so the responses they describe are pinned by
//! the writer, not by these declarations. The `#[schemars(with = …)]`
//! attributes on the three untyped response fields are where the two
//! meet.
//!
//! `skip_serializing_if` on every member is what tells the schema
//! response type uses. These types never derive serde, so the attribute is
//! inert at runtime; it is the presence annotation the schema needs.
//!
//! The legacy spellings (`attachmentId`, `mediaType`) are kept: a
//! session written before history persistence landed carries those
//! keys, and the UI's legacy fallback still reads them.

use schemars::JsonSchema;
use serde_json::Value;
use uva::{
    provider::Message,
    session::{RowTag, SurfaceOp},
};

/// One durable row of a session log, as `sessions/get` returns it in
/// `history` and `follow` returns it in `rows`.
///
/// The row's **payload is shaped by its tag**, so the schema says
/// which shape: a message row's `data` is the provider `Message` —
/// the same type the provider serializes — not an open bag. The rows
/// whose payload is free-form (a warning's message, a hook block's
/// policy, the crash-repair closer's recovery code) stay `unknown`:
/// the vocabulary declares their *tags*, not their payloads' shapes.
///
/// The tag list comes from the vocabulary itself, so the union cannot
/// drift from the tag set.
#[derive(Debug, Clone, JsonSchema)]
#[schemars(schema_with = "session_turn_schema")]
pub struct SessionTurn;

/// The row envelope — the members every row carries, whatever its
/// tag.
///
/// The published row is the union over the tags
/// ([`SessionTurn`]); this is the shape each of its branches shares.
#[derive(Debug, Clone, JsonSchema)]
pub struct SessionTurnEnvelope {
    /// The event family the row belongs to — a tag of the closed
    /// [`RowTag`] vocabulary. Absent on a row that carries no `type`
    /// at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<RowTag>,
    /// The row's payload — the event's own data, shaped by `type`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// RFC 3339 instant the writer stamped the row with.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<String>,
    /// The writer's ordinal for the row. This is the client's only
    /// cursor, and what a resume token's `last_seq` points at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// Provenance recorded beside the row, when it is not an ordinary
    /// turn boundary. Today only `"steer"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Set on a `tool_result` row whose tool failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
    /// Set on a `tool_output` row: the provider call id the streamed
    /// chunk belongs to, so a client can attach the chunk to the tool
    /// block it is streaming into.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub call_id: Option<String>,
    /// Re-readable refs to the attachments a `user_message` row's turn
    /// carried — an array of `{kind, hash, name, mime_type, byte_len,
    /// unstored}` objects. The bytes never ride the log: a multimodal
    /// payload lives in the deployment's attachment store and the ref
    /// resolves back to it by hash, so a chip renders from the ref
    /// alone. Omitted for a text-only turn and on rows written before
    /// the refs landed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachments: Option<Value>,
    /// The surface op the row applies instead of appending —
    /// `"append"` (a plain string) or a replace range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub surface_op: Option<SurfaceOp>,
}

/// One legacy attachment slot of a session detail.
///
/// Modern sessions always send `[]`: tool calls and their results
/// travel through [`SessionTurn`] rows now. The type survives because
/// the detail page still renders sessions that predate that.
#[derive(Debug, Clone, JsonSchema)]
pub struct SessionArtifact {
    /// The attachment's stable id.
    #[schemars(rename = "attachmentId")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attachment_id: Option<String>,
    /// Display name.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// One-line description.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// The attachment's content segments.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<SessionPart>>,
    /// Server-side metadata the legacy reader groups on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<SessionArtifactMetadata>,
}

/// The metadata block a legacy attachment carries.
#[derive(Debug, Clone, JsonSchema)]
pub struct SessionArtifactMetadata {
    /// The legacy discriminator (`tool_call` / `tool_result`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// The tool the recorded call addressed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_name: Option<String>,
    /// The provider call id the call and its result share.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_use_id: Option<String>,
    /// Whether a recorded tool result failed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub is_error: Option<bool>,
}

/// One content segment of a legacy attachment.
///
/// The v1 server's content parts are field-presence discriminated, so
/// at most one of `text` / `raw` / `url` / `data` is present per part.
#[derive(Debug, Clone, JsonSchema)]
pub struct SessionPart {
    /// Text the segment carried.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Structured payload the segment carried.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
    /// Base64 payload the segment carried.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    /// Remote URL the segment pointed at.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// File name the segment carried.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filename: Option<String>,
    /// MIME type the segment carried.
    #[schemars(rename = "mediaType")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub media_type: Option<String>,
}

/// Build the row's schema: a discriminated union over the row's tag.
///
/// Every branch is the row envelope ([`SessionTurnEnvelope`]) with
/// two members narrowed: `type` names the tag that branch answers
/// to, and `data` — for the rows that carry a message — is the
/// provider `Message`. Every other row keeps the envelope's own
/// "any value", because its payload is free-form: the vocabulary
/// declares the tag, not what the tag's family carries.
///
/// The tag list is read from the vocabulary's own tag union, so a
/// tag the vocabulary adds appears here without a second list to
/// keep in step.
fn session_turn_schema(
    generator: &mut schemars::SchemaGenerator,
) -> schemars::Schema {
    use serde_json::{Value, json};

    // The rows whose payload *is* the provider message — the two
    // tags asked of the vocabulary, never spelled here. A
    // `tool_result` row is not among them: the run's own row holds
    // the tool message, but the crash-repair closer holds a marker
    // of its own — one tag, two payloads, so the schema says
    // neither.
    let message_tags = [
        RowTag::UserMessage.as_wire(),
        RowTag::AssistantMessage.as_wire(),
    ];

    // Registering a type with the generator yields its reference;
    // the definitions map holds the schema itself.
    let message_ref = generator.subschema_for::<Message>();
    // Registering the envelope with the generator registers the tags
    // and ops it references, so the definitions map holds them all.
    generator.subschema_for::<SessionTurnEnvelope>();
    let definitions = generator.definitions().clone();

    let envelope = definitions["SessionTurnEnvelope"].clone();
    // The envelope is an implementation detail of the union: the
    // branches inline its members, so the document carries no
    // definition nothing points at.
    generator.definitions_mut().remove("SessionTurnEnvelope");
    let message: Value = serde_json::to_value(message_ref)
        .expect("the message reference serialises");
    let tags: Vec<String> = definitions["RowTag"]["oneOf"]
        .as_array()
        .map(|branches| {
            branches
                .iter()
                .filter_map(|branch| branch["const"].as_str())
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();

    let branches: Vec<Value> = tags
        .into_iter()
        .map(|tag| {
            let mut branch = envelope.clone();
            let properties = &mut branch
                .as_object_mut()
                .expect("the envelope is an object")["properties"]
                .as_object_mut()
                .expect("the envelope has properties");
            // This branch answers to one tag alone.
            properties.insert("type".to_owned(), json!({ "const": tag }));
            // A message row's payload is the provider's own message.
            if message_tags.contains(&tag.as_str()) {
                properties.insert("data".to_owned(), message.clone());
            }
            branch
        })
        .collect();

    serde_json::from_value(json!({ "oneOf": branches }))
        .expect("the union schema parses")
}
