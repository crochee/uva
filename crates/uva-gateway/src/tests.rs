//! The gateway crate's tests.

use std::{
    collections::VecDeque,
    io::{Read as _, Write as _},
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::{FutureExt as _, StreamExt as _, future::BoxFuture};
use parking_lot::Mutex;

use crate::{
    Gateway,
    GatewayError,
    GatewayOptions,
    GatewayState,
    client::{dial, dial_within},
    supervisor::{
        GatewayProcess,
        Launcher,
        Probe,
        Sleeper,
        SuperviseParts,
        SuperviseTimings,
        backoff_secs,
        probe_ready,
    },
};

#[test]
fn options_default_to_loopback_and_an_unbound_port() {
    let options = GatewayOptions::new(
        PathBuf::from("uva-app-server"),
        PathBuf::from("/tmp"),
    );
    assert_eq!(options.bind, "127.0.0.1");
    assert_eq!(options.port, 0);
    assert!(options.profile.is_none());
}

#[test]
fn the_ready_state_carries_the_base_and_ws_urls() {
    let state = GatewayState::Ready {
        base_url: "http://127.0.0.1:9000".into(),
        ws_url: "ws://127.0.0.1:9000/rpc".into(),
    };
    match state {
        GatewayState::Ready { base_url, ws_url } => {
            assert!(base_url.starts_with("http://"));
            assert!(ws_url.ends_with("/rpc"));
        }
        other => panic!("unexpected state: {other:?}"),
    }
}

// ---- supervisor: launch + readiness (plan Task 3) ----

/// Timings fast enough to watch a probe give up inside a test.
const TEST_TIMINGS: SuperviseTimings = SuperviseTimings {
    probe_interval: Duration::from_millis(5),
    ready_timeout: Duration::from_millis(150),
};

fn options() -> GatewayOptions {
    GatewayOptions::new(PathBuf::from("uva-app-server"), PathBuf::from("/tmp"))
}

/// A fake gateway process: it "speaks" nothing on stdout, stays
/// alive until the test sets `exited` (or the supervisor kills it).
struct FakeProcess {
    exited: Arc<AtomicBool>,
    killed: Arc<AtomicBool>,
}

impl FakeProcess {
    fn alive() -> Self {
        Self {
            exited: Arc::new(AtomicBool::new(false)),
            killed: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl GatewayProcess for FakeProcess {
    fn next_frame(&self) -> BoxFuture<'static, Option<Vec<u8>>> {
        // The gateway's stdout carries no protocol, so the fake
        // closes it at once: the supervisor sees EOF and keeps
        // waiting on `wait`.
        Box::pin(async { None })
    }

    fn wait(&self) -> BoxFuture<'static, std::io::Result<()>> {
        let exited = self.exited.clone();
        let killed = self.killed.clone();
        Box::pin(async move {
            loop {
                if exited.load(Ordering::Relaxed)
                    || killed.load(Ordering::Relaxed)
                {
                    return Ok(());
                }
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
    }

    fn kill(&self) {
        self.killed.store(true, Ordering::Relaxed);
    }
}

/// A launcher that records the ports it was asked to serve on and
/// builds each generation's process with the test's factory.
struct RecordingLauncher {
    ports: Mutex<Vec<u16>>,
    killed_flags: Mutex<Vec<Arc<AtomicBool>>>,
    make: Box<dyn Fn(u32) -> FakeProcess + Send + Sync>,
}

impl RecordingLauncher {
    fn new(
        make: impl Fn(u32) -> FakeProcess + Send + Sync + 'static,
    ) -> Arc<Self> {
        Arc::new(Self {
            ports: Mutex::new(Vec::new()),
            killed_flags: Mutex::new(Vec::new()),
            make: Box::new(make),
        })
    }

    fn ports(&self) -> Vec<u16> {
        self.ports.lock().clone()
    }

    fn launch_count(&self) -> usize {
        self.ports.lock().len()
    }

    fn all_killed(&self) -> bool {
        self.killed_flags
            .lock()
            .iter()
            .all(|flag| flag.load(Ordering::Relaxed))
    }
}

impl Launcher for RecordingLauncher {
    fn launch(
        &self,
        options: &GatewayOptions,
    ) -> std::io::Result<Box<dyn GatewayProcess>> {
        let index = {
            let mut ports = self.ports.lock();
            let index = ports.len() as u32;
            ports.push(options.port);
            index
        };
        let process = (self.make)(index);
        self.killed_flags.lock().push(process.killed.clone());
        Ok(Box::new(process))
    }
}

/// A probe that answers from a script; once the script runs out it
/// answers `200` (the steady state of a healthy gateway). The
/// counter lets a test prove how many probes ran before ready.
fn scripted_probe(
    script: Vec<Result<u16, String>>,
) -> (Probe, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let script = Arc::new(Mutex::new(VecDeque::from(script)));
    let probe: Probe = {
        let calls = calls.clone();
        Arc::new(move |_bind: &str, _port: u16| {
            let calls = calls.clone();
            let script = script.clone();
            Box::pin(async move {
                calls.fetch_add(1, Ordering::SeqCst);
                match script.lock().pop_front() {
                    Some(step) => step,
                    None => Ok(200),
                }
            })
        })
    };
    (probe, calls)
}

/// `ws://127.0.0.1:9000/rpc` → `9000`.
fn port_of(url: &str) -> u16 {
    let authority = url.split_once("://").expect("scheme").1;
    let host_port = authority.split('/').next().expect("authority");
    host_port
        .rsplit_once(':')
        .unwrap_or((host_port, "80"))
        .1
        .parse()
        .expect("port")
}

#[tokio::test]
async fn the_supervisor_reports_ready_only_after_the_probe_returns_200() {
    let launcher = RecordingLauncher::new(|_| FakeProcess::alive());
    let (probe, calls) = scripted_probe(vec![Ok(503), Ok(503), Ok(200)]);

    let gateway = Gateway::start_with(
        options(),
        SuperviseParts {
            launcher: launcher.clone(),
            probe,
            sleeper: instant_sleeper(),
            timings: TEST_TIMINGS,
        },
    )
    .await
    .expect("the gateway should become ready");

    // Both 503s were consumed before ready: a supervisor that treats
    // "the port accepts a connection" as ready would have stopped at
    // the first probe.
    assert_eq!(calls.load(Ordering::SeqCst), 3);

    let mut states = gateway.states();
    assert_eq!(
        states.next().now_or_never(),
        Some(Some(GatewayState::Starting)),
        "starting is the first state"
    );
    let ready = states
        .next()
        .now_or_never()
        .flatten()
        .expect("ready follows starting");
    let GatewayState::Ready { base_url, ws_url } = ready else {
        panic!("expected ready, got {ready:?}");
    };

    // The state's URLs, the accessors, and the port `serve` was
    // given are all the same port.
    assert_eq!(launcher.ports(), vec![port_of(&base_url)]);
    assert_eq!(port_of(&base_url), port_of(&ws_url));
    assert_eq!(gateway.base_url(), base_url);
    assert_eq!(gateway.ws_url(), ws_url);

    gateway.shutdown();
}

#[tokio::test]
async fn a_zero_port_is_resolved_and_passed_to_serve() {
    let launcher = RecordingLauncher::new(|_| FakeProcess::alive());
    let (probe, _) = scripted_probe(vec![Ok(200)]);

    let options = options();
    assert_eq!(options.port, 0, "the default asks for a free port");

    let gateway = Gateway::start_with(
        options,
        SuperviseParts {
            launcher: launcher.clone(),
            probe,
            sleeper: instant_sleeper(),
            timings: TEST_TIMINGS,
        },
    )
    .await
    .expect("ready");

    let ports = launcher.ports();
    assert_eq!(ports.len(), 1, "serve was launched exactly once");
    let served = ports[0];
    assert_ne!(served, 0, "port 0 never reaches serve");
    assert_eq!(
        gateway.base_url(),
        format!("http://127.0.0.1:{served}"),
        "base_url carries the port serve got"
    );
    assert_eq!(
        gateway.ws_url(),
        format!("ws://127.0.0.1:{served}/rpc"),
        "ws_url carries the same port"
    );

    gateway.shutdown();
}

#[tokio::test]
async fn the_supervisor_gives_up_with_a_reason_when_the_probe_never_succeeds() {
    let launcher = RecordingLauncher::new(|_| FakeProcess::alive());
    let probe: Probe = Arc::new(|_bind: &str, _port: u16| {
        Box::pin(async { Err("connection refused".to_owned()) })
    });

    let error = Gateway::start_with(
        options(),
        SuperviseParts {
            launcher: launcher.clone(),
            probe,
            sleeper: instant_sleeper(),
            timings: TEST_TIMINGS,
        },
    )
    .await
    .expect_err("the probe never succeeds");
    assert!(
        launcher.all_killed(),
        "a failed generation is killed, not leaked"
    );

    match error {
        GatewayError::NotReady { reason } => {
            assert!(!reason.is_empty());
            assert!(
                reason.contains("ready") || reason.contains("就绪"),
                "the reason is readable: {reason}"
            );
        }
        other => panic!("expected NotReady, got {other:?}"),
    }
}

#[tokio::test]
async fn the_probe_reads_the_status_line_not_just_an_open_port() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = std::thread::spawn(move || {
        let responses = [
            "HTTP/1.1 503 Service Unavailable\r\n\r\n",
            "HTTP/1.1 200 OK\r\n\r\n",
            "garbage, not even http\r\n\r\n",
        ];
        for response in responses {
            let (mut socket, _) = listener.accept().unwrap();
            let mut request = [0u8; 256];
            let _ = socket.read(&mut request);
            socket.write_all(response.as_bytes()).unwrap();
        }
        // Fourth connection: accept, drain, close without a word.
        let (mut socket, _) = listener.accept().unwrap();
        let mut request = [0u8; 256];
        let _ = socket.read(&mut request);
        drop(socket);
    });

    assert_eq!(
        probe_ready("127.0.0.1", port).await,
        Ok(503),
        "a listening-but-not-ready gateway must not pass"
    );
    assert_eq!(probe_ready("127.0.0.1", port).await, Ok(200));
    assert!(
        probe_ready("127.0.0.1", port).await.is_err(),
        "a non-HTTP line is an error, not a ready"
    );
    assert!(
        probe_ready("127.0.0.1", port).await.is_err(),
        "an immediate close is an error, not a ready"
    );
    server.join().unwrap();

    // A port with nothing listening at all is an error too.
    let ghost = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let ghost_port = ghost.local_addr().unwrap().port();
    drop(ghost);
    assert!(probe_ready("127.0.0.1", ghost_port).await.is_err());
}

// ---- supervisor: backoff restart + reaping (plan Task 4) ----

use futures::channel::mpsc::UnboundedReceiver;

/// A sleeper that records the waits instead of sleeping through
/// them, so a test asserts the backoff schedule without waiting a
/// real second.
fn recording_sleeper() -> (Sleeper, Arc<Mutex<Vec<Duration>>>) {
    let recorded = Arc::new(Mutex::new(Vec::new()));
    let sleeper: Sleeper = {
        let recorded = recorded.clone();
        Arc::new(move |duration| {
            let recorded = recorded.clone();
            Box::pin(async move {
                recorded.lock().push(duration);
            })
        })
    };
    (sleeper, recorded)
}

/// A sleeper for tests that never reach a restart.
fn instant_sleeper() -> Sleeper {
    Arc::new(|_| Box::pin(async {}))
}

/// A sleeper the test can hold open: the supervisor parks in it
/// until the test releases it, so a shutdown can land *inside* the
/// restart backoff.
fn pausable_sleeper()
-> (Sleeper, Arc<AtomicBool>, tokio::sync::oneshot::Sender<()>) {
    let parked = Arc::new(AtomicBool::new(false));
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let released = Arc::new(Mutex::new(Some(released)));
    let sleeper: Sleeper = {
        let parked = parked.clone();
        let released = released.clone();
        Arc::new(move |_duration| {
            let parked = parked.clone();
            let released = released.clone();
            Box::pin(async move {
                parked.store(true, Ordering::SeqCst);
                let receiver = released.lock().take();
                if let Some(released) = receiver {
                    let _ = released.await;
                }
            })
        })
    };
    (sleeper, parked, release)
}

/// Read the next state, failing fast instead of hanging forever.
async fn next_state(
    states: &mut UnboundedReceiver<GatewayState>,
) -> GatewayState {
    tokio::time::timeout(Duration::from_secs(5), states.next())
        .await
        .expect("a state within 5s")
        .expect("the state stream is still open")
}

#[test]
fn the_backoff_doubles_from_one_second_and_caps_at_five() {
    assert_eq!(backoff_secs(1), 1);
    assert_eq!(backoff_secs(2), 2);
    assert_eq!(backoff_secs(3), 4);
    assert_eq!(backoff_secs(4), 5);
    assert_eq!(backoff_secs(5), 5);
    assert_eq!(backoff_secs(100), 5);
}

#[tokio::test]
async fn a_dead_gateway_is_restarted_with_a_backoff() {
    // Generation 0 dies after its first ready; generation 1 lives.
    let gen0_exit = Arc::new(AtomicBool::new(false));
    let launcher = RecordingLauncher::new({
        let gen0_exit = gen0_exit.clone();
        move |index| {
            let mut process = FakeProcess::alive();
            if index == 0 {
                process.exited = gen0_exit.clone();
            }
            process
        }
    });
    let (probe, _) = scripted_probe(vec![Ok(200)]);
    let (sleeper, waits) = recording_sleeper();

    let gateway = Gateway::start_with(
        options(),
        SuperviseParts {
            launcher: launcher.clone(),
            probe,
            sleeper,
            timings: TEST_TIMINGS,
        },
    )
    .await
    .expect("ready");

    let mut states = gateway.states();
    assert!(matches!(
        next_state(&mut states).await,
        GatewayState::Starting
    ));
    assert!(matches!(
        next_state(&mut states).await,
        GatewayState::Ready { .. }
    ));

    // The gateway process dies.
    gen0_exit.store(true, Ordering::Relaxed);

    assert_eq!(
        next_state(&mut states).await,
        GatewayState::Restarting { attempt: 1 }
    );
    // Review focus #2: inside the backoff window the URLs keep
    // their old values — a caller that only reads them is told the
    // truth about *where* the gateway will return, while the state
    // stream says it is down.
    let base_before = gateway.base_url().to_owned();
    let ws_before = gateway.ws_url().to_owned();
    assert_eq!(gateway.base_url(), base_before);

    // …and the new generation comes up on the same URLs.
    assert!(matches!(
        next_state(&mut states).await,
        GatewayState::Starting
    ));
    assert_eq!(
        next_state(&mut states).await,
        GatewayState::Ready {
            base_url: base_before,
            ws_url: ws_before,
        }
    );
    assert_eq!(launcher.launch_count(), 2, "one relaunch happened");
    assert_eq!(
        waits.lock().as_slice(),
        [Duration::from_secs(1)],
        "the first restart waits the minimum backoff"
    );

    gateway.shutdown();
}

#[tokio::test]
async fn dropping_the_gateway_kills_the_child_process() {
    let launcher = RecordingLauncher::new(|_| FakeProcess::alive());
    let (probe, _) = scripted_probe(vec![Ok(200)]);
    let gateway = Gateway::start_with(
        options(),
        SuperviseParts {
            launcher: launcher.clone(),
            probe,
            sleeper: instant_sleeper(),
            timings: TEST_TIMINGS,
        },
    )
    .await
    .expect("ready");

    drop(gateway);

    // The kill is issued from the synchronous drop path; the poll
    // keeps the assertion robust anyway.
    for _ in 0..500 {
        if launcher.all_killed() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(4)).await;
    }
    assert!(
        launcher.all_killed(),
        "drop kills the gateway child, not just the handle"
    );
}

#[tokio::test]
async fn dropping_start_before_its_outcome_reaps_the_generation() {
    // The probe pends until the test releases it, so the start
    // future is still awaiting when it is dropped — exactly the "host
    // quit during boot" race.
    let (release, released) = tokio::sync::oneshot::channel::<()>();
    let gate = Arc::new(AtomicBool::new(false));
    let released = Arc::new(Mutex::new(Some(released)));
    let gate_probe = gate.clone();
    let probe: Probe = Arc::new(move |_bind: &str, _port: u16| {
        let gate = gate_probe.clone();
        let released = released.clone();
        Box::pin(async move {
            if !gate.load(Ordering::SeqCst) {
                let receiver = released.lock().take();
                if let Some(released) = receiver {
                    let _ = released.await;
                }
            }
            Ok(200)
        })
    });
    let launcher = RecordingLauncher::new(|_| FakeProcess::alive());

    // The boot runs on its own task: aborting it drops the `start`
    // future — the host quitting during boot.
    let boot = tokio::spawn(Gateway::start_with(
        options(),
        SuperviseParts {
            launcher: launcher.clone(),
            probe,
            sleeper: instant_sleeper(),
            timings: TEST_TIMINGS,
        },
    ));
    // Let the boot reach its await point, then abandon it.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        launcher.launch_count(),
        1,
        "the generation was launched before the host quit"
    );
    boot.abort();
    let _ = boot.await;
    gate.store(true, Ordering::SeqCst);
    let _ = release.send(());

    // The generation it launched must be killed, not supervised into
    // an orphan that restarts forever.
    for _ in 0..500 {
        if launcher.all_killed() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(4)).await;
    }
    assert!(
        launcher.all_killed(),
        "an abandoned start reaps its generation instead of orphaning it"
    );
}

#[tokio::test]
async fn an_occupied_port_is_replaced_by_a_free_one() {
    // The caller's port is taken: handing it to `serve` anyway would
    // leave the gateway restarting forever on a port that never
    // opens.
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let occupied_port = occupied.local_addr().unwrap().port();
    let launcher = RecordingLauncher::new(|_| FakeProcess::alive());
    let (probe, _) = scripted_probe(vec![Ok(200)]);

    let gateway = Gateway::start_with(
        GatewayOptions {
            port: occupied_port,
            ..options()
        },
        SuperviseParts {
            launcher: launcher.clone(),
            probe,
            sleeper: instant_sleeper(),
            timings: TEST_TIMINGS,
        },
    )
    .await
    .expect("ready");

    let ports = launcher.ports();
    assert_eq!(ports.len(), 1, "serve was launched exactly once");
    let served = ports[0];
    assert_ne!(
        served, occupied_port,
        "serve must not be given a port that is already taken"
    );
    assert_ne!(served, 0, "port 0 never reaches serve");
    assert_eq!(
        gateway.base_url(),
        format!("http://127.0.0.1:{served}"),
        "base_url carries the port serve got"
    );
    assert!(
        std::net::TcpListener::bind(("127.0.0.1", served)).is_ok(),
        "the port the supervisor picked must be bindable"
    );

    gateway.shutdown();
}

#[tokio::test]
async fn an_abandoned_boot_stops_the_readiness_probe() {
    // The probe never answers. Without a stop signal the supervisor
    // would burn the whole readiness budget (30s in production) for a
    // host that is already gone.
    let launcher = RecordingLauncher::new(|_| FakeProcess::alive());
    let probe: Probe = Arc::new(|_bind: &str, _port: u16| {
        Box::pin(async { Err("connection refused".to_owned()) })
    });

    // The boot runs on its own task: aborting it is the host quitting
    // during boot (a dropped `start` future drops the outcome
    // receiver, and the supervisor must notice).
    let boot = tokio::spawn(Gateway::start_with(
        options(),
        SuperviseParts {
            launcher: launcher.clone(),
            probe,
            sleeper: instant_sleeper(),
            timings: SuperviseTimings {
                probe_interval: Duration::from_millis(5),
                ready_timeout: Duration::from_secs(30),
            },
        },
    ));
    // Let the boot reach its await point, then abandon it.
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        launcher.launch_count(),
        1,
        "the generation was launched before the host quit"
    );
    boot.abort();
    let _ = boot.await;

    for _ in 0..500 {
        if launcher.all_killed() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(4)).await;
    }
    assert!(
        launcher.all_killed(),
        "an abandoned boot must stop probing instead of burning the readiness budget"
    );
}

#[tokio::test]
async fn dropping_the_gateway_inside_the_restart_backoff_does_not_revive_an_orphan()
 {
    // Generation 0 dies on its own and the supervisor waits out the
    // backoff. The host quits inside that window: the supervisor must
    // stop, not start a generation nobody is left to kill.
    let gen0_exit = Arc::new(AtomicBool::new(false));
    let launcher = RecordingLauncher::new({
        let gen0_exit = gen0_exit.clone();
        move |index| {
            let mut process = FakeProcess::alive();
            if index == 0 {
                process.exited = gen0_exit.clone();
            }
            process
        }
    });
    let (probe, _) = scripted_probe(vec![Ok(200)]);
    let (sleeper, parked, release) = pausable_sleeper();

    let gateway = Gateway::start_with(
        options(),
        SuperviseParts {
            launcher: launcher.clone(),
            probe,
            sleeper,
            timings: TEST_TIMINGS,
        },
    )
    .await
    .expect("ready");

    let mut states = gateway.states();
    assert!(matches!(
        next_state(&mut states).await,
        GatewayState::Starting
    ));
    assert!(matches!(
        next_state(&mut states).await,
        GatewayState::Ready { .. }
    ));

    // The generation dies; the supervisor enters the restart backoff.
    gen0_exit.store(true, Ordering::Relaxed);
    assert_eq!(
        next_state(&mut states).await,
        GatewayState::Restarting { attempt: 1 }
    );
    for _ in 0..500 {
        if parked.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    assert!(
        parked.load(Ordering::SeqCst),
        "the supervisor parked in the restart backoff"
    );

    // The host quits while the supervisor waits.
    drop(gateway);
    // Release the sleeper: a supervisor that only checks the flag
    // after waking must not start another generation either.
    let _ = release.send(());

    // The supervisor stops with the host: the state stream closes.
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        while states.next().await.is_some() {}
    })
    .await;
    assert!(
        closed.is_ok(),
        "the supervisor must stop when the host drops the handle"
    );
    assert_eq!(
        launcher.launch_count(),
        1,
        "no generation was started after the drop"
    );
    assert!(
        launcher.all_killed(),
        "the generation the supervisor owned was killed"
    );
}

// ---- client: JSON-RPC over the gateway WebSocket (plan Task 5) ----

use axum::{
    Router,
    extract::ws::{Message, WebSocket, WebSocketUpgrade},
    routing::get,
};
use serde_json::{Value, json};

use crate::{ClientError, GatewayClient};

/// Everything one test server connection received, and everything
/// every connection received.
type ConnLog = Arc<Mutex<Vec<String>>>;

#[derive(Default)]
struct ServerLog {
    conns: Mutex<Vec<ConnLog>>,
}

impl ServerLog {
    fn count(&self) -> usize {
        self.conns.lock().len()
    }

    fn received(&self, index: usize) -> Vec<String> {
        self.conns.lock()[index].lock().clone()
    }
}

/// Spin an in-process WebSocket server at `/rpc`. Each connection is
/// logged and handed to the test's script together with its index,
/// so a test can drop connection 0 and watch the client reconnect
/// into connection 1.
async fn serve_ws(
    script: Arc<
        dyn Fn(u32, WebSocket, ConnLog) -> BoxFuture<'static, ()> + Send + Sync,
    >,
) -> (String, Arc<ServerLog>) {
    let log = Arc::new(ServerLog::default());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let app = {
        let log = log.clone();
        Router::new().route(
            "/rpc",
            get(move |upgrade: WebSocketUpgrade| {
                let log = log.clone();
                let script = script.clone();
                async move {
                    upgrade.on_upgrade(move |socket| async move {
                        let (index, conn) = {
                            let mut conns = log.conns.lock();
                            let conn: ConnLog =
                                Arc::new(Mutex::new(Vec::new()));
                            conns.push(conn.clone());
                            (conns.len() as u32 - 1, conn)
                        };
                        script(index, socket, conn).await
                    })
                }
            }),
        )
    };
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    (format!("ws://127.0.0.1:{port}/rpc"), log)
}

/// Read the next text frame a connection receives, with the raw
/// value logged for the test's assertions.
async fn read_text(socket: &mut WebSocket) -> Option<Value> {
    while let Some(Ok(message)) = socket.recv().await {
        if let Message::Text(text) = message {
            return serde_json::from_str(&text).ok();
        }
    }
    None
}

/// Poll until the condition holds, failing fast instead of hanging.
async fn eventually(condition: impl Fn() -> bool) {
    for _ in 0..1_000 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("condition never became true");
}

/// A gateway that answers exactly one connection and then goes away:
/// the client's socket dies and every redial is refused.
async fn serve_once_then_stop() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let Ok((stream, _)) = listener.accept().await else {
            return;
        };
        // The handshake completes, then the socket hangs up.
        let socket = tokio_tungstenite::accept_async(stream).await.unwrap();
        drop(socket);
        // The listener goes with it: every redial is refused.
        drop(listener);
    });
    format!("ws://127.0.0.1:{port}/rpc")
}

/// A listener whose accept queue is full: the kernel drops the SYN
/// instead of refusing it, so the connect hangs — the blackhole a
/// dial has to give up on. The listener and the queued connections
/// come back with the port: both must stay alive for the queue to
/// stay full.
async fn saturated_listener()
-> (u16, std::net::TcpListener, Vec<tokio::net::TcpStream>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let mut held = Vec::new();
    for _ in 0..512 {
        match tokio::time::timeout(
            Duration::from_millis(200),
            tokio::net::TcpStream::connect(("127.0.0.1", port)),
        )
        .await
        {
            Ok(Ok(stream)) => held.push(stream),
            // The queue is full: the connect hangs instead of being
            // refused.
            _ => break,
        }
    }
    assert!(
        held.len() >= 64,
        "the accept queue should be full by now: {}",
        held.len()
    );
    (port, listener, held)
}

#[tokio::test]
async fn the_dial_gives_up_when_the_gateway_never_accepts() {
    let (port, _listener, _held) = saturated_listener().await;
    let url = url::Url::parse(&format!("ws://127.0.0.1:{port}/rpc")).unwrap();

    let error = tokio::time::timeout(
        Duration::from_secs(5),
        dial_within(&url, Duration::from_millis(200)),
    )
    .await
    .expect("the dial must give up instead of hanging")
    .expect_err("the dial cannot succeed");
    assert!(error.contains("timed out"), "{error}");
}

#[tokio::test]
async fn the_production_dial_gives_up_when_the_gateway_never_accepts() {
    // The path the client really takes: `dial` must come back with
    // an error, not hang on a socket nobody accepts.
    let (port, _listener, _held) = saturated_listener().await;
    let url = url::Url::parse(&format!("ws://127.0.0.1:{port}/rpc")).unwrap();

    let error = tokio::time::timeout(Duration::from_secs(30), dial(&url))
        .await
        .expect("the production dial must give up instead of hanging")
        .expect_err("the dial cannot succeed");
    assert!(error.contains("timed out"), "{error}");
}

#[tokio::test]
async fn the_dial_gives_up_when_the_handshake_never_answers() {
    // A listener that accepts the socket and then says nothing: the
    // WebSocket handshake never completes.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    let url = url::Url::parse(&format!("ws://127.0.0.1:{port}/rpc")).unwrap();

    let error = tokio::time::timeout(
        Duration::from_secs(5),
        dial_within(&url, Duration::from_millis(200)),
    )
    .await
    .expect("the dial must give up instead of hanging")
    .expect_err("the handshake cannot succeed");
    assert!(error.contains("timed out"), "{error}");
}

#[tokio::test]
async fn a_request_parked_during_the_redial_window_fails_when_the_client_closes()
 {
    // The gateway answers once and goes away: the client's socket
    // dies and every redial is refused.
    let url = serve_once_then_stop().await;
    let client = GatewayClient::connect(&url).await.expect("connect");
    eventually(|| !client.connected()).await;

    // A request queued while the client is redialing parks.
    let request = tokio::spawn({
        let client = client.clone();
        async move { client.request("sessions/list", json!({ "limit": 1 })).await }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;

    // The host closes the client: the parked request must be
    // released, not hang forever.
    client.close();

    let outcome = tokio::time::timeout(Duration::from_secs(5), request)
        .await
        .expect("the parked request must be released, not hang")
        .expect("the request task finished");
    assert!(
        outcome.is_err(),
        "a request that can no longer be delivered must fail: {outcome:?}"
    );
}

#[tokio::test]
async fn a_request_is_answered_by_the_response_with_the_same_id() {
    let (url, _log) = serve_ws(Arc::new(|_index, mut socket, conn| {
        Box::pin(async move {
            while let Some(frame) = read_text(&mut socket).await {
                let id = frame.get("id").cloned().unwrap_or(Value::Null);
                let answer = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": { "echo": frame },
                });
                socket
                    .send(Message::Text(answer.to_string().into()))
                    .await
                    .unwrap();
                conn.lock().push(frame.to_string());
            }
        })
    }))
    .await;

    let client = GatewayClient::connect(&url).await.expect("connect");
    let result = client
        .request("sessions/list", json!({"limit": 5}))
        .await
        .expect("answered");
    assert_eq!(result["echo"]["method"], "sessions/list");
    assert_eq!(result["echo"]["params"]["limit"], 5);
    assert!(
        result["echo"]["id"].as_str().unwrap().starts_with('c'),
        "ids follow the c1, c2… convention: {}",
        result["echo"]["id"]
    );

    client.close();
    assert!(matches!(
        client.notify("session/ping", Value::Null),
        Err(ClientError::Closed)
    ));
}

#[tokio::test]
async fn a_notification_reaches_every_subscriber() {
    let first_calls = Arc::new(AtomicUsize::new(0));
    let second_calls = Arc::new(AtomicUsize::new(0));
    let (url, _log) = serve_ws(Arc::new(|_index, mut socket, _conn| {
        Box::pin(async move {
            // As soon as anything arrives (the subscription
            // registration), push one notification.
            if read_text(&mut socket).await.is_some() {
                let event = json!({
                    "jsonrpc": "2.0",
                    "method": "session/event",
                    "params": {"kind": "turn"},
                });
                socket
                    .send(Message::Text(event.to_string().into()))
                    .await
                    .unwrap();
                // Hold the connection open so the client can read.
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        })
    }))
    .await;

    let client = GatewayClient::connect(&url).await.expect("connect");
    let seen: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let seen_1 = seen.clone();
    let first = first_calls.clone();
    let _subscription_a = client
        .subscribe(
            "session/event",
            Arc::new(move |_method, params| {
                seen_1.lock().push(params);
                first.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .expect("subscribe");
    let seen_2 = seen.clone();
    let second = second_calls.clone();
    let _subscription_b = client
        .subscribe(
            "session/event",
            Arc::new(move |_method, params| {
                seen_2.lock().push(params);
                second.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .expect("subscribe");

    eventually(|| first_calls.load(Ordering::SeqCst) == 1).await;
    eventually(|| second_calls.load(Ordering::SeqCst) == 1).await;
    let all = seen.lock().clone();
    assert_eq!(all.len(), 2, "each handler saw the notification once");
    assert_eq!(all[0]["kind"], "turn");

    client.close();
}

#[tokio::test]
async fn subscriptions_are_replayed_after_a_reconnect() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (url, log) = serve_ws(Arc::new(|index, mut socket, conn| {
        Box::pin(async move {
            match index {
                // Connection 0: take the registration, then die —
                // the client must notice and reconnect.
                0 => {
                    read_text(&mut socket).await;
                }
                // Connection 1: take the replayed registration, then
                // deliver one notification and stay up.
                _ => {
                    if let Some(frame) = read_text(&mut socket).await {
                        conn.lock().push(frame.to_string());
                        let event = json!({
                            "jsonrpc": "2.0",
                            "method": "session/event",
                            "params": {"generation": 2},
                        });
                        socket
                            .send(Message::Text(event.to_string().into()))
                            .await
                            .unwrap();
                        tokio::time::sleep(Duration::from_secs(10)).await;
                    }
                }
            }
        })
    }))
    .await;

    let client = GatewayClient::connect(&url).await.expect("connect");
    let call_params: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = call_params.clone();
    let counted = calls.clone();
    let _subscription = client
        .subscribe(
            "session/event",
            Arc::new(move |_method, params| {
                sink.lock().push(params);
                counted.fetch_add(1, Ordering::SeqCst);
            }),
        )
        .expect("subscribe");

    eventually(|| calls.load(Ordering::SeqCst) == 1).await;
    // Review focus #3/#4: the notification arrived exactly once — the
    // replay re-registered the method without mounting the handler a
    // second time. Give a double-mount a chance to show up.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1, "no double delivery");
    assert_eq!(call_params.lock()[0]["generation"], 2);

    eventually(|| log.count() == 2).await;
    let replayed_on_second = log
        .received(1)
        .iter()
        .filter(|text| text.contains("session/event"))
        .count();
    assert_eq!(
        replayed_on_second, 1,
        "the subscription was re-registered exactly once"
    );

    client.close();
}

#[tokio::test]
async fn a_catch_all_subscription_receives_every_notification() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (url, _log) = serve_ws(Arc::new(|_index, mut socket, _conn| {
        Box::pin(async move {
            // A catch-all registers nothing on the wire, so the
            // server cannot wait for a registration frame. The first
            // thing that does arrive is an ordinary request; answer
            // it with two notifications of different methods — a
            // catch-all handler must see both, while no per-method
            // handler was registered.
            if read_text(&mut socket).await.is_some() {
                for method in ["ready", "session/event"] {
                    let event = json!({
                        "jsonrpc": "2.0",
                        "method": method,
                        "params": {"seen": method},
                    });
                    socket
                        .send(Message::Text(event.to_string().into()))
                        .await
                        .unwrap();
                }
                tokio::time::sleep(Duration::from_secs(10)).await;
            }
        })
    }))
    .await;

    let client = GatewayClient::connect(&url).await.expect("connect");
    let seen: Arc<Mutex<Vec<(String, Value)>>> =
        Arc::new(Mutex::new(Vec::new()));
    let sink = seen.clone();
    let counted = calls.clone();
    let _subscription = client
        .subscribe_all(Arc::new(move |method, params| {
            sink.lock().push((method.to_string(), params));
            counted.fetch_add(1, Ordering::Relaxed);
        }))
        .expect("subscribe_all");
    // Any request wakes the scripted server; its answer never comes,
    // so the spawned call is aborted at the end.
    let probe =
        tokio::spawn(client.request("sessions/list", json!({"limit": 1})));

    eventually(|| calls.load(Ordering::Relaxed) == 2).await;
    let all = seen.lock().clone();
    assert_eq!(
        all.iter()
            .map(|(method, _)| method.as_str())
            .collect::<Vec<_>>(),
        vec!["ready", "session/event"],
        "the one catch-all handler saw every method"
    );
    assert_eq!(all[0].1["seen"], "ready");
    assert_eq!(all[1].1["seen"], "session/event");

    probe.abort();
    client.close();
}

#[tokio::test]
async fn a_server_error_object_becomes_a_client_error() {
    let (url, _log) = serve_ws(Arc::new(|_index, mut socket, _conn| {
        Box::pin(async move {
            while let Some(frame) = read_text(&mut socket).await {
                let id = frame.get("id").cloned().unwrap_or(Value::Null);
                let answer = json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {"code": -32000, "message": "session not found"},
                });
                socket
                    .send(Message::Text(answer.to_string().into()))
                    .await
                    .unwrap();
            }
        })
    }))
    .await;

    let client = GatewayClient::connect(&url).await.expect("connect");
    match client
        .request("sessions/follow", json!({"session_id": "nope"}))
        .await
    {
        Err(ClientError::Server { code, message }) => {
            assert_eq!(code, -32000);
            assert!(message.contains("session not found"), "{message}");
        }
        other => panic!("expected a server error, got {other:?}"),
    }

    client.close();
}

#[tokio::test]
async fn the_client_reports_a_dead_socket_as_not_connected() {
    // Every connection closes immediately: the handshake succeeds,
    // then the socket dies. A host that narrates outages (the TUI's
    // status bar) polls `connected()` because a `request` sent while
    // disconnected parks until a redial succeeds — the flag is the
    // only fast outage signal.
    let (url, _log) =
        serve_ws(Arc::new(|_index, _socket, _conn| Box::pin(async move {})))
            .await;

    let client = GatewayClient::connect(&url).await.expect("connect");
    eventually(|| !client.connected()).await;
    assert!(!client.connected(), "a dead socket must read as down");
}
