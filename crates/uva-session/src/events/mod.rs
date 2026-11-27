//! Typed session event log — the durable substrate Uva uses
//! to reconstruct agent runs.
//!
//! Adopted from `deepseek-harness`'s `core/session/src/{types,
//! surface, repair}.ts`. Uva's existing `SessionSink` trait
//! stores opaque `serde_json::Value` events; this module adds a
//! *strongly-typed* overlay on top of that wire without changing
//! the trait contract.
//!
//! See [`SessionEvent`] for the typed-event enum,
//! [`SurfaceOp`] for the append / replace discriminator, and
//! [`SurfaceRole`] for what a row does on the model-visible
//! surface. What a raw row *is* — its tag, its dialect history,
//! its current form — is declared once by the row vocabulary
//! ([`RowTag`], [`restore_row`]); this module's accessors read
//! that declaration instead of restating it.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uva_provider::Message;

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
/// The field set mirrors `uva_attachment::ImageAttachmentRef` —
/// the ref the store hands back — without depending on that crate:
/// this one stays a leaf, and the log's row shape is the contract
/// both sides share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
/// What a row of the durable log does on the model-visible surface.
///
/// Which role a tag has is declared once, with the row vocabulary
/// ([`RowTag`]): the fold reads it through
/// [`SessionEvent::surface_role`], and the raw-log classifier reads
/// the same declaration, so the two cannot drift.
///
/// A tag that does not declare a role does not compile: the
/// vocabulary's match is exhaustive over the tags, so adding one
/// forces the decision at build time instead of leaving it to a
/// reader's memory.
pub enum SurfaceRole {
    /// The row contributes a model-visible message.
    ///
    /// Its payload becomes one message on the folded surface, in the
    /// order the next model pass sees.
    Message,
    /// The row carries a surface op but contributes no message.
    ///
    /// A compaction names the span it shadows and carries the
    /// summary that replaces it; the replacement is a message, but
    /// the row itself is not one.
    OpOnly,
    /// The row is log-only.
    ///
    /// It is written to the log and read back as a record of what
    /// happened; it never reaches the model-visible surface.
    LogOnly,
}

impl SurfaceRole {
    /// 这一角色进模型可见的表面吗。
    ///
    /// 表面成员资格由角色声明，所以这个判据只有一个出处：折叠、读点
    /// 与词汇读的是同一句话。
    #[must_use]
    pub const fn is_surface(self) -> bool {
        !matches!(self, SurfaceRole::LogOnly)
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
/// `uva_provider::Message`'s exact serialization (which may
/// grow new content-part variants over time).
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEvent {
    /// User-role message.
    UserMessage {
        /// Sequence ordinal stamped by the sink.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Message payload (matches `uva_provider::Message`).
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
        /// Re-readable refs to the attachments the turn carried.
        ///
        /// The bytes are never in the row — a multimodal prompt's
        /// payload lives in the deployment's attachment store, and
        /// [`AttachmentRefRow`] is the handle that resolves back to
        /// it. Without the refs a reloaded transcript can tell that
        /// a turn existed but not that it carried anything, so a
        /// file or image turn rendered as an empty bubble. A
        /// text-only turn omits the field, keeping the common row
        /// shape (and every row written before the field existed)
        /// unchanged.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        attachments: Vec<AttachmentRefRow>,
    },
    /// Assistant-role message.
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
    /// One streamed provider delta.
    ///
    /// Preserved verbatim for replay fidelity. The agent loop emits
    /// one of these per wire delta alongside the
    /// eventually-complete `AssistantMessage`. Rebuild the turn's
    /// text with [`crate::surface::assemble_chunks`].
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
    /// Tool-result message.
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
        /// The provider call id this result answers.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool_call_id: Option<String>,
        /// The files this call changed.
        ///
        /// A tool that changes a file says so: the paths ride the
        /// row, so a client that reads the log **later** still knows
        /// what the call touched (the live diff is gone by then). The
        /// paths are absolute — the same paths the tool's own report
        /// and the protocol's diff block carry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        changed: Option<Vec<String>>,
        /// How this event joins the surface.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        surface_op: Option<SurfaceOp>,
    },
    /// Summary replacing a span of older tool-results.
    ///
    /// Used by the `SummarizingContextManager` to swap a tail of
    /// noisy tool results for one compact assistant message while
    /// keeping the durable log lossless.
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
    /// Compaction began.
    ///
    /// An unmatched `CompactionStart` (no matching
    /// `CompactionEnd`) is the crash marker the loader detects;
    /// see [`crate::repair::orphaned_compactions`].
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
    /// Compaction produced a summary.
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
    /// Compaction finished (success or failure).
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
    /// A tool call the model emitted.
    ToolCall {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Call id, tool name, raw arguments JSON.
        data: Value,
        /// The provider's call id for this call.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        /// The tool this call names.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// One partial-output chunk of a running tool call.
    ///
    /// A tool that streams (today `shell`) yields its output in
    /// chunks; the live projection renders them while the call runs,
    /// and this row is the durable copy of each chunk, so a reload
    /// replays what the client already saw instead of dropping the
    /// tool's progress with the process that produced it. The settled
    /// `tool_result` row stays the model's copy: this row never joins
    /// the model-visible surface.
    ///
    /// The writer bounds the row at the log boundary
    /// ([`MAX_TOOL_OUTPUT_BYTES`]): a chunk larger than the window
    /// keeps its tail, and `data.original_bytes` says how many output
    /// bytes the row summarized. Chunks of one call that arrive
    /// before a flush merge into one row, so a burst costs one row
    /// per flush rather than one per chunk.
    ToolOutput {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Chunk payload (`{"output": "<text>"}`, plus
        /// `original_bytes` when the tail window cut the text).
        data: Value,
        /// The provider call id the chunk belongs to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        call_id: Option<String>,
        /// The tool that produced the chunk.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tool: Option<String>,
    },
    /// Step boundary (`start` | `end`).
    Step {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// `start` or `end`, plus `turn` and `step` indices.
        data: Value,
        /// Which boundary this row is — `start` or `end`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// Which turn this step belongs to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
        /// Which step inside the turn this boundary belongs to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        step: Option<u64>,
    },
    /// Turn boundary (`start` | `end`).
    Turn {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// `start` or `end`, plus `turn` index and end reason.
        data: Value,
        /// Which boundary this row is — `start` or `end`.
        ///
        /// The same value also rides inside `data` for the raw-row
        /// readers (the TUI folds rows it never deserializes). This
        /// field is the one typed readers use; `from_value` fills it
        /// from `data` when a row predates it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kind: Option<String>,
        /// Which turn this boundary belongs to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        turn: Option<u64>,
    },
    /// Iteration boundary (`start` | `end`).
    Iteration {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// `start` or `end`, plus iteration index.
        data: Value,
    },
    /// Lifecycle warning.
    Warning {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Warning kind + message + iteration.
        data: Value,
    },
    /// Steering guard decision.
    SteeringGuard {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Guard name + action + reason.
        data: Value,
    },
    /// Steering hint injection.
    SteeringHint {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Hint name + content + injection point.
        data: Value,
    },
    /// Hook veto.
    HookBlock {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Policy name + reason + the refused call: `hook` (the
        /// registered name of the hook, gate or guard that vetoed),
        /// `reason`, `tool` and `call_id`.
        data: Value,
    },
    /// Sub-agent delegation entry.
    SubagentEnter {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Child session id + parent + depth.
        data: Value,
        /// The session the delegated child runs in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child_session_id: Option<String>,
        /// The session that delegated the child.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        parent_session_id: Option<String>,
        /// The agent the child runs as.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        agent_name: Option<String>,
        /// How deep the delegation is — the root session is `0`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        depth: Option<u64>,
    },
    /// Sub-agent delegation exit.
    SubagentExit {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Child session id + parent + end reason.
        data: Value,
        /// The session the delegated child ran in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        child_session_id: Option<String>,
        /// How deep the delegation was — the root session is `0`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        depth: Option<u64>,
        /// How the child ended.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        status: Option<String>,
    },
    /// Per-call request config snapshot.
    RequestHeader {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Epoch + provider + model + tool/system hashes.
        data: Value,
        /// The provider the request went to.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        /// The model the request named.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
    },
    /// A high-risk call held for approval.
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
    /// An operator's decision on a held call.
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
    /// A durable approval grant.
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
    /// The intent record written before a tool runs.
    ///
    /// The row is the crash marker: a dispatch with no matching
    /// `tool_result` means the side effect may have happened, so
    /// repair reports `TOOL_OUTCOME_UNKNOWN` instead of re-running
    /// the call.
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
    /// The request envelope a model call ran under.
    ///
    /// Written before the request; a resume compares the current
    /// fingerprint with this row and ends as `ConfigDrift` when
    /// they disagree.
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
    /// Per-call usage attribution.
    ///
    /// R30: the provider report is carried in two shapes. The
    /// typed `usage` field is the current wire form (serde
    /// default, so pre-R30 rows parse with `None`); the legacy
    /// `data` payload (`prompt_tokens` / `completion_tokens` /
    /// `cache_read_tokens` / `cache_write_tokens`) is still
    /// stamped so older readers keep working. Read either shape
    /// through [`SessionEvent::provider_usage`].
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
    /// Session sandbox-mode override.
    ///
    /// A policy switch (a UI control, a delegation seed, a test
    /// fixture) is recorded as one of these on the session it
    /// applies to. The log IS the store: the override survives a
    /// restart by replay, and two sessions can never see each
    /// other's state (dsh `sandbox/mode` parity). The effective
    /// policy is the last such event folded against the caller's
    /// grants and the deployment default — see
    /// `uva_tool_plugins::shell::sandbox::effective_policy`.
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
    /// Session shutdown lifecycle.
    ///
    /// Emitted by the server's `SessionController::close` after
    /// all child sessions have been signalled, so the log ends
    /// with an explicit, replayable shutdown marker.
    SessionShutdown {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Shutdown metadata (reason, child count, ...).
        data: Value,
    },
    /// A tool result a registered transformer altered.
    ///
    /// Provenance, not reconciliation: transformers run *before* the
    /// loop commits the result, so the `tool_result` row already holds
    /// the rewritten text — the log and the model agree. This row
    /// records who altered it on the way to that commit, with a digest
    /// of each side (`before` / `after`) instead of the payload.
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
    /// A prompt parked for the next run.
    ///
    /// Every queued turn parks its lossless parts on the controller
    /// — a plain text or a multimodal payload alike — and that queue
    /// is process memory. Before this row existed, a crash between
    /// the park and the run lost the turn; this row *is* the park —
    /// appended and flushed before the op returns — so a node that
    /// takes the session over reads the prompt back and re-parks it.
    ///
    /// The attachment bytes are deliberately **not** in the row: a
    /// base64 image would bloat the log and no reader wants it as
    /// text. Each part is stored in the attachment store first and
    /// the row carries [`AttachmentRefRow`]s, which resolve back to
    /// the bytes by hash.
    ///
    /// The row is spent by whoever takes it off the queue: the run
    /// started from it, or a cancel that drops it — both append a
    /// [`SessionEvent::ParkConsumed`] naming this row's `park_id`
    /// (the run before its own prompt row); see
    /// [`crate::repair::parked_prompts`]. The link is by id, not by
    /// order: a `UserInput` row says nothing about which park its
    /// run spent — and a log written before every turn parked holds
    /// `UserInput` rows no park preceded — so a position could not
    /// name the park it spent.
    ParkedPrompt {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// Handle the consumer quotes back.
        ///
        /// Unique within the log (minted when the park is made, and
        /// the same id whether or not its append reported success — a
        /// row that landed anyway is still spent under the same id). A
        /// row written before the link existed omits it; the fold
        /// warns once and skips such a row rather than guess which
        /// park it was.
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
        /// The provider the turn was submitted under, when the client
        /// selected one. A takeover resolves it by name before the
        /// recovered turn runs: the selection is a fact about the
        /// turn, not about the node that parked it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        provider: Option<String>,
        /// The model the turn was submitted under, when the client
        /// selected one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        /// `true` when parked by a `Rerun`: the prompt row that turn
        /// eventually writes replaces the turn it shadows, so the
        /// flag has to survive the park.
        #[serde(default, skip_serializing_if = "is_false")]
        rerun: bool,
    },
    /// A parked prompt left the queue.
    ///
    /// The durable half of the park/consumer link. The run started
    /// from a park writes it before that run's own prompt row, and a
    /// cancel that drops a queued park writes one per dropped turn,
    /// so the pair is in the log whatever happens next.
    /// `parked_prompts` folds the pairs by [`Self::ParkedPrompt`]'s
    /// `park_id`: a park with this row is spent, a park without it is
    /// still queued — so a prompt whose row is intact is never dropped
    /// because some *other* turn consumed a row by position, and a
    /// consumer always spends exactly the park it names.
    ///
    /// The model history, the folded surface and `fold` ignore it,
    /// and it carries no `surface_op`.
    ParkConsumed {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// The `park_id` of the [`SessionEvent::ParkedPrompt`] this
        /// consumer took.
        park_id: String,
    },
    /// A user's rating of one message.
    ///
    /// The controller appends this row when the client sends
    /// feedback on an assistant message (thumbs up / down). Nothing
    /// in the runtime reads it back: the row is the audit record an
    /// analytics surface aggregates later, so it joins the log
    /// rather than a side channel — one ordinal sequence, one
    /// durability story, and a replay that shows the rating beside
    /// the answer it is about.
    ///
    /// The rating names the row it is about by that row's
    /// **ordinal** — the `seq` the writer stamped on it, in decimal.
    /// The server resolves the ordinal against the session's own log
    /// before the row is written, so a rating that names no assistant
    /// message never reaches the log.
    ///
    /// The model history, the folded surface and `fold` ignore it,
    /// and it carries no `surface_op`.
    Feedback {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// The ordinal of the row the rating is about — the rated
        /// assistant message's `seq`, in decimal.
        ///
        /// The field keeps its name for the log's readers: the log
        /// format's version gate refuses an old log rather than
        /// migrating it, so renaming the field would make every
        /// session written before this change unreadable. What
        /// changed is what it holds — a row's ordinal, not a
        /// client-side label.
        message_id: String,
        /// `true` for a thumbs-up, `false` for a thumbs-down.
        thumbs_up: bool,
    },
    /// The session's durable goal: the objective the agent keeps
    /// working toward across turns.
    ///
    /// One row per change; the latest row is the goal's state (the
    /// same "the log is the truth" rule the approval rows follow).
    /// dsh's `tool-goal` and omp's goal mode are the reference
    /// shape: a model-created objective with a lifecycle, a token
    /// budget, and a host that can pause it.
    Goal {
        /// Sequence ordinal.
        seq: u64,
        /// ISO-8601 timestamp.
        ts: String,
        /// `"active"` | `"paused"` | `"completed"` | `"dropped"`.
        state: String,
        /// The objective, in the model's own words (empty once the
        /// goal is dropped).
        objective: String,
        /// The token budget the goal was created with, when one was
        /// given.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        budget_tokens: Option<u64>,
    },
    /// A plugin's own domain event.
    ///
    /// The escape hatch for a plugin that publishes a fact the
    /// harness has no typed variant for: the sink appends it to the
    /// durable log, so it survives a crash and replays with the rest
    /// of the row stream. A plugin payload is opaque to the loop,
    /// so the row never enters the model history, the folded
    /// surface, or `fold`. The live-only
    /// counterpart is `uva_harness::SystemEvent::Plugin`: a
    /// notification that may be dropped under backpressure. This
    /// row is the record; that one is the ping.
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

/// Cap on what one [`SessionEvent::ToolOutput`] row costs the log, in
/// bytes of the chunk text it carries.
///
/// A streaming tool yields its output as it arrives, and nothing
/// upstream bounds one yield: a single pipe read can be megabytes when
/// the process on the other end dumps a buffer. The row never reaches
/// the surface, so compaction never shields it — an unbounded chunk
/// is permanent bytes in the file. The cap keeps the **tail** window:
/// a reload wants the newest output, and a head-truncated row would
/// replay the least interesting bytes of a captured stream.
///
/// Measured like the plugin cap, on the escaped form, because that is
/// what the row stores.
pub const MAX_TOOL_OUTPUT_BYTES: usize = 64 * 1024;

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

/// Cut `text` to its **last** `max_bytes` bytes (JSON-literal cost),
/// on a char boundary.
fn cap_json_string_tail(text: &str, max_bytes: usize) -> String {
    if json_string_len(text) <= max_bytes {
        return text.to_string();
    }
    // A suffix's cost is monotone in where it starts — dropping
    // leading chars only removes their escaped cost — so a bisect
    // over char boundaries finds the longest suffix that fits
    // (`""` costs two bytes, hence the `0`-suffix floor).
    let starts: Vec<usize> =
        text.char_indices().map(|(index, _)| index).collect();
    let (mut lo, mut hi) = (0usize, starts.len());
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if json_string_len(&text[starts[mid]..]) <= max_bytes {
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    let from = starts.get(lo).copied().unwrap_or(text.len());
    text[from..].to_string()
}

/// Bound one plugin payload, reporting the encoded size it had.
pub(crate) fn bound_plugin_payload(payload: Value) -> (Value, Option<u64>) {
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
pub(crate) fn bound_plugin_kind(kind: String) -> String {
    let kept = cap_json_string(&kind, MAX_PLUGIN_KIND_BYTES);
    if kept.is_empty() {
        UNNAMED_PLUGIN_KIND.to_string()
    } else {
        kept
    }
}

/// Bound one durable event at the log boundary.
///
/// The writer runs this on every typed ingress, before the one
/// serialization, so the cap holds for a row built through the
/// variant literal as well as through the constructors in
/// [`crate::typed_event_builders`]: variant fields are public, and a
/// bound that only a constructor enforces is advisory.
///
/// Returns `None` for a row that needs no bounding — the common row
/// serialises as the caller's own reference, and only the two capped
/// rows pay a copy. Both caps are idempotent — a row this has
/// already bounded comes back unchanged, so a bound that runs over
/// its own output never cuts twice or moves `original_bytes`.
pub(crate) fn bounded_event(event: &SessionEvent) -> Option<SessionEvent> {
    match event {
        SessionEvent::Plugin { .. } | SessionEvent::ToolOutput { .. } => {
            let mut bounded = event.clone();
            bound_plugin_event(&mut bounded);
            bound_tool_output_event(&mut bounded);
            Some(bounded)
        }
        _ => None,
    }
}

/// Bound a plugin event's payload and kind on the typed fields.
fn bound_plugin_event(event: &mut SessionEvent) {
    let SessionEvent::Plugin {
        kind,
        payload,
        original_bytes,
        ..
    } = event
    else {
        return;
    };
    let (bounded_payload, bounded_original) =
        bound_plugin_payload(std::mem::take(payload));
    *payload = bounded_payload;
    if let Some(original) = bounded_original {
        *original_bytes = Some(original);
    }
    *kind = bound_plugin_kind(std::mem::take(kind));
}

/// Bound a tool-output event's chunk.
///
/// The cap reaches into the payload bag by hand: `output` is
/// free-form inside `data`, with no typed field to ask — the same
/// judgment [`merge_tool_output_row`] keeps.
fn bound_tool_output_event(event: &mut SessionEvent) {
    let SessionEvent::ToolOutput { data, .. } = event else {
        return;
    };
    let Some(output) = data.get("output").and_then(Value::as_str) else {
        return;
    };
    let kept = cap_json_string_tail(output, MAX_TOOL_OUTPUT_BYTES);
    if kept.len() == output.len() {
        return;
    }
    let original = output.len() as u64;
    if let Some(bag) = data.as_object_mut() {
        bag.insert("output".to_string(), Value::String(kept));
        bag.insert("original_bytes".to_string(), Value::from(original));
    }
}

/// Append one `tool_output` chunk to a row that has not been written
/// yet, keeping the row's tail window.
///
/// The row keeps its identity (and its ordinal), so a burst of chunks
/// becomes one row instead of one row per chunk — the log and the
/// writer's memory both stay bounded by the window rather than by the
/// tool's output rate. `data.original_bytes` accumulates the bytes the
/// row has summarized, so a reader can tell a tail window from a whole
/// chunk and know how much it stands for.
pub(crate) fn merge_tool_output_row(existing: &mut Value, chunk: &Value) {
    let chunk_text = chunk
        .get("data")
        .and_then(|data| data.get("output"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let Some(object) = existing.as_object_mut() else {
        return;
    };
    let data = object
        .entry("data".to_string())
        .or_insert_with(|| serde_json::json!({}));
    let Some(data) = data.as_object_mut() else {
        return;
    };
    let prior = data
        .get("output")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let summarized = data
        .get("original_bytes")
        .and_then(Value::as_u64)
        .unwrap_or(prior.len() as u64)
        .saturating_add(chunk_text.len() as u64);
    let mut combined = prior;
    combined.push_str(&chunk_text);
    let kept = cap_json_string_tail(&combined, MAX_TOOL_OUTPUT_BYTES);
    let truncated = kept.len() < combined.len();
    data.insert("output".to_string(), Value::String(kept));
    if truncated {
        data.insert("original_bytes".to_string(), Value::from(summarized));
    } else {
        data.remove("original_bytes");
    }
}

/// A structural boundary's kind, read from the row payload: the loop
/// writes `kind`, a suspension's rows write `phase`.
///
/// The typed accessors read their typed field first and fall back here,
/// because the fields are public: a hand-built event (never normalised
/// through [`SessionEvent::from_value`]) would otherwise silently lose
/// its boundary, and `interrupted_turn_closers` would emit nothing.
fn payload_boundary(data: &Value) -> Option<&str> {
    data.get("kind")
        .and_then(Value::as_str)
        .or_else(|| data.get("phase").and_then(Value::as_str))
}

/// Complete the typed fields of a row written before the typed layer
/// existed.
///
/// This is the **read side's** leniency, and the whole of it. The
/// typed fields are the reader-facing truth — a reader asks the event
/// for a value and never spells a key — while the `data` bag still
/// carries the same keys, because the raw-row readers (the TUI folds
/// rows it never deserializes) read it there. The two views meet
/// here, once, at the seam ([`SessionEvent::from_value`]), instead of
/// at every read site.
///
/// **What it fills**: a typed field that is *empty* takes the value
/// the payload bag carries under the same name — `turn` from
/// `data.turn`, the boundary kind from `data.kind` (or `data.phase`,
/// the spelling a suspension's rows used), a tool result's call id
/// from `data.tool_call_id` or `data.call_id`.
///
/// **What it does not do**: it never overwrites a typed field that is
/// already there, and it never rewrites the payload bag. A row whose
/// two views contradict each other therefore keeps its contradiction
/// — the reader does not excuse a writer that wrote one.
///
/// A row the vocabulary writes today needs none of this: the
/// constructors in [`crate::typed_event_builders`] write the payload
/// bag and the typed fields together, so the reader has nothing to
/// complete and the row comes back byte for byte as it went in. The
/// constructors are the one owner of a row's shape, and their own
/// tests are where a shape that drifts fails.
fn normalize(mut event: SessionEvent) -> SessionEvent {
    /// The payload's `key`, as a string.
    fn string(data: &Value, key: &str) -> Option<String> {
        data.get(key).and_then(Value::as_str).map(str::to_owned)
    }
    /// The payload's `key`, as an unsigned number.
    fn number(data: &Value, key: &str) -> Option<u64> {
        data.get(key).and_then(Value::as_u64)
    }
    /// The boundary kind — the loop writes `kind`, a suspension's
    /// rows write `phase`; both spellings mark the boundary.
    fn boundary(data: &Value) -> Option<String> {
        payload_boundary(data).map(str::to_owned)
    }

    match &mut event {
        SessionEvent::Turn {
            data, kind, turn, ..
        } => {
            if kind.is_none() {
                *kind = boundary(data);
            }
            if turn.is_none() {
                *turn = number(data, "turn");
            }
        }
        SessionEvent::Step {
            data,
            kind,
            turn,
            step,
            ..
        } => {
            if kind.is_none() {
                *kind = boundary(data);
            }
            if turn.is_none() {
                *turn = number(data, "turn");
            }
            if step.is_none() {
                *step = number(data, "step");
            }
        }
        SessionEvent::ToolCall {
            data,
            call_id,
            name,
            ..
        } => {
            if call_id.is_none() {
                *call_id = string(data, "call_id");
            }
            if name.is_none() {
                *name = string(data, "name");
            }
        }
        SessionEvent::ToolResult {
            data, tool_call_id, ..
        } => {
            if tool_call_id.is_none() {
                *tool_call_id = string(data, "tool_call_id")
                    .or_else(|| string(data, "call_id"));
            }
        }
        SessionEvent::SubagentEnter {
            data,
            child_session_id,
            parent_session_id,
            agent_name,
            depth,
            ..
        } => {
            if child_session_id.is_none() {
                *child_session_id = string(data, "child_session_id");
            }
            if parent_session_id.is_none() {
                *parent_session_id = string(data, "parent_session_id");
            }
            if agent_name.is_none() {
                *agent_name = string(data, "agent_name");
            }
            if depth.is_none() {
                *depth = number(data, "depth");
            }
        }
        SessionEvent::SubagentExit {
            data,
            child_session_id,
            depth,
            status,
            ..
        } => {
            if child_session_id.is_none() {
                *child_session_id = string(data, "child_session_id");
            }
            if depth.is_none() {
                *depth = number(data, "depth");
            }
            if status.is_none() {
                *status = string(data, "status");
            }
        }
        SessionEvent::RequestHeader {
            data,
            provider,
            model,
            ..
        } => {
            if provider.is_none() {
                *provider = string(data, "provider");
            }
            if model.is_none() {
                *model = string(data, "model");
            }
        }
        _ => {}
    }
    event
}

impl SessionEvent {
    #[must_use]
    /// The model message this row carries.
    ///
    /// The row's payload read the way the vocabulary reads it: the
    /// provider's own encoding first, the hand-written
    /// `{"role": …, "content": […]}` shape second, and a payload that
    /// carries text or a call id last. The same decoding the row
    /// vocabulary uses for a current row — one implementation, so a
    /// reader of a typed event and a reader of a raw row cannot
    /// disagree about what a row carries.
    pub fn message(&self) -> Option<Message> {
        match self {
            Self::UserMessage { data, .. }
            | Self::AssistantMessage { data, .. }
            | Self::ToolResult { data, .. } => {
                crate::events::vocab::restore::decode_message(data)
            }
            Self::Compaction { data, .. } => data
                .get("summary")
                .and_then(Value::as_str)
                .map(Message::assistant),
            _ => None,
        }
    }

    #[must_use]
    /// Which boundary this row is — `start` or `end`.
    ///
    /// The loop writes `kind`; a suspension's rows write `phase`. Both
    /// spellings mark the boundary, and both land in the typed field.
    /// A row that was never normalised (a hand-built event) still
    /// answers from its payload, so the typed field and `data` cannot
    /// disagree about whether a boundary exists.
    pub fn boundary_kind(&self) -> Option<&str> {
        match self {
            Self::Step { kind, data, .. } | Self::Turn { kind, data, .. } => {
                kind.as_deref().or_else(|| payload_boundary(data))
            }
            Self::Iteration { data, .. } => payload_boundary(data),
            _ => None,
        }
    }

    #[must_use]
    /// Which turn this boundary belongs to.
    pub fn boundary_turn(&self) -> Option<u64> {
        match self {
            Self::Step { turn, .. } | Self::Turn { turn, .. } => *turn,
            _ => None,
        }
    }

    #[must_use]
    /// Which step inside the turn this boundary belongs to.
    pub fn boundary_step(&self) -> Option<u64> {
        match self {
            Self::Step { step, .. } => *step,
            _ => None,
        }
    }

    #[must_use]
    /// The provider call id this row names — a call's own id, or the
    /// id a result answers.
    pub fn tool_call_id(&self) -> Option<&str> {
        match self {
            Self::ToolCall { call_id, .. } => call_id.as_deref(),
            Self::ToolResult { tool_call_id, .. } => tool_call_id.as_deref(),
            _ => None,
        }
    }

    /// The provider call this streamed chunk belongs to — only a
    /// `tool_output` row has one. A `tool_call`'s own id is a
    /// different fact ([`Self::tool_call_id`]) and never routes a
    /// row into the writer's merge.
    #[must_use]
    pub(crate) fn tool_output_call(&self) -> Option<&str> {
        match self {
            Self::ToolOutput { call_id, .. } => call_id.as_deref(),
            _ => None,
        }
    }

    #[must_use]
    /// The tool this call names.
    pub fn tool_call_name(&self) -> Option<&str> {
        match self {
            Self::ToolCall { name, .. } => name.as_deref(),
            _ => None,
        }
    }

    #[must_use]
    /// The provider the request went to.
    pub fn request_provider(&self) -> Option<&str> {
        match self {
            Self::RequestHeader { provider, .. } => provider.as_deref(),
            _ => None,
        }
    }

    #[must_use]
    /// The model the request named.
    pub fn request_model(&self) -> Option<&str> {
        match self {
            Self::RequestHeader { model, .. } => model.as_deref(),
            _ => None,
        }
    }

    #[must_use]
    /// The session a delegated child runs in.
    pub fn delegation_child(&self) -> Option<&str> {
        match self {
            Self::SubagentEnter {
                child_session_id, ..
            }
            | Self::SubagentExit {
                child_session_id, ..
            } => child_session_id.as_deref(),
            _ => None,
        }
    }

    #[must_use]
    /// The session that delegated the child.
    pub fn delegation_parent(&self) -> Option<&str> {
        match self {
            Self::SubagentEnter {
                parent_session_id, ..
            } => parent_session_id.as_deref(),
            _ => None,
        }
    }

    #[must_use]
    /// The agent the delegated child runs as.
    pub fn delegation_agent(&self) -> Option<&str> {
        match self {
            Self::SubagentEnter { agent_name, .. } => agent_name.as_deref(),
            _ => None,
        }
    }

    #[must_use]
    /// How deep the delegation is — the root session is `0`.
    pub fn delegation_depth(&self) -> Option<u64> {
        match self {
            Self::SubagentEnter { depth, .. }
            | Self::SubagentExit { depth, .. } => *depth,
            _ => None,
        }
    }

    #[must_use]
    /// How a delegated child ended.
    pub fn delegation_status(&self) -> Option<&str> {
        match self {
            Self::SubagentExit { status, .. } => status.as_deref(),
            _ => None,
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
            | Self::ToolOutput { seq, .. }
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
            | Self::SessionShutdown { seq, .. }
            | Self::ToolResultTransformed { seq, .. }
            | Self::ParkedPrompt { seq, .. }
            | Self::ParkConsumed { seq, .. }
            | Self::Feedback { seq, .. }
            | Self::Goal { seq, .. }
            | Self::Plugin { seq, .. }
            | Self::SandboxMode { seq, .. }
            | Self::Usage { seq, .. } => *seq,
        }
    }

    #[must_use]
    /// The row-vocabulary tag of this event.
    ///
    /// The event side of the correspondence with [`RowTag`]:
    /// exhaustive over the variants (a new variant does not compile
    /// until it names its tag), with the tag side —
    /// [`RowTag::empty_event`] — tied to it by a test that chains
    /// the two, so neither match can drift a pairing.
    pub const fn tag(&self) -> RowTag {
        match self {
            Self::UserMessage { .. } => RowTag::UserMessage,
            Self::AssistantMessage { .. } => RowTag::AssistantMessage,
            Self::AssistantChunk { .. } => RowTag::AssistantChunk,
            Self::ToolResult { .. } => RowTag::ToolResult,
            Self::Compaction { .. } => RowTag::Compaction,
            Self::CompactionStart { .. } => RowTag::CompactionStart,
            Self::CompactionSummary { .. } => RowTag::CompactionSummary,
            Self::CompactionEnd { .. } => RowTag::CompactionEnd,
            Self::ToolCall { .. } => RowTag::ToolCall,
            Self::ToolOutput { .. } => RowTag::ToolOutput,
            Self::Step { .. } => RowTag::Step,
            Self::Turn { .. } => RowTag::Turn,
            Self::Iteration { .. } => RowTag::Iteration,
            Self::Warning { .. } => RowTag::Warning,
            Self::SteeringGuard { .. } => RowTag::SteeringGuard,
            Self::SteeringHint { .. } => RowTag::SteeringHint,
            Self::HookBlock { .. } => RowTag::HookBlock,
            Self::SubagentEnter { .. } => RowTag::SubagentEnter,
            Self::SubagentExit { .. } => RowTag::SubagentExit,
            Self::RequestHeader { .. } => RowTag::RequestHeader,
            Self::ApprovalAsked { .. } => RowTag::ApprovalAsked,
            Self::ApprovalDecided { .. } => RowTag::ApprovalDecided,
            Self::ApprovalGranted { .. } => RowTag::ApprovalGranted,
            Self::ToolDispatch { .. } => RowTag::ToolDispatch,
            Self::RequestSnapshot { .. } => RowTag::RequestSnapshot,
            Self::Usage { .. } => RowTag::Usage,
            Self::SandboxMode { .. } => RowTag::SandboxMode,
            Self::SessionShutdown { .. } => RowTag::SessionShutdown,
            Self::ToolResultTransformed { .. } => RowTag::ToolResultTransformed,
            Self::ParkedPrompt { .. } => RowTag::ParkedPrompt,
            Self::ParkConsumed { .. } => RowTag::ParkConsumed,
            Self::Feedback { .. } => RowTag::Feedback,
            Self::Goal { .. } => RowTag::Goal,
            Self::Plugin { .. } => RowTag::Plugin,
        }
    }

    #[must_use]
    /// The role this row plays on the model-visible surface.
    ///
    /// Declared once, on the event's [`RowTag`]; read back here.
    /// The declaration's match is exhaustive over the vocabulary, so
    /// a new variant does not compile until its role is declared —
    /// a row can neither join nor stay out of the model-visible
    /// surface by accident.
    pub const fn surface_role(&self) -> SurfaceRole {
        self.tag().surface_role()
    }

    /// True when the event can carry a `surface_op` and enter the
    /// replay view.
    ///
    /// Read off the declared [`SurfaceRole`]: a row is surface
    /// eligible when its role is [`SurfaceRole::Message`] or
    /// [`SurfaceRole::OpOnly`]. The role is the one declaration of
    /// this fact, so the fold and this predicate cannot disagree.
    #[must_use]
    pub const fn is_surface_eligible(&self) -> bool {
        self.surface_role().is_surface()
    }

    /// The payload and the surface op this row carries.
    ///
    /// One match decides both, so the two cannot drift apart: a
    /// variant sits in one group or the other, and the group says
    /// whether the row carries a payload and whether it carries an
    /// op. The match is exhaustive over the variants, so a new
    /// variant does not compile until it says which group it is in
    /// — the gap the two catch-all arms left open, where a new
    /// surface-eligible row passed the role check and was then
    /// dropped by the fold with a warning.
    fn surface_parts(&self) -> (Option<&Value>, Option<&SurfaceOp>) {
        match self {
            Self::UserMessage {
                data, surface_op, ..
            }
            | Self::AssistantMessage {
                data, surface_op, ..
            }
            | Self::ToolResult {
                data, surface_op, ..
            } => (Some(data), surface_op.as_ref()),
            Self::Compaction {
                data, surface_op, ..
            } => (Some(data), Some(surface_op)),
            // Rows the vocabulary declares log-only: neither a
            // payload nor an op. They are written to the log and read
            // back as records of what happened; they never reach the
            // model-visible surface.
            Self::AssistantChunk { .. }
            | Self::CompactionStart { .. }
            | Self::CompactionSummary { .. }
            | Self::CompactionEnd { .. }
            | Self::ToolCall { .. }
            | Self::ToolOutput { .. }
            | Self::Step { .. }
            | Self::Turn { .. }
            | Self::Iteration { .. }
            | Self::Warning { .. }
            | Self::SteeringGuard { .. }
            | Self::SteeringHint { .. }
            | Self::HookBlock { .. }
            | Self::SubagentEnter { .. }
            | Self::SubagentExit { .. }
            | Self::RequestHeader { .. }
            | Self::ApprovalAsked { .. }
            | Self::ApprovalDecided { .. }
            | Self::ApprovalGranted { .. }
            | Self::ToolDispatch { .. }
            | Self::RequestSnapshot { .. }
            | Self::Usage { .. }
            | Self::SandboxMode { .. }
            | Self::SessionShutdown { .. }
            | Self::ToolResultTransformed { .. }
            | Self::ParkedPrompt { .. }
            | Self::ParkConsumed { .. }
            | Self::Feedback { .. }
            | Self::Goal { .. }
            | Self::Plugin { .. } => (None, None),
        }
    }

    /// The payload this row contributes to the surface.
    ///
    /// `None` for a log-only row. Every surface-eligible variant
    /// carries its payload in `data`, so the rule is one line — and
    /// the match that decides it is exhaustive over the variants, so
    /// a new variant must say whether it carries one.
    pub(crate) fn surface_payload(&self) -> Option<&Value> {
        self.surface_parts().0
    }

    /// `surface_op` value if the event carries one.
    ///
    /// The op and the payload come from one match over the variants,
    /// so the two are never decided apart: a new variant must say
    /// which group it is in before it compiles.
    #[must_use]
    pub fn surface_op(&self) -> Option<&SurfaceOp> {
        self.surface_parts().1
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
    /// shapes that pre-date the typed layer (a bare `{"role": ...}`
    /// message, the `UserInput` envelope, etc.) — the row vocabulary's
    /// restore turns those into current rows, and this decoder is the
    /// typed path it restores *from*.
    ///
    /// An unknown `type` is not refused by the persistence layer: the
    /// writer stores the row verbatim, and the read side reports it as
    /// version skew. This decoder returns `None` for it rather than
    /// inventing an event.
    #[must_use]
    pub fn from_value(v: &Value) -> Option<Self> {
        let obj = v.as_object()?;
        let tag = obj.get("type")?.as_str()?;
        let known = RowTag::from_wire(tag).is_some();
        if !known {
            return None;
        }
        serde_json::from_value(v.clone()).ok().map(normalize)
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

mod vocab;

/// 折叠后的表面载荷解成的模型消息——行词汇对折叠载荷的唯一解码。
///
/// 历史投影（[`crate::history`]）问它，而不是自己再写一套。
pub(crate) use vocab::restore::decode_folded_payload;
pub use vocab::{
    CurrentRow,
    RowDefect,
    RowDialect,
    RowTag,
    Rows,
    UnreadableLog,
    publish_row,
    publish_rows,
    restore_row,
};

#[cfg(test)]
mod tests;
