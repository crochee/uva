//! Elysper Server binary
//!
//! Server starts with empty/default config. APIs populate config at runtime.

use std::{path::PathBuf, sync::Arc};

use anyhow::Result;
use clap::Parser;
use elysper::core::{AtomicCancelToken, CancelToken};
use elysper_server::{
    build_info,
    shutdown::{
        HTTP_DRAIN_DEADLINE,
        TRACE_FLUSH_DEADLINE,
        drain_session_controllers,
    },
};
use tokio::signal;
use tracing::{error, info};

#[derive(Parser, Debug)]
#[command(name = "elysper-server")]
#[command(
    about = "Elysper Agent HTTP Server",
    version = build_info::LONG_VERSION,
)]
struct Args {
    /// Working directory
    #[arg(short, long, default_value = ".")]
    directory: PathBuf,

    /// Path to the deployment configuration file.
    ///
    /// Every section read at boot comes from this file: `providers`
    /// (base_url / api_key / models), `agents` (`max_steps`,
    /// `compaction`, `strategy`), `[tools]`, `auth`, `cors`,
    /// `operations`, and `mcp_servers`. The format is
    /// chosen by extension, so `.yaml`, `.yml`, and `.toml` all work.
    ///
    /// When omitted — or when the named file is missing or does not
    /// parse — the server tries `<directory>/config.toml`, then
    /// `<directory>/.elysper/config.toml`, and finally its own defaults;
    /// the provider then falls back to
    /// `<directory>/.agents/config.toml` and the environment
    /// (`OPENAI_API_KEY` / `ANTHROPIC_API_KEY`).
    #[arg(long, value_name = "PATH")]
    config: Option<PathBuf>,

    /// Host to bind to.
    ///
    /// Precedence: this flag, then `ELYSPER_HOST`, then `host` in the
    /// deployment config, then [`DEFAULT_HOST`] (127.0.0.1). The
    /// container images set `ELYSPER_HOST=0.0.0.0` — a container that
    /// binds loopback is unreachable through a published port.
    #[arg(long, env = "ELYSPER_HOST")]
    host: Option<String>,

    /// Port to bind to. Same precedence as `--host`.
    #[arg(short, long, env = "ELYSPER_PORT")]
    port: Option<u16>,
}

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    // Initialize tracing via `elysper-telemetry` so the OTLP
    // pipeline (ELYSPER_OTLP_ENDPOINT, etc.) and structured
    // layers from that crate are wired up. Falls back to
    // console tracing when OTLP is unset, matching the
    // `tracing_subscriber::fmt` behaviour the previous
    // inlined setup provided.
    elysper::telemetry::init_tracing(&elysper::telemetry::TelemetryConfig {
        service_name: "elysper-server".to_string(),
        log_level: std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
    })
    .map_err(|e| anyhow::anyhow!("failed to init tracing: {e}"))?;

    let args = Args::parse();

    // Create cancellation token for graceful shutdown
    let cancel_token = Arc::new(AtomicCancelToken::new());
    let cancel_token_for_shutdown = Arc::clone(&cancel_token);

    // Spawn task to handle shutdown signals. Ctrl-C and (on unix)
    // SIGTERM — the container stop signal — walk the same
    // cancellation path, so `docker stop` and a keyboard interrupt
    // drain identically.
    #[cfg(unix)]
    {
        let cancel_token_for_sigterm = Arc::clone(&cancel_token);
        tokio::spawn(async move {
            match signal::unix::signal(signal::unix::SignalKind::terminate()) {
                Ok(mut stream) => {
                    // First signal: graceful. A second one is the
                    // operator's "force" — tokio has replaced the
                    // default disposition, so this listener is the
                    // only thing that can still terminate the process.
                    if stream.recv().await.is_some() {
                        info!(
                            "Received SIGTERM, initiating graceful shutdown..."
                        );
                        cancel_token_for_sigterm.cancel();
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
                cancel_token_for_shutdown.cancel();
            }
            Err(e) => {
                error!("Failed to listen for shutdown signal: {}", e);
            }
        }
    });

    // Create and run server. The boot resolves the deployment config
    // once and returns the address it resolved from the CLI, the
    // environment, and that config — so what is logged, bound, and
    // announced is one value (R54).
    let booted = elysper_server::server::boot_server(
        args.directory.clone(),
        args.config.as_ref(),
        elysper_server::config::server::BindOverrides {
            host: args.host,
            // `args.port` is moved below; the address carries the value.
            port: args.port,
        },
    )
    .await
    .map_err(|e| anyhow::anyhow!("failed to build server: {e}"))?;
    let address = booted.address;
    info!(address = %address, "binding");
    let listener =
        tokio::net::TcpListener::bind((address.host.as_str(), address.port))
            .await
            .map_err(|e| anyhow::anyhow!("failed to bind {address}: {e}"))?;

    // Announce the probe endpoints once the router is built and
    // the listener is bound: from this point on, `/livez`
    // answers liveness (process serves HTTP) and `/readyz`
    // answers readiness (bootstrap completed before the bind, so
    // the first probe already reports ready). Orchestrators and
    // load balancers should wire these two URLs into their
    // liveness / readiness probe configuration.
    info!(
        host = %address.host,
        port = address.port,
        livez = format!("http://{address}/livez"),
        readyz = format!("http://{address}/readyz"),
        "listening; probes ready"
    );

    // Drain the session controllers (R19), *concurrently* with the
    // graceful shutdown below. The order matters both ways:
    //
    // - the drain must not wait for `axum::serve` to return: a
    //   connected SSE client (`GET .../follow`) holds its body open
    //   until the controller dies, so an "await serve, then close"
    //   drain would deadlock and the orchestrator would SIGKILL
    //   (exit 137) with every lease unreleased.
    // - the controllers must close for the serve future to finish at
    //   all: closing a controller ends its follow streams, which is
    //   what lets axum's graceful drain complete.
    //
    // `close()` walks the full chain per session: cancel the
    // in-flight run → the controller loop exits → `bus.release(...)`
    // → the writer flushes. The closes run concurrently under one
    // global deadline chosen to fit inside a container's 10 s stop
    // grace period.
    let drain_token = Arc::clone(&cancel_token);
    let drain_state = Arc::clone(&booted.state);
    let drain = tokio::spawn(async move {
        drain_token.cancelled().await;
        drain_session_controllers(&drain_state).await;
    });

    // The HTTP server. Its future owns the listener and the accept
    // loop, so it has to be *polled* for the node to answer anything:
    // it therefore runs as its own task while `main` waits for the
    // signal.
    //
    // The graceful-shutdown future resolves when the cancel token
    // fires — that is the *signal*, not the end of the server's life.
    // The serve future therefore runs for the server's whole lifetime
    // and only returns once the node is stopping. It must never be
    // wrapped in a timeout that starts at boot: a boot-anchored
    // timeout tears the node down `HTTP_DRAIN_DEADLINE` after start,
    // with no signal ever sent.
    let serve_token = Arc::clone(&cancel_token);
    let mut serve_task = tokio::spawn(async move {
        let served = axum::serve(listener, booted.router)
            .with_graceful_shutdown(async move {
                serve_token.cancelled().await;
            })
            .await;
        if let Err(error) = served {
            error!("shutdown: the HTTP server stopped with an error: {error}");
        }
    });

    // The node serves until a signal arrives — only now does the
    // shutdown budget start.
    cancel_token.cancelled().await;

    // The post-signal HTTP drain, bounded: the graceful drain stops
    // *accepting* the moment the token fires, and closing the
    // controllers (above) ends their follow streams — but any
    // connection that still lingers past `HTTP_DRAIN_DEADLINE` (a
    // misbehaving client, a future route whose body never ends) must
    // not hold the process past a container's 10 s stop grace either.
    // On timeout the serve task is aborted — its connections die with
    // the process, after the controller drain has already released
    // every lease.
    let served =
        tokio::time::timeout(HTTP_DRAIN_DEADLINE, &mut serve_task).await;
    match served {
        Ok(Ok(())) => {}
        Ok(Err(join_error)) => {
            error!("shutdown: the HTTP server task failed: {join_error}");
        }
        Err(_) => {
            error!(
                "shutdown: HTTP drain exceeded {HTTP_DRAIN_DEADLINE:?}; \
                 dropping lingering connections"
            );
            serve_task.abort();
        }
    }

    // The listener is down; the drain started with the signal, so all
    // it can have left is the tail of the last close. Bounded — never
    // a new hang.
    let _ = drain.await;

    // R-OPT-1 Step 3: drain pending OTLP spans + metrics
    // *after* the listener has stopped accepting new connections and
    // in-flight requests have completed, so the last batch of
    // telemetry leaves the process. Without this call a SIGTERM run
    // loses the trailing tail. The flush is synchronous (a provider
    // shutdown + `force_flush` whose exporter timeout is not ours to
    // configure), so it runs on the blocking pool under its own
    // deadline: after the 8 s HTTP/controller drain only ~1 s of the
    // 10 s container grace is left, and losing the telemetry tail is
    // strictly better than exiting 137 after every lease was already
    // released.
    let flush =
        tokio::task::spawn_blocking(
            || match elysper::telemetry::flush_traces() {
                Ok(()) => Ok(()),
                Err(e) => {
                    error!("Failed to flush OTLP traces on shutdown: {}", e);
                    Err(())
                }
            },
        );
    if tokio::time::timeout(TRACE_FLUSH_DEADLINE, flush)
        .await
        .is_err()
    {
        error!(
            "shutdown: OTLP trace flush exceeded {TRACE_FLUSH_DEADLINE:?}; \
             leaving without it"
        );
    }

    info!("Server shutdown complete");

    Ok(())
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::Args;

    /// The container images set `ELYSPER_HOST` / `ELYSPER_PORT`
    /// instead of passing flags. This asserts the binding on the
    /// **parsed CLI model** — not on a substring of this file's
    /// source text — so a rename of the env var or a dropped
    /// `env = …` attribute fails here.
    #[test]
    fn the_cli_binds_the_env_vars_the_images_set() {
        let command = Args::command();
        let env_of = |id: &str| {
            command
                .get_arguments()
                .find(|arg| arg.get_id() == id)
                .and_then(|arg| arg.get_env())
                .map(|value| value.to_string_lossy().into_owned())
        };
        assert_eq!(env_of("host").as_deref(), Some("ELYSPER_HOST"));
        assert_eq!(env_of("port").as_deref(), Some("ELYSPER_PORT"));
    }
}
