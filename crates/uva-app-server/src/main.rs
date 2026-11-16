//! The Uva app server.
//!
//! One binary, two modes:
//!
//! - `core` (the default) — the headless agent runtime: JSON-RPC
//!   2.0 on stdin/stdout, one Content-Length-framed message per
//!   `Content-Length: <n>\r\n\r\n<payload>` frame (see `rpc::framing`).
//!   This is the mode a TUI, a desktop shell or any other host
//!   spawns as a sidecar.
//! - `serve` — the gateway: it spawns **one** core subprocess and
//!   serves it over a WebSocket (`/rpc`), alongside the HTTP probes
//!   and the web front-end's static files.
//!
//! The core owns the agent logic; the gateway owns no logic at all.
//! It pumps frames in both directions and supervises the subprocess.
//!
//! ```text
//! uva-app-server core --directory /srv/uva
//! uva-app-server serve --port 8080 --web-dir ./dist
//! ```
//!
//! Running the binary with no subcommand is `core`: the human entry
//! point is `serve`.
use std::{path::PathBuf, sync::Arc, time::Duration};

use anyhow::Result;
use clap::{CommandFactory, Parser};
use tokio::signal;
use tracing::{error, info, warn};
use uva::core::{AtomicCancelToken, CancelToken};
use uva_app_server::{
    boot::{boot_core, dump_profile},
    build_info,
    cli::{Args, Mode},
    config::server::BindOverrides,
    gateway,
    profile::{Profile, ProfileWatcher, discover_home_patch, resolve_files},
    rpc::{RpcRouter, StioPump, methods},
    state::AppState,
};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    // Initialize tracing via `uva-telemetry` so the OTLP
    // pipeline (UVA_OTLP_ENDPOINT, etc.) and structured
    // layers from that crate are wired up. Falls back to console
    // tracing when OTLP is unset.
    //
    // The logs go to **stderr**: in `core` mode stdout is the
    // protocol channel, and a log line there would reach the peer
    // as a malformed frame.
    uva::telemetry::init_tracing(&uva::telemetry::TelemetryConfig {
        service_name: "uva-app-server".to_string(),
        log_level: std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        log_writer: uva::telemetry::LogWriter::Stderr,
    })
    .map_err(|e| anyhow::anyhow!("failed to init tracing: {e}"))?;
    let args = Args::parse();
    // stdout is the core's protocol channel: its help and version
    // render on stderr, so a peer never reads them as frames.
    if let Some(Mode::Core(core)) = &args.mode {
        if core.version {
            eprintln!("{}", build_info::LONG_VERSION);
            return Ok(());
        }
        if core.help {
            eprint!("{}", core_help());
            return Ok(());
        }
    }
    let inputs = ProfileInputs {
        directory: args.directory.clone(),
        config: args.config.clone(),
        bundle: args.bundle.clone(),
        profile: args.profile.clone(),
        patch: args.patch.clone(),
    };
    if args.dump_config {
        inputs.warn_retired_document();
        let profile = inputs.resolve()?;
        // The registered factory directory answers every row's
        // manifest, so the dump prints the `provides` / effective
        // `inject` the kernel will actually order and bind.
        // This mode prints the requested document and exits before the
        // JSON-RPC pump starts.
        print!("{}", dump_profile(&profile)); // stdout-ok: dump-config
        return Ok(());
    }
    let cancel_token = Arc::new(AtomicCancelToken::new());
    spawn_signal_handlers(Arc::clone(&cancel_token));
    match args.mode {
        None | Some(Mode::Core(_)) => run_core(inputs, cancel_token).await,
        Some(Mode::Translate(translate)) => {
            uva_app_server::translate::run(&translate, &args.directory)
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
/// The core's help text, rendered from the same command definition
/// clap would print — the core renders it on stderr because stdout is
/// its protocol channel.
fn core_help() -> String {
    let mut command = Args::command();
    // `build` is what propagates the global flags into the
    // sub-commands; without it the core's help would list only its
    // own two flags.
    command.build();
    match command.find_subcommand_mut("core") {
        Some(core) => core.render_long_help().to_string(),
        None => String::new(),
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
    /// The document layer's path: the explicit `--profile`, or the
    /// project's configuration layer.
    ///
    /// The default is `<directory>/.agents/profile.toml` — the same
    /// layer the provider fallback (`<directory>/.agents/config.toml`)
    /// and the seeded skills live in. The document is generated state
    /// as much as it is source (the settings surface rewrites it
    /// whole), so it does not belong in the workspace root, where it
    /// would be a stray artefact in every source checkout that runs a
    /// server.
    fn document_path(&self) -> PathBuf {
        self.profile.clone().unwrap_or_else(|| {
            uva_app_server::profile::project_document(&self.directory)
        })
    }

    /// Say so when the deployment still carries the retired document.
    ///
    /// The document layer moved out of the workspace root into the
    /// project's configuration layer. A deployment that still has the
    /// old file would silently boot the built-in assembly, so the
    /// boot names the file it no longer reads.
    fn warn_retired_document(&self) {
        if self.profile.is_some() {
            return;
        }
        let retired = self.directory.join("profile.toml");
        if retired.is_file() {
            warn!(
                path = %retired.display(),
                "the workspace-root profile document is no longer read; \
                 the document layer is {}",
                self.document_path().display()
            );
        }
    }

    /// Resolve the profile the CLI asked for.
    ///
    /// Layer order: bundle patch → profile document (else
    /// `<directory>/.agents/profile.toml` when present) → home patch
    /// (`~/.agents/profile.toml` or `UVA_HOME_PROFILE`, only when
    /// it exists) → the CLI `--patch` overlay. `config.yaml`'s
    /// joins the profile layer, and a duplicate id fails. Absent
    /// optional files are skipped, never errors.
    fn resolve(&self) -> Result<Profile> {
        let profile_path = self.profile.clone().or_else(|| {
            let default = self.document_path();
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
    /// patch created while the process runs counts as a change on
    /// the next poll.
    /// The transactional write path's layer paths.
    ///
    /// The document layer is the explicit `--profile` or
    /// `<directory>/.agents/profile.toml` (the same default
    /// `resolve` reads); the other three are whatever the deployment
    /// declared.
    fn store(&self) -> uva_app_server::profile::ProfileStore {
        uva_app_server::profile::ProfileStore {
            bundle: self.bundle.clone(),
            doc: self.document_path(),
            home: uva_app_server::profile::discover_home_patch(),
            overlay: self.patch.clone(),
        }
    }

    fn watched_paths(&self) -> Vec<PathBuf> {
        let mut paths = Vec::new();
        if let Some(bundle) = &self.bundle {
            paths.push(bundle.clone());
        }
        paths.push(self.document_path());
        if let Some(explicit) =
            std::env::var_os(uva_app_server::profile::HOME_PATCH_ENV)
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
    inputs.warn_retired_document();
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
        Some(inputs.store()),
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
    uva_app_server::shutdown::drain_session_controllers(&shutdown_state).await;
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
    use super::*;

    /// The document layer is the project's configuration layer.
    ///
    /// The document in `<workspace>/.agents/` is the one the core
    /// reads and the one the settings surface writes back; the
    /// workspace-root file it used to be is not read at all.
    #[test]
    fn the_document_layer_lives_in_the_project_configuration_layer() {
        let dir = tempfile::tempdir().expect("a temp workspace");
        let agents = dir.path().join(".agents");
        std::fs::create_dir_all(&agents).expect("the configuration layer");
        std::fs::write(
            agents.join("profile.toml"),
            "[core]\nentries = [{ id = \"probe-document-row\", runtime = \
             \"native\" }]\n",
        )
        .expect("the document");
        std::fs::write(
            dir.path().join("profile.toml"),
            "[core]\nentries = [{ id = \"probe-retired-row\", runtime = \
             \"native\" }]\n",
        )
        .expect("the retired document");

        let inputs = ProfileInputs {
            directory: dir.path().to_path_buf(),
            config: None,
            bundle: None,
            profile: None,
            patch: None,
        };
        let resolved = inputs.resolve().expect("the profile resolves");
        assert!(
            resolved.contains("probe-document-row"),
            "the document in the configuration layer is read: {resolved:?}"
        );
        assert!(
            !resolved.contains("probe-retired-row"),
            "the retired workspace-root document is not read: {resolved:?}"
        );
        assert_eq!(
            inputs.store().doc,
            agents.join("profile.toml"),
            "the write path commits to the document layer"
        );
    }
}
