//! Agent-facing `grep` tool: search the files' contents.
//!
//! The reference harnesses both ship this surface: dsh's
//! `tool-fs-search` registers `grep` next to `glob`
//! (`packages/fs/tool-fs-search/src/grep.ts`), and omp lists `grep`
//! among its built-ins (`packages/coding-agent/src/tools/builtin-names.ts`).
//! dsh runs it through a bundled ripgrep; this crate walks the tree
//! and matches the lines itself, so the answer needs no external
//! binary and the walk stays inside the workspace.
//!
//! The matches come back as `path:line:text` ✓ — the shape ripgrep
//! prints ✓, and the shape a model already reads.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use regex::Regex;
use schemars_derive::JsonSchema;
use serde::Deserialize;
use uva_tool::{
    Context,
    ExecutionMode,
    Tool,
    ToolAnnotations,
    ToolOutput,
    ToolOutputDefinition,
    workspace::{check_path_safety, resolve_path},
};

use crate::files::{FileError, FileSystemOwner, LocalFileSystem};

/// The `grep` tool brick as a native kernel plugin.
pub mod plugin;

pub use plugin::GrepToolPlugin;

#[cfg(test)]
mod tests;

/// The most matches one call answers (dsh's `GREP_MAX_MATCHES` ✓).
pub const GREP_MAX_MATCHES: usize = 250;

/// The most bytes one matched line shows (dsh's `GREP_MAX_LINE_BYTES` ✓).
pub const GREP_MAX_LINE_BYTES: usize = 2000;

/// The most bytes one file contributes to the search ✓.
///
/// A file bigger than this is skipped: a model that greps a build
/// artefact wants the source, not the artefact ✓.
const GREP_MAX_FILE_BYTES: u64 = 2 * 1024 * 1024;

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
struct GrepRequest {
    #[schemars(
        description = "The regular expression to search for, e.g. `fn\\s+main` or `TODO:`."
    )]
    pattern: String,
    #[schemars(
        description = "The directory to search under, relative to the workspace root. Defaults to the workspace root."
    )]
    #[serde(default)]
    path: Option<String>,
    #[schemars(
        description = "Only search the files whose name matches this glob, e.g. `*.rs`."
    )]
    #[serde(default)]
    include: Option<String>,
}

/// `grep` — search the files' contents.
pub struct GrepTool {
    owner: Arc<dyn FileSystemOwner>,
}

impl Default for GrepTool {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for GrepTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GrepTool").finish_non_exhaustive()
    }
}

impl GrepTool {
    /// The tool over the deployment's own disk.
    #[must_use]
    pub fn new() -> Self {
        Self {
            owner: Arc::new(LocalFileSystem),
        }
    }

    /// The tool over `owner`: the files it reads live wherever the
    /// owner says they do.
    #[must_use]
    pub fn with_owner(owner: Arc<dyn FileSystemOwner>) -> Self {
        Self { owner }
    }
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }

    fn description(&self) -> &str {
        "Search the files' contents with a regular expression. The \
         matches come back as `path:line:text`, the shape a file search \
         prints."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(GrepRequest))
            .expect("GrepRequest schema is always serializable")
    }

    fn output_definition(&self) -> ToolOutputDefinition {
        ToolOutputDefinition::passthrough("grep")
            .with_kind(uva_tool::output::RenderKind::Text)
            .with_title("Grep")
    }

    /// Search is read-only ✓ — it may run beside its siblings ✓.
    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Parallel
    }

    fn annotations(&self) -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(true),
            destructive_hint: Some(false),
            idempotent_hint: Some(true),
            open_world_hint: Some(false),
        })
    }

    async fn call(
        &self,
        input: serde_json::Value,
        context: &Context,
    ) -> ToolOutput {
        let request: GrepRequest = match serde_json::from_value(input) {
            Ok(r) => r,
            Err(e) => {
                return ToolOutput::error(format!("Invalid arguments: {e}"));
            }
        };
        if request.pattern.is_empty() {
            return ToolOutput::error(
                "`pattern` is empty: there is nothing to search for.",
            );
        }
        let pattern = match Regex::new(&request.pattern) {
            Ok(pattern) => pattern,
            Err(error) => {
                return ToolOutput::error(format!(
                    "`pattern` is not a valid regular expression: {error}"
                ));
            }
        };

        // The owner decides where this session's files live. A
        // client-owned workspace cannot be searched: the ACP file
        // methods read and write named files, they do not list a
        // directory — so the answer would be a lie.
        if self.owner.session_root(&context.session_id).is_some() {
            return ToolOutput::error(
                "This session's files live on the client, and the client \
                 does not offer a directory listing: the search cannot \
                 see them. Ask the client for the file names instead.",
            );
        }

        let workspace_root = context.workspace_root.clone();
        let root: PathBuf = match request.path.as_deref() {
            Some(path) => {
                if let Some(error) = check_path_safety(&workspace_root, path) {
                    return ToolOutput::error(error);
                }
                resolve_path(&workspace_root, path)
            }
            None => workspace_root.clone(),
        };
        if !root.is_dir() {
            return ToolOutput::error(format!(
                "'{}' is not a directory.",
                root.display()
            ));
        }

        let walk = crate::walk::files_under(&root);
        let mut matches: Vec<String> = Vec::new();
        let mut searched = 0usize;
        let mut truncated_matches = false;
        for file in &walk.files {
            if let Some(include) = request.include.as_deref()
                && !crate::walk::matches_glob(include, &file.relative)
            {
                continue;
            }
            let bytes = match self
                .owner
                .read_file(&context.session_id, &file.path)
                .await
            {
                Ok(bytes) => bytes,
                // A file that cannot be read is skipped ✓ — the
                // search answers for the files it could read ✓.
                Err(FileError::Missing) => continue,
                Err(FileError::Failed(_)) => continue,
            };
            if bytes.len() as u64 > GREP_MAX_FILE_BYTES {
                continue;
            }
            let Ok(text) = String::from_utf8(bytes) else {
                // A binary file is not text: it is skipped ✓.
                continue;
            };
            searched += 1;
            for (index, line) in text.lines().enumerate() {
                if !pattern.is_match(line) {
                    continue;
                }
                if matches.len() >= GREP_MAX_MATCHES {
                    truncated_matches = true;
                    break;
                }
                matches.push(format!(
                    "{}:{}:{}",
                    file.relative,
                    index + 1,
                    truncate_line(line)
                ));
            }
            if truncated_matches {
                break;
            }
        }

        if matches.is_empty() {
            let mut body = format!("No line matches `{}`.", request.pattern);
            if walk.truncated {
                body.push_str(
                    " The search stopped at its file bound, so a match \
                     outside the searched files is possible.",
                );
            }
            return ToolOutput::text(body);
        }
        let mut body = matches.join("\n");
        if truncated_matches {
            body.push_str(&format!(
                "\n\n(More matches exist; the answer stops at {} of \
                 them.)",
                GREP_MAX_MATCHES
            ));
        } else if walk.truncated {
            body.push_str(
                "\n\n(The search stopped at its file bound, so more \
                 matches may exist outside the searched files.)",
            );
        }
        let _ = searched;
        ToolOutput::text(body)
    }
}

/// The matched line, shortened to the answer's line budget ✓.
fn truncate_line(line: &str) -> String {
    if line.len() <= GREP_MAX_LINE_BYTES {
        return line.to_string();
    }
    let mut end = GREP_MAX_LINE_BYTES;
    while end > 0 && !line.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &line[..end])
}
