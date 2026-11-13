//! `uva-mcp-server` — stdio MCP server binary.
//!
//! Spawnable by any MCP client (`pi`, a Claude Code plugin, the
//! Anthropic SDK, …) via:
//!
//! ```text
//! uva-mcp-server --workspace /path/to/project
//! ```
//!
//! See `lib.rs` for the wire shape and the tool surface; the
//! binary is a thin shim around [`uva_mcp_server::run`].
//!
//! ## Why the binary wraps `tokio::io::stdin()` / `stdout()`
//!
//! The library's `run` function is generic over any
//! `tokio::io::AsyncRead` / `AsyncWrite`, so it never names a
//! runtime type in its public API (§3.7). The binary itself
//! is the one place that does — and is exempt, exactly as
//! `uva-app-server` is.

use std::process::ExitCode;

use uva_mcp::ServerInfo;
use uva_mcp_server::{Command, ServerSurface};

#[tokio::main]
async fn main() -> ExitCode {
    // stderr-only logs so stdout stays a pure JSON-RPC channel.
    let _ = tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_target(false)
        .try_init();

    // stdout is the JSON-RPC channel: `--help`, `--version` and
    // usage errors all render to stderr, so a peer never reads a
    // help text as a malformed frame.
    let args = match Command::parse(std::env::args().skip(1)) {
        Command::Run(args) => args,
        Command::Help(text) => {
            eprint!("{text}");
            return ExitCode::SUCCESS;
        }
        Command::Usage(text) => {
            eprint!("{text}");
            return ExitCode::from(2);
        }
    };
    let config = match args.into_config() {
        Ok(config) => config,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };

    tracing::info!(
        server = %config.server_name,
        workspace = %config.workspace_root.display(),
        "uva-mcp-server starting",
    );

    let surface = ServerSurface::new(&config.workspace_root);
    let info =
        ServerInfo::new(config.server_name.clone(), env!("CARGO_PKG_VERSION"));

    // The library's `run` is generic over `AsyncRead` / `AsyncWrite`.
    // The binary hands it the process's own — and keeps the runtime
    // types entirely inside `main`, where the §3.7 exemption applies.
    match uva_mcp_server::run(
        surface,
        info,
        tokio::io::stdin(),
        tokio::io::stdout(),
    )
    .await
    {
        Ok(()) => {
            tracing::info!("stdin closed; exiting cleanly");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(%error, "stdio pump failed");
            ExitCode::from(1)
        }
    }
}
