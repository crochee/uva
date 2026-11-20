//! The artefact set a dispatched child leaves on disk.
//!
//! Every child gets one set, written into its **own** session
//! directory: `artifacts/<id>.{md,jsonl,patch}`. The convention is
//! documented in `docs/log-format.md` (§ "The artefact set"); this
//! module is its implementation.
//!
//! - `.jsonl` — a frozen copy of the child's transcript at the moment
//!   the round ended. The live log keeps growing; the artefact says
//!   what the round had produced.
//! - `.md` — the same transcript rendered for a human: who spoke,
//!   which tools ran, how the round ended.
//! - `.patch` — the diff of an isolated round (a child dispatched into
//!   its own git worktree). Absent when the round was not isolated or
//!   changed nothing: an empty patch would be a claim, and there is
//!   nothing to claim.
//!
//! The child's session is the durable truth, so the artefact set is a
//! projection of it, never a second source. Writing is synchronous and
//! best-effort: a round that produced a real answer must not fail
//! because its summary could not be written.

use std::{
    fmt::Write as _,
    io,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use serde_json::Value;
use uva_session::{CurrentRow, RowTag, SessionEvent, restore_row};

/// The directory the artefact set lives in, inside a session
/// directory.
pub const ARTIFACTS_DIR: &str = "artifacts";

/// The session log's file name inside a session directory.
///
/// The log format fixes this name (`docs/log-format.md`), so the
/// artefact set can be projected from it without the host handing the
/// surface a second path.
pub const SESSION_LOG_FILE: &str = "events.jsonl";

/// What the artefacts record about the round that just ended.
#[derive(Clone, Debug)]
pub struct RoundRecord<'a> {
    /// The child session id, and the artefact set's file stem.
    pub id: &'a str,
    /// The peer agent the child ran as.
    pub agent: &'a str,
    /// The session that delegated the child.
    pub parent: &'a str,
    /// How deep the delegation is.
    pub depth: usize,
    /// How the round ended: `completed`, `cancelled` or `error`.
    pub status: &'a str,
    /// The isolated round's diff, when one was produced.
    ///
    /// Borrowed: the diff is the round's own, and a second copy of a
    /// whole patch is a copy nobody asked for.
    pub patch: Option<&'a str>,
}

/// Where the artefacts landed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArtifactPaths {
    /// The human-readable transcript.
    pub markdown: PathBuf,
    /// The frozen transcript copy.
    pub transcript: PathBuf,
    /// The isolated round's diff, when there was one.
    pub patch: Option<PathBuf>,
}

/// Project a child's round into its artefact set.
///
/// `session_dir` is the child's own session directory; the transcript
/// is read from `<session_dir>/events.jsonl` and the set is written to
/// `<session_dir>/artifacts/`.
///
/// # Errors
///
/// Returns the I/O error when the session log cannot be read (a child
/// without a durable session has no artefact set) or the set cannot be
/// written.
pub fn write(
    session_dir: &Path,
    record: &RoundRecord<'_>,
) -> io::Result<ArtifactPaths> {
    let raw = std::fs::read_to_string(session_dir.join(SESSION_LOG_FILE))?;
    let dir = session_dir.join(ARTIFACTS_DIR);
    std::fs::create_dir_all(&dir)?;

    let transcript = dir.join(format!("{}.jsonl", record.id));
    std::fs::write(&transcript, &raw)?;

    let markdown = dir.join(format!("{}.md", record.id));
    std::fs::write(&markdown, render(record, &raw))?;

    let patch = record
        .patch
        .filter(|patch| !patch.trim().is_empty())
        .map(|patch| {
            let path = dir.join(format!("{}.patch", record.id));
            std::fs::write(&path, patch).map(|()| path)
        })
        .transpose()?;

    Ok(ArtifactPaths {
        markdown,
        transcript,
        patch,
    })
}

/// The markdown projection of a transcript.
fn render(record: &RoundRecord<'_>, raw: &str) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "# Sub-agent `{}`\n", record.id);
    let _ = writeln!(out, "| | |");
    let _ = writeln!(out, "|---|---|");
    let _ = writeln!(out, "| agent | `{}` |", record.agent);
    let _ = writeln!(out, "| parent session | `{}` |", record.parent);
    let _ = writeln!(out, "| depth | {} |", record.depth);
    let _ = writeln!(out, "| round | {} |", record.status);
    let _ = writeln!(out, "| session | `{}` |", record.id);
    let _ = writeln!(out, "\n## Transcript");

    for line in raw.lines().map(str::trim).filter(|line| !line.is_empty()) {
        let Ok(row) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        render_row(&mut out, &row);
    }
    out
}

/// One transcript row, as markdown.
///
/// The row's **restored** tag decides what the set does with it: the
/// tags the transcript is made of are rendered, the rest are left out
/// on purpose (rows the model never saw — the streamed chunks, the
/// usage counters, the request headers — and rows another row already
/// says). The match is exhaustive over the vocabulary, so a new tag
/// does not compile until the set says what it does with it; see
/// `docs/adr/0006-row-vocabulary-owns-the-row.md`.
///
/// A row written in an old dialect is restored into the current
/// vocabulary first, so it is rendered in current shape: the set
/// shows what the round did, not which spelling the log happened to
/// use. A row the current vocabulary does not cover — a tag this
/// build does not know, or no tag at all — is left out: the set is a
/// projection of the typed log, and those rows have no typed reading.
///
/// The values come from the row's typed event: a field the event
/// declares is read from there, never spelled out here.
fn render_row(out: &mut String, row: &Value) {
    let restored = restore_row(row);
    let Some(tag) = restored.tag() else {
        return;
    };
    match tag {
        RowTag::UserMessage => render_user_message(out, &restored),
        RowTag::AssistantMessage => render_assistant_message(out, &restored),
        RowTag::ToolCall => render_tool_call(out, &restored),
        RowTag::ToolResult => render_tool_result(out, &restored),
        RowTag::SubagentEnter => render_subagent_enter(out, &restored),
        RowTag::SubagentExit => render_subagent_exit(out, &restored),
        RowTag::Turn => render_turn(out, &restored),
        RowTag::Warning => render_warning(out, &restored),
        RowTag::HookBlock => render_hook_block(out, &restored),
        // The rest of the vocabulary: rows the model never saw and
        // rows the transcript already carries in another row.
        //
        // The streamed chunks of a running turn are the sharpest
        // case: the settled `assistant_message` row is the model's
        // copy, and the artefact set is the transcript, not a dump of
        // the log.
        RowTag::AssistantChunk
        | RowTag::Compaction
        | RowTag::CompactionStart
        | RowTag::CompactionSummary
        | RowTag::CompactionEnd
        | RowTag::ToolOutput
        | RowTag::Step
        | RowTag::Iteration
        | RowTag::SteeringGuard
        | RowTag::SteeringHint
        | RowTag::RequestHeader
        | RowTag::ApprovalAsked
        | RowTag::ApprovalDecided
        | RowTag::ApprovalGranted
        | RowTag::ToolDispatch
        | RowTag::RequestSnapshot
        | RowTag::Usage
        | RowTag::SandboxMode
        | RowTag::SessionShutdown
        | RowTag::ToolResultTransformed
        | RowTag::ParkedPrompt
        | RowTag::ParkConsumed
        | RowTag::Feedback
        | RowTag::Plugin => {}
    }
}

/// A user turn: the prompt the round started from. A prompt steered
/// into a live run says so.
fn render_user_message(out: &mut String, row: &CurrentRow<'_>) {
    let Some(SessionEvent::UserMessage { data, source, .. }) = row.event()
    else {
        return;
    };
    let heading = match source.as_deref() {
        Some("steer") => "User (steer)",
        _ => "User",
    };
    section(out, heading, &message_text(&data));
}

/// An assistant turn: the answer the model gave.
fn render_assistant_message(out: &mut String, row: &CurrentRow<'_>) {
    let Some(SessionEvent::AssistantMessage { data, .. }) = row.event() else {
        return;
    };
    section(out, "Assistant", &message_text(&data));
}

/// A tool call: which tool the model asked for, and with which
/// arguments.
fn render_tool_call(out: &mut String, row: &CurrentRow<'_>) {
    let Some(SessionEvent::ToolCall { data, name, .. }) = row.event() else {
        return;
    };
    let name = name.as_deref().unwrap_or_default();
    let _ = writeln!(out, "\n### Tool call `{name}`\n");
    fenced(out, "json", &pretty(Some(&data)));
}

/// A tool result: what the tool said, or that it failed.
fn render_tool_result(out: &mut String, row: &CurrentRow<'_>) {
    let Some(SessionEvent::ToolResult { data, is_error, .. }) = row.event()
    else {
        return;
    };
    let heading = if is_error {
        "### Tool result (failed)\n"
    } else {
        "### Tool result\n"
    };
    let _ = writeln!(out, "\n{heading}");
    let text = message_text(&data);
    let text = if text.trim().is_empty() {
        pretty(Some(&data))
    } else {
        text
    };
    fenced(out, "text", &text);
}

/// A delegation: the child session that was opened, and who ran it.
fn render_subagent_enter(out: &mut String, row: &CurrentRow<'_>) {
    let Some(SessionEvent::SubagentEnter {
        child_session_id,
        parent_session_id,
        agent_name,
        depth,
        ..
    }) = row.event()
    else {
        return;
    };
    let child = child_session_id.as_deref().unwrap_or_default();
    let parent = parent_session_id.as_deref().unwrap_or_default();
    let agent = agent_name.as_deref().unwrap_or_default();
    let depth = depth.unwrap_or_default();
    let _ = writeln!(
        out,
        "\n> sub-agent `{child}` opened: agent `{agent}`, \
         parent `{parent}`, depth {depth}"
    );
}

/// The delegation's end: the child session closed, and how it ended.
fn render_subagent_exit(out: &mut String, row: &CurrentRow<'_>) {
    let Some(SessionEvent::SubagentExit {
        child_session_id,
        status,
        ..
    }) = row.event()
    else {
        return;
    };
    let child = child_session_id.as_deref().unwrap_or_default();
    let status = status.as_deref().unwrap_or_default();
    let _ = writeln!(out, "\n> sub-agent `{child}` closed: {status}");
}

/// A turn boundary: which turn the run was in, and why it ended.
fn render_turn(out: &mut String, row: &CurrentRow<'_>) {
    let Some(SessionEvent::Turn { data, kind, .. }) = row.event() else {
        return;
    };
    let kind = kind.as_deref().unwrap_or_default();
    let reason = data
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if reason.is_empty() {
        let _ = writeln!(out, "\n> turn {kind}");
    } else {
        let _ = writeln!(out, "\n> turn {kind}: {reason}");
    }
}

/// A warning: a fact the runtime could not report any other way.
fn render_warning(out: &mut String, row: &CurrentRow<'_>) {
    let Some(SessionEvent::Warning { data, .. }) = row.event() else {
        return;
    };
    let message = data
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let warning = data
        .get("kind")
        .and_then(Value::as_str)
        .unwrap_or("warning");
    let _ = writeln!(out, "\n> **{warning}**: {message}");
}

/// A veto: which policy refused the call, and why.
fn render_hook_block(out: &mut String, row: &CurrentRow<'_>) {
    let Some(SessionEvent::HookBlock { data, .. }) = row.event() else {
        return;
    };
    let hook = data.get("hook").and_then(Value::as_str).unwrap_or_default();
    let reason = data
        .get("reason")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let tool = data.get("tool").and_then(Value::as_str).unwrap_or_default();
    let _ = writeln!(out, "\n> **blocked** by `{hook}`: `{tool}` — {reason}");
}

/// A `### heading` plus its body.
fn section(out: &mut String, heading: &str, body: &str) {
    let body = body.trim();
    let _ = writeln!(out, "\n### {heading}\n");
    if !body.is_empty() {
        let _ = writeln!(out, "{body}");
    }
}

/// A fenced block. `~~~` keeps row content that already carries
/// backticks readable.
fn fenced(out: &mut String, language: &str, body: &str) {
    let _ = writeln!(out, "~~~{language}");
    let _ = writeln!(out, "{}", body.trim_end());
    let _ = writeln!(out, "~~~");
}

/// The text of a message row's payload, when it is a provider
/// message; the raw payload otherwise.
fn message_text(data: &Value) -> String {
    match uva_provider::Message::deserialize(data) {
        Ok(message) => message.content.extract_text().unwrap_or_default(),
        Err(_) => pretty(Some(data)),
    }
}

/// A JSON value, pretty-printed.
fn pretty(value: Option<&Value>) -> String {
    match value {
        Some(value) => serde_json::to_string_pretty(value)
            .unwrap_or_else(|_| value.to_string()),
        None => String::new(),
    }
}
