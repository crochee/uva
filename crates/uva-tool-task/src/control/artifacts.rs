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

use serde_json::Value;

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
    pub patch: Option<String>,
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
        .as_deref()
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

/// One transcript row, as markdown. Rows the model never saw (partial
/// chunks, usage counters, request headers) are left out: the set is a
/// transcript, not a dump.
fn render_row(out: &mut String, row: &Value) {
    let kind = row.get("type").and_then(Value::as_str).unwrap_or_default();
    match kind {
        "user_message" => {
            let source = row.get("source").and_then(Value::as_str);
            let heading = match source {
                Some("steer") => "User (steer)",
                _ => "User",
            };
            section(out, heading, &message_text(row));
        }
        "assistant_message" => {
            section(out, "Assistant", &message_text(row));
        }
        "tool_call" => {
            let name = row.get("name").and_then(Value::as_str).unwrap_or("");
            let _ = writeln!(out, "\n### Tool call `{name}`\n");
            fenced(out, "json", &pretty(row.get("data")));
        }
        "tool_result" => {
            let failed = row.get("is_error").and_then(Value::as_bool);
            let heading = if failed == Some(true) {
                "### Tool result (failed)\n"
            } else {
                "### Tool result\n"
            };
            let _ = writeln!(out, "\n{heading}");
            let text = message_text(row);
            let text = if text.trim().is_empty() {
                pretty(row.get("data"))
            } else {
                text
            };
            fenced(out, "text", &text);
        }
        "subagent_enter" => {
            let child = string(row, "child_session_id");
            let parent = string(row, "parent_session_id");
            let agent = string(row, "agent_name");
            let depth = row.get("depth").and_then(Value::as_u64).unwrap_or(0);
            let _ = writeln!(
                out,
                "\n> sub-agent `{child}` opened: agent `{agent}`, \
                 parent `{parent}`, depth {depth}"
            );
        }
        "subagent_exit" => {
            let child = string(row, "child_session_id");
            let status = string(row, "status");
            let _ = writeln!(out, "\n> sub-agent `{child}` closed: {status}");
        }
        "turn" => {
            let kind = row
                .pointer("/data/kind")
                .and_then(Value::as_str)
                .unwrap_or("");
            let reason = row
                .pointer("/data/reason")
                .and_then(Value::as_str)
                .unwrap_or("");
            if reason.is_empty() {
                let _ = writeln!(out, "\n> turn {kind}");
            } else {
                let _ = writeln!(out, "\n> turn {kind}: {reason}");
            }
        }
        "warning" => {
            let message = row
                .pointer("/data/message")
                .and_then(Value::as_str)
                .unwrap_or("");
            let warning = row
                .pointer("/data/kind")
                .and_then(Value::as_str)
                .unwrap_or("warning");
            let _ = writeln!(out, "\n> **{warning}**: {message}");
        }
        "hook_block" => {
            let hook = string(row, "hook");
            let reason = string(row, "reason");
            let tool = string(row, "tool");
            let _ = writeln!(
                out,
                "\n> **blocked** by `{hook}`: `{tool}` — {reason}"
            );
        }
        _ => {}
    }
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
fn message_text(row: &Value) -> String {
    let Some(data) = row.get("data") else {
        return String::new();
    };
    match serde_json::from_value::<uva_provider::Message>(data.clone()) {
        Ok(message) => message.content.extract_text().unwrap_or_default(),
        Err(_) => pretty(Some(data)),
    }
}

/// A row field that is either a top-level string or a string inside
/// `data`.
fn string(row: &Value, key: &str) -> String {
    row.get(key)
        .and_then(Value::as_str)
        .or_else(|| {
            row.pointer(&format!("/data/{key}")).and_then(Value::as_str)
        })
        .unwrap_or_default()
        .to_string()
}

/// A JSON value, pretty-printed.
fn pretty(value: Option<&Value>) -> String {
    match value {
        Some(value) => serde_json::to_string_pretty(value)
            .unwrap_or_else(|_| value.to_string()),
        None => String::new(),
    }
}
