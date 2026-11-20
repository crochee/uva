//! What one `task` tool call means: the parsed arguments and
//! the constants that bound the seam.

use serde_json::{Map, Value};

use crate::{args, gate::GateSpec, worktree::WorktreeSpec};

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
}

impl TaskSpec {
    /// Parse the tool-call JSON arguments.
    ///
    /// Every argument is read through its declaration in the
    /// `args` module: the names, the shapes, the bounds and the
    /// refusals live there, so the parser cannot drift away from
    /// the schema the model was shown.
    ///
    /// `agent` must be a non-empty string and `prompt` must be a
    /// string: a call that leaves either out is refused, the same
    /// way the schema states them. `gate` and `isolation` are
    /// parsed when present; missing / null leaves the corresponding
    /// field as `None`.
    ///
    /// An argument that is gone is refused with the reason it is
    /// gone: the model was told about it, so a call that still
    /// carries it gets an answer, not silence.
    pub fn from_value(value: &Value) -> Result<Self, String> {
        args::refuse_removed(value)?;
        Ok(Self {
            agent: args::read_text_named(&args::AGENT, value)?,
            prompt: args::read_text_named(&args::PROMPT, value)?,
            gate: parse_gate(value)?,
            isolation: parse_isolation(value)?,
        })
    }
}

/// Read the optional `gate` out of a tool-call payload.
///
/// The declaration states the shapes the gate is read as: an object
/// with a `command`, a bare command string, or null (no gate).
fn parse_gate(payload: &Value) -> Result<Option<GateSpec>, String> {
    match args::read_named(&args::GATE, payload)? {
        args::Read::Absent | args::Read::Null => Ok(None),
        args::Read::Str(program) => Ok(Some(GateSpec::from(program))),
        args::Read::Object(members) => Ok(Some(parse_gate_object(members)?)),
        _ => Err(args::refusal_named(&args::GATE, payload)),
    }
}

/// Read the members of a `gate` object.
fn parse_gate_object(members: &Map<String, Value>) -> Result<GateSpec, String> {
    let command = args::read_argv_named(&args::GATE_COMMAND, members)?;
    Ok(GateSpec::from(command))
}

/// Read the optional `isolation` out of a tool-call payload.
///
/// The declaration states the shapes the knob is read as: a flag, an
/// object, or null (no isolation).
fn parse_isolation(payload: &Value) -> Result<Option<WorktreeSpec>, String> {
    match args::read_named(&args::ISOLATION, payload)? {
        args::Read::Absent | args::Read::Null => Ok(None),
        args::Read::Bool(false) => Ok(None),
        args::Read::Bool(true) => Ok(Some(WorktreeSpec)),
        args::Read::Object(_) => Ok(Some(WorktreeSpec)),
        _ => Err(args::refusal_named(&args::ISOLATION, payload)),
    }
}
