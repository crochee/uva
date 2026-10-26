//! Typed session event log — the durable substrate Synthia uses
//! to reconstruct agent runs.
//!
//! Adopted from `deepseek-harness`'s `core/session/src/{types,
//! surface, repair}.ts`. Synthia's existing `SessionSink` trait
//! stores opaque `serde_json::Value` events; this module adds a
//! *strongly-typed* overlay on top of that wire without changing
//! the trait contract.
//!
//! See [`SessionEvent`] for the typed-event enum and
//! [`SurfaceOp`] for the append / replace discriminator.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::token_meter::UsageBuckets;

/// `serde` skip predicate: a flag that is `false` carries no
/// information, so the wire keeps the shape it had before the flag
/// existed.
fn is_false(flag: &bool) -> bool {
    !*flag
}

/// `serde` skip predicate: an empty id is the absence of the field,
/// which is how a row written before the field existed reads back.
fn is_empty_str(value: &str) -> bool {
    value.is_empty()
}

/// One attachment a parked prompt carries: a **re-readable
/// reference**, never the bytes.
///
/// A multimodal prompt's image/audio payload is stored in the
/// deployment's attachment store before the row is written
/// (content-addressed, so the store dedups and the hash identifies
/// the bytes), and the row carries this handle. `hash` is what makes
/// the row a reference at all: the metadata this crate used to emit
/// (`kind` / `name` / `mime_type` / `byte_len`) let a reloaded
/// transcript say "an image was here" but gave it no way to fetch
/// one, so a parked prompt could not be replayed.
///
/// The field set mirrors `synthia_attachment::ImageAttachmentRef` —
/// the ref the store hands back — without depending on that crate:
/// this one stays a leaf, and the log's row shape is the contract
/// both sides share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentRefRow {
    /// The composer's own vocabulary: `"image"`, `"audio"` or
    /// `"file"`.
    pub kind: String,
    /// Content hash (hex SHA-256) the attachment store resolves.
    /// `None` for a part whose bytes were never local — a remote URL
    /// is a reference to something this deployment does not store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hash: Option<String>,
    /// What the composer showed as the attachment's label.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// MIME type, when the part carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
    /// Size in bytes, when it is knowable without fetching the
    /// bytes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub byte_len: Option<u64>,
    /// Why the bytes are **not** in the store, when `hash` is `None`.
    ///
    /// Having no hash means one of two different things — the payload
    /// was never local (a remote URL) or the store refused it (an
    /// unsupported MIME, a payload over the cap) — and a row that
    /// omitted the reason made them the same row on the wire. A reader
    /// that shows "attachment unavailable" cannot otherwise tell a
    /// reference it will never resolve from a store that dropped it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unstored: Option<String>,
}

/// Borrowed view of a [`SurfaceOp::Replace`] payload.
#[derive(Debug, Clone, Copy)]
pub struct ReplaceRange<'a> {
    /// Inclusive start index of the replaced range.
    pub start: usize,
    /// Exclusive end index of the replaced range.
    pub end: usize,
    /// Cited source-event seqs.
    pub source_event_seqs: &'a [u64],
}

/// How a [`SessionEvent`] enters the model-visible surface.
///
/// Wire format:
///
/// - `"append"` (string) — plain append at the tail.
/// - `{"start": usize, "end": usize, "source_event_seqs":
///   [u64]}` (struct) — atomic replacement of `[start, end)`
///   in the folded surface. `source_event_seqs` cites the seqs
///   being shadowed so the fold can reject provenance violations.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SurfaceOp {
    /// Plain append at the tail of the surface. Serialised as
    /// the snake_case string `"append"`.
    AppendString(String),
    /// Replacement of an inclusive range `[start, end)`
    /// (0-indexed) of the previously-folded surface.
    /// `source_event_seqs` cites the seqs being shadowed.
    Replace {
        /// Inclusive start index of the replaced range (in the
        /// folded surface).
        start: usize,
        /// Exclusive end index of the replaced range.
        end: usize,
        /// Seqs of the events being shadowed. The fold validator
        /// rejects replace ops whose `source_event_seqs` either
        /// (a) reference seqs outside the previous-log prefix or
        /// (b) are non-monotonic with the new event's seq.
        source_event_seqs: Vec<u64>,
    },
}

impl SurfaceOp {
    /// Wire name for the append form.
    pub const APPEND: &'static str = "append";

    /// Build the append variant.
    #[must_use]
    pub fn append() -> Self {
        Self::AppendString(Self::APPEND.to_string())
    }

    /// True when the op replaces a previously-folded range.
    #[must_use]
    pub const fn is_replace(&self) -> bool {
        matches!(self, Self::Replace { .. })
    }

    /// True when the op appends to the tail of the surface.
    #[must_use]
    pub const fn is_append(&self) -> bool {
        matches!(self, Self::AppendString(_))
    }

    /// Replace variant accessor (`None` for append).
    #[must_use]
    pub fn as_replace(&self) -> Option<ReplaceRange<'_>> {
        match self {
            Self::Replace {
                start,
                end,
                source_event_seqs,
            } => Some(ReplaceRange {
                start: *start,
                end: *end,
                source_event_seqs,
            }),
            Self::AppendString(_) => None,
        }
    }
}

impl Default for SurfaceOp {
    fn default() -> Self {
        Self::append()
    }
}

/// One entry in the durable session event log.
///
/// `seq` is the monotonic ordinal assigned by the sink at append
/// time. Callers MUST NOT pick seq manually — the sink stamps it
/// on `read()` (returning events in the canonical order) and on
/// `fold_surface` (which validates seq continuity).
///
/// The `data` payload on message-emitting variants is a JSON
/// `Value` so the wire shape stays decoupled from
/// `synthia_provider::Message`'s exact serialization (which may
/// grow new content-part variants over time).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// User-role message — surface-eligible.
    #[serde(rename = "user_message")]
    UserMessage {
        /// Sequence ordinal stamped by the sink.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Message payload (matches `synthia_provider::Message`).
        data: Value,
        /// Where the turn came from when it is not the ordinary
        /// turn boundary — today only `"steer"`, a message the
        /// user injected into a run that was already live.
        ///
        /// The row is the delivery mechanism: the loop re-derives
        /// its transcript from the log at every step boundary, so
        /// a row appended mid-run is in the next request and never
        /// in the one already in flight. `None` is the ordinary
        /// queue path, and the field is omitted from the wire so
        /// an old reader keeps the shape it always had.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<String>,
        /// How this event joins the surface.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        surface_op: Option<SurfaceOp>,
    },
    /// Assistant-role message — surface-eligible.
    #[serde(rename = "assistant_message")]
    AssistantMessage {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Message payload.
        data: Value,
        /// How this event joins the surface.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        surface_op: Option<SurfaceOp>,
    },
    /// One streamed provider delta, preserved verbatim for
    /// replay fidelity — log-only.
    ///
    /// The agent loop emits one of these per wire delta
    /// alongside the eventually-complete `AssistantMessage`.
    /// Rebuild the turn's text with
    /// [`crate::surface::assemble_chunks`].
    #[serde(rename = "assistant_chunk")]
    AssistantChunk {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Monotonic ordinal of this chunk within its turn
        /// (0-based, assigned by the emitter).
        chunk_seq: u64,
        /// Provider stop reason, when this chunk is terminal.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        finish_reason: Option<String>,
        /// Chunk payload (`{"delta": "<text>"}`).
        data: Value,
    },
    /// Tool-result message — surface-eligible.
    #[serde(rename = "tool_result")]
    ToolResult {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Message payload.
        data: Value,
        /// True when the tool failed.
        ///
        /// The payload is the tool *message* — the tool's output and
        /// the id of the call it answers — and the message carries no
        /// failure flag of its own. The flag rides beside it, so the
        /// durable row says what the live event said: without it, a
        /// failed tool reads as a successful one after a reload.
        #[serde(default, skip_serializing_if = "is_false")]
        is_error: bool,
        /// How this event joins the surface.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        surface_op: Option<SurfaceOp>,
    },
    /// Summary replacing a span of older tool-results.
    ///
    /// Used by the `SummarizingContextManager` to swap a tail of
    /// noisy tool results for one compact assistant message while
    /// keeping the durable log lossless.
    #[serde(rename = "compaction")]
    Compaction {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Summary text (assistant message body).
        data: Value,
        /// Surface op (replace with cited source seqs).
        surface_op: SurfaceOp,
    },
    /// Compaction began — log-only.
    ///
    /// An unmatched `CompactionStart` (no matching
    /// `CompactionEnd`) is the crash marker the loader detects;
    /// see [`crate::repair::orphaned_compactions`].
    #[serde(rename = "compaction_start")]
    CompactionStart {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Opaque id correlating Start/Summary/End.
        token: String,
        /// Compaction metadata (reason, budget, ...).
        data: Value,
    },
    /// Compaction produced a summary — log-only.
    #[serde(rename = "compaction_summary")]
    CompactionSummary {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// `token` of the matching `CompactionStart`.
        token: String,
        /// Summary payload (`{"summary": "<text>"}`).
        data: Value,
    },
    /// Compaction finished (success or failure) — log-only.
    #[serde(rename = "compaction_end")]
    CompactionEnd {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// `token` of the matching `CompactionStart`.
        token: String,
        /// `"committed"` | `"failed"` | `"interrupted"` (see
        /// [`CompactionLifecycleOutcome`]).
        outcome: String,
        /// Terminal-state payload.
        data: Value,
    },
    /// A tool call the model emitted — log-only, no surface.
    #[serde(rename = "tool_call")]
    ToolCall {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Call id, tool name, raw arguments JSON.
        data: Value,
    },
    /// Step boundary (`start` | `end`) — log-only.
    #[serde(rename = "step")]
    Step {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// `start` or `end`, plus `turn` and `step` indices.
        data: Value,
    },
    /// Turn boundary (`start` | `end`) — log-only.
    #[serde(rename = "turn")]
    Turn {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// `start` or `end`, plus `turn` index and end reason.
        data: Value,
    },
    /// Iteration boundary (`start` | `end`) — log-only.
    #[serde(rename = "iteration")]
    Iteration {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// `start` or `end`, plus iteration index.
        data: Value,
    },
    /// Lifecycle warning — log-only.
    #[serde(rename = "warning")]
    Warning {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Warning kind + message + iteration.
        data: Value,
    },
    /// Steering guard decision — log-only.
    #[serde(rename = "steering_guard")]
    SteeringGuard {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Guard name + action + reason.
        data: Value,
    },
    /// Steering hint injection — log-only.
    #[serde(rename = "steering_hint")]
    SteeringHint {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Hint name + content + injection point.
        data: Value,
    },
    /// Hook veto — log-only.
    #[serde(rename = "hook_block")]
    HookBlock {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Hook name + reason.
        data: Value,
    },
    /// Sub-agent delegation entry — log-only.
    #[serde(rename = "subagent_enter")]
    SubagentEnter {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Child session id + parent + depth.
        data: Value,
    },
    /// Sub-agent delegation exit — log-only.
    #[serde(rename = "subagent_exit")]
    SubagentExit {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Child session id + parent + end reason.
        data: Value,
    },
    /// Per-call request config snapshot — log-only.
    #[serde(rename = "request_header")]
    RequestHeader {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Epoch + provider + model + tool/system hashes.
        data: Value,
    },
    /// A high-risk call held for approval — log-only.
    #[serde(rename = "approval_asked")]
    ApprovalAsked {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Request id the decision refers to.
        id: String,
        /// Provider call id of the held call.
        call_id: String,
        /// Tool the call addresses.
        tool: String,
        /// Digest of the call's arguments.
        arguments_hash: String,
        /// `"low"` | `"high"` | `"unknown"` at the gate site.
        risk: String,
        /// Agent the call belongs to.
        agent: String,
    },
    /// An operator's decision on a held call — log-only.
    #[serde(rename = "approval_decided")]
    ApprovalDecided {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Request id the decision answers.
        id: String,
        /// Provider call id of the held call.
        call_id: String,
        /// `"approved"` | `"denied"` | `"cancelled"`.
        status: String,
        /// Who decided (`"resume"`, an operator id, …).
        by: String,
        /// Decision payload (a grant scope, a denial reason, …).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        payload: Option<Value>,
    },
    /// A durable approval grant — log-only.
    #[serde(rename = "approval_granted")]
    ApprovalGranted {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Request id the grant was born from.
        id: String,
        /// Tool the grant covers.
        tool: String,
        /// `"session"` | `"deployment"`.
        scope: String,
        /// Workspace the grant is bound to.
        workspace_hash: String,
    },
    /// The intent record written before a tool runs — log-only.
    ///
    /// The row is the crash marker: a dispatch with no matching
    /// `tool_result` means the side effect may have happened, so
    /// repair reports `TOOL_OUTCOME_UNKNOWN` instead of re-running
    /// the call.
    #[serde(rename = "tool_dispatch")]
    ToolDispatch {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Provider call id.
        call_id: String,
        /// Tool the call addresses.
        tool: String,
        /// Digest of the call's arguments.
        arguments_hash: String,
    },
    /// The request envelope a model call ran under — log-only.
    ///
    /// Written before the request; a resume compares the current
    /// fingerprint with this row and ends as `ConfigDrift` when
    /// they disagree.
    #[serde(rename = "request_snapshot")]
    RequestSnapshot {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Provider id.
        provider: String,
        /// Model name.
        model: String,
        /// Hash of the sampling parameters.
        params_hash: String,
        /// Hash of the offered tool definitions.
        tools_hash: String,
        /// Hash of the assembled system prompt.
        system_hash: String,
        /// Prompt-section ids that contributed.
        sections: Vec<String>,
        /// Hash of the approval policy.
        policy_hash: String,
    },
    /// Per-call usage attribution — log-only.
    ///
    /// R30: the provider report is carried in two shapes. The
    /// typed `usage` field is the current wire form (serde
    /// default, so pre-R30 rows parse with `None`); the legacy
    /// `data` payload (`prompt_tokens` / `completion_tokens` /
    /// `cache_read_tokens` / `cache_write_tokens`) is still
    /// stamped so older readers keep working. Read either shape
    /// through [`SessionEvent::provider_usage`].
    #[serde(rename = "usage")]
    Usage {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Epoch + token breakdown (legacy envelope).
        data: Value,
        /// Typed disjoint provider usage report (R30), absent on
        /// older rows.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<UsageBuckets>,
    },
    /// Session sandbox-mode override — log-only.
    ///
    /// A policy switch (a UI control, a delegation seed, a test
    /// fixture) is recorded as one of these on the session it
    /// applies to. The log IS the store: the override survives a
    /// restart by replay, and two sessions can never see each
    /// other's state (dsh `sandbox/mode` parity). The effective
    /// policy is the last such event folded against the caller's
    /// grants and the deployment default — see
    /// `synthia_tool_shell::sandbox::effective_policy`.
    #[serde(rename = "sandbox_mode")]
    SandboxMode {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Execution-policy wire name (`"read-only"` /
        /// `"workspace-write"` / `"danger-full-access"`). Carried
        /// as a string rather than an enum: a log written by a
        /// newer build must round-trip here, and a mode this
        /// build does not know is ignored by the policy fold.
        mode: String,
    },
    /// Session shutdown lifecycle — log-only.
    ///
    /// Emitted by the server's `SessionController::close` after
    /// all child sessions have been signalled, so the log ends
    /// with an explicit, replayable shutdown marker.
    #[serde(rename = "session_shutdown")]
    LifecycleShutdown {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Shutdown metadata (reason, child count, ...).
        data: Value,
    },
    /// A tool result a registered transformer altered — log-only.
    ///
    /// Provenance, not reconciliation: transformers run *before* the
    /// loop commits the result, so the `tool_result` row already holds
    /// the rewritten text — the log and the model agree. This row
    /// records who altered it on the way to that commit, with a digest
    /// of each side (`before` / `after`) instead of the payload.
    #[serde(rename = "tool_result_transformed")]
    ToolResultTransformed {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// The transformers that ran, in registration order. A name may
        /// contain any character, so they are a list, not a join.
        transformers: Vec<String>,
        /// Tool the rewritten result belongs to.
        tool_name: String,
        /// Provider call id the result answers.
        call_id: String,
        /// Digest of the result before the rewrite.
        before: String,
        /// Digest of the result the model reads.
        after: String,
    },
    /// A prompt parked for the next run — log-only.
    ///
    /// The multimodal op path parks its lossless parts on the
    /// controller (the text queue carries a `String` and nothing
    /// else), and that slot used to be process memory: a crash
    /// between the park and the run lost the turn. This row *is* the
    /// park — appended and flushed before the op returns — so a node
    /// that takes the session over reads the prompt back and re-parks
    /// it.
    ///
    /// The attachment bytes are deliberately **not** in the row: a
    /// base64 image would bloat the log and no reader wants it as
    /// text. Each part is stored in the attachment store first and
    /// the row carries [`AttachmentRefRow`]s, which resolve back to
    /// the bytes by hash.
    ///
    /// The row is consumed by the run **started from it**, which
    /// appends a [`SessionEvent::ParkConsumed`] naming this row's
    /// `park_id` before that run's own prompt row; see
    /// [`crate::repair::parked_prompts`]. The link is by id, not by
    /// order: a text `Prompt` is not parked yet writes a `UserInput`
    /// row too, so a position could not name the park it spent.
    #[serde(rename = "parked_prompt")]
    ParkedPrompt {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Handle the consuming run quotes back.
        ///
        /// Unique within the log (minted when the park is made, and
        /// the same id whether or not its append reported success — a
        /// row that landed anyway is still spent by its own run). A
        /// row written before the link existed omits it and is treated
        /// as still pending.
        #[serde(default, skip_serializing_if = "is_empty_str")]
        park_id: String,
        /// The prompt's text parts, concatenated.
        text: String,
        /// Re-readable refs to the turn's attachments.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<AttachmentRefRow>,
        /// The agent the turn named, when the client selected one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_name: Option<String>,
        /// `true` when parked by a `Rerun`: the prompt row that turn
        /// eventually writes replaces the turn it shadows, so the
        /// flag has to survive the park.
        #[serde(default, skip_serializing_if = "is_false")]
        rerun: bool,
    },
    /// A run took a parked prompt — log-only.
    ///
    /// The durable half of the park/run link. Only the run started
    /// from a park writes it, and it is written before that run's own
    /// prompt row, so the pair is in the log whatever happens next.
    /// `parked_prompts` folds the pairs by [`Self::ParkedPrompt`]'s
    /// `park_id`: a park with this row is spent, a park without it is
    /// still queued — so a prompt whose row is intact is never dropped
    /// because some *other* turn consumed a row by position, and a run
    /// that started from a park always spends exactly that park.
    ///
    /// Log-only: the model history, the folded surface and `fold`
    /// ignore it, and it carries no `surface_op`.
    #[serde(rename = "park_consumed")]
    ParkConsumed {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// The `park_id` of the [`SessionEvent::ParkedPrompt`] this
        /// run took.
        park_id: String,
    },
    /// A plugin's own domain event — log-only.
    ///
    /// The escape hatch for a plugin that publishes a fact the
    /// harness has no typed variant for: the sink appends it to the
    /// durable log, so it survives a crash and replays with the rest
    /// of the row stream. It is **log-only** — a plugin payload is
    /// opaque to the loop, so the row never enters the model
    /// history, the folded surface, or `fold`. The live-only
    /// counterpart is `synthia_harness::SystemEvent::Plugin`: a
    /// notification that may be dropped under backpressure. This
    /// row is the record; that one is the ping.
    #[serde(rename = "plugin")]
    Plugin {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Plugin-defined event kind. A runtime value, hence a
        /// `String`: a plugin's kind cannot satisfy `'static`
        /// without leaking the string it sent. Capped at
        /// [`MAX_PLUGIN_KIND_BYTES`]; an empty kind is recorded as
        /// `"unnamed"`.
        kind: String,
        /// Plugin-defined payload. A value over
        /// [`MAX_PLUGIN_PAYLOAD_BYTES`] is replaced by a JSON
        /// *string* holding its leading bytes, and
        /// `original_bytes` says what it was.
        payload: Value,
        /// Encoded size of the payload the plugin produced, in
        /// bytes — present only when the cap truncated it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        original_bytes: Option<u64>,
    },
}

/// Cap on what one [`SessionEvent::Plugin`] payload costs the row, in
/// bytes of the encoded form.
///
/// The payload arrives from a plugin process, so nothing bounds it
/// upstream; an unbounded blob would ride every append and every
/// replay of the log. 64 KiB fits a domain event's structured body
/// (attributes, a metric sample, a ticket reference) while keeping a
/// pathological payload from dominating the file.
///
/// Measured on the *escaped* form, because that is what the row
/// carries: a payload stored as a JSON string is escaped a second
/// time when the row is serialised, so a quote-dense body costs up to
/// ~1.6× its raw length. A cap on the raw length would promise bytes
/// the row never honours.
pub const MAX_PLUGIN_PAYLOAD_BYTES: usize = 64 * 1024;

/// Cap on a plugin row's `kind`, in bytes of the encoded form.
///
/// A kind is a filter key, not a body: without a cap a plugin moves
/// its payload into it.
pub const MAX_PLUGIN_KIND_BYTES: usize = 256;

/// What a plugin row records when the plugin sent no kind.
///
/// A row whose filter key is `""` is indistinguishable from a row
/// missing the field, so the degenerate case gets a name a consumer
/// can count.
const UNNAMED_PLUGIN_KIND: &str = "unnamed";

/// Bytes one JSON string literal costs — what the row carries once
/// the value is serialised.
fn json_string_len(text: &str) -> usize {
    serde_json::to_string(text)
        .map(|literal| literal.len())
        .unwrap_or(0)
}

/// Cut `text` at a char boundary so its JSON literal costs at most
/// `max_bytes`.
fn cap_json_string(text: &str, max_bytes: usize) -> String {
    if json_string_len(text) <= max_bytes {
        return text.to_string();
    }
    // Escaping only grows with the prefix, so the fit is monotone
    // and a bisect over char boundaries finds the longest prefix
    // that still fits (`""` costs two bytes, hence the `0` floor).
    let mut boundaries: Vec<usize> =
        text.char_indices().map(|(index, _)| index).collect();
    boundaries.push(text.len());
    let (mut lo, mut hi) = (0, boundaries.len() - 1);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if json_string_len(&text[..boundaries[mid]]) <= max_bytes {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    text[..boundaries[lo]].to_string()
}

/// Bound one plugin payload, reporting the encoded size it had.
fn bound_plugin_payload(payload: Value) -> (Value, Option<u64>) {
    let Ok(encoded) = serde_json::to_string(&payload) else {
        // A `Value` cannot fail to serialise; keep it verbatim if
        // one ever does rather than dropping the row's content.
        return (payload, None);
    };
    if encoded.len() <= MAX_PLUGIN_PAYLOAD_BYTES {
        return (payload, None);
    }
    let original = encoded.len() as u64;
    let kept = cap_json_string(&encoded, MAX_PLUGIN_PAYLOAD_BYTES);
    (Value::String(kept), Some(original))
}

/// Bound one plugin kind, naming an empty one.
fn bound_plugin_kind(kind: String) -> String {
    let kept = cap_json_string(&kind, MAX_PLUGIN_KIND_BYTES);
    if kept.is_empty() {
        UNNAMED_PLUGIN_KIND.to_string()
    } else {
        kept
    }
}

/// Bound a durable `plugin` row at the log boundary.
///
/// The writer runs this on every ingress path, so the cap holds for
/// a row built through the variant literal as well as through
/// [`SessionEvent::plugin`]: variant fields are public, and a bound
/// that only a constructor enforces is advisory. Idempotent — a row
/// this has already bounded is left alone (its payload is a string
/// whose literal fits, so the size check passes).
pub(crate) fn bound_plugin_row(mut row: Value) -> Value {
    let Some(object) = row.as_object_mut() else {
        return row;
    };
    if object.get("type").and_then(Value::as_str) != Some("plugin") {
        return row;
    }
    let payload = object.remove("payload").unwrap_or(Value::Null);
    let (payload, original_bytes) = bound_plugin_payload(payload);
    object.insert("payload".to_string(), payload);
    if let Some(original) = original_bytes {
        object.insert("original_bytes".to_string(), Value::from(original));
    }
    let kind = match object.remove("kind") {
        Some(Value::String(kind)) => kind,
        // A row with no kind is a row a consumer cannot filter: give
        // it the degenerate name rather than leaving the slot open.
        _ => String::new(),
    };
    object.insert("kind".to_string(), Value::String(bound_plugin_kind(kind)));
    row
}

impl SessionEvent {
    /// Build a [`SessionEvent::Plugin`] row with a bounded payload.
    ///
    /// The writer enforces the same bound on every append path, so
    /// this is the ergonomic entry point rather than the enforcement
    /// point. A payload over [`MAX_PLUGIN_PAYLOAD_BYTES`] (encoded)
    /// becomes a JSON string of its leading bytes, and the size it
    /// had lands in `original_bytes`.
    #[must_use]
    pub fn plugin(
        seq: u64,
        ts: impl Into<String>,
        kind: impl Into<String>,
        payload: Value,
    ) -> Self {
        let (payload, original_bytes) = bound_plugin_payload(payload);
        Self::Plugin {
            seq,
            ts: ts.into(),
            kind: bound_plugin_kind(kind.into()),
            payload,
            original_bytes,
        }
    }

    /// Sequence ordinal stamped by the sink.
    #[must_use]
    pub const fn seq(&self) -> u64 {
        match self {
            Self::UserMessage { seq, .. }
            | Self::AssistantMessage { seq, .. }
            | Self::AssistantChunk { seq, .. }
            | Self::ToolResult { seq, .. }
            | Self::Compaction { seq, .. }
            | Self::CompactionStart { seq, .. }
            | Self::CompactionSummary { seq, .. }
            | Self::CompactionEnd { seq, .. }
            | Self::ToolCall { seq, .. }
            | Self::Step { seq, .. }
            | Self::Turn { seq, .. }
            | Self::Iteration { seq, .. }
            | Self::Warning { seq, .. }
            | Self::SteeringGuard { seq, .. }
            | Self::SteeringHint { seq, .. }
            | Self::HookBlock { seq, .. }
            | Self::SubagentEnter { seq, .. }
            | Self::SubagentExit { seq, .. }
            | Self::RequestHeader { seq, .. }
            | Self::ApprovalAsked { seq, .. }
            | Self::ApprovalDecided { seq, .. }
            | Self::ApprovalGranted { seq, .. }
            | Self::ToolDispatch { seq, .. }
            | Self::RequestSnapshot { seq, .. }
            | Self::LifecycleShutdown { seq, .. }
            | Self::ToolResultTransformed { seq, .. }
            | Self::ParkedPrompt { seq, .. }
            | Self::ParkConsumed { seq, .. }
            | Self::Plugin { seq, .. }
            | Self::SandboxMode { seq, .. }
            | Self::Usage { seq, .. } => *seq,
        }
    }

    /// True when the event can carry a `surface_op` and enter the
    /// replay view.
    #[must_use]
    pub const fn is_surface_eligible(&self) -> bool {
        matches!(
            self,
            Self::UserMessage { .. }
                | Self::AssistantMessage { .. }
                | Self::ToolResult { .. }
                | Self::Compaction { .. }
        )
    }

    /// `surface_op` value if the event carries one.
    #[must_use]
    pub fn surface_op(&self) -> Option<&SurfaceOp> {
        match self {
            Self::UserMessage { surface_op, .. }
            | Self::AssistantMessage { surface_op, .. }
            | Self::ToolResult { surface_op, .. } => surface_op.as_ref(),
            Self::Compaction { surface_op, .. } => Some(surface_op),
            _ => None,
        }
    }

    /// Provider usage reported by this event, if any.
    ///
    /// [`SessionEvent::Usage`] rows carry the report either in
    /// the typed R30 `usage` field or — on pre-R30 rows — in the
    /// legacy `data` payload (`prompt_tokens` /
    /// `completion_tokens` / `cache_read_tokens` /
    /// `cache_write_tokens`). Both shapes resolve to the same
    /// disjoint [`UsageBuckets`]. Non-usage events, and usage
    /// rows without a decodable report, return `None`.
    #[must_use]
    pub fn provider_usage(&self) -> Option<UsageBuckets> {
        let Self::Usage { usage, data, .. } = self else {
            return None;
        };
        match usage {
            Some(buckets) => Some(*buckets),
            None => parse_legacy_usage(data),
        }
    }

    /// Policy wire name carried by a
    /// [`SessionEvent::SandboxMode`] row, if this is one.
    ///
    /// Returns the raw string, not a parsed policy: the wire layer
    /// does not know the policy enum, and an unknown mode must
    /// survive a read-back for a newer build to interpret.
    #[must_use]
    pub fn sandbox_mode(&self) -> Option<&str> {
        match self {
            Self::SandboxMode { mode, .. } => Some(mode),
            _ => None,
        }
    }

    /// Parse a [`SessionEvent`] from the opaque `serde_json::Value`
    /// produced by `SessionSink::read`. Returns `None` for legacy
    /// shapes that pre-date the typed layer (`{"role": ...}`,
    /// `{"type": "UserInput"}`, etc.) — callers handle those via
    /// the existing `events_to_messages` projection.
    #[must_use]
    pub fn from_value(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        let tag = obj.get("type")?.as_str()?;
        let known = matches!(
            tag,
            "user_message"
                | "assistant_message"
                | "assistant_chunk"
                | "tool_result"
                | "compaction"
                | "compaction_start"
                | "compaction_summary"
                | "compaction_end"
                | "tool_call"
                | "step"
                | "turn"
                | "iteration"
                | "warning"
                | "steering_guard"
                | "steering_hint"
                | "hook_block"
                | "subagent_enter"
                | "subagent_exit"
                | "request_header"
                | "approval_asked"
                | "approval_decided"
                | "approval_granted"
                | "tool_dispatch"
                | "request_snapshot"
                | "usage"
                | "sandbox_mode"
                | "session_shutdown"
                | "tool_result_transformed"
                | "parked_prompt"
                | "park_consumed"
                | "plugin"
        );
        if !known {
            return None;
        }
        serde_json::from_value(v.clone()).ok()
    }

    /// Stable wire-tag string (snake_case).
    #[must_use]
    pub const fn type_tag(&self) -> &'static str {
        match self {
            Self::UserMessage { .. } => "user_message",
            Self::AssistantMessage { .. } => "assistant_message",
            Self::AssistantChunk { .. } => "assistant_chunk",
            Self::ToolResult { .. } => "tool_result",
            Self::Compaction { .. } => "compaction",
            Self::CompactionStart { .. } => "compaction_start",
            Self::CompactionSummary { .. } => "compaction_summary",
            Self::CompactionEnd { .. } => "compaction_end",
            Self::ToolCall { .. } => "tool_call",
            Self::Step { .. } => "step",
            Self::Turn { .. } => "turn",
            Self::Iteration { .. } => "iteration",
            Self::Warning { .. } => "warning",
            Self::SteeringGuard { .. } => "steering_guard",
            Self::SteeringHint { .. } => "steering_hint",
            Self::HookBlock { .. } => "hook_block",
            Self::SubagentEnter { .. } => "subagent_enter",
            Self::SubagentExit { .. } => "subagent_exit",
            Self::RequestHeader { .. } => "request_header",
            Self::ApprovalAsked { .. } => "approval_asked",
            Self::ApprovalDecided { .. } => "approval_decided",
            Self::ApprovalGranted { .. } => "approval_granted",
            Self::ToolDispatch { .. } => "tool_dispatch",
            Self::RequestSnapshot { .. } => "request_snapshot",
            Self::Usage { .. } => "usage",
            Self::SandboxMode { .. } => "sandbox_mode",
            Self::LifecycleShutdown { .. } => "session_shutdown",
            Self::ToolResultTransformed { .. } => "tool_result_transformed",
            Self::ParkedPrompt { .. } => "parked_prompt",
            Self::ParkConsumed { .. } => "park_consumed",
            Self::Plugin { .. } => "plugin",
        }
    }
}

/// Decode the pre-R30 usage payload shape (`data` carries the
/// buckets as flat keys). Returns `None` when the required
/// `prompt_tokens` key is absent — such a row is not a sample.
fn parse_legacy_usage(data: &Value) -> Option<UsageBuckets> {
    let input_tokens = data.get("prompt_tokens")?.as_u64()?;
    let output_tokens = data
        .get("completion_tokens")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    let cache_read_tokens =
        data.get("cache_read_tokens").and_then(Value::as_u64);
    let cache_write_tokens =
        data.get("cache_write_tokens").and_then(Value::as_u64);
    Some(UsageBuckets {
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_write_tokens,
    })
}

/// Opaque correlation id for one compaction lifecycle.
///
/// Threaded through [`SessionEvent::CompactionStart`],
/// [`SessionEvent::CompactionSummary`], and
/// [`SessionEvent::CompactionEnd`] so the three can be matched
/// without the emitter inventing an id scheme. Serialises
/// transparently as the bare string.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SurfaceToken(String);

impl SurfaceToken {
    /// Mint a fresh ULID-backed token.
    #[must_use]
    pub fn new() -> Self {
        Self(ulid::Ulid::generate().to_string())
    }

    /// Wrap a token string the caller already minted — restore
    /// from a durable log, or pin a token in a test.
    #[must_use]
    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Borrow the underlying string.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for SurfaceToken {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SurfaceToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Terminal state of one compaction lifecycle.
///
/// Serialises as the `outcome` string carried by
/// [`SessionEvent::CompactionEnd`]; [`Self::Interrupted`] is the
/// state a loader synthesises for an orphaned
/// [`SessionEvent::CompactionStart`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionLifecycleOutcome {
    /// The summary was committed to the surface.
    Committed,
    /// The compaction errored; the surface is unchanged.
    Failed,
    /// The process died mid-compaction.
    Interrupted,
}

impl CompactionLifecycleOutcome {
    /// Stable wire tag (snake_case), matching the `outcome`
    /// field on [`SessionEvent::CompactionEnd`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Committed => "committed",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

/// Every typed event variant synthesia currently understands. Mirrors
/// dsh `KNOWN_SESSION_EVENT_TYPES` (the persistence layer refuses
/// to interpret a log containing a type outside this set unless
/// the row carries the legacy envelope marker).
pub const KNOWN_SESSION_EVENT_TYPES: &[&str] = &[
    "user_message",
    "assistant_message",
    "assistant_chunk",
    "tool_result",
    "compaction",
    "compaction_start",
    "compaction_summary",
    "compaction_end",
    "tool_call",
    "step",
    "turn",
    "iteration",
    "warning",
    "steering_guard",
    "steering_hint",
    "hook_block",
    "subagent_enter",
    "subagent_exit",
    "request_header",
    "approval_asked",
    "approval_decided",
    "approval_granted",
    "tool_dispatch",
    "request_snapshot",
    "usage",
    "sandbox_mode",
    "session_shutdown",
    "tool_result_transformed",
    "parked_prompt",
    "park_consumed",
    "plugin",
];

/// Build an empty event skeleton for one of the typed `type_tag`
/// strings. Returns `None` for any string outside
/// [`KNOWN_SESSION_EVENT_TYPES`]. The skeleton has zeroed `seq`,
/// an empty `ts`, a `Value::Null` `data` (an empty `mode` on the
/// `data`-less `sandbox_mode` variant), and (for
/// surface-eligible variants) the `append` `surface_op` so the
/// caller can recognise the discriminant before populating the
/// payload.
#[must_use]
pub fn empty_event_of(tag: &str) -> Option<SessionEvent> {
    let event = match tag {
        "user_message" => SessionEvent::UserMessage {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
            source: None,
            surface_op: Some(SurfaceOp::AppendString("append".to_string())),
        },
        "assistant_message" => SessionEvent::AssistantMessage {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
            surface_op: Some(SurfaceOp::AppendString("append".to_string())),
        },
        "assistant_chunk" => SessionEvent::AssistantChunk {
            seq: 0,
            ts: String::new(),
            chunk_seq: 0,
            finish_reason: None,
            data: Value::Null,
        },
        "tool_result" => SessionEvent::ToolResult {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
            is_error: false,
            surface_op: Some(SurfaceOp::AppendString("append".to_string())),
        },
        "compaction" => SessionEvent::Compaction {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
            surface_op: SurfaceOp::AppendString("append".to_string()),
        },
        "compaction_start" => SessionEvent::CompactionStart {
            seq: 0,
            ts: String::new(),
            token: String::new(),
            data: Value::Null,
        },
        "compaction_summary" => SessionEvent::CompactionSummary {
            seq: 0,
            ts: String::new(),
            token: String::new(),
            data: Value::Null,
        },
        "compaction_end" => SessionEvent::CompactionEnd {
            seq: 0,
            ts: String::new(),
            token: String::new(),
            outcome: String::new(),
            data: Value::Null,
        },
        "tool_call" => SessionEvent::ToolCall {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
        },
        "step" => SessionEvent::Step {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
        },
        "turn" => SessionEvent::Turn {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
        },
        "iteration" => SessionEvent::Iteration {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
        },
        "warning" => SessionEvent::Warning {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
        },
        "steering_guard" => SessionEvent::SteeringGuard {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
        },
        "steering_hint" => SessionEvent::SteeringHint {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
        },
        "hook_block" => SessionEvent::HookBlock {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
        },
        "subagent_enter" => SessionEvent::SubagentEnter {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
        },
        "subagent_exit" => SessionEvent::SubagentExit {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
        },
        "request_header" => SessionEvent::RequestHeader {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
        },
        "approval_asked" => SessionEvent::ApprovalAsked {
            seq: 0,
            ts: String::new(),
            id: String::new(),
            call_id: String::new(),
            tool: String::new(),
            arguments_hash: String::new(),
            risk: String::new(),
            agent: String::new(),
        },
        "approval_decided" => SessionEvent::ApprovalDecided {
            seq: 0,
            ts: String::new(),
            id: String::new(),
            call_id: String::new(),
            status: String::new(),
            by: String::new(),
            payload: None,
        },
        "approval_granted" => SessionEvent::ApprovalGranted {
            seq: 0,
            ts: String::new(),
            id: String::new(),
            tool: String::new(),
            scope: String::new(),
            workspace_hash: String::new(),
        },
        "tool_dispatch" => SessionEvent::ToolDispatch {
            seq: 0,
            ts: String::new(),
            call_id: String::new(),
            tool: String::new(),
            arguments_hash: String::new(),
        },
        "request_snapshot" => SessionEvent::RequestSnapshot {
            seq: 0,
            ts: String::new(),
            provider: String::new(),
            model: String::new(),
            params_hash: String::new(),
            tools_hash: String::new(),
            system_hash: String::new(),
            sections: Vec::new(),
            policy_hash: String::new(),
        },
        "usage" => SessionEvent::Usage {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
            usage: None,
        },
        "sandbox_mode" => SessionEvent::SandboxMode {
            seq: 0,
            ts: String::new(),
            mode: String::new(),
        },
        "session_shutdown" => SessionEvent::LifecycleShutdown {
            seq: 0,
            ts: String::new(),
            data: Value::Null,
        },
        "tool_result_transformed" => SessionEvent::ToolResultTransformed {
            seq: 0,
            ts: String::new(),
            transformers: Vec::new(),
            tool_name: String::new(),
            call_id: String::new(),
            before: String::new(),
            after: String::new(),
        },
        "parked_prompt" => SessionEvent::ParkedPrompt {
            seq: 0,
            ts: String::new(),
            park_id: String::new(),
            text: String::new(),
            attachments: Vec::new(),
            agent_name: None,
            rerun: false,
        },
        "park_consumed" => SessionEvent::ParkConsumed {
            seq: 0,
            ts: String::new(),
            park_id: String::new(),
        },
        "plugin" => SessionEvent::Plugin {
            seq: 0,
            ts: String::new(),
            kind: String::new(),
            payload: Value::Null,
            original_bytes: None,
        },
        _ => return None,
    };
    Some(event)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    /// Non-`data` fields a tag requires beyond `seq`/`ts`.
    fn extra_fields(tag: &str) -> Value {
        match tag {
            "assistant_chunk" => json!({"chunk_seq": 7}),
            "compaction_start" | "compaction_summary" => {
                json!({"token": "t1"})
            }
            "compaction_end" => json!({"token": "t1", "outcome": "committed"}),
            "sandbox_mode" => json!({"mode": "workspace-write"}),
            "approval_asked" => json!({
                "id": "a1", "call_id": "c1", "tool": "shell",
                "arguments_hash": "sha256:x", "risk": "high",
                "agent": "default",
            }),
            "approval_decided" => json!({
                "id": "a1", "call_id": "c1", "status": "approved",
                "by": "resume",
            }),
            "approval_granted" => json!({
                "id": "a1", "tool": "shell", "scope": "session",
                "workspace_hash": "sha256:w",
            }),
            "tool_dispatch" => json!({
                "call_id": "c1", "tool": "shell",
                "arguments_hash": "sha256:x",
            }),
            "tool_result_transformed" => json!({
                "transformers": ["eraser"], "tool_name": "read",
                "call_id": "c1", "before": "sha256:a",
                "after": "sha256:b",
            }),
            "parked_prompt" => json!({
                "park_id": "park-1", "text": "queued",
            }),
            "park_consumed" => json!({"park_id": "park-1"}),
            "plugin" => json!({
                "kind": "acme.span", "payload": {"span": "s1"},
            }),
            "request_snapshot" => json!({
                "provider": "fake", "model": "fake-1",
                "params_hash": "p", "tools_hash": "t",
                "system_hash": "s", "sections": ["identity"],
                "policy_hash": "pol",
            }),
            _ => json!({}),
        }
    }

    fn event_json(tag: &str, seq: u64, with_surface_op: bool) -> Value {
        let mut v = json!({
            "seq": seq,
            "ts": "2026-09-10T00:00:00Z",
            "type": tag,
            "data": {"text": "hello"},
        });
        let obj = v.as_object_mut().unwrap();
        for (key, value) in extra_fields(tag).as_object().unwrap() {
            obj.insert(key.clone(), value.clone());
        }
        if with_surface_op {
            obj.insert("surface_op".to_string(), json!("append"));
        }
        v
    }

    #[test]
    fn parses_user_message_with_surface_op() {
        let v = event_json("user_message", 0, true);
        let ev = SessionEvent::from_value(&v).expect("parses");
        assert!(ev.is_surface_eligible());
        assert_eq!(ev.seq(), 0);
        assert!(ev.surface_op().unwrap().is_append());
        assert_eq!(ev.type_tag(), "user_message");
    }

    #[test]
    fn parses_assistant_message_with_replace_op() {
        let v = json!({
            "seq": 7,
            "ts": "2026-09-10T00:00:00Z",
            "type": "assistant_message",
            "data": {"text": "summary"},
            "surface_op": {
                "start": 1,
                "end": 5,
                "source_event_seqs": [3, 4],
            }
        });
        let ev = SessionEvent::from_value(&v).expect("parses");
        let op = ev.surface_op().expect("has surface_op");
        assert!(op.is_replace());
        let r = op.as_replace().unwrap();
        assert_eq!(r.start, 1);
        assert_eq!(r.end, 5);
        assert_eq!(r.source_event_seqs, &[3, 4]);
    }

    #[test]
    fn parses_compaction_with_surface_op() {
        let v = json!({
            "seq": 12,
            "ts": "2026-09-10T00:00:00Z",
            "type": "compaction",
            "data": {"text": "summary"},
            "surface_op": {
                "start": 0,
                "end": 4,
                "source_event_seqs": [1, 2, 3, 4]
            }
        });
        let ev = SessionEvent::from_value(&v).expect("parses");
        assert!(ev.is_surface_eligible());
        assert!(ev.surface_op().unwrap().is_replace());
    }

    #[test]
    fn log_only_events_carry_no_surface_op() {
        let log_only = [
            "assistant_chunk",
            "compaction_start",
            "compaction_summary",
            "compaction_end",
            "tool_call",
            "step",
            "turn",
            "iteration",
            "warning",
            "steering_guard",
            "steering_hint",
            "hook_block",
            "subagent_enter",
            "subagent_exit",
            "request_header",
            "approval_asked",
            "approval_decided",
            "approval_granted",
            "tool_dispatch",
            "request_snapshot",
            "usage",
            "sandbox_mode",
            "session_shutdown",
            "tool_result_transformed",
            "parked_prompt",
            "park_consumed",
            "plugin",
        ];
        for tag in log_only {
            let v = event_json(tag, 99, false);
            let ev = SessionEvent::from_value(&v)
                .unwrap_or_else(|| panic!("{tag} should parse"));
            assert!(!ev.is_surface_eligible(), "{tag} should not be surface");
            assert!(ev.surface_op().is_none(), "{tag} should carry no op");
        }
    }

    #[test]
    fn a_plugin_payload_under_the_cap_is_kept_verbatim() {
        let payload = json!({"span": "s1", "attrs": {"attempt": 1}});
        let ev = SessionEvent::plugin(
            5,
            "2026-10-26T00:00:00Z",
            "acme.span",
            payload.clone(),
        );
        let SessionEvent::Plugin {
            kind,
            payload: kept,
            original_bytes,
            ..
        } = &ev
        else {
            panic!("a plugin row, got {ev:?}");
        };
        assert_eq!(kind, "acme.span");
        assert_eq!(kept, &payload, "under the cap the payload is untouched");
        assert_eq!(*original_bytes, None);
        assert_eq!(ev.type_tag(), "plugin");
        assert!(!ev.is_surface_eligible());
        assert!(ev.surface_op().is_none());
    }

    #[test]
    fn a_plugin_payload_over_the_cap_is_cut_to_its_encoded_budget() {
        // Quote-dense filler: the kept text is stored as a JSON
        // string and escaped a second time when the row is written,
        // so a raw-length cap would let the row carry ~1.5× the
        // constant it promises.
        let filler = "\"".repeat(MAX_PLUGIN_PAYLOAD_BYTES);
        let ev = SessionEvent::plugin(
            6,
            "2026-10-26T00:00:01Z",
            "acme.blob",
            json!({"blob": filler}),
        );
        let SessionEvent::Plugin {
            payload,
            original_bytes,
            ..
        } = &ev
        else {
            panic!("a plugin row, got {ev:?}");
        };
        let kept = payload
            .as_str()
            .expect("a truncated payload is a JSON string");
        // What the row actually carries is the escaped literal.
        let stored = serde_json::to_string(payload).expect("serialises");
        assert!(
            stored.len() <= MAX_PLUGIN_PAYLOAD_BYTES,
            "the row carries at most the cap, got {}",
            stored.len()
        );
        assert!(
            original_bytes.is_some_and(|n| n > MAX_PLUGIN_PAYLOAD_BYTES as u64),
            "the pre-cap size is recorded: {original_bytes:?}"
        );
        // Valid UTF-8 is the log's contract: the cut backs off to a
        // boundary rather than splitting a char.
        assert!(std::str::from_utf8(kept.as_bytes()).is_ok());

        // The record survives the wire with its truncation marker.
        let written = serde_json::to_value(&ev).expect("the row serialises");
        assert_eq!(
            written["original_bytes"],
            json!(original_bytes.expect("recorded"))
        );
        let replayed = SessionEvent::from_value(&written).expect("replays");
        assert_eq!(replayed.seq(), 6);
        assert_eq!(replayed.type_tag(), "plugin");
    }

    #[test]
    fn a_plugin_kind_is_capped_and_an_empty_one_is_named() {
        let long = "k".repeat(MAX_PLUGIN_KIND_BYTES * 4);
        let ev = SessionEvent::plugin(7, "t", long, Value::Null);
        let SessionEvent::Plugin { kind, .. } = &ev else {
            panic!("a plugin row, got {ev:?}");
        };
        let stored = serde_json::to_string(kind).expect("the kind serialises");
        assert!(
            stored.len() <= MAX_PLUGIN_KIND_BYTES,
            "a kind is a filter key, not a body: {} bytes",
            stored.len()
        );

        let empty = SessionEvent::plugin(8, "t", "", Value::Null);
        let SessionEvent::Plugin { kind, .. } = &empty else {
            panic!("a plugin row, got {empty:?}");
        };
        assert_eq!(
            kind, "unnamed",
            "an empty filter key is indistinguishable from a missing one"
        );
    }

    #[test]
    fn bounding_a_row_twice_is_a_no_op() {
        // `append_copied` re-spills a stamped row that already went
        // through `enqueue`, so the bound runs over its own output.
        // If the second pass re-cut, a bounded row would lose bytes
        // every time it was re-spilled, and `original_bytes` would
        // drift off the size the plugin actually sent.
        let filler = "\"".repeat(MAX_PLUGIN_PAYLOAD_BYTES);
        let row = serde_json::to_value(SessionEvent::plugin(
            9,
            "2026-10-26T00:00:02Z",
            "k".repeat(MAX_PLUGIN_KIND_BYTES * 4),
            json!({"blob": filler}),
        ))
        .expect("the row serialises");

        let once = bound_plugin_row(row);
        let twice = bound_plugin_row(once.clone());
        assert_eq!(twice, once);
    }

    #[test]
    fn rejects_unknown_legacy_shape() {
        let legacy = json!({"role": "user", "text": "hi"});
        assert!(SessionEvent::from_value(&legacy).is_none());
        let legacy2 = json!({"type": "UserInput", "data": {"text": "go"}});
        assert!(SessionEvent::from_value(&legacy2).is_none());
    }

    #[test]
    fn round_trips_through_json() {
        let v = json!({
            "seq": 3,
            "ts": "2026-09-10T00:00:00Z",
            "type": "tool_result",
            "data": {"call_id": "c1", "content": "ok"},
            "surface_op": "append"
        });
        let ev = SessionEvent::from_value(&v).unwrap();
        let s = serde_json::to_string(&ev).unwrap();
        let back = SessionEvent::from_value(
            &serde_json::from_str::<Value>(&s).unwrap(),
        )
        .expect("round-trip");
        assert_eq!(back.seq(), 3);
        assert!(back.is_surface_eligible());
        assert!(back.surface_op().unwrap().is_append());
    }

    #[test]
    fn a_tool_result_row_carries_its_failure_flag() {
        let failed = json!({
            "seq": 4,
            "ts": "2026-09-10T00:00:00Z",
            "type": "tool_result",
            "data": {"role": "tool", "tool_call_id": "c1", "content": "boom"},
            "is_error": true,
        });
        let ev = SessionEvent::from_value(&failed).expect("the row parses");
        assert!(
            matches!(ev, SessionEvent::ToolResult { is_error: true, .. }),
            "the flag survives the parse: {ev:?}"
        );
        let written = serde_json::to_value(&ev).expect("the row serialises");
        assert_eq!(written["is_error"], json!(true));
        // The flag is beside the message, not inside it: the payload
        // stays exactly what the loop put into the transcript.
        assert_eq!(written["data"]["role"], json!("tool"));
        assert_eq!(written["data"]["tool_call_id"], json!("c1"));
    }

    #[test]
    fn a_tool_result_row_without_a_failure_keeps_todays_wire_shape() {
        let row = json!({
            "seq": 4,
            "ts": "2026-09-10T00:00:00Z",
            "type": "tool_result",
            "data": {"role": "tool", "tool_call_id": "c1", "content": "ok"},
        });
        let ev = SessionEvent::from_value(&row).expect("the row parses");
        assert!(
            matches!(
                ev,
                SessionEvent::ToolResult {
                    is_error: false,
                    ..
                }
            ),
            "a row without the flag is a tool that returned: {ev:?}"
        );
        let written = serde_json::to_value(&ev).expect("the row serialises");
        assert!(
            written.get("is_error").is_none(),
            "a tool that returned keeps the wire it had: {written}"
        );
    }

    /// A `user_message` row written before `source` existed has no
    /// such key, and must keep deserialising: the field is an
    /// annotation on top of the wire, not a new requirement.
    #[test]
    fn an_old_user_row_without_source_still_deserializes() {
        let row = json!({
            "type": "user_message",
            "seq": 1,
            "ts": "2026-10-25T00:00:00Z",
            "data": {"role": "user", "content": "hi"},
            "surface_op": "append",
        });
        let event: SessionEvent =
            serde_json::from_value(row).expect("the old row parses");
        match event {
            SessionEvent::UserMessage { source, .. } => {
                assert!(source.is_none(), "no source means the ordinary turn")
            }
            other => panic!("expected a user_message row, got {other:?}"),
        }
    }

    /// The steer row's source survives the wire, and an ordinary row
    /// keeps the shape it always had — no `"source": null` on every
    /// user turn.
    #[test]
    fn a_user_row_carries_its_source_only_when_it_has_one() {
        let steer = SessionEvent::UserMessage {
            seq: 3,
            ts: "2026-10-25T00:00:00Z".to_string(),
            data: json!({"role": "user", "content": "focus on the tests"}),
            source: Some("steer".to_string()),
            surface_op: None,
        };
        let written = serde_json::to_value(&steer).expect("serialises");
        assert_eq!(written["source"], json!("steer"));

        let ordinary = SessionEvent::UserMessage {
            seq: 4,
            ts: "2026-10-25T00:00:00Z".to_string(),
            data: json!({"role": "user", "content": "hi"}),
            source: None,
            surface_op: None,
        };
        let written = serde_json::to_value(&ordinary).expect("serialises");
        assert!(
            written.get("source").is_none(),
            "an ordinary turn keeps its shape: {written}"
        );
    }

    #[test]
    fn surface_op_default_is_append() {
        assert!(SurfaceOp::default().is_append());
    }

    #[test]
    fn seq_accessor_returns_payload_value() {
        let v = event_json("step", 42, false);
        let ev = SessionEvent::from_value(&v).unwrap();
        assert_eq!(ev.seq(), 42);
    }

    #[test]
    fn append_helper_round_trips_through_string_form() {
        let op = SurfaceOp::append();
        let s = serde_json::to_string(&op).unwrap();
        assert_eq!(s, "\"append\"");
        let parsed: SurfaceOp = serde_json::from_str(&s).unwrap();
        assert!(parsed.is_append());
    }

    #[test]
    fn compaction_event_requires_surface_op() {
        let v = json!({
            "seq": 1,
            "ts": "2026-09-10T00:00:00Z",
            "type": "compaction",
            "data": {"text": "summary"},
        });
        // Compaction always carries a surface op; missing one
        // MUST fail deserialization.
        assert!(SessionEvent::from_value(&v).is_none());
    }

    #[test]
    fn known_session_event_types_matches_type_tag_arm_count() {
        // Every wire tag in `KNOWN_SESSION_EVENT_TYPES` must round-trip
        // through `empty_event_of` -> `type_tag`. The round trip must
        // be stable across the wire alphabet.
        for tag in KNOWN_SESSION_EVENT_TYPES {
            let event = empty_event_of(tag).expect(tag);
            assert_eq!(event.type_tag(), *tag);
        }
    }

    #[test]
    fn known_session_event_types_round_trips_via_serde() {
        // Each skeleton must serialise + deserialise without losing
        // its type discriminant; downstream consumers may persist
        // the skeleton and expect the shape to round-trip.
        for tag in KNOWN_SESSION_EVENT_TYPES {
            let event = empty_event_of(tag).expect(tag);
            let json = serde_json::to_value(&event).unwrap();
            assert_eq!(json["type"].as_str(), Some(*tag));
            let restored: SessionEvent = serde_json::from_value(json).unwrap();
            assert_eq!(restored.type_tag(), *tag);
        }
    }

    #[test]
    fn empty_event_of_returns_none_for_unknown_tag() {
        assert!(empty_event_of("not_a_real_event").is_none());
        assert!(empty_event_of("").is_none());
    }

    #[test]
    fn known_session_event_types_has_one_entry_per_variant() {
        // The const is the public contract; every variant of
        // `SessionEvent` must appear exactly once. This catches
        // the case where a new variant is added but the const
        // is forgotten.
        let mut seen = std::collections::HashSet::new();
        for tag in KNOWN_SESSION_EVENT_TYPES {
            assert!(
                seen.insert(*tag),
                "duplicate tag in KNOWN_SESSION_EVENT_TYPES: {tag}"
            );
        }
        assert_eq!(
            seen.len(),
            31,
            "expected 31 typed event variants; add the new one to KNOWN_SESSION_EVENT_TYPES"
        );
    }

    #[test]
    fn assistant_chunk_round_trips_and_omits_finish_reason_when_none() {
        let v = json!({
            "seq": 4,
            "ts": "2026-09-10T00:00:00Z",
            "type": "assistant_chunk",
            "chunk_seq": 2,
            "data": {"delta": "he"},
        });
        let ev = SessionEvent::from_value(&v).expect("parses");
        assert_eq!(ev.seq(), 4);
        assert_eq!(ev.type_tag(), "assistant_chunk");
        assert!(!ev.is_surface_eligible());
        let SessionEvent::AssistantChunk {
            chunk_seq,
            finish_reason,
            ..
        } = &ev
        else {
            panic!("wrong variant");
        };
        assert_eq!(*chunk_seq, 2);
        assert!(finish_reason.is_none());

        // `None` is omitted from the wire rather than written as
        // `null`, so a non-terminal chunk stays shape-stable.
        let s = serde_json::to_string(&ev).unwrap();
        assert!(!s.contains("finish_reason"), "got {s}");
        let back: SessionEvent = serde_json::from_str(&s).unwrap();
        assert_eq!(back.type_tag(), "assistant_chunk");

        // A terminal chunk carries the provider stop reason.
        let terminal = json!({
            "seq": 5,
            "ts": "2026-09-10T00:00:00Z",
            "type": "assistant_chunk",
            "chunk_seq": 3,
            "finish_reason": "end_turn",
            "data": {"delta": ""},
        });
        let ev = SessionEvent::from_value(&terminal).expect("parses");
        let SessionEvent::AssistantChunk { finish_reason, .. } = &ev else {
            panic!("wrong variant");
        };
        assert_eq!(finish_reason.as_deref(), Some("end_turn"));
        let s = serde_json::to_string(&ev).unwrap();
        assert!(s.contains("\"finish_reason\":\"end_turn\""), "got {s}");
    }

    #[test]
    fn lifecycle_shutdown_round_trips() {
        let v = json!({
            "seq": 11,
            "ts": "2026-09-10T00:00:00Z",
            "type": "session_shutdown",
            "data": {"reason": "controller_close", "children": 2},
        });
        let ev = SessionEvent::from_value(&v).expect("parses");
        assert_eq!(ev.type_tag(), "session_shutdown");
        assert_eq!(ev.seq(), 11);
        assert!(!ev.is_surface_eligible());
        assert!(ev.surface_op().is_none());

        let s = serde_json::to_string(&ev).unwrap();
        let restored = SessionEvent::from_value(
            &serde_json::from_str::<Value>(&s).unwrap(),
        )
        .expect("round-trip");
        assert_eq!(restored.type_tag(), "session_shutdown");
        assert_eq!(restored.seq(), 11);
    }

    /// R31: the sandbox-mode override rides the log as one
    /// log-only row. A row written by a newer build — a mode this
    /// build does not know — must still parse, because the policy
    /// fold ignores what it cannot interpret rather than failing
    /// the log.
    #[test]
    fn sandbox_mode_row_parses_and_preserves_unknown_modes() {
        let ev =
            SessionEvent::from_value(&event_json("sandbox_mode", 9, false))
                .expect("parses");
        assert_eq!(ev.type_tag(), "sandbox_mode");
        assert_eq!(ev.seq(), 9);
        assert_eq!(ev.sandbox_mode(), Some("workspace-write"));
        assert!(!ev.is_surface_eligible());
        assert!(ev.surface_op().is_none());

        let unknown = json!({
            "seq": 10,
            "ts": "2026-09-12T00:00:00Z",
            "type": "sandbox_mode",
            "mode": "future-mode",
        });
        let ev = SessionEvent::from_value(&unknown).expect("parses");
        assert_eq!(ev.sandbox_mode(), Some("future-mode"));

        // A row that predates the variant never misreads as one.
        let legacy = SessionEvent::from_value(&event_json("warning", 1, false))
            .expect("parses");
        assert!(legacy.sandbox_mode().is_none());
    }

    #[test]
    fn compaction_outcome_serialises_snake_case() {
        for (outcome, wire) in [
            (CompactionLifecycleOutcome::Committed, "committed"),
            (CompactionLifecycleOutcome::Failed, "failed"),
            (CompactionLifecycleOutcome::Interrupted, "interrupted"),
        ] {
            assert_eq!(
                serde_json::to_string(&outcome).unwrap(),
                format!("\"{wire}\"")
            );
            let back: CompactionLifecycleOutcome =
                serde_json::from_str(&format!("\"{wire}\"")).unwrap();
            assert_eq!(back, outcome);
            assert_eq!(outcome.as_str(), wire);
        }
        assert!(
            serde_json::from_str::<CompactionLifecycleOutcome>("\"bogus\"")
                .is_err()
        );
    }

    #[test]
    fn surface_token_mints_ulids_and_serialises_transparently() {
        let minted = SurfaceToken::new();
        assert_eq!(minted.as_str().len(), 26, "ULID text is 26 chars");
        assert_eq!(minted.to_string(), minted.as_str());
        assert_eq!(
            serde_json::to_string(&minted).unwrap(),
            format!("\"{minted}\"")
        );

        let pinned = SurfaceToken::from_string("t1");
        assert_eq!(serde_json::to_string(&pinned).unwrap(), "\"t1\"");
        let back: SurfaceToken = serde_json::from_str("\"t1\"").unwrap();
        assert_eq!(back, pinned);
        assert_eq!(
            SurfaceToken::from_string(String::from("t1")),
            pinned,
            "Eq spans both `Into<String>` forms"
        );
        let mut set = std::collections::HashSet::new();
        assert!(set.insert(pinned.clone()));
        assert!(set.contains(&back));
        assert!(set.insert(SurfaceToken::default()));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn pre_r30_usage_row_parses_without_typed_field() {
        // Old-session payload: no `usage` key at all. The typed
        // field MUST default to `None` and stay out of the
        // serialised form.
        let row = json!({
            "type": "usage",
            "seq": 4,
            "ts": "2026-09-10T00:00:00Z",
            "data": {
                "prompt_tokens": 900,
                "completion_tokens": 60,
                "total_tokens": 960,
                "cache_read_tokens": 128,
                "cache_write_tokens": 32
            }
        });
        let event = SessionEvent::from_value(&row).expect("old row parses");
        assert_eq!(
            event.provider_usage(),
            Some(UsageBuckets {
                input_tokens: 900,
                output_tokens: 60,
                cache_read_tokens: Some(128),
                cache_write_tokens: Some(32),
            })
        );
        let round = serde_json::to_value(&event).unwrap();
        assert!(round.get("usage").is_none(), "None field is skipped");
    }

    #[test]
    fn typed_usage_field_wins_over_legacy_data() {
        let row = json!({
            "type": "usage",
            "seq": 5,
            "ts": "2026-09-10T00:00:00Z",
            "data": {"prompt_tokens": 1, "completion_tokens": 1},
            "usage": {
                "input_tokens": 700,
                "output_tokens": 40,
                "cache_read_tokens": 12
            }
        });
        let event = SessionEvent::from_value(&row).expect("new row parses");
        let buckets = event.provider_usage().expect("report");
        assert_eq!(buckets.input_tokens, 700);
        assert_eq!(buckets.output_tokens, 40);
        assert_eq!(buckets.cache_read_tokens, Some(12));
        assert_eq!(buckets.cache_write_tokens, None);
    }

    #[test]
    fn usage_row_without_a_decodable_report_has_none_provider_usage() {
        // A usage row whose data carries no `prompt_tokens` is not
        // a provider report; readers must see `None` rather than a
        // fabricated zero sample.
        let row = json!({
            "type": "usage",
            "seq": 6,
            "ts": "2026-09-10T00:00:00Z",
            "data": {"note": "no buckets"}
        });
        let event = SessionEvent::from_value(&row).expect("parses");
        assert!(event.provider_usage().is_none());
        assert!(
            SessionEvent::UserMessage {
                seq: 0,
                ts: String::new(),
                data: json!({}),
                source: None,
                surface_op: None,
            }
            .provider_usage()
            .is_none()
        );
    }

    #[test]
    fn typed_usage_field_round_trips_through_json() {
        let event = SessionEvent::Usage {
            seq: 9,
            ts: "2026-09-10T00:00:00Z".to_string(),
            data: json!({"prompt_tokens": 5}),
            usage: Some(UsageBuckets {
                input_tokens: 5,
                output_tokens: 1,
                cache_read_tokens: None,
                cache_write_tokens: Some(3),
            }),
        };
        let restored = round_trip_value(&event);
        assert_eq!(restored.provider_usage(), event.provider_usage());
        assert_eq!(restored.type_tag(), "usage");
    }

    fn round_trip_value(event: &SessionEvent) -> SessionEvent {
        let value = serde_json::to_value(event).unwrap();
        SessionEvent::from_value(&value).expect("round-trip")
    }

    /// The audit row: the log must be able to name the plugin that
    /// rewrote a tool result, and to tell the two versions apart.
    #[test]
    fn a_transformed_result_row_names_its_plugin_and_its_two_versions() {
        let row = json!({
            "type": "tool_result_transformed",
            "seq": 11,
            "ts": "2026-10-26T00:00:00Z",
            "transformers": ["eraser", "a, b"],
            "tool_name": "read",
            "call_id": "c1",
            "before": "sha256:aaaa",
            "after": "sha256:bbbb",
        });
        let event = SessionEvent::from_value(&row).expect("the row parses");
        let SessionEvent::ToolResultTransformed {
            transformers,
            tool_name,
            call_id,
            before,
            after,
            ..
        } = &event
        else {
            panic!("the row is an audit row: {event:?}");
        };
        assert_eq!(transformers, &["eraser", "a, b"]);
        assert_eq!(tool_name, "read");
        assert_eq!(call_id, "c1");
        assert_ne!(before, after, "the two versions must be distinguishable");

        // Log-only: it never joins the model's history or the surface.
        assert!(!event.is_surface_eligible());
        assert!(event.surface_op().is_none());
        assert_eq!(event.type_tag(), "tool_result_transformed");
        assert_eq!(event.seq(), 11);
        assert_eq!(
            round_trip_value(&event).type_tag(),
            "tool_result_transformed"
        );
    }
}
