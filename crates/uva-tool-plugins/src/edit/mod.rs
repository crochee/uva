//! Agent-facing `edit` tool.
//!
//! This is a self-contained tool plugin depending only on the
//! `uva-tool` paradigm crate.
//!
//! Replace an exact piece of a file's text. The contract is the one
//! the reference harnesses converged on:
//!
//! - **DeepSeek Harness**'s `str_replace_editor`: the old text must
//!   appear **verbatim**; more than one occurrence is refused, and the
//!   refusal names the lines so the model can add context
//!   (`packages/fs/tool-str-replace-editor/src/index.ts:295-320`).
//! - **omp**'s `edit`: the same exact-match rule, plus the two
//!   refusals that make a failed edit explainable — an identical
//!   replacement, and a `replace_all` over whitespace — plus a
//!   "not found" that says *why* it did not match
//!   (`packages/coding-agent/src/edit/magic-tools.ts:191-210`).
//!
//! Why the tool exists: `write` replaces a whole file, so a
//! one-line change means rewriting everything around it. A precise
//! edit is what the model actually means, and it is what keeps a
//! reviewer's diff small.
//!
//! Paths are resolved against `Context::workspace_root` if relative,
//! and must stay inside the workspace — the shared `check_path_safety`
//! guard from `uva_tool::workspace` is enforced. The bytes live
//! wherever the [`FileSystemOwner`] says: the deployment's disk, or
//! the client's own workspace when the client owns it.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use schemars_derive::JsonSchema;
use serde::Deserialize;
use uva_tool::{
    Context,
    ExecutionMode,
    Tool,
    ToolAnnotations,
    ToolOutput,
    workspace::{check_path_safety, resolve_path},
};

use crate::files::{FileError, FileSystemOwner, LocalFileSystem};

/// The `edit` tool brick as a native kernel plugin.
pub mod plugin;

pub use plugin::EditToolPlugin;

#[cfg(test)]
mod tests;

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
struct EditRequest {
    #[schemars(
        description = "Absolute path, or workspace-relative path, of the file to edit."
    )]
    file_path: String,
    #[schemars(
        description = "The exact text to replace. It must appear verbatim in the file; include enough surrounding context to make it unique."
    )]
    old_string: String,
    #[schemars(
        description = "The text that replaces `old_string`. An empty string deletes the matched text."
    )]
    new_string: String,
    #[serde(default)]
    #[schemars(
        extend("default" = false),
        description = "When true, replace every occurrence of `old_string` instead of requiring it to be unique. Default: false."
    )]
    replace_all: Option<bool>,
}

/// `edit` — replace an exact piece of a file's text.
pub struct EditTool {
    owner: Arc<dyn FileSystemOwner>,
}

impl Default for EditTool {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for EditTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EditTool").finish_non_exhaustive()
    }
}

impl EditTool {
    /// The tool over the deployment's own disk.
    #[must_use]
    pub fn new() -> Self {
        Self {
            owner: Arc::new(LocalFileSystem),
        }
    }

    /// The tool over `owner`: the files it edits live wherever the
    /// owner says they do.
    ///
    /// A client that owns the workspace installs an owner that asks
    /// the client — so the model edits the user's files, not the
    /// deployment's copy of them.
    #[must_use]
    pub fn with_owner(owner: Arc<dyn FileSystemOwner>) -> Self {
        Self { owner }
    }
}

/// Where the old text sits, and how many times.
struct Matches {
    /// The byte offsets of every occurrence, in order.
    offsets: Vec<usize>,
}

impl Matches {
    /// The 1-based line of every occurrence.
    fn lines(&self, text: &str) -> Vec<usize> {
        let mut lines = Vec::with_capacity(self.offsets.len());
        let mut line = 1;
        let mut cursor = 0;
        for offset in &self.offsets {
            for byte in text.as_bytes()[cursor..*offset].iter() {
                if *byte == b'\n' {
                    line += 1;
                }
            }
            lines.push(line);
            cursor = *offset;
        }
        lines
    }
}

/// Every occurrence of `needle` in `haystack`, as byte offsets.
fn find_all(haystack: &str, needle: &str) -> Matches {
    let mut offsets = Vec::new();
    if needle.is_empty() {
        return Matches { offsets };
    }
    let mut cursor = 0;
    while let Some(found) = haystack[cursor..].find(needle) {
        let offset = cursor + found;
        offsets.push(offset);
        cursor = offset + needle.len();
    }
    Matches { offsets }
}

/// The old text, quoted for the model's eyes.
///
/// The text can be long; the model needs to see enough of it to
/// recognise what it copied, not a wall of it.
fn quote_for_the_model(text: &str) -> String {
    const LIMIT: usize = 200;
    if text.chars().count() <= LIMIT {
        return format!("`{text}`");
    }
    let head: String = text.chars().take(LIMIT).collect();
    format!("`{head}…`")
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        "edit"
    }

    fn description(&self) -> &str {
        "Replace an exact piece of a file's text. `old_string` must appear \
         verbatim (whitespace matters) and, unless `replace_all` is true, \
         exactly once — a file whose old text appears more than once is \
         refused with the lines it appears on, so the model can add \
         context. `new_string` replaces it; an empty `new_string` deletes \
         the matched text."
    }

    fn parameters(&self) -> serde_json::Value {
        // Schema is generated from `EditRequest` via `schemars`, so
        // the type and the LLM-facing schema cannot drift.
        serde_json::to_value(schemars::schema_for!(EditRequest))
            .expect("EditRequest schema is always serializable")
    }

    /// Mutating tool: the loop must never run two of these at once.
    ///
    /// An edit is a read-modify-write (`read` → match → `write`), so
    /// two calls in one LLM pass sharing a target path would lose one
    /// of them — the same reason `write` with `mode = "append"` runs
    /// sequentially.
    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    /// MCP-native descriptor hints for permission gates.
    ///
    /// `edit` mutates the workspace, and it is not idempotent: the
    /// second run of the same call finds no old text and is refused.
    /// It is not destructive either — it replaces the text it was
    /// given and nothing else.
    fn annotations(&self) -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
        })
    }

    async fn call(
        &self,
        input: serde_json::Value,
        context: &Context,
    ) -> ToolOutput {
        let request: EditRequest = match serde_json::from_value(input) {
            Ok(r) => r,
            Err(e) => {
                return ToolOutput::error(format!("Invalid arguments: {e}"));
            }
        };

        // The refusals that make a failed edit explainable, before
        // anything is read: an empty or identical replacement cannot
        // be a change, and replacing whitespace everywhere is never
        // what the model meant.
        if request.old_string.is_empty() {
            return ToolOutput::error(
                "`old_string` is empty: there is nothing to replace.",
            );
        }
        if request.old_string == request.new_string {
            return ToolOutput::error(
                "`old_string` and `new_string` are identical: the file \
                 would not change.",
            );
        }
        let replace_all = request.replace_all.unwrap_or(false);
        if replace_all && request.old_string.trim().is_empty() {
            return ToolOutput::error(
                "`replace_all` cannot replace whitespace-only text: it \
                 would match everywhere.",
            );
        }

        // The owner decides where this session's files live: the
        // deployment's workspace, or the client's own directory when
        // the client owns the workspace.
        let workspace_root = self
            .owner
            .session_root(&context.session_id)
            .unwrap_or_else(|| context.workspace_root.clone());
        if let Some(err) =
            check_path_safety(&workspace_root, &request.file_path)
        {
            return ToolOutput::error(err);
        }
        let resolved: PathBuf =
            resolve_path(&workspace_root, &request.file_path);

        let bytes =
            match self.owner.read_file(&context.session_id, &resolved).await {
                Ok(bytes) => bytes,
                Err(FileError::Missing) => {
                    return ToolOutput::error(format!(
                        "The file '{}' does not exist. Use `write` to create \
                     it.",
                        resolved.display()
                    ));
                }
                Err(FileError::Failed(message)) => {
                    return ToolOutput::error(format!(
                        "The file '{}' could not be read: {message}",
                        resolved.display()
                    ));
                }
            };
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => {
                return ToolOutput::error(format!(
                    "The file '{}' is not valid UTF-8 and cannot be \
                     edited: {error}",
                    resolved.display()
                ));
            }
        };

        let matches = find_all(&text, &request.old_string);
        if matches.offsets.is_empty() {
            return ToolOutput::error(self.explain_miss(
                &resolved,
                &text,
                &request.old_string,
                &request.new_string,
            ));
        }
        if matches.offsets.len() > 1 && !replace_all {
            let lines = matches.lines(&text);
            return ToolOutput::error(format!(
                "The old text appears {} times in '{}' (lines {}). Include \
                 more context so it is unique, or set `replace_all` to true \
                 to replace every occurrence.",
                matches.offsets.len(),
                resolved.display(),
                lines
                    .iter()
                    .map(usize::to_string)
                    .collect::<Vec<String>>()
                    .join(", "),
            ));
        }

        // The replacement: every occurrence, or the first one only.
        let mut updated = String::with_capacity(text.len());
        let mut cursor = 0;
        for offset in &matches.offsets {
            updated.push_str(&text[cursor..*offset]);
            updated.push_str(&request.new_string);
            cursor = offset + request.old_string.len();
        }
        updated.push_str(&text[cursor..]);

        if let Err(error) = self
            .owner
            .write_file(
                &context.session_id,
                &resolved,
                updated.as_bytes(),
                false,
            )
            .await
        {
            return ToolOutput::error(format!(
                "The file '{}' could not be written: {error}",
                resolved.display()
            ));
        }

        let lines = matches.lines(&text);
        let summary = if replace_all {
            format!(
                "Replaced {} occurrences in '{}'.",
                matches.offsets.len(),
                resolved.display()
            )
        } else {
            format!(
                "Replaced the text on line {} of '{}'.",
                lines[0],
                resolved.display()
            )
        };
        ToolOutput::text(summary)
    }
}

impl EditTool {
    /// Why the old text did not match.
    ///
    /// The model gets one of two next moves: the change is already
    /// there, or the text it copied is not the file's text. Naming
    /// the difference is the difference between a model that retries
    /// correctly and one that edits the wrong thing.
    fn explain_miss(
        &self,
        path: &std::path::Path,
        text: &str,
        old_string: &str,
        new_string: &str,
    ) -> String {
        if !new_string.is_empty() && text.contains(new_string) {
            return format!(
                "The old text does not appear in '{}', and the new text \
                 already does: this change is already applied.",
                path.display()
            );
        }
        format!(
            "The old text does not appear verbatim in '{}':\n{}\nRead the \
             file again and copy the exact text — whitespace and indentation \
             matter.",
            path.display(),
            quote_for_the_model(old_string),
        )
    }
}
