//! SIGTERM graceful shutdown + lease handover (Task 9).
//!
//! The container stop signal must walk the same path as Ctrl-C:
//! stop accepting requests → cancel in-flight runs → each controller
//! loop exits and releases its lease → the writer flushes → the
//! process exits 0. A peer node pointed at the same working directory
//! must then read the session history without repairs.
//!
//! The same file also guards the other half of that promise: the
//! shutdown budget bounds the **post-signal** drain, not the server's
//! uptime, so a node that was never signalled keeps serving.
//!
//! Unix-gated (R18): SIGTERM has no `signal::unix` equivalent on the
//! non-unix builds this crate otherwise supports.

#![cfg(unix)]

use std::{
    io::{Read, Write},
    net::TcpStream,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

/// SIGTERM's numeric value on every unix this test runs on.
const SIGTERM: i32 = 15;

/// How long the process may take to exit after SIGTERM.
const EXIT_DEADLINE: Duration = Duration::from_secs(10);

/// The server's own HTTP drain deadline (the budget the graceful
/// shutdown runs under). It bounds the **post-signal** drain, so a
/// node that has not been signalled has to still be serving well
/// past it.
const HTTP_DRAIN_DEADLINE: Duration =
    elysper_server::shutdown::HTTP_DRAIN_DEADLINE;

/// How long a node has to keep serving after boot. Comfortably past
/// [`HTTP_DRAIN_DEADLINE`], so a node that tore itself down on its
/// own is caught.
const UPTIME_WINDOW: Duration =
    Duration::from_secs(HTTP_DRAIN_DEADLINE.as_secs() + 6);

/// A node: the spawned binary plus the port it answers on.
struct Node {
    child: Child,
    port: u16,
}

impl Node {
    /// Start `elysper-server` on a free port, serving `dir`.
    fn start(dir: &std::path::Path) -> Node {
        let port = free_port();
        // Hermetic: no ambient API keys (a developer shell may export
        // real ones, and the provider fallback would then make this
        // test hit the network), no inherited ELYSPER_* overrides.
        let path = std::env::var_os("PATH").unwrap_or_default();
        let home = std::env::var_os("HOME").unwrap_or_default();
        let tmpdir = std::env::var_os("TMPDIR").unwrap_or_default();
        let scrubbed_env = [
            ("PATH", path.to_str().unwrap_or("")),
            ("HOME", home.to_str().unwrap_or("")),
            ("TMPDIR", tmpdir.to_str().unwrap_or("")),
        ];
        let child = Command::new(env!("CARGO_BIN_EXE_elysper-server"))
            .arg("--directory")
            .arg(dir)
            .arg("--config")
            .arg(dir.join("config.yaml"))
            .arg("--host")
            .arg("127.0.0.1")
            .arg("--port")
            .arg(port.to_string())
            .env_clear()
            .envs(scrubbed_env)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn elysper-server");
        let node = Node { child, port };
        node.wait_ready();
        node
    }

    /// Poll `/livez` until the node answers, then it is serving.
    fn wait_ready(&self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok((200, _)) = http(self.port, "GET", "/livez", None) {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "node on port {} did not become ready",
                self.port
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Whether the process is still alive. A node that tore itself
    /// down is the failure this file guards against.
    fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }
}

/// A failing assert must not leak the server process: it would hold
/// the port and the temp dir until the CI runner reaped it. Kill on
/// an already-exited child is an Err we ignore.
impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Node {
    /// One request against this node.
    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> (u16, String) {
        http(self.port, method, path, body).expect("http request")
    }

    /// Open an SSE `follow` connection and hold it open — the shape a
    /// connected UI keeps while the orchestrator stops the node. The
    /// returned stream is deliberately never read to EOF: its body
    /// ends only when the *server* ends it.
    fn open_follow_stream(&self, session: &str) -> TcpStream {
        let mut stream =
            TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        let request = format!(
            "GET /api/v1/sessions/{session}/follow HTTP/1.1\r\n\
             host: 127.0.0.1:{}\r\naccept: text/event-stream\r\n\
             connection: keep-alive\r\n\r\n",
            self.port
        );
        stream
            .write_all(request.as_bytes())
            .expect("send follow GET");
        stream
    }

    /// Send SIGTERM, then assert the process exits 0 within the
    /// deadline (polling `try_wait`: std has no `wait_timeout`).
    fn terminate_and_assert_clean_exit(mut self) {
        send_signal(self.child.id(), SIGTERM);
        let deadline = Instant::now() + EXIT_DEADLINE;
        loop {
            match self.child.try_wait().expect("try_wait node") {
                Some(status) => {
                    assert!(
                        status.success(),
                        "node must exit 0 on SIGTERM, got {status:?}"
                    );
                    return;
                }
                None => assert!(
                    Instant::now() < deadline,
                    "node did not exit within {}s of SIGTERM",
                    EXIT_DEADLINE.as_secs()
                ),
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

/// `kill(2)` through a direct extern: one signal, no libc crate.
fn send_signal(pid: u32, signal: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: `pid` is a live child that has not been reaped (the
    // caller waits strictly after this call) and `signal` is SIGTERM.
    let rc = unsafe { kill(pid as i32, signal) };
    assert_eq!(rc, 0, "kill({pid}, SIGTERM) failed");
}

/// Minimal blocking HTTP/1.1 client — enough for the JSON endpoints
/// this test drives, without a client dependency in dev-deps.
fn http(
    port: u16,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> std::io::Result<(u16, String)> {
    let mut stream = TcpStream::connect(("127.0.0.1", port))?;
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let body = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nhost: 127.0.0.1:{port}\r\n\
         content-type: application/json\r\n\
         content-length: {}\r\nconnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes())?;
    let mut response = String::new();
    stream.read_to_string(&mut response)?;
    let status = response
        .split_whitespace()
        .nth(1)
        .and_then(|code| code.parse::<u16>().ok())
        .unwrap_or(0);
    let body = response
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.to_string())
        .unwrap_or_default();
    Ok((status, body))
}

/// A port nothing is bound to (the standard free-port pattern).
fn free_port() -> u16 {
    let listener =
        std::net::TcpListener::bind("127.0.0.1:0").expect("bind 127.0.0.1:0");
    listener.local_addr().expect("local addr").port()
}

/// The keyless demo deployment: `[providers.demo] type = "demo"` —
/// no network, no API key, no database.
fn write_config(dir: &std::path::Path) {
    std::fs::write(
        dir.join("config.yaml"),
        r#"version: "1.0"
host: "127.0.0.1"
default_provider: demo
providers:
  demo:
    type: demo
    default_model: demo
"#,
    )
    .expect("write config.yaml");
}

/// Wait until `needle` shows up in the session's events endpoint.
fn wait_for_history(port: u16, session: &str, needle: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok((200, body)) = http(
            port,
            "GET",
            &format!("/api/v1/sessions/{session}/events"),
            None,
        ) && body.contains(needle)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "history never contained {needle:?}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// The full handover: node A runs a turn, SIGTERMs, exits 0; node B
/// on the same working directory reads the history back.
#[test]
fn sigterm_drains_and_a_peer_takes_the_session_over() {
    let dir = tempfile::TempDir::new().expect("temp workspace");
    write_config(dir.path());
    const SESSION: &str = "handover-1";

    // Node A: create the session, run one turn, wait for it on disk.
    let node_a = Node::start(dir.path());
    let (status, body) = node_a.request(
        "POST",
        "/api/v1/chat/sessions",
        Some(&format!(r#"{{"session_id":"{SESSION}"}}"#)),
    );
    assert_eq!(status, 200, "create session failed: {body}");
    let (status, body) = node_a.request(
        "POST",
        &format!("/api/v1/chat/sessions/{SESSION}/messages"),
        Some(r#"{"text":"hello handover"}"#),
    );
    assert_eq!(status, 200, "send message failed: {body}");
    wait_for_history(node_a.port, SESSION, "demo echo: hello handover");

    // A connected UI: hold an SSE `follow` stream open across the
    // SIGTERM. The drain must still complete (closing the controller
    // ends its follow streams, which is what lets the HTTP drain
    // finish) — a shutdown that waits on this connection first would
    // hang here until the orchestrator SIGKILLed the node.
    let _follow = node_a.open_follow_stream(SESSION);
    // Give the server a moment to register the subscriber, so the
    // pinned case is genuinely exercised.
    std::thread::sleep(Duration::from_millis(300));

    // SIGTERM: the process must exit 0 within the deadline.
    node_a.terminate_and_assert_clean_exit();

    // The drain really ran to completion: the controller loop's exit
    // path appends (and flushes) the terminal `lifecycle_shutdown`
    // marker row — the same code path that releases the cluster
    // lease — before the process may leave. A process merely dropped
    // by the runtime would leave no marker. (The lease release itself
    // is skipped when no lease is held; asserting it would need the
    // `cluster-redis` feature and a redis instance — out of scope for
    // a keyless, networkless test.)
    let log = std::fs::read_to_string(
        dir.path()
            .join("sessions")
            .join("dev")
            .join(SESSION)
            .join("events.jsonl"),
    )
    .expect("session log must exist after node A's shutdown");
    assert!(
        log.contains("\"type\":\"session_shutdown\""),
        "node A's log must end with the lifecycle_shutdown marker: {log}"
    );

    // Node B: same working directory, same session, history readable.
    // The session-detail route reads the transcript straight from disk
    // (its registry miss falls back to the log), which is exactly the
    // takeover shape a peer node sees.
    let node_b = Node::start(dir.path());
    let (status, body) =
        node_b.request("GET", &format!("/api/v1/sessions/{SESSION}"), None);
    assert_eq!(status, 200, "node B could not read the session: {body}");
    assert!(
        body.contains("hello handover") && body.contains("demo echo:"),
        "node B's history must carry the turn node A ran: {body}"
    );

    // Cleanup: node B exits the same graceful way.
    node_b.terminate_and_assert_clean_exit();
}

/// The other half of the shutdown promise: the budget bounds the
/// **post-signal** drain, not the server's uptime.
///
/// `axum::serve` only returns when the graceful-shutdown future
/// resolves — that is, when the cancel token fires. Wrapping the
/// serve future in the drain timeout therefore bounded the *uptime*:
/// every node tore itself down `HTTP_DRAIN_DEADLINE` after boot, with
/// no signal ever sent. The handover test above cannot see it — it
/// signals within seconds of boot, so the timeout never fires on its
/// own.
#[test]
fn the_server_keeps_serving_until_a_signal_arrives() {
    let dir = tempfile::TempDir::new().expect("temp workspace");
    write_config(dir.path());

    let mut node = Node::start(dir.path());

    // Well past the server's own drain deadline: nothing has been
    // signalled, so nothing may have stopped the node.
    std::thread::sleep(UPTIME_WINDOW);
    assert!(
        node.is_running(),
        "the server exited on its own after {UPTIME_WINDOW:?} — the \
         shutdown budget must bound the post-signal drain, not the \
         uptime"
    );

    // Still serving: the probes answer.
    let (status, body) = http(node.port, "GET", "/readyz", None)
        .unwrap_or_else(|error| {
            panic!(
                "readyz must answer after {UPTIME_WINDOW:?}, got {error} — \
                 the shutdown budget must bound the post-signal drain, \
                 not the uptime"
            )
        });
    assert_eq!(status, 200, "readyz must answer: {body}");

    // And a signal still ends it the graceful way.
    node.terminate_and_assert_clean_exit();
}
