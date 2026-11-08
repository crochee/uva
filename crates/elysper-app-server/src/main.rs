//! The Elysper app server.
//!
//! One binary, two modes:
//!
//! - `core` (the default) — the headless agent runtime: JSON-RPC
//!   2.0 on stdin/stdout, one JSON object per line. This is the
//!   mode a TUI, a desktop shell or any other host spawns as a
//!   sidecar.
//! - `serve` — the gateway: it spawns **one** core subprocess and
//!   serves it over a WebSocket (`/rpc`), alongside the HTTP probes
//!   and the web front-end's static files.
//!
//! The core owns the agent logic; the gateway owns no logic at all.
//! It pumps frames in both directions and supervises the subprocess.
//!
//! ```text
//! elysper-app-server core --directory /srv/elysper
//! elysper-app-server serve --port 8080 --web-dir ./dist
//! ```
//!
//! Running the binary with no subcommand is `core`: the human entry
//! point is `serve`.
use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::Result;
use clap::{Parser, Subcommand};
use elysper::core::{AtomicCancelToken, CancelToken};
use elysper_app_server::{
    boot::{boot_core, dump_profile},
    build_info,
    config::server::BindOverrides,
    gateway,
    profile::{Profile, ProfileWatcher, discover_home_patch, resolve_files},
    rpc::{RpcRouter, StioPump, methods},
    state::AppState,
};
use tokio::signal;
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(name = "elysper-app-server")]
#[command(
    about = "Elysper agent runtime: a headless JSON-RPC core, or a gateway that serves it",
    version = build_info::LONG_VERSION,
)]
struct Args {
    /// The mode to run in.
    ///
    /// Omitted → `core`: the binary speaks JSON-RPC on stdin/stdout
    /// and waits for a peer.
    #[command(subcommand)]
    mode: Option<Mode>,
    /// Working directory.
    ///
    /// The workspace the agent runtime owns: sessions, schedules,
    /// workflows, evals and attachments live here.
    #[arg(short, long, default_value = ".", global = true)]
    directory: PathBuf,
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
    /// `<directory>/.elysper/config.toml`, then the global
    /// `~/.agents/config.toml`, and finally its own defaults; the
    /// provider then falls back to
    /// `<directory>/.agents/config.toml` and the environment
    /// (`OPENAI_API_KEY` / `ANTHROPIC_API_KEY`).
    #[arg(long, value_name = "PATH", global = true)]
    config: Option<PathBuf>,
    /// Path to the plugin profile TOML.
    ///
    /// The profile owns plugin assembly; deployment basics (bind
    /// address, workspace root, logging) stay in `config.yaml`. When
    /// omitted, `<directory>/profile.toml` is used if it exists, and
    /// config.yaml's plugin sections are not consulted at all (the
    /// compatibility layer.
    #[arg(long, value_name = "PATH", global = true)]
    profile: Option<PathBuf>,
    /// Path to a bundle patch TOML (the lowest layer).
    ///
    /// A bundle seeds or adjusts defaults before the profile document
    /// is applied; the profile and the CLI overlay win over it. Missing
    /// files are simply not layered.
    #[arg(long, value_name = "PATH", global = true)]
    bundle: Option<PathBuf>,
    /// Path to a patch TOML layered over the profile and home patch.
    ///
    /// The CLI overlay is the last layer, so its `[[replace]]` values
    /// win over every file. An id the assembly does not contain fails
    /// the boot instead of being ignored.
    #[arg(long, value_name = "PATH", global = true)]
    patch: Option<PathBuf>,
    /// Resolve the profile and print the assembled tree, then exit.
    ///
    /// The output is deterministic (stable entry order, sorted object
    /// keys), so two runs over the same inputs are byte-identical.
    #[arg(long, global = true)]
    dump_config: bool,
}
/// The two modes the binary runs in.
#[derive(Subcommand, Debug)]
enum Mode {
    /// The headless core: JSON-RPC 2.0 on stdin/stdout.
    ///
    /// stdout carries the protocol and nothing else; the log stream
    /// goes to stderr. The core announces itself with a `ready`
    /// notification once the boot is done.
    Core,
    /// The gateway: a WebSocket front for a core subprocess.
    Serve(ServeArgs),
    /// Migrate a legacy config.yaml into a profile TOML's
    /// `model-adapter` row, then exit.
    Translate(elysper_app_server::translate::TranslateArgs),
}
/// The gateway's arguments.
#[derive(clap::Args, Debug)]
struct ServeArgs {
    /// The host to bind.
    ///
    /// The default is loopback: the gateway speaks to the core it
    /// spawned, and the web front-end that talks to it runs on the
    /// same machine. A container sets `ELYSPER_HOST=0.0.0.0` — a
    /// gateway bound to loopback is unreachable through a published
    /// port.
    #[arg(long, env = "ELYSPER_HOST", default_value = "127.0.0.1")]
    bind: String,
    /// The port to bind.
    #[arg(short, long, env = "ELYSPER_PORT", default_value_t = 8080)]
    port: u16,
    /// The directory holding the web front-end's built files.
    ///
    /// Takes precedence over the assets baked in with the
    /// `embed-web` feature, so a development build can point at a
    /// freshly built `dist` without rebuilding the binary.
    #[arg(long, value_name = "PATH")]
    web_dir: Option<PathBuf>,
}
#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    // Initialize tracing via `elysper-telemetry` so the OTLP
    // pipeline (ELYSPER_OTLP_ENDPOINT, etc.) and structured
    // layers from that crate are wired up. Falls back to console
    // tracing when OTLP is unset.
    //
    // The logs go to **stderr**: in `core` mode stdout is the
    // protocol channel, and a log line there would reach the peer
    // as a malformed frame.
    elysper::telemetry::init_tracing(&elysper::telemetry::TelemetryConfig {
        service_name: "elysper-app-server".to_string(),
        log_level: std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        log_writer: elysper::telemetry::LogWriter::Stderr,
    })
    .map_err(|e| anyhow::anyhow!("failed to init tracing: {e}"))?;
    let args = Args::parse();
    let inputs = ProfileInputs {
        directory: args.directory.clone(),
        config: args.config.clone(),
        bundle: args.bundle.clone(),
        profile: args.profile.clone(),
        patch: args.patch.clone(),
    };
    if args.dump_config {
        let profile = inputs.resolve()?;
        // The registered factory directory answers every row's
        // manifest, so the dump prints the `provides` / `inject` the
        // kernel will actually order and bind.
        // This mode prints the requested document and exits before the
        // JSON-RPC pump starts.
        print!("{}", dump_profile(&profile, &inputs.directory)?); // stdout-ok: dump-config
        return Ok(());
    }
    let cancel_token = Arc::new(AtomicCancelToken::new());
    spawn_signal_handlers(Arc::clone(&cancel_token));
    match args.mode {
        None | Some(Mode::Core) => run_core(inputs, cancel_token).await,
        Some(Mode::Translate(translate)) => {
            elysper_app_server::translate::run(&translate, &args.directory)
        }
        Some(Mode::Serve(serve)) => {
            let options = gateway::ServeOptions {
                bind: serve.bind,
                port: serve.port,
                directory: args.directory,
                config: args.config,
                bundle: args.bundle,
                profile: args.profile,
                patch: args.patch,
                web_dir: serve.web_dir,
            };
            gateway::serve(options, cancel_token).await
        }
    }
}
/// The CLI's profile inputs, kept together so the boot and the HMR
/// loop resolve the same layers.
#[derive(Debug, Clone)]
struct ProfileInputs {
    directory: PathBuf,
    config: Option<PathBuf>,
    bundle: Option<PathBuf>,
    profile: Option<PathBuf>,
    patch: Option<PathBuf>,
}

impl ProfileInputs {
    /// Resolve the profile the CLI asked for.
    ///
    /// Layer order: bundle patch → profile document (else
    /// `<directory>/profile.toml` when present) → home patch
    /// (`~/.agents/profile.toml` or `ELYSPER_HOME_PROFILE`, only when
    /// it exists) → the CLI `--patch` overlay. `config.yaml`'s
    /// joins the profile layer, and a duplicate id fails. Absent
    /// optional files are skipped, never errors.
    fn resolve(&self) -> Result<Profile> {
        let profile_path = self.profile.clone().or_else(|| {
            let default = self.directory.join("profile.toml");
            default.exists().then_some(default)
        });
        let profile = resolve_files(
            self.bundle.as_deref(),
            profile_path.as_deref(),
            discover_home_patch().as_deref(),
            self.patch.as_deref(),
        )?;
        Ok(profile)
    }

    /// Every file a change can affect the resolved profile through.
    ///
    /// Paths that do not exist yet are still watched: a profile or
    /// patch created while the process runs counts as a change on the
    /// next poll.
    fn watched_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(bundle) = &self.bundle {
            paths.push(bundle.clone());
        }
        paths.push(
            self.profile
                .clone()
                .unwrap_or_else(|| self.directory.join("profile.toml")),
        );
        if let Some(explicit) =
            std::env::var_os(elysper_app_server::profile::HOME_PATCH_ENV)
        {
            paths.push(PathBuf::from(explicit));
        } else if let Some(home) =
            std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
        {
            paths
                .push(PathBuf::from(home).join(".agents").join("profile.toml"));
        }
        if let Some(patch) = &self.patch {
            paths.push(patch.clone());
        }
        paths.sort();
        paths.dedup();
        paths
    }
}

/// Run the headless core: boot, announce, pump.
async fn run_core(
    inputs: ProfileInputs,
    cancel_token: Arc<AtomicCancelToken>,
) -> Result<()> {
    // The resolved profile is the boot's mount source: an explicit
    // core row set is authoritative, a legacy-only document falls back
    // to the built-in native set.
    let profile = inputs.resolve()?;
    info!(
        core_entries = profile.core.len(),
        ui_entries = profile.ui.len(),
        hmr = profile.hmr,
        "resolved the plugin profile"
    );
    let booted = boot_core(
        inputs.directory.clone(),
        inputs.config.as_ref(),
        Some(profile.clone()),
        BindOverrides::default(),
    )
    .await
    .map_err(|e| anyhow::anyhow!("failed to boot the core: {e}"))?;
    let state = booted.state;
    if profile.hmr {
        tokio::spawn(run_profile_hmr(
            Arc::clone(&state),
            inputs,
            Arc::clone(&cancel_token),
        ));
    } else {
        info!("profile HMR is off; a profile change requires a restart");
    }
    let mut router = RpcRouter::new();
    methods::register_all(&mut router);
    let router = Arc::new(router);
    let outbound = state.rpc.outbound().clone();
    let receiver = state
        .rpc
        .take_receiver()
        .await
        .expect("the core owns the envelope channel");
    info!(
        workspace_root = %state.workspace_root.display(),
        methods = router.method_names().len(),
        "core ready"
    );
    // The drain needs the state after the pump has it, so the pump
    // takes one clone and the shutdown path keeps the other.
    let shutdown_state = Arc::clone(&state);
    let pump = StioPump::new(state, router, receiver);
    tokio::select! {
        result = pump.run(outbound) => {
            result?;
        }
        () = cancel_token.cancelled() => {
            info!("core shutting down");
        }
    }
    // Tear the kernel down before draining: the MCP tick loop stops,
    // its generations are withdrawn (remote tools disappear), and
    // dropping the mount handle unloads every remaining service in
    // reverse order.
    shutdown_state.shutdown_kernel().await;
    // Both exits drain: a SIGTERM and a closed stdin (the gateway went
    // away) must leave the same terminal marker in every live session's
    // log — the marker that proves the controllers released their
    // leases and flushed before the process left.
    elysper_app_server::shutdown::drain_session_controllers(&shutdown_state)
        .await;
    Ok(())
}
/// Poll the profile files and remount only the affected plugins.
///
/// A change re-resolves the layers and hands the result to
/// [`AppState::reload_profile`], which diffs against the mounted set:
/// `removed` rows unload, `added` / `remounted` rows mount as one new
/// group, and a volatile-only change does not touch a fiber. Turns
/// already streaming keep the service handles they resolved when they
/// started; the next turn resolves the new graph. A profile that turns
/// HMR off stops the loop and reports that a restart is required.
async fn run_profile_hmr(
    state: Arc<AppState>,
    inputs: ProfileInputs,
    cancel_token: Arc<AtomicCancelToken>,
) {
    let mut watcher = ProfileWatcher::new(inputs.watched_paths());
    if let Err(error) = watcher.poll() {
        warn!(%error, "could not stamp the profile files; HMR starts anyway");
    }
    let mut ticker = tokio::time::interval(Duration::from_secs(1));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    ticker.tick().await;
    loop {
        tokio::select! {
            _ = ticker.tick() => {}
            () = cancel_token.cancelled() => break,
        }
        match watcher.poll() {
            Ok(false) => continue,
            Ok(true) => {}
            Err(error) => {
                warn!(%error, "could not poll the profile files");
                continue;
            }
        }
        let resolved = match inputs.resolve() {
            Ok(profile) => profile,
            Err(error) => {
                warn!(%error, "profile change could not be resolved; keeping the old set");
                continue;
            }
        };
        match state.reload_profile(resolved).await {
            Ok(diff) if diff.restart_required => {
                warn!(
                    "profile changed while `hmr = false`; a restart is \
                     required to apply it"
                );
                break;
            }
            Ok(diff) => info!(
                added = ?diff.added,
                removed = ?diff.removed,
                remounted = ?diff.remounted,
                volatile = ?diff.volatile_changed,
                "profile reloaded"
            ),
            Err(error) => {
                warn!(%error, "profile reload failed; keeping the mounted set");
            }
        }
    }
}

/// Install the shutdown handlers.
///
/// Ctrl-C and (on unix) SIGTERM — the container stop signal — walk
/// the same cancellation path, so `docker stop` and a keyboard
/// interrupt drain identically.
fn spawn_signal_handlers(cancel_token: Arc<AtomicCancelToken>) {
    #[cfg(unix)]
    {
        let cancel_token = Arc::clone(&cancel_token);
        tokio::spawn(async move {
            match signal::unix::signal(signal::unix::SignalKind::terminate()) {
                Ok(mut stream) => {
                    if stream.recv().await.is_some() {
                        info!(
                            "Received SIGTERM, initiating graceful shutdown..."
                        );
                        cancel_token.cancel();
                    }
                    if stream.recv().await.is_some() {
                        error!("Second SIGTERM received, aborting");
                        std::process::exit(1);
                    }
                }
                Err(e) => {
                    error!("Failed to listen for SIGTERM: {}", e);
                }
            }
        });
    }
    tokio::spawn(async move {
        match signal::ctrl_c().await {
            Ok(()) => {
                info!("Received Ctrl+C, initiating graceful shutdown...");
                cancel_token.cancel();
            }
            Err(e) => {
                error!("Failed to listen for shutdown signal: {}", e);
            }
        }
    });
}
#[cfg(test)]
mod tests {
    use clap::{CommandFactory, Parser};

    use super::{Args, Mode};

    /// The container images set `ELYSPER_HOST` / `ELYSPER_PORT`
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
        assert_eq!(env_of("bind").as_deref(), Some("ELYSPER_HOST"));
        assert_eq!(env_of("port").as_deref(), Some("ELYSPER_PORT"));
    }
    /// The binary with no subcommand is the core: the human entry
    /// point is `serve`, and a bare invocation must not hang on a
    /// gateway nobody asked for.
    #[test]
    fn a_bare_invocation_is_the_core() {
        let args = Args::try_parse_from(["elysper-app-server"])
            .expect("a bare invocation parses");
        assert!(args.mode.is_none());
    }
    /// The core and the gateway are two named modes.
    #[test]
    fn the_two_modes_parse() {
        let args = Args::try_parse_from(["elysper-app-server", "core"])
            .expect("the core mode parses");
        assert!(matches!(args.mode, Some(Mode::Core)));
        let args = Args::try_parse_from([
            "elysper-app-server",
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
}
