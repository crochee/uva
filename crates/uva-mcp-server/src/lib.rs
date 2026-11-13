//! `uva-mcp-server` — a stdio MCP server that exposes uva's
//! plugin-brick tools (`read` / `write` / `shell` / `todo` /
//! `web_fetch` / `schedule` / `search` + a `uva` overview tool) to
//! any MCP client (e.g. the `pi` coding agent).
//!
//! ## Why this crate exists
//!
//! `uva-app-server` exposes its management surface over HTTP + an
//! in-process MCP server that lets the deployment's own agents
//! manage themselves. That's the **inbound** path (the agent reaches
//! into its own deployment). This crate is the **outbound** path:
//! any foreign MCP client can spawn
//! `uva-mcp-server` as a subprocess, send newline-delimited
//! JSON-RPC 2.0 over its stdin, and read the responses on its
//! stdout — without dragging uva into its own process.
//!
//! The two paths share the same protocol implementation:
//! [`uva_mcp::server::serve`] drives the request loop over a
//! [`uva_mcp::LoopbackServer`] inbox, and the same call wires
//! the plugin-brick tools here. A successful call against this
//! binary is therefore also a successful end-to-end proof of the
//! in-process MCP server seam — the only difference is the
//! transport: here it is newline-delimited JSON over stdio, where
//! `uva-app-server` registers one in its own address space.
//!
//! ## Wire
//!
//! ```text
//! → {"jsonrpc":"2.0","id":1,"method":"initialize","params":{…}}
//! ← {"jsonrpc":"2.0","id":1,"result":{"protocolVersion":…}}
//! → {"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}
//! ← {"jsonrpc":"2.0","id":2,"result":{"tools":[…]}}
//! → {"jsonrpc":"2.0","id":3,"method":"tools/call",
//!    "params":{"name":"read","arguments":{"file_path":"…"}}}
//! ← {"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text",
//!    "text":"…"}],"isError":false}}
//! ```
//!
//! Every request gets a response. stderr carries the operator logs;
//! stdout is reserved for JSON-RPC. The server exits when the
//! client side closes its stdin (EOF).
//!
//! ## Command line
//!
//! [`cli`] owns the command line: [`cli::Args`] is the parsed form
//! ([`cli::Command`] decides between "run", "help" and "usage
//! error"), and the binary is a thin shim over it. `--help` and
//! `--version` render to **stderr**, because stdout is the protocol
//! channel here — see [`cli`] for the full rule.
//!
//! ## Runtime
//!
//! The library surface (`build`, `tools`) is runtime-neutral —
//! `serve` returns a future any executor drives. The `main`
//! binary pins tokio because it owns a stdin reader that must be
//! spawned; consumers driving `serve` themselves do not inherit
//! that dependency.

use std::{path::PathBuf, sync::Arc};

use futures::future::BoxFuture;
use serde_json::{Value, json};
use uva_core::SharedClock;
use uva_mcp::ServerTool;
use uva_tool::{Context, Tool, ToolOutput};
use uva_tool_plugins::{
    read::ReadTool,
    scheduler::SchedulerTool,
    search::SearchTool,
    shell::ShellTool,
    todo::TodoWriteTool,
    web::WebFetchTool,
    write::WriteTool,
};

pub mod cli;
pub mod stdio;

pub use cli::{Args, Command, ServerConfig};
pub use stdio::run;

/// Tool surface this server publishes — every plugin brick the
/// facade ships behind a feature, plus one synthetic `uva`
/// overview tool that returns the registered tool list, the
/// workspace root, and the binary version. Callers that want a
/// smaller set build their own [`ServerSurface`] (filtering the
/// `tools` vector) and pass it to [`stdio::run`].
pub struct ServerSurface {
    /// Workspace root file-touching tools read from / write to.
    pub workspace_root: PathBuf,
    /// Wall clock the schedule and search tools read "now" from.
    pub clock: SharedClock,
}

impl ServerSurface {
    /// Build the surface over `workspace_root` and
    /// [`SharedClock::system()`].
    pub fn new(workspace_root: impl Into<PathBuf>) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            clock: SharedClock::system(),
        }
    }

    /// Same, with an injected clock — how tests pin time.
    #[must_use]
    pub fn with_clock(
        workspace_root: impl Into<PathBuf>,
        clock: SharedClock,
    ) -> Self {
        Self {
            workspace_root: workspace_root.into(),
            clock,
        }
    }

    /// The [`Context`] every plugin-brick tool is called with.
    /// One per binary: a stateless process keeps one session id,
    /// the workspace it was started in, and the default truncation
    /// config the [`Context::default`] provides.
    pub fn context(&self, session_id: &str) -> Context {
        Context::new(session_id.to_string(), self.workspace_root.clone())
    }

    /// Assemble the full plugin-brick surface as
    /// [`uva_mcp::ServerTool`]s the in-process server can serve.
    ///
    /// The returned tools share one [`Context`] (built once over
    /// `session_id`) — every call sees the same workspace root and
    /// truncation config, exactly as a foreign MCP client's
    /// subprocess would experience it.
    pub fn tools(&self, session_id: &str) -> Vec<ServerTool> {
        let ctx = Arc::new(self.context(session_id));
        let read = bridge(Arc::new(ReadTool::new()), Arc::clone(&ctx));
        let write = bridge(Arc::new(WriteTool::new()), Arc::clone(&ctx));
        let shell = bridge(Arc::new(ShellTool::new()), Arc::clone(&ctx));
        let todo = bridge(Arc::new(TodoWriteTool::new()), Arc::clone(&ctx));
        let web = bridge(Arc::new(WebFetchTool::new()), Arc::clone(&ctx));
        // The schedule tool needs a `ScheduleStore` rooted at the
        // workspace's `.uva/schedules/` directory. Boot reload
        // is best-effort (handled by the tool itself).
        let schedule_root = self.workspace_root.join(".uva").join("schedules");
        std::fs::create_dir_all(&schedule_root).ok();
        let store = Arc::new(uva_scheduler::ScheduleStore::new(&schedule_root));
        let schedule = bridge(
            Arc::new(SchedulerTool::with_clock(store, self.clock.clone())),
            Arc::clone(&ctx),
        );
        // The search tool needs a `Registry`. The empty registry is
        // a valid registry: `register_engine` is the host-side
        // decision, not the tool's.
        let search_registry = Arc::new(uva_search::Registry::default());
        let search = bridge(
            Arc::new(SearchTool::new(search_registry)),
            Arc::clone(&ctx),
        );
        // The `uva` overview tool: not a plugin brick — a
        // synthetic tool that lists everything the server exposes.
        let uva = overview_tool(
            self.workspace_root.clone(),
            env!("CARGO_PKG_VERSION").to_string(),
            session_id.to_string(),
        );
        vec![read, write, shell, todo, web, schedule, search, uva]
    }

    /// The tools assembled for this surface — exposed for tests
    /// that want to introspect the surface without driving the
    /// stdio pump.
    #[must_use]
    pub fn tools_vec(&self, session_id: &str) -> Vec<ServerTool> {
        self.tools(session_id)
    }
}

/// Adapt an `Arc<dyn Tool>` to the [`uva_mcp::ServerTool`]
/// handler signature: the call site is exactly the
/// `uva-mcp` in-process server's contract.
fn bridge(tool: Arc<dyn Tool>, ctx: Arc<Context>) -> ServerTool {
    let name = tool.name().to_string();
    let description = tool.description().to_string();
    let input_schema = tool.parameters();
    let annotations = tool.annotations();
    let handler = Arc::new(move |input: Value| {
        let tool = Arc::clone(&tool);
        let ctx = Arc::clone(&ctx);
        Box::pin(async move {
            let output = tool.call(input, &ctx).await;
            render_output(output)
        }) as BoxFuture<'static, Result<String, String>>
    });
    ServerTool {
        name,
        description,
        input_schema,
        handler,
        annotations,
    }
}

/// Render a [`ToolOutput`] into the `String` the in-process
/// server writes into a `text` content block.
///
/// The shape mirrors what the JSON wire would have carried: a
/// single text block, plain or the JSON of any structured
/// payload. Errors surface as `Err(_)` so the server marks the
/// call `isError: true` (preserving the failure for the caller)
/// instead of dropping the request.
///
/// Plugin-brick tools return a single-part `Text` content block
/// on the happy path. If a future tool returns multimodal parts
/// (an image, audio), we fall back to JSON so the wire shape is
/// never lossy.
fn render_output(output: ToolOutput) -> Result<String, String> {
    let is_error = output.is_error == Some(true);
    let rendered = match output.content.as_slice() {
        [uva_provider::types::ContentPart::Text(text)] => text.text.clone(),
        _ => serde_json::to_string_pretty(&output.content)
            .unwrap_or_else(|_| "<unserializable ToolOutput>".to_string()),
    };
    if is_error {
        return Err(rendered);
    }
    Ok(rendered)
}

/// The synthetic `uva` overview tool: returns the binary
/// version, the workspace root, and the list of every tool the
/// server exposes. It exists so a foreign MCP client has one
/// stable entry point to discover what is wired in this process.
fn overview_tool(
    workspace_root: PathBuf,
    version: String,
    session_id: String,
) -> ServerTool {
    let input_schema = json!({
        "type": "object",
        "properties": {
            "format": {
                "type": "string",
                "enum": ["json", "text"],
                "description": "Output shape. Defaults to `json`."
            }
        },
        "required": [],
    });
    let handler = Arc::new(move |input: Value| {
        let workspace_root = workspace_root.clone();
        let version = version.clone();
        let session_id = session_id.clone();
        Box::pin(async move {
            let format = input
                .get("format")
                .and_then(Value::as_str)
                .unwrap_or("json");
            let tools = vec![
                "read",
                "write",
                "shell",
                "TodoWrite",
                "web_fetch",
                "schedule",
                "search",
                "uva",
            ];
            let payload = json!({
                "name": "uva",
                "version": version,
                "session_id": session_id,
                "workspace_root": workspace_root.display().to_string(),
                "tools": tools,
            });
            Ok(match format {
                "text" => {
                    let mut out = format!("uva-mcp-server {version}\n");
                    out.push_str(&format!("session: {session_id}\n"));
                    out.push_str(&format!(
                        "workspace: {}\n",
                        workspace_root.display()
                    ));
                    out.push_str("tools:\n");
                    for tool in tools {
                        out.push_str(&format!("  - {tool}\n"));
                    }
                    out
                }
                _ => serde_json::to_string_pretty(&payload)
                    .unwrap_or_else(|_| payload.to_string()),
            })
        }) as BoxFuture<'static, Result<String, String>>
    });
    ServerTool {
        name: "uva".to_string(),
        description: "Overview of this uva-mcp-server process: \
             version, workspace root, and the full list of tools it \
             exposes. The entry point a foreign MCP client calls once \
             to discover what is wired here."
            .to_string(),
        input_schema,
        handler,
        annotations: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The `uva` overview tool returns the seven plugin-brick
    /// tools plus itself; the format knob picks between JSON and
    /// the text rendering.
    #[tokio::test]
    async fn overview_tool_round_trips() {
        let workspace = tempdir_in_cwd();
        let tool = overview_tool(
            workspace.clone(),
            "0.1.0".to_string(),
            "s1".to_string(),
        );
        let json_out = (tool.handler)(json!({})).await.unwrap();
        let parsed: Value = serde_json::from_str(&json_out).unwrap();
        let names: Vec<&str> = parsed["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect();
        for expected in [
            "read",
            "write",
            "shell",
            "TodoWrite",
            "web_fetch",
            "schedule",
            "search",
            "uva",
        ] {
            assert!(
                names.contains(&expected),
                "overview missing `{expected}`: {names:?}"
            );
        }

        let text_out = (tool.handler)(json!({"format": "text"})).await.unwrap();
        assert!(text_out.contains("uva-mcp-server 0.1.0"));
        assert!(text_out.contains("read"));
    }

    /// Every plugin-brick tool builds and the in-process server
    /// exposes all eight tools. This is the structural proof that
    /// the stdio binary will advertise the documented surface.
    #[test]
    fn plugin_brick_tools_assemble() {
        let workspace = tempdir_in_cwd();
        let surface = ServerSurface::new(&workspace);
        let tools = surface.tools("test-session");
        let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
        for expected in [
            "read",
            "write",
            "shell",
            "TodoWrite",
            "web_fetch",
            "schedule",
            "search",
            "uva",
        ] {
            assert!(
                names.contains(&expected),
                "missing tool `{expected}` in {names:?}"
            );
        }
    }

    /// The `bridge` adapter calls the underlying plugin-brick tool
    /// with the assembled context and surfaces its output as the
    /// MCP call result text.
    #[tokio::test]
    async fn bridge_passes_input_through_to_the_tool() {
        let workspace = tempdir_in_cwd();
        let path = workspace.join("hello.txt");
        std::fs::write(&path, "first line\nsecond line\n").unwrap();

        let ctx = Arc::new(Context::new("test".into(), workspace.clone()));
        let read = Arc::new(ReadTool::new());
        let bridged = bridge(read, Arc::clone(&ctx));
        let result = (bridged.handler)(json!({
            "file_path": path.to_string_lossy(),
            "offset": 1,
            "limit": 1,
        }))
        .await
        .unwrap();
        assert!(result.contains("first line"), "got: {result}");
        assert!(!result.contains("second line"), "got: {result}");
    }

    /// `render_output` flips `is_error: true` into `Err(_)` so
    /// the in-process server marks the call as a tool-level
    /// failure rather than dropping the response.
    #[test]
    fn render_output_errors_are_surfaced_as_err() {
        let ok = ToolOutput::text("hello");
        assert_eq!(render_output(ok).unwrap(), "hello");

        let err = ToolOutput::error("boom");
        let err_str = render_output(err).unwrap_err();
        assert!(err_str.contains("boom"), "{err_str}");
    }

    fn tempdir_in_cwd() -> PathBuf {
        let dir = std::env::temp_dir()
            .join(format!("uva-mcp-server-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}

// Re-export the helper that drives the server from the binary's
// `main` so the library surface stays self-contained for tests.

/// The README, **compiled**.
///
/// The `rust` fences in [`README.md`](../README.md) are doctests, so
/// a wiring recipe that stops compiling fails
/// `cargo test -p uva-mcp-server`.
#[cfg(doctest)]
#[doc = include_str!("../README.md")]
pub struct Readme;
