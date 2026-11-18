//! The row vocabulary — the one place «what a durable session row
//! is» is declared.
//!
//! [`RowTag`] closes the typed `type` tags a row carries, and
//! [`RowDialect`] closes the four legacy envelopes a pre-typed
//! writer wrote. The dialect is a **normalization input**, not a
//! runtime class: [`restore_row`] turns any raw row into a
//! **current row** — its tag in the current vocabulary, the model
//! message it projects to, its place on the surface — so a reader
//! never branches on a dialect.
//!
//! [`Rows`] borrows a log's raw rows and restores them; constructing
//! one is the fail-closed check that says whether this build can
//! interpret the log at all.
//!
//! The tag's wire spelling, its [`SurfaceRole`](super::SurfaceRole)
//! and its row kind are declared on [`RowTag`] alone; the events
//! enum reads them back through its correspondence with the tag,
//! pinned from both directions by tests.

mod publish;
pub(crate) mod restore;

pub use publish::{publish_row, publish_rows};
pub use restore::{CurrentRow, RowDefect, Rows, UnreadableLog, restore_row};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    events::{SessionEvent, SurfaceOp, SurfaceRole},
    surface::fold::RowKind,
};

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
/// The closed vocabulary of typed row tags — the `type` strings a
/// durable session row written through the typed layer carries.
///
/// The variant name *is* the wire tag: the enum derives its entire
/// spelling from `rename_all = "snake_case"`, so the name and the
/// wire string cannot drift apart. The enum also derives
/// [`JsonSchema`], publishing the tags as a `oneOf` of consts — the
/// union the wire contract (and the frontend types generated from
/// it) derives from, replacing a hand-written mirror.
///
/// The vocabulary only grows, and only here: a new tag makes every
/// exhaustive match over this enum (this module's, and every
/// consumer's) fail to compile until the new row says what it is.
pub enum RowTag {
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
    /// A compaction lifecycle ended (committed / failed /
    /// interrupted).
    CompactionEnd,
    /// One model tool call, separate from the turn's message.
    ToolCall,
    /// A partial-output chunk of a running tool call (`data.output`
    /// is the text; `call_id` names the call). Log-only: the
    /// settled `tool_result` row is the model's copy, and a reload
    /// replays the progress from these rows.
    ToolOutput,
    /// One step boundary inside a turn.
    Step,
    /// One turn boundary of the run.
    Turn,
    /// One iteration boundary of the run.
    Iteration,
    /// A fact the runtime could not report any other way
    /// (`data.kind` is a stable slug, `data.message` says what
    /// happened).
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

impl RowTag {
    /// Every tag, in the enum's declaration order — the order the
    /// wire schema publishes them in. Test-only: the published
    /// surface is the enum and its schema, not a list.
    #[cfg(test)]
    pub(crate) const ALL: [RowTag; 33] = [
        RowTag::UserMessage,
        RowTag::AssistantMessage,
        RowTag::AssistantChunk,
        RowTag::ToolResult,
        RowTag::Compaction,
        RowTag::CompactionStart,
        RowTag::CompactionSummary,
        RowTag::CompactionEnd,
        RowTag::ToolCall,
        RowTag::ToolOutput,
        RowTag::Step,
        RowTag::Turn,
        RowTag::Iteration,
        RowTag::Warning,
        RowTag::SteeringGuard,
        RowTag::SteeringHint,
        RowTag::HookBlock,
        RowTag::SubagentEnter,
        RowTag::SubagentExit,
        RowTag::RequestHeader,
        RowTag::ApprovalAsked,
        RowTag::ApprovalDecided,
        RowTag::ApprovalGranted,
        RowTag::ToolDispatch,
        RowTag::RequestSnapshot,
        RowTag::Usage,
        RowTag::SandboxMode,
        RowTag::SessionShutdown,
        RowTag::ToolResultTransformed,
        RowTag::ParkedPrompt,
        RowTag::ParkConsumed,
        RowTag::Feedback,
        RowTag::Plugin,
    ];

    /// The wire string: the variant's name under the enum's
    /// snake_case naming rule.
    ///
    /// A round-trip test holds this and the derived serialisation
    #[must_use]
    /// to the same spelling for every tag.
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::UserMessage => "user_message",
            Self::AssistantMessage => "assistant_message",
            Self::AssistantChunk => "assistant_chunk",
            Self::ToolResult => "tool_result",
            Self::Compaction => "compaction",
            Self::CompactionStart => "compaction_start",
            Self::CompactionSummary => "compaction_summary",
            Self::CompactionEnd => "compaction_end",
            Self::ToolCall => "tool_call",
            Self::ToolOutput => "tool_output",
            Self::Step => "step",
            Self::Turn => "turn",
            Self::Iteration => "iteration",
            Self::Warning => "warning",
            Self::SteeringGuard => "steering_guard",
            Self::SteeringHint => "steering_hint",
            Self::HookBlock => "hook_block",
            Self::SubagentEnter => "subagent_enter",
            Self::SubagentExit => "subagent_exit",
            Self::RequestHeader => "request_header",
            Self::ApprovalAsked => "approval_asked",
            Self::ApprovalDecided => "approval_decided",
            Self::ApprovalGranted => "approval_granted",
            Self::ToolDispatch => "tool_dispatch",
            Self::RequestSnapshot => "request_snapshot",
            Self::Usage => "usage",
            Self::SandboxMode => "sandbox_mode",
            Self::SessionShutdown => "session_shutdown",
            Self::ToolResultTransformed => "tool_result_transformed",
            Self::ParkedPrompt => "parked_prompt",
            Self::ParkConsumed => "park_consumed",
            Self::Feedback => "feedback",
            Self::Plugin => "plugin",
        }
    }

    /// Parse a wire string into a tag.
    ///
    /// `None` for anything outside the vocabulary — every legacy
    /// dialect, every near-miss spelling, every future tag a newer
    /// build wrote. Total and allocation-free, so the classifier
    /// can ask on every row without paying for the ones that do not
    #[must_use]
    /// answer.
    pub fn from_wire(tag: &str) -> Option<Self> {
        match tag {
            "user_message" => Some(Self::UserMessage),
            "assistant_message" => Some(Self::AssistantMessage),
            "assistant_chunk" => Some(Self::AssistantChunk),
            "tool_result" => Some(Self::ToolResult),
            "compaction" => Some(Self::Compaction),
            "compaction_start" => Some(Self::CompactionStart),
            "compaction_summary" => Some(Self::CompactionSummary),
            "compaction_end" => Some(Self::CompactionEnd),
            "tool_call" => Some(Self::ToolCall),
            "tool_output" => Some(Self::ToolOutput),
            "step" => Some(Self::Step),
            "turn" => Some(Self::Turn),
            "iteration" => Some(Self::Iteration),
            "warning" => Some(Self::Warning),
            "steering_guard" => Some(Self::SteeringGuard),
            "steering_hint" => Some(Self::SteeringHint),
            "hook_block" => Some(Self::HookBlock),
            "subagent_enter" => Some(Self::SubagentEnter),
            "subagent_exit" => Some(Self::SubagentExit),
            "request_header" => Some(Self::RequestHeader),
            "approval_asked" => Some(Self::ApprovalAsked),
            "approval_decided" => Some(Self::ApprovalDecided),
            "approval_granted" => Some(Self::ApprovalGranted),
            "tool_dispatch" => Some(Self::ToolDispatch),
            "request_snapshot" => Some(Self::RequestSnapshot),
            "usage" => Some(Self::Usage),
            "sandbox_mode" => Some(Self::SandboxMode),
            "session_shutdown" => Some(Self::SessionShutdown),
            "tool_result_transformed" => Some(Self::ToolResultTransformed),
            "parked_prompt" => Some(Self::ParkedPrompt),
            "park_consumed" => Some(Self::ParkConsumed),
            "feedback" => Some(Self::Feedback),
            "plugin" => Some(Self::Plugin),
            _ => None,
        }
    }
}

impl RowTag {
    #[must_use]
    /// The role a row of this tag plays on the model-visible
    /// surface — the one declaration. The event's
    /// [`SessionEvent::surface_role`] and the raw-log classifier
    /// both read it here, so the two cannot drift. Exhaustive over
    /// the vocabulary: a tag added without declaring its role does
    /// not compile.
    pub const fn surface_role(self) -> SurfaceRole {
        match self {
            Self::UserMessage | Self::AssistantMessage | Self::ToolResult => {
                SurfaceRole::Message
            }
            Self::Compaction => SurfaceRole::OpOnly,
            Self::AssistantChunk
            | Self::CompactionStart
            | Self::CompactionSummary
            | Self::CompactionEnd
            | Self::ToolCall
            | Self::ToolOutput
            | Self::Step
            | Self::Turn
            | Self::Iteration
            | Self::Warning
            | Self::SteeringGuard
            | Self::SteeringHint
            | Self::HookBlock
            | Self::SubagentEnter
            | Self::SubagentExit
            | Self::RequestHeader
            | Self::ApprovalAsked
            | Self::ApprovalDecided
            | Self::ApprovalGranted
            | Self::ToolDispatch
            | Self::RequestSnapshot
            | Self::Usage
            | Self::SandboxMode
            | Self::SessionShutdown
            | Self::ToolResultTransformed
            | Self::ParkedPrompt
            | Self::ParkConsumed
            | Self::Feedback
            | Self::Plugin => SurfaceRole::LogOnly,
        }
    }

    #[must_use]
    /// The kind of surface row a tag folds as — `None` for a
    /// log-only row, which contributes nothing to the
    /// model-visible surface. The one declaration; the fold's
    /// placement reads it here. Exhaustive over the vocabulary: a
    /// tag added without saying where it folds does not compile.
    pub(crate) const fn row_kind(self) -> Option<RowKind> {
        match self {
            Self::UserMessage => Some(RowKind::User),
            Self::AssistantMessage => Some(RowKind::Assistant),
            Self::ToolResult => Some(RowKind::Tool),
            Self::Compaction => Some(RowKind::Compaction),
            // The log-only rows: written to the log and read back
            // as records of what happened; they never reach the
            // model-visible surface, so the fold has no kind for
            // them.
            Self::AssistantChunk
            | Self::CompactionStart
            | Self::CompactionSummary
            | Self::CompactionEnd
            | Self::ToolCall
            | Self::ToolOutput
            | Self::Step
            | Self::Turn
            | Self::Iteration
            | Self::Warning
            | Self::SteeringGuard
            | Self::SteeringHint
            | Self::HookBlock
            | Self::SubagentEnter
            | Self::SubagentExit
            | Self::RequestHeader
            | Self::ApprovalAsked
            | Self::ApprovalDecided
            | Self::ApprovalGranted
            | Self::ToolDispatch
            | Self::RequestSnapshot
            | Self::Usage
            | Self::SandboxMode
            | Self::SessionShutdown
            | Self::ToolResultTransformed
            | Self::ParkedPrompt
            | Self::ParkConsumed
            | Self::Feedback
            | Self::Plugin => None,
        }
    }

    #[must_use]
    /// 这一类行丢了也不影响重建会话吗。
    ///
    /// 「可忽略」是逐标签声明的：声明成可忽略的行，写入者会在行的
    /// 顶层（`type` 的兄弟）盖上 `"ignorable": true`，于是不认识这
    /// 个标签的旧构建能安全跳过它，而不是拒读整份日志。
    ///
    /// 默认不可忽略。忘声明的代价是旧构建读不了新日志；多声明的
    /// 代价是旧构建静默丢掉一行必需事件、于是重建出一个错误的会话。
    /// 后者更糟，所以默认取严格的一边。
    ///
    /// 今天 33 个标签全部不可忽略——每一个都是重建会话要读的东西。
    /// 新增标签时在这里表态。
    pub const fn ignorable(self) -> bool {
        match self {
            Self::UserMessage
            | Self::AssistantMessage
            | Self::AssistantChunk
            | Self::ToolResult
            | Self::Compaction
            | Self::CompactionStart
            | Self::CompactionSummary
            | Self::CompactionEnd
            | Self::ToolCall
            | Self::ToolOutput
            | Self::Step
            | Self::Turn
            | Self::Iteration
            | Self::Warning
            | Self::SteeringGuard
            | Self::SteeringHint
            | Self::HookBlock
            | Self::SubagentEnter
            | Self::SubagentExit
            | Self::RequestHeader
            | Self::ApprovalAsked
            | Self::ApprovalDecided
            | Self::ApprovalGranted
            | Self::ToolDispatch
            | Self::RequestSnapshot
            | Self::Usage
            | Self::SandboxMode
            | Self::SessionShutdown
            | Self::ToolResultTransformed
            | Self::ParkedPrompt
            | Self::ParkConsumed
            | Self::Feedback
            | Self::Plugin => false,
        }
    }

    #[must_use]
    /// An empty event skeleton for this tag: zeroed `seq`, an
    /// empty `ts`, a `Value::Null` payload, and (for the
    /// surface-eligible tags) the `append` `surface_op` — enough to
    /// recognise the discriminant before populating the payload.
    ///
    /// This is the tag side of the correspondence with
    /// [`SessionEvent`]: exhaustive over the vocabulary (a tag
    /// without a constructor does not compile), and tied to the
    /// event side's [`SessionEvent::tag`] by a test that chains
    /// the two — so neither match can drift a pairing.
    pub fn empty_event(self) -> SessionEvent {
        match self {
            Self::UserMessage => SessionEvent::UserMessage {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
                source: None,
                surface_op: Some(SurfaceOp::append()),
                attachments: Vec::new(),
            },
            Self::AssistantMessage => SessionEvent::AssistantMessage {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
                surface_op: Some(SurfaceOp::append()),
            },
            Self::AssistantChunk => SessionEvent::AssistantChunk {
                seq: 0,
                ts: String::new(),
                chunk_seq: 0,
                finish_reason: None,
                data: Value::Null,
            },
            Self::ToolResult => SessionEvent::ToolResult {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
                is_error: false,
                tool_call_id: None,
                surface_op: Some(SurfaceOp::append()),
            },
            Self::Compaction => SessionEvent::Compaction {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
                surface_op: SurfaceOp::append(),
            },
            Self::CompactionStart => SessionEvent::CompactionStart {
                seq: 0,
                ts: String::new(),
                token: String::new(),
                data: Value::Null,
            },
            Self::CompactionSummary => SessionEvent::CompactionSummary {
                seq: 0,
                ts: String::new(),
                token: String::new(),
                data: Value::Null,
            },
            Self::CompactionEnd => SessionEvent::CompactionEnd {
                seq: 0,
                ts: String::new(),
                token: String::new(),
                outcome: String::new(),
                data: Value::Null,
            },
            Self::ToolCall => SessionEvent::ToolCall {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
                call_id: None,
                name: None,
            },
            Self::ToolOutput => SessionEvent::ToolOutput {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
                call_id: None,
                tool: None,
            },
            Self::Step => SessionEvent::Step {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
                kind: None,
                turn: None,
                step: None,
            },
            Self::Turn => SessionEvent::Turn {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
                kind: None,
                turn: None,
            },
            Self::Iteration => SessionEvent::Iteration {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
            },
            Self::Warning => SessionEvent::Warning {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
            },
            Self::SteeringGuard => SessionEvent::SteeringGuard {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
            },
            Self::SteeringHint => SessionEvent::SteeringHint {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
            },
            Self::HookBlock => SessionEvent::HookBlock {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
            },
            Self::SubagentEnter => SessionEvent::SubagentEnter {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
                child_session_id: None,
                parent_session_id: None,
                agent_name: None,
                depth: None,
            },
            Self::SubagentExit => SessionEvent::SubagentExit {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
                child_session_id: None,
                depth: None,
                status: None,
            },
            Self::RequestHeader => SessionEvent::RequestHeader {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
                provider: None,
                model: None,
            },
            Self::ApprovalAsked => SessionEvent::ApprovalAsked {
                seq: 0,
                ts: String::new(),
                id: String::new(),
                call_id: String::new(),
                tool: String::new(),
                arguments_hash: String::new(),
                risk: String::new(),
                agent: String::new(),
            },
            Self::ApprovalDecided => SessionEvent::ApprovalDecided {
                seq: 0,
                ts: String::new(),
                id: String::new(),
                call_id: String::new(),
                status: String::new(),
                by: String::new(),
                payload: None,
            },
            Self::ApprovalGranted => SessionEvent::ApprovalGranted {
                seq: 0,
                ts: String::new(),
                id: String::new(),
                tool: String::new(),
                scope: String::new(),
                workspace_hash: String::new(),
            },
            Self::ToolDispatch => SessionEvent::ToolDispatch {
                seq: 0,
                ts: String::new(),
                call_id: String::new(),
                tool: String::new(),
                arguments_hash: String::new(),
            },
            Self::RequestSnapshot => SessionEvent::RequestSnapshot {
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
            Self::Usage => SessionEvent::Usage {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
                usage: None,
            },
            Self::SandboxMode => SessionEvent::SandboxMode {
                seq: 0,
                ts: String::new(),
                mode: String::new(),
            },
            Self::SessionShutdown => SessionEvent::SessionShutdown {
                seq: 0,
                ts: String::new(),
                data: Value::Null,
            },
            Self::ToolResultTransformed => {
                SessionEvent::ToolResultTransformed {
                    seq: 0,
                    ts: String::new(),
                    transformers: Vec::new(),
                    tool_name: String::new(),
                    call_id: String::new(),
                    before: String::new(),
                    after: String::new(),
                }
            }
            Self::ParkedPrompt => SessionEvent::ParkedPrompt {
                seq: 0,
                ts: String::new(),
                park_id: String::new(),
                text: String::new(),
                attachments: Vec::new(),
                agent_name: None,
                provider: None,
                model: None,
                rerun: false,
            },
            Self::ParkConsumed => SessionEvent::ParkConsumed {
                seq: 0,
                ts: String::new(),
                park_id: String::new(),
            },
            Self::Feedback => SessionEvent::Feedback {
                seq: 0,
                ts: String::new(),
                message_id: String::new(),
                thumbs_up: false,
            },
            Self::Plugin => SessionEvent::Plugin {
                seq: 0,
                ts: String::new(),
                kind: String::new(),
                payload: Value::Null,
                original_bytes: None,
            },
        }
    }
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema,
)]
/// The closed set of legacy row dialects — the envelope `type`
/// strings a pre-typed writer wrote, which a durable log can still
/// carry.
///
/// No writer in this crate emits one any more, and none ever will
/// again — a log's dialect content only shrinks as old sessions age
/// out. They stay in the vocabulary so a reader can *name* what an
/// old row is (and answer for its payload) instead of guessing, and
/// so the wire contract publishes the same closed set to every
/// consumer.
pub enum RowDialect {
    /// The controller's user-prompt envelope: the turn's text under
    /// `data.text`.
    UserInput,
    /// One streamed model part per row: a single `ContentPart`
    /// under `data`.
    Model,
    /// The lossless whole-message envelope: a complete provider
    /// message under `data`.
    Message,
    /// The recursive sub-agent trace envelope: `data` is
    /// `[child metadata, inner event]`. A delegated child lives in
    /// its own session now, but logs written before the delegation
    /// channel's retirement still carry these rows.
    Agent,
}

impl RowDialect {
    /// The wire string — the envelope's original `type` spelling,
    /// verbatim: the legacy dialects predate this crate's naming
    #[must_use]
    /// rules, so their wire form is the historical one.
    pub const fn as_wire(self) -> &'static str {
        match self {
            Self::UserInput => "UserInput",
            Self::Model => "Model",
            Self::Message => "Message",
            Self::Agent => "Agent",
        }
    }

    /// Parse a wire string into a dialect; `None` for anything
    #[must_use]
    /// else. Allocation-free, like [`RowTag::from_wire`].
    pub fn from_wire(tag: &str) -> Option<Self> {
        match tag {
            "UserInput" => Some(Self::UserInput),
            "Model" => Some(Self::Model),
            "Message" => Some(Self::Message),
            "Agent" => Some(Self::Agent),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests;
