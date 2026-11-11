//! Supervise the gateway process for a host.
//!
//! A host (the TUI, the desktop shell) does not embed the gateway —
//! it launches `elysper-app-server serve` as a child process and
//! speaks to it over its WebSocket. This module owns the child side
//! of that contract:
//!
//! - **launch** — `serve --bind ADDR --port PORT --directory DIR`
//!   plus the optional `--web-dir` / `--profile` / `--bundle`
//!   / `--patch`, with the child's stderr inherited so its logs land
//!   on the host's stderr;
//! - **readiness** — the probe reads the `/readyz` **status line**
//!   (`HTTP/1.1 200`), never just "the port accepts a connection":
//!   a gateway that is listening but whose core is still booting is
//!   not ready;
//! - **restart** — a dead generation is relaunched with the same
//!   1s → 5s backoff `serve` itself uses for the core;
//! - **reap** — dropping the [`Gateway`] handle kills the child and
//!   waits for it, so no orphan survives the host.
//!
//! The spawn step, the probe, and the backoff sleep are seams: the
//! production implementations run real OS processes and sockets, and
//! the tests script them, so the whole loop is exercised without a
//! gateway binary.

use std::{
    io,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use futures::{
    channel::{mpsc, oneshot},
    future::BoxFuture,
};
use parking_lot::Mutex;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

/// How often the readiness probe retries while the gateway boots.
const PROBE_INTERVAL: Duration = Duration::from_millis(200);

/// How long the probe may keep failing before the supervisor gives
/// up on a generation. The gateway boots its own core, so the budget
/// has to cover a cold start, not just a bind.
const READY_TIMEOUT: Duration = Duration::from_secs(30);

/// How long a single probe read may hang before it counts as a
/// failure. A gateway that accepts the connection but never answers
/// is not ready and must not wedge the loop.
const PROBE_READ_TIMEOUT: Duration = Duration::from_secs(5);

/// How often the process monitor re-checks `try_wait`. The poll is
/// what lets a synchronous `kill()` always find the child handle
/// free (the monitor never holds it across an await).
const WAIT_POLL: Duration = Duration::from_millis(50);

/// Where the gateway binary is and how it is launched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayOptions {
    /// `elysper-app-server` executable path.
    pub executable: PathBuf,
    /// The address the gateway binds; defaults to `127.0.0.1`.
    pub bind: String,
    /// The gateway port; `0` means this crate picks a free port.
    pub port: u16,
    /// The workspace directory (passed through to
    /// `serve --directory`).
    pub directory: PathBuf,
    /// The plugin profile TOML, when overriding the default lookup.
    pub profile: Option<PathBuf>,
    /// The bundle seed TOML (the lowest layer), if any.
    pub bundle: Option<PathBuf>,
    /// The patch TOML layered over the profile, if any.
    pub patch: Option<PathBuf>,
    /// The directory holding the web front-end's built files.
    pub web_dir: Option<PathBuf>,
}

impl GatewayOptions {
    /// Options with the host defaults: loopback bind, an
    /// OS-chosen port, and no profile layering.
    pub fn new(executable: PathBuf, directory: PathBuf) -> Self {
        Self {
            executable,
            bind: "127.0.0.1".to_owned(),
            port: 0,
            directory,
            profile: None,
            bundle: None,
            patch: None,
            web_dir: None,
        }
    }
}

/// What the supervisor observed about the gateway process.
///
/// The stream is a narration: [`Gateway::states`] yields one of
/// these per transition, so a host can render "starting…", lose the
/// "ready" badge during a restart, and show the failure reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayState {
    /// The gateway process was launched and the probe is running.
    Starting,
    /// The probe read a `200` status line; `base_url` and `ws_url`
    /// are live. Emitted again after every restart, with the same
    /// URLs — the port is chosen once and reused across generations.
    Ready { base_url: String, ws_url: String },
    /// The gateway process died; generation `attempt` is being
    /// relaunched after the backoff. The URLs stay valid to read but
    /// nothing answers on them until the next `Ready`.
    Restarting { attempt: u32 },
    /// The supervisor gave up (the probe never succeeded) and
    /// stopped; no further states follow.
    Failed { reason: String },
}

/// Why [`Gateway::start`] could not hand back a ready gateway.
#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    /// The child could not be spawned at all — including the
    /// up-front failure to reserve a free port for it.
    #[error("failed to spawn {program}: {source}")]
    Spawn {
        /// The executable that was launched.
        program: String,
        /// The OS error.
        source: std::io::Error,
    },
    /// The process ran but `/readyz` never answered `200` in time.
    #[error("the gateway never became ready: {reason}")]
    NotReady {
        /// What the last probe attempt saw.
        reason: String,
    },
    /// The gateway was shut down before the first ready.
    #[error("the gateway was shut down")]
    Shutdown,
}

/// The supervisor's handle to one launched gateway.
///
/// Production wraps a [`tokio::process::Child`]; a test scripts its
/// death. `next_frame` drains the child's stdout (which carries no
/// protocol — the gateway speaks over its WebSocket) so a stray
/// write can never block the child on a full pipe.
pub(crate) trait GatewayProcess: Send + Sync + 'static {
    /// Read the next chunk of the child's stdout; `None` once it
    /// ends.
    fn next_frame(&self) -> BoxFuture<'static, Option<Vec<u8>>>;
    /// Resolve when the process has ended (and, for the production
    /// impl, reap it).
    fn wait(&self) -> BoxFuture<'static, io::Result<()>>;
    /// Signal the process to die, from any context.
    fn kill(&self);
}

/// How the supervisor launches one gateway generation.
///
/// The production impl spawns the real `serve` binary; the tests
/// record the options and hand back a scripted process.
pub(crate) trait Launcher: Send + Sync + 'static {
    /// Launch the gateway described by `options`.
    fn launch(
        &self,
        options: &GatewayOptions,
    ) -> io::Result<Box<dyn GatewayProcess>>;
}

/// The backoff sleep as the supervisor injects it: production waits
/// for real, the tests record the schedule and return at once.
pub(crate) type Sleeper =
    Arc<dyn Fn(Duration) -> BoxFuture<'static, ()> + Send + Sync>;

/// The production sleeper.
fn production_sleeper() -> Sleeper {
    Arc::new(|duration| {
        Box::pin(async move { tokio::time::sleep(duration).await })
    })
}

/// How long the first restart waits before the gateway is spawned
/// again — the same floor `serve` uses for the core.
const RESTART_BACKOFF_MIN_SECS: u64 = 1;

/// The longest a restart waits.
const RESTART_BACKOFF_MAX_SECS: u64 = 5;

/// The wait before generation `attempt` (1-based) relaunches:
/// `min(1s · 2^(attempt-1), 5s)`.
pub(crate) const fn backoff_secs(attempt: u32) -> u64 {
    let capped = if attempt < 1 {
        1
    } else if attempt > 4 {
        4
    } else {
        attempt
    };
    let secs = RESTART_BACKOFF_MIN_SECS * (1 << (capped - 1));
    if secs > RESTART_BACKOFF_MAX_SECS {
        RESTART_BACKOFF_MAX_SECS
    } else {
        secs
    }
}

/// The readiness probe as the supervisor injects it: dial
/// `bind:port`, ask `/readyz`, and report the HTTP status code.
pub(crate) type Probe = Arc<
    dyn Fn(&str, u16) -> BoxFuture<'static, Result<u16, String>> + Send + Sync,
>;

/// The knobs the supervisor loops on; tests shrink them so a probe
/// can give up inside milliseconds.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SuperviseTimings {
    /// Pause between two probe attempts.
    pub probe_interval: Duration,
    /// Total budget for one generation's probe before `Failed`.
    pub ready_timeout: Duration,
}

/// The URLs the gateway answers on. Chosen once at `start` and
/// immutable for the handle's life, so [`Gateway::base_url`] can
/// return a `&str` and a restart cannot tear the pair apart.
struct GatewayUrls {
    base_url: String,
    ws_url: String,
}

/// What the handle and the supervisor task share.
struct SuperviseShared {
    states_tx: mpsc::UnboundedSender<GatewayState>,
    /// The one receiver [`Gateway::states`] hands out.
    states_rx: Mutex<Option<mpsc::UnboundedReceiver<GatewayState>>>,
    /// The generation the supervisor owns, so a synchronous
    /// `shutdown` can kill it.
    current: Mutex<Option<Arc<dyn GatewayProcess>>>,
    shutdown: AtomicBool,
    /// Wakes the supervisor when the host stops, so a wait in the
    /// restart backoff ends at once instead of after the full
    /// window.
    stopped: tokio::sync::Notify,
}

/// The supervisor's injectable seams and loop timings, bundled so
/// they travel as one object into the supervision task.
pub(crate) struct SuperviseParts {
    /// How one gateway generation is launched.
    pub(crate) launcher: Arc<dyn Launcher>,
    /// The `/readyz` probe.
    pub(crate) probe: Probe,
    /// The restart backoff sleep.
    pub(crate) sleeper: Sleeper,
    /// The probe cadence and budget.
    pub(crate) timings: SuperviseTimings,
}

impl std::fmt::Debug for Gateway {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Gateway")
            .field("base_url", &self.urls.base_url)
            .field("ws_url", &self.urls.ws_url)
            .finish_non_exhaustive()
    }
}

/// A supervised, ready gateway.
///
/// Dropping the handle kills the child and reaps it — the host never
/// needs to (and never should) manage the process itself.
pub struct Gateway {
    urls: Arc<GatewayUrls>,
    shared: Arc<SuperviseShared>,
}

impl Gateway {
    /// Launch `elysper-app-server serve` and resolve once its
    /// `/readyz` answers `200`.
    ///
    /// The URLs are immutable from here on: `port: 0` is resolved to
    /// a concrete port before the child is spawned, and restarts
    /// reuse it.
    pub fn start(
        options: GatewayOptions,
    ) -> BoxFuture<'static, Result<Gateway, GatewayError>> {
        Box::pin(Self::start_with(
            options,
            SuperviseParts {
                launcher: Arc::new(ProcessLauncher),
                probe: production_probe(),
                sleeper: production_sleeper(),
                timings: SuperviseTimings {
                    probe_interval: PROBE_INTERVAL,
                    ready_timeout: READY_TIMEOUT,
                },
            },
        ))
    }

    /// `start` with the seams the tests script.
    pub(crate) async fn start_with(
        options: GatewayOptions,
        parts: SuperviseParts,
    ) -> Result<Gateway, GatewayError> {
        let port =
            pick_port(&options.bind, options.port).map_err(|source| {
                GatewayError::Spawn {
                    program: options.executable.display().to_string(),
                    source,
                }
            })?;
        let effective = GatewayOptions { port, ..options };
        let urls = Arc::new(GatewayUrls {
            base_url: format!("http://{}:{port}", effective.bind),
            ws_url: format!("ws://{}:{port}/rpc", effective.bind),
        });
        let (states_tx, states_rx) = mpsc::unbounded();
        let shared = Arc::new(SuperviseShared {
            states_tx: states_tx.clone(),
            states_rx: Mutex::new(Some(states_rx)),
            current: Mutex::new(None),
            shutdown: AtomicBool::new(false),
            stopped: tokio::sync::Notify::new(),
        });
        let (outcome_tx, outcome_rx) = oneshot::channel();
        tokio::spawn(supervise(
            effective,
            shared.clone(),
            parts,
            urls.clone(),
            Some(outcome_tx),
        ));
        match outcome_rx.await {
            Ok(Ok(())) => Ok(Gateway { urls, shared }),
            Ok(Err(error)) => Err(error),
            // The supervisor task ended before the first generation
            // resolved — only a shutdown can do that.
            Err(_) => Err(GatewayError::Shutdown),
        }
    }

    /// The gateway's HTTP root, e.g. `http://127.0.0.1:9000`.
    pub fn base_url(&self) -> &str {
        &self.urls.base_url
    }

    /// The gateway's WebSocket endpoint, `base_url` + `/rpc` on the
    /// same port.
    pub fn ws_url(&self) -> &str {
        &self.urls.ws_url
    }

    /// The state narration, starting with the very first
    /// [`GatewayState::Starting`]. The first call takes the stream;
    /// later calls get an empty one.
    pub fn states(&self) -> mpsc::UnboundedReceiver<GatewayState> {
        match self.shared.states_rx.lock().take() {
            Some(receiver) => receiver,
            None => mpsc::unbounded().1,
        }
    }

    /// Stop supervising: kill the live generation and reap it. Idempotent.
    pub fn shutdown(&self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        // Wake the supervisor: a wait in the restart backoff must
        // end now, not after the window — a generation started
        // afterwards would be supervised by nobody.
        self.shared.stopped.notify_one();
        if let Some(process) = self.shared.current.lock().as_ref() {
            process.kill();
        }
    }
}

impl Drop for Gateway {
    /// The host never manages the process itself: dropping the
    /// handle is the whole shutdown.
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Resolve the port the gateway is launched on: the caller's port
/// when it is free, an OS-chosen one when it is `0` **or already
/// taken**. The check releases the port again so the gateway can
/// bind it; a brief race between the release and the child's bind is
/// accepted (the same trade every "pick a port, then exec" host
/// makes).
fn pick_port(bind: &str, port: u16) -> io::Result<u16> {
    if port != 0 {
        match std::net::TcpListener::bind((bind, port)) {
            Ok(listener) => {
                drop(listener);
                return Ok(port);
            }
            // The port is taken: handing it to `serve` anyway would
            // leave the gateway restarting forever on a port that
            // never opens.
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {}
            Err(error) => return Err(error),
        }
    }
    let listener = std::net::TcpListener::bind((bind, 0))?;
    Ok(listener.local_addr()?.port())
}

/// The production probe: one `GET /readyz` whose **status line** is
/// parsed. Anything but a readable `HTTP/x.y <code>` first line is
/// an error — a gateway that merely accepts the connection has not
/// said it is ready.
pub(crate) async fn probe_ready(bind: &str, port: u16) -> Result<u16, String> {
    let mut stream = tokio::net::TcpStream::connect((bind, port))
        .await
        .map_err(|error| format!("connect failed: {error}"))?;
    let request = format!(
        "GET /readyz HTTP/1.1\r\nHost: {bind}:{port}\r\nConnection: close\r\n\r\n"
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("write failed: {error}"))?;

    let mut buf = Vec::new();
    let mut chunk = [0u8; 256];
    loop {
        let read =
            tokio::time::timeout(PROBE_READ_TIMEOUT, stream.read(&mut chunk))
                .await
                .map_err(|_| "the readiness probe timed out".to_owned())?
                .map_err(|error| format!("read failed: {error}"))?;
        if read == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..read]);
        if buf.contains(&b'\n') || buf.len() > 8192 {
            break;
        }
    }

    let text = String::from_utf8_lossy(&buf);
    let status_line = text.lines().next().unwrap_or("");
    status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("no status line: {status_line:?}"))?
        .parse::<u16>()
        .map_err(|_| format!("unparseable status line: {status_line:?}"))
}

/// The production probe as an injectable [`Probe`].
fn production_probe() -> Probe {
    Arc::new(|bind: &str, port: u16| {
        let bind = bind.to_owned();
        Box::pin(async move { probe_ready(&bind, port).await })
    })
}

/// Why the readiness probe gave up early: nobody was waiting for the
/// answer any more.
const STOPPED_BEFORE_READY: &str =
    "the gateway was stopped before it became ready";

/// Probe until the gateway is ready or the budget runs out.
///
/// `stopped` tells the loop that nobody is waiting for the answer any
/// more — the host dropped the handle, or abandoned the boot.
/// Probing on would burn the whole readiness budget for nobody.
async fn probe_until_ready(
    probe: &Probe,
    bind: &str,
    port: u16,
    timings: SuperviseTimings,
    stopped: &(dyn Fn() -> bool + Send + Sync),
) -> Result<(), String> {
    let deadline = tokio::time::Instant::now() + timings.ready_timeout;
    let mut last;
    loop {
        if stopped() {
            return Err(STOPPED_BEFORE_READY.to_owned());
        }
        match probe(bind, port).await {
            Ok(200) => return Ok(()),
            Ok(code) => last = format!("HTTP status {code}"),
            Err(reason) => last = reason,
        }
        if stopped() {
            return Err(STOPPED_BEFORE_READY.to_owned());
        }
        if tokio::time::Instant::now() >= deadline {
            return Err(format!(
                "readyz never answered 200 (last attempt: {last})"
            ));
        }
        tokio::time::sleep(timings.probe_interval).await;
    }
}

/// Whether the supervisor must stop: the host stopped, or the caller
/// that awaited the first outcome went away.
fn stop_requested(
    shared: &SuperviseShared,
    outcome: &Option<oneshot::Sender<Result<(), GatewayError>>>,
) -> bool {
    shared.shutdown.load(Ordering::SeqCst)
        || outcome.as_ref().is_some_and(oneshot::Sender::is_canceled)
}

/// Emit one state to every listener.
fn emit(shared: &SuperviseShared, state: GatewayState) {
    let _ = shared.states_tx.unbounded_send(state);
}

/// Answer `start`'s outcome exactly once, reporting whether anyone
/// was left to hear it.
fn reply(
    outcome: &mut Option<oneshot::Sender<Result<(), GatewayError>>>,
    result: Result<(), GatewayError>,
) -> bool {
    outcome
        .take()
        .is_some_and(|sender| sender.send(result).is_ok())
}

/// The supervision loop. Runs on its own task; the first
/// generation's outcome is reported through `outcome`, later
/// generations only narrate through the states channel. A dead
/// generation is restarted after a 1s → 5s backoff; a shutdown
/// stops the loop, reaps, and returns.
async fn supervise(
    options: GatewayOptions,
    shared: Arc<SuperviseShared>,
    parts: SuperviseParts,
    urls: Arc<GatewayUrls>,
    mut outcome: Option<oneshot::Sender<Result<(), GatewayError>>>,
) {
    let SuperviseParts {
        launcher,
        probe,
        sleeper,
        timings,
    } = parts;
    let mut attempt = 0_u32;
    loop {
        // A shutdown that landed between two generations must not
        // start another one: the new child would be supervised by a
        // task nobody owns, and nothing could ever kill it.
        if shared.shutdown.load(Ordering::SeqCst) {
            reap_current(&shared).await;
            return;
        }

        emit(&shared, GatewayState::Starting);

        let process: Arc<dyn GatewayProcess> = match launcher.launch(&options) {
            Ok(process) => process.into(),
            Err(source) => {
                let error = GatewayError::Spawn {
                    program: options.executable.display().to_string(),
                    source,
                };
                emit(
                    &shared,
                    GatewayState::Failed {
                        reason: error.to_string(),
                    },
                );
                reply(&mut outcome, Err(error));
                return;
            }
        };
        *shared.current.lock() = Some(process.clone());

        // A shutdown that landed while the child was starting: the
        // generation would be an orphan unless we kill it here.
        if shared.shutdown.load(Ordering::SeqCst) {
            reap_current(&shared).await;
            return;
        }

        let probe_result = probe_until_ready(
            &probe,
            &options.bind,
            options.port,
            timings,
            &|| stop_requested(&shared, &outcome),
        )
        .await;
        if let Err(reason) = probe_result {
            process.kill();
            let _ = process.wait().await;
            *shared.current.lock() = None;
            if stop_requested(&shared, &outcome) {
                // Nobody is waiting for the answer any more: there is
                // no failure to report, only a stop.
                reply(&mut outcome, Err(GatewayError::Shutdown));
                return;
            }
            emit(
                &shared,
                GatewayState::Failed {
                    reason: reason.clone(),
                },
            );
            reply(&mut outcome, Err(GatewayError::NotReady { reason }));
            return;
        }

        emit(
            &shared,
            GatewayState::Ready {
                base_url: urls.base_url.clone(),
                ws_url: urls.ws_url.clone(),
            },
        );
        // The caller of `start` vanished before its outcome — nobody
        // owns this gateway anymore, so reap the generation instead
        // of supervising (and restarting) an orphan forever. Later
        // generations have no outcome left to answer and supervise on
        // the live handle's behalf.
        let awaited_outcome = outcome.is_some();
        if !reply(&mut outcome, Ok(())) && awaited_outcome {
            shared.shutdown.store(true, Ordering::SeqCst);
            process.kill();
            let _ = process.wait().await;
            *shared.current.lock() = None;
            return;
        }

        // Hold the fort until the process ends.
        let _ = monitor(&process).await;

        if shared.shutdown.load(Ordering::SeqCst) {
            reap_current(&shared).await;
            return;
        }

        attempt += 1;
        emit(&shared, GatewayState::Restarting { attempt });
        // The backoff ends at once when the host stops: a
        // generation started after that would be supervised by
        // nobody.
        let mut backoff = sleeper(Duration::from_secs(backoff_secs(attempt)));
        let stopped = shared.stopped.notified();
        tokio::pin!(stopped);
        tokio::select! {
            _ = &mut backoff => {}
            _ = &mut stopped => {}
        }
    }
}

/// Kill and reap the generation the supervisor still owns, so no
/// child outlives the task that supervises it.
async fn reap_current(shared: &SuperviseShared) {
    let owned = shared.current.lock().take();
    if let Some(process) = owned {
        process.kill();
        let _ = process.wait().await;
    }
}

/// Watch one generation until it ends: drain its stdout while it
/// lives, and resolve on the process's own exit.
async fn monitor(process: &Arc<dyn GatewayProcess>) -> io::Result<()> {
    let mut stdout_open = true;
    loop {
        if !stdout_open {
            return process.wait().await;
        }
        tokio::select! {
            frame = process.next_frame() => {
                if frame.is_none() {
                    stdout_open = false;
                }
            }
            result = process.wait() => return result,
        }
    }
}

/// The production launcher: spawn the real `serve` binary.
struct ProcessLauncher;

impl Launcher for ProcessLauncher {
    fn launch(
        &self,
        options: &GatewayOptions,
    ) -> io::Result<Box<dyn GatewayProcess>> {
        let mut command = tokio::process::Command::new(&options.executable);
        command
            .arg("serve")
            .arg("--bind")
            .arg(&options.bind)
            .arg("--port")
            .arg(options.port.to_string())
            .arg("--directory")
            .arg(&options.directory);
        for (flag, path) in [
            ("--profile", &options.profile),
            ("--bundle", &options.bundle),
            ("--patch", &options.patch),
            ("--web-dir", &options.web_dir),
        ] {
            if let Some(path) = path {
                command.arg(flag).arg(path);
            }
        }
        // stdin: the gateway never reads commands from us. stdout:
        // piped only so `next_frame` can drain it — it carries no
        // protocol. stderr: inherited, so the gateway's logs land on
        // the host's terminal.
        command
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::inherit())
            .kill_on_drop(true);
        let mut child = command.spawn()?;
        let stdout = child.stdout.take();
        Ok(Box::new(ChildProcess {
            child: Arc::new(Mutex::new(Some(child))),
            stdout: Arc::new(tokio::sync::Mutex::new(stdout)),
        }))
    }
}

/// The production [`GatewayProcess`]: one spawned `serve` child.
struct ChildProcess {
    /// The child, if not yet reaped. A synchronous mutex is enough:
    /// the monitor polls `try_wait` and never holds it across an
    /// await, which is also what lets `kill` always get through.
    child: Arc<Mutex<Option<tokio::process::Child>>>,
    /// The drained stdout pipe.
    stdout: Arc<tokio::sync::Mutex<Option<tokio::process::ChildStdout>>>,
}

impl GatewayProcess for ChildProcess {
    fn next_frame(&self) -> BoxFuture<'static, Option<Vec<u8>>> {
        let stdout = self.stdout.clone();
        Box::pin(async move {
            let mut pipe = stdout.lock().await;
            let stream = pipe.as_mut()?;
            let mut chunk = [0u8; 4096];
            match stream.read(&mut chunk).await {
                Ok(0) | Err(_) => {
                    *pipe = None;
                    None
                }
                Ok(read) => Some(chunk[..read].to_vec()),
            }
        })
    }

    fn wait(&self) -> BoxFuture<'static, io::Result<()>> {
        let child = self.child.clone();
        Box::pin(async move {
            loop {
                let reaped = {
                    let mut guard = child.lock();
                    match guard.as_mut() {
                        Some(live) => live.try_wait()?.is_some(),
                        None => true,
                    }
                };
                if reaped {
                    *child.lock() = None;
                    return Ok(());
                }
                tokio::time::sleep(WAIT_POLL).await;
            }
        })
    }

    fn kill(&self) {
        if let Some(child) = self.child.lock().as_mut() {
            let _ = child.start_kill();
        }
    }
}
