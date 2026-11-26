//! Agent-facing `glob` tool: find the files that match a pattern.
//!
//! The reference harnesses both ship this surface: dsh's
//! `tool-fs-search` registers `glob` next to `grep`
//! (`packages/fs/tool-fs-search/src/glob.ts`), and omp lists `glob`
//! among its built-ins (`packages/coding-agent/src/tools/builtin-names.ts`).
//! dsh runs it through a bundled ripgrep; this crate walks the tree
//! itself, so the answer needs no external binary and the walk stays
//! inside the workspace.
//!
//! The paths come back relative to the search root ✓ — the text the
//! model reads is the text it can hand to `read` ✓.

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
    ToolOutputDefinition,
    workspace::{check_path_safety, resolve_path},
};

use crate::files::{FileSystemOwner, LocalFileSystem};

/// The `glob` tool brick as a native kernel plugin.
pub mod plugin;

pub use plugin::GlobToolPlugin;

#[cfg(test)]
mod tests;

/// The most paths one call answers (dsh's `GLOB_MAX_RESULTS` ✓).
pub const GLOB_MAX_RESULTS: usize = 100;

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
struct GlobRequest {
    #[schemars(
        description = "The glob to match, e.g. `**/*.rs` or `src/*.ts`. `*` matches inside one path segment, `**` matches any number of segments."
    )]
    pattern: String,
    #[schemars(
        description = "The directory to search under, relative to the workspace root. Defaults to the workspace root."
    )]
    #[serde(default)]
    path: Option<String>,
}

/// `glob` — find the files that match a pattern.
pub struct GlobTool {
    owner: Arc<dyn FileSystemOwner>,
}

impl Default for GlobTool {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for GlobTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GlobTool").finish_non_exhaustive()
    }
}

impl GlobTool {
    /// The tool over the deployment's own disk.
    #[must_use]
    pub fn new() -> Self {
        Self {
            owner: Arc::new(LocalFileSystem),
        }
    }

    /// The tool over `owner`: the files it finds live wherever the
    /// owner says they do.
    #[must_use]
    pub fn with_owner(owner: Arc<dyn FileSystemOwner>) -> Self {
        Self { owner }
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        "glob"
    }

    fn description(&self) -> &str {
        "Find the files whose path matches a glob (`**/*.rs`, `src/*.ts`). \
         The paths come back relative to the search root, newest first."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(GlobRequest))
            .expect("GlobRequest schema is always serializable")
    }

    fn output_definition(&self) -> ToolOutputDefinition {
        ToolOutputDefinition::passthrough("glob")
            .with_kind(uva_tool::output::RenderKind::Text)
            .with_title("Glob")
    }

    /// Discovery is read-only ✓ — it may run beside its siblings ✓.
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
        let request: GlobRequest = match serde_json::from_value(input) {
            Ok(r) => r,
            Err(e) => {
                return ToolOutput::error(format!("Invalid arguments: {e}"));
            }
        };
        if request.pattern.trim().is_empty() {
            return ToolOutput::error(
                "`pattern` is empty: there is nothing to match.",
            );
        }

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
        let mut matches: Vec<crate::walk::WalkedFile> = walk
            .files
            .into_iter()
            .filter(|file| {
                crate::walk::matches_glob(&request.pattern, &file.relative)
            })
            .collect();
        // Newest first ✓ (dsh's `--sort=modified` ✓).
        matches.sort_by(|left, right| {
            right
                .modified
                .cmp(&left.modified)
                .then_with(|| left.relative.cmp(&right.relative))
        });
        matches.truncate(GLOB_MAX_RESULTS);

        if matches.is_empty() {
            let mut body = format!("No file matches `{}`.", request.pattern);
            if walk.truncated {
                body.push_str(
                    " The search stopped at its file bound, so a match \
                     outside the searched files is possible.",
                );
            }
            return ToolOutput::text(body);
        }
        let mut body = matches
            .iter()
            .map(|file| file.relative.clone())
            .collect::<Vec<String>>()
            .join("\n");
        if walk.truncated {
            body.push_str(
                "\n\n(The search stopped at its file bound, so more \
                 matches may exist outside the searched files.)",
            );
        }
        ToolOutput::text(body)
    }
}
