//! The recoverable half: every summarised tool call stored
//! verbatim, so `context_tree_query` can hand the model the
//! original output on demand.

use parking_lot::Mutex;
use serde::{Deserialize, Serialize};
use synthia_provider::{ContentPart, Message, Role};

/// What one archived record preserves.
///
/// The archive outlives a single crate's assumptions: a compaction
/// that widens its range backward (to keep every `tool_result` with
/// its issuing `tool_use`) also orphans the assistant turn's text
/// and thinking, so those are archived too. Without a discriminator
/// an assistant record would look like a tool call with an empty
/// tool name, and `context_tree_query` could not tell the caller
/// which of the two it is reading.
#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallRecordKind {
    /// A `tool` message: one tool call's result, keyed by its
    /// canonical `tool_use_id`.
    #[default]
    ToolResult,
    /// An assistant turn whose text/thinking the compaction had to
    /// shadow. Its `tool_call_id` is synthetic (`assistant-turn:…`),
    /// so it is recoverable by listing the archive rather than by
    /// resolving a `tool_use_id` the model was shown.
    AssistantTurn,
}

/// One entry's worth of context held in the in-memory archive.
///
/// Stored verbatim — callers may surface these to the model via a
/// `context_tree_query` tool when it needs to recover the raw
/// output (e.g. it called the tool earlier and only saw the
/// summary, then needs the full text to answer a follow-up).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ToolCallRecord {
    /// Stable `tool_use_id` from the original `ToolUse`, or a
    /// synthetic `assistant-turn:…` id for an
    /// [`AssistantTurn`](ToolCallRecordKind::AssistantTurn).
    pub tool_call_id: String,
    /// Tool name (e.g. `"read_file"`, `"shell"`), or `"assistant"`
    /// for an [`AssistantTurn`](ToolCallRecordKind::AssistantTurn).
    pub tool_name: String,
    /// Original argument object the model sent. `Null` for an
    /// assistant turn.
    pub args: serde_json::Value,
    /// The text the tool returned, after any per-tool truncation, or
    /// the assistant turn's text + thinking.
    pub result_text: String,
    /// `true` when the tool surfaced a non-zero exit / error.
    pub is_error: bool,
    /// Which message this record preserves. Older serialised
    /// records predate the field and read back as tool results.
    #[serde(default)]
    pub kind: ToolCallRecordKind,
}

/// Process-wide archive of every tool call that has been
/// summarised (or could be recovered). Stored behind a single
/// `Mutex<HashMap>` — the volume per session is bounded by
/// `MAX_ITERATIONS × tools_per_turn` and the access pattern is
/// "summary once, query rarely", so a `RwLock` would only buy
/// complexity without throughput.
#[derive(Default)]
pub struct ToolCallArchive {
    by_id: Mutex<std::collections::HashMap<String, ToolCallRecord>>,
}

impl ToolCallArchive {
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert one record, overwriting any previous entry with the
    /// same `tool_call_id`. Returns the inserted record.
    pub fn insert(&self, record: ToolCallRecord) -> ToolCallRecord {
        let id = record.tool_call_id.clone();
        self.by_id.lock().insert(id, record.clone());
        record
    }

    /// Look up by canonical `tool_call_id` *or* short alias.
    pub fn get(&self, key: &str) -> Option<ToolCallRecord> {
        self.by_id.lock().get(key).cloned()
    }

    /// Snapshot every record. Used by `context_tree_query` to list
    /// what's available; cheap because the map stays small.
    pub fn list(&self) -> Vec<ToolCallRecord> {
        self.by_id.lock().values().cloned().collect()
    }

    /// Number of archived records.
    pub fn len(&self) -> usize {
        self.by_id.lock().len()
    }

    /// `true` when the archive is empty.
    pub fn is_empty(&self) -> bool {
        self.by_id.lock().is_empty()
    }
}

/// Build the archive entry for one message inside a replaced range.
///
/// Tool results keep their canonical `tool_use_id`, which is the
/// identity `context_tree_query` resolves. An assistant turn has no
/// such id, so its record gets a synthetic one derived from the
/// tool calls it issued (unique per call group) and the
/// [`ToolCallRecordKind::AssistantTurn`] discriminator.
pub(super) fn archive_record_for(
    index: usize,
    msg: &Message,
) -> Option<ToolCallRecord> {
    match msg.role {
        Role::Tool => tool_record(msg),
        Role::Assistant => assistant_record(index, msg),
        _ => None,
    }
}

fn tool_record(msg: &Message) -> Option<ToolCallRecord> {
    let tool_call_id = msg.tool_call_id.clone().unwrap_or_default();
    if tool_call_id.is_empty() {
        return None;
    }
    let mut tool_name = String::new();
    let mut is_error = false;
    let mut result_text = String::new();
    let mut args = serde_json::Value::Null;

    for part in msg.content.iter() {
        match part {
            ContentPart::ToolResult(tr) => {
                tool_name = tr.tool_name.clone().unwrap_or_default();
                is_error = tr.is_error.unwrap_or(false);
                if let Some(structured) = &tr.structured_content {
                    args = structured.clone();
                }
                for inner in &tr.content {
                    if let ContentPart::Text(tc) = inner {
                        result_text.push_str(&tc.text);
                    }
                }
            }
            ContentPart::Text(tc) => {
                result_text.push_str(&tc.text);
            }
            _ => {}
        }
    }

    Some(ToolCallRecord {
        tool_call_id,
        tool_name,
        args,
        result_text,
        is_error,
        kind: ToolCallRecordKind::ToolResult,
    })
}

fn assistant_record(index: usize, msg: &Message) -> Option<ToolCallRecord> {
    let result_text = super::prompt::extract_assistant_text(msg)?;
    let issuing: Vec<String> = msg
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::ToolUse(call) => Some(call.id.clone()),
            _ => None,
        })
        .collect();
    let tool_call_id = if issuing.is_empty() {
        format!("assistant-turn-{index}")
    } else {
        format!("assistant-turn:{}", issuing.join("+"))
    };
    Some(ToolCallRecord {
        tool_call_id,
        tool_name: "assistant".to_string(),
        args: serde_json::Value::Null,
        result_text,
        is_error: false,
        kind: ToolCallRecordKind::AssistantTurn,
    })
}
