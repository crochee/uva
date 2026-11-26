//! Agent-facing `lsp` tool: what the compiler says about a file.
//!
//! The reference harnesses both ship a language-server surface — dsh's
//! `tool-lsp` (four query operations over `ctx.lsp`,
//! `packages/lsp/tool-lsp/src/index.ts`) and omp's `lsp` tool
//! (`packages/coding-agent/src/lsp/tool.ts`). This tool speaks the
//! same surface: the **diagnostics** the compiler published ✓ and the
//! four **navigation** operations ✓ — where a symbol is defined ✓,
//! where it is used ✓, what implements it ✓, and what the server
//! says about it ✓.
//!
//! The server is the deployment's: a language, a command, its
//! arguments ✓ (the `[lsp]` section of the server configuration ✓).
//! The server is started for the workspace, asked about one file, and
//! shut down ✓ — the tool owns the server's life for the call ✓.
//!
//! The tool answers with what the server published ✓ — the severity,
//! the place and the message ✓ — and says plainly when the server
//! said nothing ✓: a model that reads "no diagnostics" must be able
//! to trust it, so a server that failed is named as a failure ✓.

use std::{path::PathBuf, sync::Arc, time::Duration};

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

/// The `lsp` tool brick as a native kernel plugin.
pub mod plugin;

pub use plugin::LspToolPlugin;

/// The client half of the tool ✓.
pub mod client;

pub use client::{Diagnostic, Diagnostics, LspError};

#[cfg(test)]
mod tests;

/// One language server the deployment knows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LspServer {
    /// The language this server serves (`"rust"`, `"typescript"`, …) ✓.
    pub language: String,
    /// The command that starts the server ✓.
    pub command: String,
    /// The arguments the command takes ✓.
    pub args: Vec<String>,
}

impl LspServer {
    /// The server for `language`, when the deployment knows one ✓.
    pub fn for_language<'a>(
        servers: &'a [LspServer],
        language: &str,
    ) -> Option<&'a LspServer> {
        servers.iter().find(|server| server.language == language)
    }
}

/// How the tool finds the servers ✓.
pub trait LspServers: Send + Sync {
    /// Every server the deployment knows ✓.
    fn servers(&self) -> Vec<LspServer>;
}

/// What the tool is asked ✓.
///
/// The names are dsh's ✓ — the model reads the same word the
/// reference harness's tool publishes ✓.
#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
enum Operation {
    /// The compiler's diagnostics for the file ✓.
    #[default]
    Diagnostics,
    /// Where the symbol under the cursor is defined ✓.
    #[serde(alias = "definition")]
    GoToDefinition,
    /// Where the symbol under the cursor is used ✓.
    #[serde(alias = "references")]
    FindReferences,
    /// What implements the trait under the cursor ✓.
    #[serde(alias = "implementation")]
    GoToImplementation,
    /// What the server says about the symbol under the cursor ✓.
    Hover,
}

impl Operation {
    /// The protocol's operation, when this one is a navigation ✓.
    fn navigation(self) -> Option<client::Operation> {
        match self {
            Self::Diagnostics => None,
            Self::GoToDefinition => Some(client::Operation::Definition),
            Self::FindReferences => Some(client::Operation::References),
            Self::GoToImplementation => Some(client::Operation::Implementation),
            Self::Hover => Some(client::Operation::Hover),
        }
    }
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
struct LspRequest {
    #[schemars(
        description = "The file to ask about, relative to the workspace root or absolute."
    )]
    file_path: String,
    #[schemars(
        description = "The file's language, e.g. `rust` or `typescript`. The deployment's configured language server answers for it."
    )]
    language: String,
    #[schemars(
        description = "What to ask: `diagnostics` (the compiler's words), `goToDefinition`, `findReferences`, `goToImplementation` or `hover`. Defaults to `diagnostics`."
    )]
    #[serde(default)]
    operation: Operation,
    #[schemars(
        description = "The cursor's 1-based line — required by every navigation operation."
    )]
    #[serde(default)]
    line: Option<usize>,
    #[schemars(
        description = "The cursor's 1-based column — required by every navigation operation."
    )]
    #[serde(default)]
    character: Option<usize>,
    #[schemars(
        extend("default" = 20),
        description = "How long to wait for the server's diagnostics, in seconds. Capped at 120."
    )]
    #[serde(default)]
    timeout_secs: Option<u64>,
}

/// The longest a diagnostic wait runs ✓.
const MAX_WAIT_SECS: u64 = 120;

/// `lsp` — the compiler's diagnostics for one file.
pub struct LspTool {
    servers: Arc<dyn LspServers>,
    owner: Arc<dyn FileSystemOwner>,
}

impl std::fmt::Debug for LspTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LspTool").finish_non_exhaustive()
    }
}

impl LspTool {
    /// The tool over `servers` and the deployment's own disk ✓.
    #[must_use]
    pub fn new(servers: Arc<dyn LspServers>) -> Self {
        Self {
            servers,
            owner: Arc::new(LocalFileSystem),
        }
    }

    /// The tool over `owner`: the file it reads lives wherever the
    /// owner says it does ✓.
    #[must_use]
    pub fn with_owner(mut self, owner: Arc<dyn FileSystemOwner>) -> Self {
        self.owner = owner;
        self
    }
}

#[async_trait]
impl Tool for LspTool {
    fn name(&self) -> &str {
        "lsp"
    }

    fn description(&self) -> &str {
        "Ask the deployment's language server about a file. \
         `diagnostics` answers with the compiler's own words — the \
         severity, the place and the message — so the model reads \
         them instead of grepping for them. The navigation operations \
         answer with the places the server pointed at: \
         `goToDefinition`, `findReferences` (the declaration \
         included), `goToImplementation`, and `hover` (what the \
         server says about the symbol). Every navigation operation \
         takes the cursor's 1-based `line` and `character`."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(LspRequest))
            .expect("LspRequest schema is always serializable")
    }

    fn output_definition(&self) -> ToolOutputDefinition {
        ToolOutputDefinition::passthrough("lsp")
            .with_kind(uva_tool::output::RenderKind::Text)
            .with_title("Diagnostics")
    }

    /// Asking a language server is read-only, but it starts a process:
    /// the tool runs alone, never beside another call ✓.
    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
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
        let request: LspRequest = match serde_json::from_value(input) {
            Ok(r) => r,
            Err(e) => {
                return ToolOutput::error(format!("Invalid arguments: {e}"));
            }
        };

        let servers = self.servers.servers();
        let Some(server) = LspServer::for_language(&servers, &request.language)
        else {
            let known: Vec<&str> = servers
                .iter()
                .map(|server| server.language.as_str())
                .collect();
            return ToolOutput::error(format!(
                "The deployment has no language server for `{}`. The \
                 languages it knows: {}.",
                request.language,
                if known.is_empty() {
                    "none".to_string()
                } else {
                    known.join(", ")
                }
            ));
        };

        let navigation = request.operation.navigation();

        // A navigation operation needs the cursor ✓ — and it needs it
        // before the file is read ✓: a missing cursor is a refusal
        // the model can fix, not a file to fetch ✓.
        if navigation.is_some()
            && (request.line.is_none() || request.character.is_none())
        {
            return ToolOutput::error(
                "A navigation operation needs the cursor: give the \
                 1-based `line` and `character` of the symbol."
                    .to_string(),
            );
        }

        // The file is read through the owner ✓ — the same bytes the
        // server is asked about ✓.
        let workspace_root = context.workspace_root.clone();
        if let Some(error) =
            check_path_safety(&workspace_root, &request.file_path)
        {
            return ToolOutput::error(error);
        }
        let path: PathBuf = resolve_path(&workspace_root, &request.file_path);
        let bytes = match self.owner.read_file(&context.session_id, &path).await
        {
            Ok(bytes) => bytes,
            Err(error) => {
                return ToolOutput::error(format!(
                    "The file '{}' could not be read: {error}",
                    path.display()
                ));
            }
        };
        let Ok(text) = String::from_utf8(bytes) else {
            return ToolOutput::error(format!(
                "The file '{}' is not valid UTF-8; a language server \
                 reads text.",
                path.display()
            ));
        };

        let wait = Duration::from_secs(
            request.timeout_secs.unwrap_or(20).min(MAX_WAIT_SECS),
        );

        let client = match client::LspClient::start(
            &server.command,
            &server.args,
            &workspace_root,
        )
        .await
        {
            Ok(client) => client,
            Err(error) => {
                return ToolOutput::error(format!(
                    "The language server for `{}` could not be asked: \
                     {error}",
                    request.language
                ));
            }
        };

        let answer = match navigation {
            None => {
                let answer = client
                    .diagnostics(&path, &text, &request.language, wait)
                    .await;
                client.shutdown().await;
                answer.map(|diagnostics| {
                    if diagnostics.is_empty() {
                        format!("No diagnostics for '{}'.", path.display())
                    } else {
                        render(&workspace_root, &path, &diagnostics)
                    }
                })
            }
            Some(operation) => {
                let answer = client
                    .query(
                        operation,
                        &path,
                        &text,
                        &request.language,
                        request.line.unwrap_or(1),
                        request.character.unwrap_or(1),
                    )
                    .await;
                client.shutdown().await;
                answer.map(|answer| match answer {
                    client::QueryResult::Locations(locations) => {
                        render_locations(&workspace_root, &locations)
                    }
                    client::QueryResult::Hover(Some(text)) => text,
                    client::QueryResult::Hover(None) => {
                        "The server says nothing about that position."
                            .to_string()
                    }
                })
            }
        };

        match answer {
            Ok(text) => ToolOutput::text(text),
            Err(error) => ToolOutput::error(format!(
                "The language server for `{}` did not answer: {error}",
                request.language
            )),
        }
    }
}

/// The diagnostics, one per line ✓.
fn render(
    workspace_root: &std::path::Path,
    path: &std::path::Path,
    diagnostics: &[Diagnostic],
) -> String {
    let mut body = String::new();
    for diagnostic in diagnostics {
        body.push_str(&format!(
            "{}:{}:{}: {}: {}",
            relative(workspace_root, path).display(),
            diagnostic.line,
            diagnostic.column,
            diagnostic.severity,
            diagnostic.message
        ));
        if let Some(source) = &diagnostic.source {
            body.push_str(&format!(" ({source})"));
        }
        body.push('\n');
    }
    body.trim_end().to_string()
}

/// The places the server pointed at, one per line ✓.
fn render_locations(
    workspace_root: &std::path::Path,
    locations: &[client::Location],
) -> String {
    if locations.is_empty() {
        return "The server pointed at no place.".to_string();
    }
    let mut body = String::new();
    for location in locations {
        body.push_str(&format!(
            "{}:{}:{}",
            relative(workspace_root, &location.path).display(),
            location.line,
            location.column
        ));
        body.push('\n');
    }
    body.trim_end().to_string()
}

/// The path as the workspace names it ✓.
///
/// A path outside the workspace keeps its own name ✓ — the model can
/// still act on it ✓.
fn relative(
    workspace_root: &std::path::Path,
    path: &std::path::Path,
) -> std::path::PathBuf {
    path.strip_prefix(workspace_root)
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|_| path.to_path_buf())
}
