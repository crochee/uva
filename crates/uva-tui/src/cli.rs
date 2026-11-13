//! The command line of the terminal projection.
//!
//! The definitions live in the library so they can be unit-tested and
//! so the binary stays a thin shell.
//!
//! Unlike the headless core, the TUI's stdout is the **terminal**,
//! not a protocol channel — so clap's conventional `--help` on
//! stdout is right here.

use std::path::PathBuf;

use clap::Parser;

/// The command line of the terminal projection.
#[derive(Parser, Debug)]
#[command(
    name = "uva-tui",
    about = "A terminal projection of the uva gateway",
    version
)]
pub struct Args {
    /// The workspace the gateway serves (passed as `--directory`).
    #[arg(short, long, default_value = ".")]
    pub directory: PathBuf,
    /// The plugin profile the gateway resolves (passed as `--profile`).
    #[arg(long, value_name = "PATH")]
    pub profile: Option<PathBuf>,
    /// The `uva-app-server` binary (which runs `serve`).
    ///
    /// Defaults to `uva-app-server` on `PATH`.
    #[arg(long, env = "UVA_APP_SERVER", value_name = "PATH")]
    pub app_server: Option<PathBuf>,
    /// Attach to a gateway that is already running instead of
    /// launching one (`http://host:port` or `ws://host:port/rpc`).
    #[arg(long, value_name = "URL")]
    pub attach: Option<String>,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    /// clap refuses to build a command whose arguments contradict each
    /// other. The check only runs when a test asks for it, so this is
    /// where it runs for good.
    #[test]
    fn the_command_definition_is_consistent() {
        Args::command().debug_assert();
    }

    /// The defaults the projection starts from: the current directory
    /// and the binary on `PATH`.
    #[test]
    fn the_defaults_parse() {
        let args = Args::try_parse_from(["uva-tui"])
            .expect("a bare invocation parses");
        assert_eq!(args.directory, PathBuf::from("."));
        assert!(args.app_server.is_none());
        assert!(args.attach.is_none());
    }

    /// `--attach` skips the spawn entirely: the projection talks to a
    /// gateway that is already running.
    #[test]
    fn attaching_is_a_flag() {
        let args = Args::try_parse_from([
            "uva-tui",
            "--attach",
            "ws://127.0.0.1:8080/rpc",
        ])
        .expect("the attach flag parses");
        assert_eq!(args.attach.as_deref(), Some("ws://127.0.0.1:8080/rpc"));
    }
}
