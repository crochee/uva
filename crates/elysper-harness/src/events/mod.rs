//! Agent lifecycle events: `AgentEvent`, `SystemEvent`,
//! `WarningKind`, `AgentMeta`, `SessionEndReason`.
//!
//! This crate emits the `Model` / `ModelDone` / `System` / `Agent`
//! shapes. A policy decision a consumer wants to publish rides
//! `SystemEvent::Plugin` (or a host-registered `Hook`), so the wire
//! carries only variants the harness itself can produce.

use serde::{Deserialize, Serialize};
use elysper_provider::{ContentPart, SamplingResult};

use crate::agent::approval::Suspension;

// --- AgentEvent ---------------------------------------------------------

/// Serialized with serde internally tagged for dispatch.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum AgentEvent {
    Model(ContentPart),
    ModelDone(SamplingResult),
    System(SystemEvent),
    Agent(AgentMeta, Box<AgentEvent>),
}

impl AgentEvent {
    /// Short, stable label for log lines and metrics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Model(_) => "Model",
            Self::ModelDone(_) => "ModelDone",
            Self::System(_) => "System",
            Self::Agent(_, _) => "Agent",
        }
    }

    /// Conservative byte-estimate for log lines and metrics: sums the payload's string lengths plus a fixed per-event overhead instead of
    /// serializing, so the per-event metric path pays no `serde_json` pass; it tracks the wire JSON within a small factor, never undershooting.
    pub fn size_hint(&self) -> usize {
        const OVERHEAD: usize = 64;
        match self {
            Self::Model(part) => OVERHEAD + content_part_len(part),
            Self::ModelDone(result) => {
                OVERHEAD
                    + result.text.len()
                    + result.reasoning.len()
                    + result
                        .tool_calls
                        .iter()
                        .map(tool_use_len)
                        .sum::<usize>()
                    // usage counters + stop_reason + signature
                    + OVERHEAD
            }
            Self::System(event) => OVERHEAD + system_event_len(event),
            Self::Agent(meta, inner) => {
                OVERHEAD
                    + meta.parent_session_id.len()
                    + meta.child_session_id.len()
                    + meta.agent_name.as_deref().map(str::len).unwrap_or(0)
                    + meta.prompt.as_deref().map(str::len).unwrap_or(0)
                    // requested/effective model + thinking fields
                    + OVERHEAD
                    + inner.size_hint()
            }
        }
    }

    /// Returns `true` if the host should persist this event from
    /// the stream as a durable session fact.
    ///
    /// No `AgentEvent` is durable today: the durable facts —
    /// one `assistant_message` row per model turn, one
    /// `tool_result` row per result — are written by the harness
    /// itself through the writer installed on the agent, and a
    /// host that wants them persisted must install one. The
    /// events are the live stream; the log's source is the
    /// writer, never the event tap.
    pub fn is_durable(&self) -> bool {
        match self {
            Self::Model(_) | Self::ModelDone(_) | Self::System(_) => false,
            Self::Agent(_, inner) => inner.is_durable(),
        }
    }

    /// Convenience constructor for an ephemeral text delta.
    pub fn text_delta(text: impl Into<String>) -> Self {
        Self::Model(ContentPart::Text(elysper_provider::TextContent {
            text: text.into(),
            cache_control: None,
        }))
    }

    pub fn usage(
        input_tokens: usize,
        output_tokens: usize,
        cache_read_tokens: Option<usize>,
        cache_creation_tokens: Option<usize>,
    ) -> Self {
        Self::System(SystemEvent::Usage {
            input_tokens,
            output_tokens,
            cache_read_tokens,
            cache_creation_tokens,
        })
    }
}

// --- SystemEvent --------------------------------------------------------

/// Lifecycle, diagnostic, and terminal state changes reported via
/// [`AgentEvent::System`].
///
/// **No variant here is durable.** The whole event stream is live —
/// [`AgentEvent::is_durable`] answers `false` for every
/// `System(_)` — and the durable facts (one `assistant_message` row
/// per model turn, one `tool_result` row per result) are written by
/// the harness's own writer, never by a consumer persisting this
/// stream.
///
/// [`Self::Plugin`] is a plugin's own domain event on that live
/// stream: droppable under backpressure. The durable half of the
/// same fact is `elysper_session::SessionEvent::Plugin`, a log row
/// that survives a crash; a consumer that must not miss something
/// reads the row, not this event.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SystemEvent {
    SessionStarted {
        session_id: String,
    },
    SessionEnded {
        reason: SessionEndReason,
    },
    SessionInterrupted {
        reason: String,
    },
    Progress {
        message: String,
        step: usize,
        total: usize,
    },
    ToolProgress {
        tool_name: String,
        call_id: String,
        output: elysper_tool::ToolOutput,
    },
    Warning {
        kind: WarningKind,
        message: String,
        iteration: Option<usize>,
    },
    Usage {
        input_tokens: usize,
        output_tokens: usize,
        cache_read_tokens: Option<usize>,
        cache_creation_tokens: Option<usize>,
    },
    Suspended(Suspension),
    // Compaction began: the span the summary will replace.
    CompactionStarted {
        // Why compaction ran (`context_pressure`).
        reason: String,
        // First shadowed surface index.
        start: usize,
        // One past the last shadowed surface index.
        end: usize,
        // First shadowed log ordinal (absent when nothing is cited).
        first_seq: Option<u64>,
        // Last shadowed log ordinal.
        last_seq: Option<u64>,
    },
    // Compaction finished, successfully or not.
    CompactionFinished {
        // Why compaction ran.
        reason: String,
        // Messages the replace shadowed (`0` when it failed).
        shadowed_messages: usize,
        // Tokens the shadowed span cost before pruning.
        shadowed_tokens: usize,
        // Terminal state (`committed` / `failed` / `interrupted`).
        outcome: elysper_session::CompactionLifecycleOutcome,
        // First shadowed log ordinal.
        first_seq: Option<u64>,
        // Last shadowed log ordinal.
        last_seq: Option<u64>,
    },
    // A plugin's own domain event — live only. Never persisted, and
    // droppable under backpressure; the durable row is
    // `elysper_session::SessionEvent::Plugin`.
    // Live events were dropped because the consumer fell behind the
    // event channel's soft cap. The durable log still holds every
    // corresponding row; this is the notification of the gap.
    EventsDropped {
        // How many live events were dropped.
        count: usize,
    },
    Plugin {
        // Plugin-defined kind. A runtime value, hence a `String`: a
        // plugin's kind cannot satisfy `'static` without leaking the
        // string it sent. Unbounded today because nothing constructs
        // this variant; the producer that does must bound it at the
        // plugin-host boundary, like the payload below.
        kind: String,
        // Plugin-defined payload, as the plugin sent it. Live, so no
        // log bounds it — but the server serialises every run event
        // and fans it out to the cluster bus, so the producer that
        // constructs this variant must bound it (and `kind`) before
        // the event becomes an `AgentEvent`.
        payload: serde_json::Value,
    },
}

impl SystemEvent {
    /// Short, stable label for log lines and metrics.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SessionStarted { .. } => "SessionStarted",
            Self::SessionEnded { .. } => "SessionEnded",
            Self::SessionInterrupted { .. } => "SessionInterrupted",
            Self::Progress { .. } => "Progress",
            Self::ToolProgress { .. } => "ToolProgress",
            Self::Warning { .. } => "Warning",
            Self::Usage { .. } => "Usage",
            Self::Suspended(_) => "Suspended",
            Self::CompactionStarted { .. } => "CompactionStarted",
            Self::CompactionFinished { .. } => "CompactionFinished",
            Self::EventsDropped { .. } => "EventsDropped",
            Self::Plugin { .. } => "Plugin",
        }
    }
}

/// Classification of a [`SystemEvent::Warning`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WarningKind {
    Loop,
    ContextCompaction,
    Hook,
    Guard,
    StructuredOutput,
    // A turn carried non-text parts the session log cannot reproduce.
    Attachments,
}

// --- AgentMeta ----------------------------------------------------------

/// Carried as the first argument of [`AgentEvent::Agent`].
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentMeta {
    pub parent_session_id: String,
    pub child_session_id: String,
    pub parent_depth: usize,
    /// The model the *caller* asked for; `None` when they specified none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    /// The thinking effort the *caller* asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_thinking: Option<String>,
    /// The model the descriptor / frontmatter actually resolved to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_model: Option<String>,
    /// The thinking effort the descriptor / frontmatter actually resolved to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_thinking: Option<String>,
    /// `true` iff descriptor / frontmatter overrode a caller-specified value.
    #[serde(default)]
    pub overrides_applied: bool,
    /// Registry name of the delegated peer (the `task` tool's `agent` field).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    /// The dispatch prompt; set on the child's **first** forwarded event only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
}

impl AgentMeta {
    pub fn new(
        parent_session_id: impl Into<String>,
        child_session_id: impl Into<String>,
        parent_depth: usize,
    ) -> Self {
        Self {
            parent_session_id: parent_session_id.into(),
            child_session_id: child_session_id.into(),
            parent_depth,
            requested_model: None,
            requested_thinking: None,
            effective_model: None,
            effective_thinking: None,
            overrides_applied: false,
            agent_name: None,
            prompt: None,
        }
    }

    /// Builder: record the registry name of the delegated peer.
    #[must_use]
    pub fn with_agent_name(mut self, name: Option<String>) -> Self {
        self.agent_name = name;
        self
    }

    /// Builder: carry the dispatch prompt (first forwarded event only).
    #[must_use]
    pub fn with_prompt(mut self, prompt: Option<String>) -> Self {
        self.prompt = prompt;
        self
    }
}

// --- SessionEndReason ---------------------------------------------------

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum SessionEndReason {
    Completed,
    Cancelled,
    Error(String),
    MaxIterations,
    // The run stopped with calls still awaiting approval; a resume
    // continues it.
    Suspended,
    // The request envelope changed since the suspension, so the
    // resume would not run the call the operator approved.
    ConfigDrift,
}

impl SessionEndReason {
    /// Stable wire name (snake_case) for logs and hooks.
    ///
    /// Crate-internal: the name is how a log row spells the reason,
    /// not a mapping a consumer needs to reproduce.
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Error(_) => "error",
            Self::MaxIterations => "max_iterations",
            Self::Suspended => "suspended",
            Self::ConfigDrift => "config_drift",
        }
    }
}

// --- size_hint payload walkers -------------------------------------------

fn content_part_len(part: &ContentPart) -> usize {
    match part {
        ContentPart::Text(t) => t.text.len(),
        ContentPart::Reasoning(r) => {
            r.text.len() + r.signature.as_deref().map(str::len).unwrap_or(0)
        }
        ContentPart::Image(i) => i.data.len() + i.mime_type.len(),
        ContentPart::Audio(a) => a.data.len() + a.mime_type.len(),
        ContentPart::ToolUse(u) => tool_use_len(u),
        ContentPart::ToolResult(r) => {
            r.tool_use_id.len()
                + r.tool_name.as_deref().map(str::len).unwrap_or(0)
                + r.content.iter().map(content_part_len).sum::<usize>()
        }
        ContentPart::Resource(l) => {
            l.uri.len()
                + l.name.len()
                + l.title.as_deref().map(str::len).unwrap_or(0)
        }
    }
}

fn tool_use_len(call: &elysper_provider::ToolUse) -> usize {
    call.id.len() + call.name.len() + json_len(&call.input)
}

fn json_len(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Null => 4,
        serde_json::Value::Bool(_) => 4,
        serde_json::Value::Number(n) => number_len(n),
        serde_json::Value::String(s) => s.len() + 2,
        serde_json::Value::Array(items) => {
            2 + items.iter().map(json_len).sum::<usize>() + items.len()
        }
        serde_json::Value::Object(fields) => {
            2 + fields
                .iter()
                .map(|(k, v)| k.len() + 4 + json_len(v))
                .sum::<usize>()
        }
    }
}

fn number_len(n: &serde_json::Number) -> usize {
    if let Some(v) = n.as_u64() {
        return u64_digits(v);
    }
    if let Some(v) = n.as_i64() {
        return i64_digits(v);
    }
    if let Some(v) = n.as_f64() {
        // A float's shortest round-trip form is at most one
        // digit per bit of magnitude; `to_bits` is a monotone
        // stand-in for the printed magnitude.
        return match v.to_bits() {
            0 => 1,
            bits => u64_digits(bits),
        };
    }
    1
}

fn u64_digits(v: u64) -> usize {
    let mut digits = 1;
    let mut rest = v;
    while rest >= 10 {
        rest /= 10;
        digits += 1;
    }
    digits
}

fn i64_digits(v: i64) -> usize {
    u64_digits(v.unsigned_abs()) + usize::from(v < 0)
}

fn system_event_len(event: &SystemEvent) -> usize {
    match event {
        SystemEvent::SessionStarted { session_id } => session_id.len(),
        SystemEvent::SessionEnded { reason } => reason.as_str().len(),
        SystemEvent::SessionInterrupted { reason } => reason.len(),
        SystemEvent::Progress { message, .. } => message.len(),
        SystemEvent::ToolProgress {
            tool_name,
            call_id,
            output,
        } => {
            tool_name.len()
                + call_id.len()
                + output.content.iter().map(content_part_len).sum::<usize>()
        }
        SystemEvent::Warning { message, .. } => message.len(),
        SystemEvent::Usage { .. } => 0,
        SystemEvent::Suspended(suspension) => match suspension {
            crate::agent::approval::Suspension::AwaitingApproval {
                interrupts,
                resume,
            } => {
                interrupts
                    .iter()
                    .map(|request| {
                        request.id.len()
                            + request.call_id.len()
                            + request.tool.len()
                            + request.arguments_hash.len()
                            + request.agent.len()
                    })
                    .sum::<usize>()
                    + resume.pending.iter().map(String::len).sum::<usize>()
                    + resume.request_snapshot_hash.len()
            }
        },
        // Reason + two indices + counters: the digits are bounded
        // by `u64::MAX`'s 20 characters.
        SystemEvent::CompactionStarted { reason, .. } => reason.len() + 96,
        SystemEvent::CompactionFinished { reason, .. } => reason.len() + 96,
        // The payload is the plugin's: its serialised size is the
        // cost a consumer buffers, so walk it like any other payload.
        SystemEvent::EventsDropped { .. } => 0,
        SystemEvent::Plugin { kind, payload } => kind.len() + json_len(payload),
    }
}

#[cfg(test)]
mod tests;
