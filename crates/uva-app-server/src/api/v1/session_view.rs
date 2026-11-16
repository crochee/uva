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
//! These types are **schema views**. They are not serialized or
//! deserialized by this crate — the routes keep writing
//! `serde_json::Value` — so the responses they describe are pinned by
//! the writer, not by these declarations. The `#[schemars(with = …)]`
//! attributes on the three untyped response fields are where the two
//! meet.
//!
//! `skip_serializing_if` on every member is what tells the schema
//! emitter the member may be absent — the same annotation every other
//! response type uses. These types never derive serde, so the attribute is
//! inert at runtime; it is the presence annotation the schema needs.
//!
//! The legacy spellings (`attachmentId`, `mediaType`) are kept: a
//! session written before history persistence landed carries those
//! keys, and the UI's legacy fallback still reads them.

use schemars::JsonSchema;
use serde_json::Value;
use uva::session::SurfaceOp;

/// The closed vocabulary of `type` tags a durable row can carry —
/// `uva_session::KNOWN_SESSION_EVENT_TYPES` re-typed, so the schema
/// emitter can close [`SessionTurn`]'s `type` field over it instead
/// of an open string. The contract catalog exports it as the
/// `SessionEventType` union, and `uva-web`'s row accounting is a
/// `Record` over that generated union, so a tag added here fails the
/// web typecheck until the renderer says what it does with the row.
///
/// A legacy row written before the typed vocabulary carries a
/// pre-vocabulary envelope string (`UserInput`, `Model`, `Agent`) or
/// no `type` at all — which is why the field it types stays optional.
#[derive(Debug, Clone, Copy, PartialEq, Eq, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SessionEventType {
    /// One user turn: `data` is the provider message the run sent.
    UserMessage,
    /// One assistant turn: `data` is the whole provider message.
    AssistantMessage,
    /// A streamed delta of the running assistant turn.
    AssistantChunk,
    /// One tool outcome: `data` is the committed tool message.
    ToolResult,
    /// A committed compaction: `data.summary` replaces a span.
    Compaction,
    /// A compaction lifecycle began.
    CompactionStart,
    /// The summary a compaction produced.
    CompactionSummary,
    /// A compaction lifecycle ended (committed / failed / interrupted).
    CompactionEnd,
    /// One model tool call, separate from the turn's message.
    ToolCall,
    /// A partial-output chunk of a running tool call (`data.output`
    /// is the text; `call_id` names the call). Log-only: the settled
    /// `tool_result` row is the model's copy, and a reload replays
    /// the progress from these rows.
    ToolOutput,
    /// One step boundary inside a turn.
    Step,
    /// One turn boundary of the run.
    Turn,
    /// One iteration boundary of the run.
    Iteration,
    /// A fact the runtime could not report any other way (`data.kind`
    /// is a stable slug, `data.message` says what happened).
    Warning,
    /// A steering guard decision.
    SteeringGuard,
    /// A steering hint the run injected.
    SteeringHint,
    /// A hook blocked an action.
    HookBlock,
    /// The run entered a delegated child session.
    SubagentEnter,
    /// The delegated child session ended.
    SubagentExit,
    /// The provider envelope one request ran under.
    RequestHeader,
    /// A held call asked the host for approval.
    ApprovalAsked,
    /// The host answered a held call.
    ApprovalDecided,
    /// A held call was authorized.
    ApprovalGranted,
    /// One tool dispatch toward its executor.
    ToolDispatch,
    /// The request envelope fingerprint a resume compared against.
    RequestSnapshot,
    /// Token usage counters for a stretch of the run.
    Usage,
    /// The sandbox mode a run executed under.
    SandboxMode,
    /// The session's writer shut down.
    SessionShutdown,
    /// A tool result rewritten by a result-transform seam.
    ToolResultTransformed,
    /// A prompt parked while a turn was in flight.
    ParkedPrompt,
    /// A parked prompt was consumed into a new turn.
    ParkConsumed,
    /// User feedback on a completed turn (log-only).
    Feedback,
    /// One plugin event: `data.kind` names the plugin's event.
    Plugin,
}

/// One durable row of a session log, as `sessions/get` returns it in
/// `history` and `follow` returns it in `rows`.
#[derive(Debug, Clone, JsonSchema)]
pub struct SessionTurn {
    /// The event family the row belongs to — one of the closed
    /// [`SessionEventType`] vocabulary. Absent on a legacy row that
    /// was not written through the typed event vocabulary.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub r#type: Option<SessionEventType>,
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
