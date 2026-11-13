//! The command line of the application binary.
//!
//! The definitions live in the library so they can be unit-tested and
//! so the binary stays a thin shell: [`Args`] is the parsed form,
//! [`Mode`] the sub-command, and [`Mode::Core`] the one that owns
//! stdout.
//!
//! ## The stdout rule
//!
//! `core` speaks JSON-RPC 2.0 on stdin/stdout, so **stdout carries
//! frames and nothing else**. clap's automatic `--help` / `--version`
//! print on stdout, which in `core` mode would reach the peer as a
//! malformed frame — the same failure a stray log line causes. The
//! core therefore takes both as ordinary flags ([`CoreArgs`]) and
//! renders them to **stderr**; every other mode keeps the
//! conventional stdout behaviour.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::build_info;

/// The command line of the application binary.
#[derive(Parser, Debug)]
#[command(name = "uva-app-server")]
#[command(
    about = "Uva agent runtime: a headless JSON-RPC core, or a gateway that serves it",
    version = build_info::LONG_VERSION,
)]
pub struct Args {
    /// The mode to run in.
    ///
    /// Omitted → `core`: the binary speaks JSON-RPC on stdin/stdout
    /// and waits for a peer.
    #[command(subcommand)]
    pub mode: Option<Mode>,
    /// Working directory.
    ///
    /// The workspace the agent runtime owns: sessions, schedules,
    /// workflows, evals and attachments live here.
    #[arg(short, long, default_value = ".", global = true)]
    pub directory: PathBuf,
    /// Path to the deployment configuration file (ServerConfig).
    ///
    /// The sections read at boot come from this file: `agents`
    /// (`max_steps`, `compaction`, `strategy`), `[tools]`,
    /// `[operations]`, `[plugins]`, `[cluster]`, and the bind/limit
    /// scalars. Provider entries do NOT: they come from the profile's
    /// `model-adapter` row or `<directory>/.agents/config.toml` — run
    /// `translate` to migrate a legacy yaml `providers` section. The
    /// format is chosen by extension, so `.yaml`, `.yml`, and `.toml`
    /// all work.
    ///
    /// When omitted — or when the named file is missing or does not
    /// parse — the runtime tries `<directory>/config.toml`, then
    /// `<directory>/.uva/config.toml`, then the global
    /// `~/.agents/config.toml`, and finally its own defaults; the
    /// provider then falls back to
    /// `<directory>/.agents/config.toml` and the environment
    /// (`OPENAI_API_KEY` / `ANTHROPIC_API_KEY`).
    #[arg(long, value_name = "PATH", global = true)]
    pub config: Option<PathBuf>,
    /// Path to the plugin profile TOML.
    ///
    /// The profile owns plugin assembly; deployment basics (bind
    /// address, workspace root, logging) stay in `config.yaml`. When
    /// omitted, `<directory>/profile.toml` is used if it exists, and
    /// config.yaml's plugin sections are not consulted at all (the
    /// compatibility layer.
    #[arg(long, value_name = "PATH", global = true)]
    pub profile: Option<PathBuf>,
    /// Path to a bundle patch TOML (the lowest layer).
    ///
    /// A bundle seeds or adjusts defaults before the profile document
    /// is applied; the profile and the CLI overlay win over it. Missing
    /// files are simply not layered.
    #[arg(long, value_name = "PATH", global = true)]
    pub bundle: Option<PathBuf>,
    /// Path to a patch TOML layered over the profile and home patch.
    ///
    /// The CLI overlay is the last layer, so its `[[replace]]` values
    /// win over every file. An id the assembly does not contain fails
    /// the boot instead of being ignored.
    #[arg(long, value_name = "PATH", global = true)]
    pub patch: Option<PathBuf>,
    /// Resolve the profile and print the assembled tree, then exit.
    ///
    /// The output is deterministic (stable entry order, sorted object
    /// keys), so two runs over the same inputs are byte-identical.
    #[arg(long, global = true)]
    pub dump_config: bool,
}

/// The modes the binary runs in.
#[derive(Subcommand, Debug)]
pub enum Mode {
    /// The headless core: JSON-RPC 2.0 on stdin/stdout.
    ///
    /// stdout carries the protocol and nothing else; the log stream
    /// goes to stderr. The core announces itself with a `ready`
    /// notification once the boot is done.
    Core(CoreArgs),
    /// The gateway: a WebSocket front for a core subprocess.
    Serve(ServeArgs),
    /// Migrate a legacy config.yaml into a profile TOML's
    /// `model-adapter` row, then exit.
    Translate(crate::translate::TranslateArgs),
}

/// The core's own flags.
///
/// clap's automatic `--help` / `--version` are disabled here: they
/// print on stdout, and stdout is the core's protocol channel. The
/// binary renders both to stderr instead (see the module docs).
#[derive(clap::Args, Debug)]
#[command(disable_help_flag = true, disable_version_flag = true)]
pub struct CoreArgs {
    /// Print this help on stderr and exit.
    #[arg(short, long, action = clap::ArgAction::SetTrue)]
    pub help: bool,
    /// Print the version on stderr and exit.
    #[arg(short = 'V', long, action = clap::ArgAction::SetTrue)]
    pub version: bool,
}

/// The gateway's arguments.
#[derive(clap::Args, Debug)]
pub struct ServeArgs {
    /// The host to bind.
    ///
    /// The default is loopback: the gateway speaks to the core it
    /// spawned, and the web front-end that talks to it runs on the
    /// same machine. A container sets `UVA_HOST=0.0.0.0` — a
    /// gateway bound to loopback is unreachable through a published
    /// port.
    #[arg(long, env = "UVA_HOST", default_value = "127.0.0.1")]
    pub bind: String,
    /// The port to bind.
    #[arg(short, long, env = "UVA_PORT", default_value_t = 8080)]
    pub port: u16,
    /// The directory holding the web front-end's built files.
    ///
    /// Takes precedence over the assets baked in with the
    /// `embed-web` feature, so a development build can point at a
    /// freshly built `dist` without rebuilding the binary.
    #[arg(long, value_name = "PATH")]
    pub web_dir: Option<PathBuf>,
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    /// clap refuses to build a command whose arguments contradict each
    /// other (a duplicated short flag, a default that clashes with an
    /// option). The check only runs when a test asks for it, so this
    /// is where it runs for good.
    #[test]
    fn the_command_definition_is_consistent() {
        Args::command().debug_assert();
    }

    /// The container images set `UVA_HOST` / `UVA_PORT`
    /// instead of passing flags. This asserts the binding on the
    /// **parsed CLI model** — not on a substring of this file's
    /// source text — so a rename of the env var or a dropped
    /// `env = …` attribute fails here.
    #[test]
    fn the_gateway_binds_the_env_vars_the_images_set() {
        let command = Args::command();
        let env_of = |id: &str| {
            command
                .get_subcommands()
                .find(|sub| sub.get_name() == "serve")
                .and_then(|serve| {
                    serve.get_arguments().find(|arg| arg.get_id() == id)
                })
                .and_then(|arg| arg.get_env())
                .map(|value| value.to_string_lossy().into_owned())
        };
        assert_eq!(env_of("bind").as_deref(), Some("UVA_HOST"));
        assert_eq!(env_of("port").as_deref(), Some("UVA_PORT"));
    }

    /// The binary with no subcommand is the core: the human entry
    /// point is `serve`, and a bare invocation must not hang on a
    /// gateway nobody asked for.
    #[test]
    fn a_bare_invocation_is_the_core() {
        let args = Args::try_parse_from(["uva-app-server"])
            .expect("a bare invocation parses");
        assert!(args.mode.is_none());
    }

    /// The core and the gateway are two named modes.
    #[test]
    fn the_modes_parse() {
        let args = Args::try_parse_from(["uva-app-server", "core"])
            .expect("the core mode parses");
        assert!(matches!(args.mode, Some(Mode::Core(_))));
        let args = Args::try_parse_from([
            "uva-app-server",
            "serve",
            "--bind",
            "0.0.0.0",
            "--port",
            "9000",
            "--web-dir",
            "/tmp/dist",
        ])
        .expect("the serve mode parses");
        match args.mode {
            Some(Mode::Serve(serve)) => {
                assert_eq!(serve.bind, "0.0.0.0");
                assert_eq!(serve.port, 9000);
                assert_eq!(
                    serve.web_dir,
                    Some(std::path::PathBuf::from("/tmp/dist"))
                );
            }
            other => panic!("expected the serve mode, got {other:?}"),
        }
    }

    /// The core takes `--help` and `--version` as ordinary flags: they
    /// are the ones that must not reach stdout.
    #[test]
    fn the_core_parses_help_and_version_as_flags() {
        let args = Args::try_parse_from(["uva-app-server", "core", "--help"])
            .expect("the core's help flag parses");
        let Some(Mode::Core(core)) = args.mode else {
            panic!("the core mode must parse");
        };
        assert!(core.help);
        assert!(!core.version);

        let args =
            Args::try_parse_from(["uva-app-server", "core", "--version"])
                .expect("the core's version flag parses");
        let Some(Mode::Core(core)) = args.mode else {
            panic!("the core mode must parse");
        };
        assert!(core.version);
    }
}
