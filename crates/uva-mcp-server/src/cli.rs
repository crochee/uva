//! The command line of the stdio MCP server.
//!
//! The binary is a **protocol peer**: stdout carries JSON-RPC frames
//! and nothing else. That is why `--help` and `--version` render to
//! **stderr** here ([`Command::parse`]) where a conventional CLI would
//! print them on stdout — a help text on stdout reaches the peer as a
//! malformed frame, exactly like a stray log line would.
//!
//! The flag spellings are a contract with the MCP clients that spawn
//! this binary (`pi`, a Claude Code plugin, the Anthropic SDK):
//! `--workspace` / `-w` and `--name` / `-n` keep their meaning, their
//! defaults, and their failure modes.

use std::path::PathBuf;

use clap::Parser;

/// The binary's own name, used as clap's program name and as the
/// synthetic argv[0] the parser expects.
const BINARY_NAME: &str = "uva-mcp-server";

/// The parsed command line.
#[derive(Debug, Clone, Parser)]
#[command(
    name = BINARY_NAME,
    about = "A stdio MCP server exposing uva's plugin-brick tools",
    version
)]
pub struct Args {
    /// Workspace root the plugin-brick tools are confined to.
    ///
    /// Must be an existing directory; defaults to the current one.
    #[arg(short, long, value_name = "PATH")]
    workspace: Option<PathBuf>,
    /// Server name the JSON-RPC handshake reports back.
    #[arg(short, long, value_name = "NAME")]
    name: Option<String>,
}

impl Args {
    /// Resolve the arguments into the server configuration.
    ///
    /// `--workspace` defaults to the current directory and must be an
    /// existing directory; `--name` defaults to `uva`.
    pub fn into_config(self) -> Result<ServerConfig, String> {
        let workspace_root = match self.workspace {
            Some(path) => canonicalize(path)
                .map_err(|error| format!("--workspace: {error}"))?,
            None => {
                std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
            }
        };
        if !workspace_root.is_dir() {
            return Err(format!(
                "--workspace `{}` is not a directory",
                workspace_root.display()
            ));
        }
        Ok(ServerConfig {
            workspace_root,
            server_name: self.name.unwrap_or_else(|| "uva".to_string()),
        })
    }
}

/// The resolved server configuration.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Workspace root the plugin-brick tools are confined to.
    pub workspace_root: PathBuf,
    /// Server name the JSON-RPC handshake reports back.
    pub server_name: String,
}

/// What the command line asked for.
#[derive(Debug)]
pub enum Command {
    /// Run the server with these arguments.
    Run(Args),
    /// `--help` or `--version`: print this text on **stderr** and
    /// exit 0.
    Help(String),
    /// A usage error: print this text on **stderr** and exit 2.
    Usage(String),
}

impl Command {
    /// Parse the process arguments under the protocol-mode stdout
    /// discipline.
    ///
    /// `argv` is the argument list **without** the program name, the
    /// same shape `std::env::args().skip(1)` hands over.
    pub fn parse(argv: impl IntoIterator<Item = String>) -> Self {
        let mut full = Vec::new();
        full.push(BINARY_NAME.to_string());
        full.extend(argv);
        match Args::try_parse_from(full) {
            Ok(args) => Command::Run(args),
            Err(error) => match error.kind() {
                clap::error::ErrorKind::DisplayHelp
                | clap::error::ErrorKind::DisplayVersion
                | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand => {
                    Command::Help(error.render().to_string())
                }
                _ => Command::Usage(error.render().to_string()),
            },
        }
    }
}

/// Best-effort canonicalize: a non-existent path is reported as
/// `No such file or directory` and the caller surfaces it.
fn canonicalize(path: PathBuf) -> std::io::Result<PathBuf> {
    std::fs::canonicalize(&path).or_else(|error| {
        if path.exists() {
            Ok(path)
        } else {
            Err(std::io::Error::new(
                error.kind(),
                format!("path does not exist: {}", path.display()),
            ))
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(argv: &[&str]) -> Command {
        Command::parse(argv.iter().map(|arg| arg.to_string()))
    }

    /// The defaults the clients rely on: the workspace is the current
    /// directory and the handshake reports `uva`.
    #[test]
    fn defaults_are_the_current_directory_and_the_uva_name() {
        let Command::Run(args) = parse(&[]) else {
            panic!("empty argv must parse");
        };
        let config = args.into_config().unwrap();
        assert_eq!(config.server_name, "uva");
        assert!(
            config.workspace_root.is_dir(),
            "{:?}",
            config.workspace_root
        );
    }

    /// Both spellings of both flags reach the same configuration.
    #[test]
    fn long_and_short_spellings_agree() {
        let Command::Run(long) = parse(&["--workspace", ".", "--name", "pi"])
        else {
            panic!("long spellings must parse");
        };
        let Command::Run(short) = parse(&["-w", ".", "-n", "pi"]) else {
            panic!("short spellings must parse");
        };
        let long = long.into_config().unwrap();
        let short = short.into_config().unwrap();
        assert_eq!(long.server_name, "pi");
        assert_eq!(long.server_name, short.server_name);
        assert_eq!(long.workspace_root, short.workspace_root);
    }

    /// An unknown flag is a usage error — the binary exits 2 without
    /// starting the protocol loop.
    #[test]
    fn unknown_flag_is_a_usage_error() {
        assert!(matches!(parse(&["--nope"]), Command::Usage(_)));
    }

    /// A flag without its value is a usage error too.
    #[test]
    fn missing_value_is_a_usage_error() {
        assert!(matches!(parse(&["--workspace"]), Command::Usage(_)));
    }

    /// `--help` and `--version` are notices, not failures: they exit 0.
    #[test]
    fn help_and_version_are_notices() {
        assert!(matches!(parse(&["--help"]), Command::Help(_)));
        assert!(matches!(parse(&["-h"]), Command::Help(_)));
        assert!(matches!(parse(&["--version"]), Command::Help(_)));
    }

    /// A workspace that is not a directory fails the run — the
    /// directory check is the tool sandbox's boundary, so it must not
    /// be a warning.
    #[test]
    fn workspace_must_be_a_directory() {
        let file = std::env::temp_dir().join("uva-mcp-server-not-a-dir");
        std::fs::write(&file, b"x").unwrap();
        let Command::Run(args) =
            parse(&["--workspace", file.to_str().unwrap()])
        else {
            panic!("a path argument must parse");
        };
        let error = args.into_config().unwrap_err();
        assert!(error.contains("is not a directory"), "{error}");
        let _ = std::fs::remove_file(&file);
    }
}
