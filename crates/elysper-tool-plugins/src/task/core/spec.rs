//! What one `task` tool call means: the parsed arguments and
//! the constants that bound the seam.

use serde_json::Value;

use crate::task::{
    gate::GateSpec,
    worktree::{self, WorktreeSpec},
};

/// The tool name the model calls to delegate to a peer agent.
pub const TASK_TOOL_NAME: &str = "task";

/// Maximum sub-agent nesting depth (0 = top-level session).
pub const MAX_SUBAGENT_DEPTH: usize = 3;

/// Parsed arguments of one `task` tool call.
///
/// The two core fields — `agent` + `prompt` — are required;
/// `gate` and `isolation` are optional knobs (R30 Slice D).
/// `gate` is always available; `isolation` is feature-gated at
/// the schema level via
/// `ToolFeatures::is_enabled("worktree_isolation")` so
/// deployments that opt out of the capability never advertise
/// it to the model.
///
/// R-OPT-1 Step 6.1: `trace_context` is an OTel W3C wire-format
/// snapshot ([`TaskTraceContext`]) that the parent's loop
/// hands to the child so the child's spans chain into the
/// parent's trace tree. The wire shape (hex-encoded IDs)
/// means the model never sees it: the field is parsed from
/// the same JSON the tool receives, but is **not** echoed back
/// to the model by any of the schema-coercion paths.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TaskSpec {
    /// Registry name of the peer agent to run.
    pub agent: String,
    /// The prompt handed to the child as its user input.
    pub prompt: String,
    /// Optional verification command run against the child's
    /// working tree after the child finishes.
    pub gate: Option<GateSpec>,
    /// Optional worktree-isolation marker. When `Some`, the
    /// child runs in a fresh detached worktree off the parent's
    /// base cwd.
    pub isolation: Option<WorktreeSpec>,
    /// Parent's W3C trace context for the child to extend.
    /// Carries `traceparent` / `tracestate` in their canonical
    /// hex / vendor-list shape. `None` means the child starts a
    /// root span (an orphan trace from OTel's point of view).
    pub trace_context: Option<TaskTraceContext>,
}

/// Wire-shape snapshot of an inbound W3C `traceparent`
/// header. We can't directly deserialize `ExtractedTraceContext`
/// (its `TraceId`/`SpanId`/`TraceState` OTel-typed fields do
/// not have `serde` derives), so the delegator formats the
/// hex strings once on the way in and the runner parses them
/// back on the way out.
#[derive(
    Clone, Debug, Default, PartialEq, Eq, serde::Deserialize, serde::Serialize,
)]
pub struct TaskTraceContext {
    /// 32-hex-char trace id (lowercase).
    pub trace_id_hex: String,
    /// 16-hex-char parent span id (lowercase).
    pub parent_span_id_hex: String,
    /// Sampled-bit byte (one of `0x01` "sampled" / `0x00`).
    pub trace_flags_byte: u8,
    /// Vendor-list `tracestate` header value, verbatim.
    pub trace_state: String,
}

impl TaskSpec {
    /// Parse the tool-call JSON arguments.
    ///
    /// `agent` must be a non-empty string; `prompt` defaults to
    /// an empty string (a child may legitimately be asked to act
    /// on its own instructions). `gate`, `isolation` and
    /// `trace_context` are parsed when present; missing / null
    /// leaves the corresponding field as `None`.
    pub fn from_value(value: &Value) -> Result<Self, String> {
        let Some(agent) = value.get("agent").and_then(Value::as_str) else {
            return Err("task tool requires a string `agent` field".to_string());
        };
        if agent.is_empty() {
            return Err("`agent` must be a non-empty agent name".to_string());
        };
        let prompt = value
            .get("prompt")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let gate = parse_gate_field(value.get("gate"))?;
        let isolation = worktree::parse_worktree_field(value.get("isolation"))?;
        let trace_context =
            parse_trace_context_field(value.get("trace_context"))?;
        Ok(Self {
            agent: agent.to_string(),
            prompt: prompt.to_string(),
            gate,
            isolation,
            trace_context,
        })
    }
}

/// Parse the optional `trace_context` object out of a
/// tool-call payload. Expected shape:
///
/// ```json
/// {
///   "trace_id_hex": "0af7651916cd43dd8448eb211c80319c",
///   "parent_span_id_hex": "b7ad6b7169203331",
///   "trace_flags_byte": 1,
///   "trace_state": "vendor1=value1"
/// }
/// ```
///
/// Missing / null is `Ok(None)`. A malformed value (wrong
/// hex length, non-string id, …) is `Err`.
fn parse_trace_context_field(
    value: Option<&Value>,
) -> Result<Option<TaskTraceContext>, String> {
    let Some(value) = value else { return Ok(None) };
    if value.is_null() {
        return Ok(None);
    }
    let obj = value
        .as_object()
        .ok_or_else(|| "trace_context must be an object".to_string())?;
    let trace_id_hex = obj
        .get("trace_id_hex")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            "trace_context.trace_id_hex must be a string".to_string()
        })?;
    if trace_id_hex.len() != 32 {
        return Err(format!(
            "trace_context.trace_id_hex must be 32 hex chars, got {}",
            trace_id_hex.len()
        ));
    }
    let parent_span_id_hex = obj
        .get("parent_span_id_hex")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            "trace_context.parent_span_id_hex must be a string".to_string()
        })?;
    if parent_span_id_hex.len() != 16 {
        return Err(format!(
            "trace_context.parent_span_id_hex must be 16 hex chars, got {}",
            parent_span_id_hex.len()
        ));
    }
    let trace_flags_byte = obj
        .get("trace_flags_byte")
        .and_then(Value::as_u64)
        .ok_or_else(|| {
        "trace_context.trace_flags_byte must be a u8-compatible integer"
            .to_string()
    })? as u8;
    let trace_state = obj
        .get("trace_state")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    Ok(Some(TaskTraceContext {
        trace_id_hex: trace_id_hex.to_string(),
        parent_span_id_hex: parent_span_id_hex.to_string(),
        trace_flags_byte,
        trace_state,
    }))
}

/// Parse the optional `gate` object out of a tool-call payload.
///
/// Accepts three shapes:
/// - `null` / missing ⇒ `Ok(None)` (no gate).
/// - `{"command": ["cargo", "test"], "timeout_secs": 60}` ⇒
///   `Ok(Some(GateSpec { … }))`.
/// - `"make test"` (bare string) ⇒ `Ok(Some(GateSpec::from(...)))`
///   for backwards compatibility with the simpler shape.
/// - Anything else ⇒ `Err("gate: …")`.
fn parse_gate_field(value: Option<&Value>) -> Result<Option<GateSpec>, String> {
    let Some(value) = value else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    if let Some(s) = value.as_str() {
        return Ok(Some(GateSpec::from(s)));
    }
    let obj = value
        .as_object()
        .ok_or_else(|| "gate must be a string or object".to_string())?;
    let command = obj
        .get("command")
        .ok_or_else(|| "gate object requires a `command` field".to_string())?;
    let argv: Vec<String> = match command {
        Value::String(s) => vec![s.clone()],
        Value::Array(arr) => arr
            .iter()
            .map(|v| {
                v.as_str().map(str::to_string).ok_or_else(|| {
                    "gate command entries must be strings".to_string()
                })
            })
            .collect::<Result<Vec<_>, _>>()?,
        _ => return Err("gate.command must be string or array".to_string()),
    };
    if argv.is_empty() {
        return Err(
            "gate.command must contain at least one argument".to_string()
        );
    }
    let timeout_secs = obj
        .get("timeout_secs")
        .and_then(Value::as_u64)
        .map(std::time::Duration::from_secs);
    let mut spec = GateSpec {
        command: argv,
        timeout: timeout_secs,
    };
    if let Some(t) = timeout_secs {
        spec = spec.with_timeout(t);
    }
    Ok(Some(spec))
}
