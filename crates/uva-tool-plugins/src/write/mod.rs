//! Agent-facing `write` tool.
//!
//! This is a self-contained tool plugin depending only on the
//! `uva-tool` paradigm crate.
//!
//! Create or overwrite a workspace file. Supports two write modes:
//! - `overwrite` (default): replace the file's contents entirely.
//! - `append`: append to the existing file's contents (no-op on
//!   non-existent files; the parent directory is created when
//!   `create_directories = true`).
//!
//! Paths are resolved against `Context::workspace_root` if relative,
//! and must stay inside the workspace — the shared `check_path_safety`
//! guard from `uva_tool::workspace` is enforced.

use std::{path::PathBuf, sync::Arc};

use async_trait::async_trait;
use schemars_derive::JsonSchema;
use serde::Deserialize;
use uva_tool::{
    Context,
    DIFF_METADATA_KEY,
    ExecutionMode,
    FileDiff,
    Tool,
    ToolAnnotations,
    ToolLocation,
    ToolOutput,
    workspace::{check_path_safety, resolve_path},
};

use crate::files::{FileError, FileSystemOwner, LocalFileSystem};

/// The `write` tool brick as a native kernel plugin.
pub mod plugin;

pub use plugin::WriteToolPlugin;

/// Write mode for [`WriteTool`].
///
/// Serializes as `"overwrite"` / `"append"` (snake_case).
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema,
)]
#[serde(rename_all = "snake_case")]
enum WriteMode {
    #[default]
    Overwrite,
    Append,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
struct WriteRequest {
    #[schemars(
        description = "Absolute path, or workspace-relative path, of the file to write."
    )]
    file_path: String,
    #[schemars(description = "Text content to write.")]
    content: String,
    #[serde(default)]
    #[schemars(
        extend("default" = "overwrite"),
        description = "Write mode. `overwrite` (default) replaces the file's contents; `append` appends to the existing file (creates it if missing)."
    )]
    mode: Option<WriteMode>,
    #[serde(default)]
    #[schemars(
        extend("default" = true),
        description = "When true (default), missing parent directories are created automatically. Set to false to require the directory to exist."
    )]
    create_directories: Option<bool>,
}

/// `write` — create or append to a workspace file.
pub struct WriteTool {
    owner: Arc<dyn FileSystemOwner>,
}

impl Default for WriteTool {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for WriteTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WriteTool").finish_non_exhaustive()
    }
}

impl WriteTool {
    /// The tool over the deployment's own disk.
    #[must_use]
    pub fn new() -> Self {
        Self {
            owner: Arc::new(LocalFileSystem),
        }
    }

    /// The tool over `owner`: the files it writes live wherever the
    /// owner says they do.
    ///
    /// A client that owns the workspace installs an owner that asks
    /// the client — so the model writes the user's files, not the
    /// deployment's copy of them.
    #[must_use]
    pub fn with_owner(owner: Arc<dyn FileSystemOwner>) -> Self {
        Self { owner }
    }
}

impl WriteTool {
    /// The file this call touches, resolved the way the call resolves
    /// it.
    ///
    /// The owner decides where this session's files live (the
    /// deployment's workspace, or the client's own directory when the
    /// client owns it), and the path must stay inside that root.
    /// The published location and the file the call touches come from
    /// this one answer, so they cannot drift apart.
    fn resolve(
        &self,
        file_path: &str,
        context: &Context,
    ) -> Result<PathBuf, String> {
        let root = self
            .owner
            .session_root(&context.session_id)
            .unwrap_or_else(|| context.workspace_root.clone());
        match check_path_safety(&root, file_path) {
            Some(error) => Err(error),
            None => Ok(resolve_path(&root, file_path)),
        }
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        "write"
    }

    fn description(&self) -> &str {
        "Create or append to a workspace file. Default `mode` is \
         `overwrite` (replaces contents); `mode=append` appends to the \
         existing file. With `create_directories=true` (default), missing \
         parent directories are created automatically."
    }

    fn parameters(&self) -> serde_json::Value {
        // Schema is generated from `WriteRequest` via `schemars`,
        // so the type and the LLM-facing schema cannot drift —
        // including `additionalProperties: false` and the
        // `mode` / `create_directories` defaults, all declared
        // inline via `#[schemars(extend(...))]`.
        serde_json::to_value(schemars::schema_for!(WriteRequest))
            .expect("WriteRequest schema is always serializable")
    }

    /// R17: write results are path + byte-count; a UI can show
    /// a compact "wrote N bytes to P" card.
    fn output_definition(&self) -> uva_tool::output::ToolOutputDefinition {
        uva_tool::output::ToolOutputDefinition::passthrough("write")
            .with_kind(uva_tool::output::RenderKind::Write)
            .with_title("Write")
    }

    /// Mutating tool: the loop must never run two of these at once.
    ///
    /// `mode = "append"` is a read-modify-write (`read_to_string` →
    /// build → `fs::write`), so two calls in one LLM pass sharing a
    /// target path would lose one write — reproduced against the real
    /// tool: two concurrent `append` calls to one path left only the
    /// second body on disk. `mode = "overwrite"` is one truncate+write
    /// per call, so a reader can observe the intermediate empty file.
    /// [`ExecutionMode::Sequential`] is the framework's answer to both:
    /// the loop runs the Parallel bucket to completion, then this bucket
    /// strictly one call at a time — the same rule `shell`, `web_fetch`
    /// and the MCP proxy tools follow.
    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    /// R124: MCP-native descriptor hints for permission gates.
    /// `write` mutates the workspace; `append` is not idempotent
    /// (each call concatenates), `overwrite` is destructive (each
    /// call replaces), so the conservative aggregate is
    /// `destructive = true, idempotent = false`. No network.
    fn annotations(&self) -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(true),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
        })
    }

    fn locations(
        &self,
        input: &serde_json::Value,
        context: &Context,
    ) -> Vec<ToolLocation> {
        let Ok(request) = serde_json::from_value::<WriteRequest>(input.clone())
        else {
            return Vec::new();
        };
        // The file the call really touches: the resolution is the
        // call's own, so the published location cannot drift from
        // the file it reads or writes.
        match self.resolve(&request.file_path, context) {
            Ok(path) => vec![ToolLocation {
                path: path.display().to_string(),
            }],
            Err(_) => Vec::new(),
        }
    }

    async fn call(
        &self,
        input: serde_json::Value,
        context: &Context,
    ) -> ToolOutput {
        let request: WriteRequest = match serde_json::from_value(input) {
            Ok(r) => r,
            Err(e) => {
                return ToolOutput::error(format!("Invalid arguments: {e}"));
            }
        };

        // The owner decides where this session's files live: the
        // deployment's workspace, or the client's own directory when
        // the client owns the workspace — and the path must stay
        // inside that root. The resolution is the tool's own, so the
        // published location and the file the call touches agree.
        let resolved = match self.resolve(&request.file_path, context) {
            Ok(path) => path,
            Err(error) => return ToolOutput::error(error),
        };

        let mode = request.mode.unwrap_or_default();
        let should_create_dirs = request.create_directories.unwrap_or(true);

        // Whether the file was there, and what it held: the owner
        // answers, because the file may live on the client and not on
        // this disk. One read serves both questions.
        let prior =
            match self.owner.read_file(&context.session_id, &resolved).await {
                Ok(bytes) => Some(bytes),
                Err(FileError::Missing) => None,
                // A read that failed for another reason does not decide
                // the question; the write below will say what happened.
                Err(_) => None,
            };
        let existed = prior.is_some();
        // The prior content as text, for the change report: a file
        // that is not valid UTF-8 has no text to show (overwrite mode
        // does not care; append mode refuses below).
        let prior_text: Option<String> = prior
            .as_ref()
            .and_then(|bytes| std::str::from_utf8(bytes).ok())
            .map(str::to_owned);
        let new_text = if mode == WriteMode::Append {
            let prior = match prior {
                Some(bytes) => match String::from_utf8(bytes) {
                    Ok(text) => text,
                    Err(e) => {
                        return ToolOutput::error(format!(
                            "Failed to read existing file '{}' for \
                             append: {e}",
                            resolved.display()
                        ));
                    }
                },
                None => String::new(),
            };
            format!("{prior}{}", request.content)
        } else {
            request.content
        };

        if let Err(error) = self
            .owner
            .write_file(
                &context.session_id,
                &resolved,
                new_text.as_bytes(),
                should_create_dirs,
            )
            .await
        {
            return ToolOutput::error(format!(
                "Failed to write file '{}': {}",
                resolved.display(),
                error
            ));
        }

        let action = if existed && mode == WriteMode::Append {
            "Appended to"
        } else if existed {
            "Updated"
        } else {
            "Created"
        };
        // The change the tool made, as data: the client renders it as
        // a diff, the model reads the sentence. The old text is the
        // file's prior content — a file that did not exist has none.
        ToolOutput::text(format!("{action} file: {}", resolved.display()))
            .with_metadata(
                DIFF_METADATA_KEY,
                FileDiff::new(
                    resolved.display().to_string(),
                    prior_text,
                    new_text.clone(),
                )
                .to_metadata(),
            )
    }
}

#[cfg(test)]
mod tests;
