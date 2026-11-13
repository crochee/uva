//! SIGTERM graceful shutdown + session handover, on the stdio core.
//!
//! The container stop signal must walk the same path as Ctrl-C:
//! stop accepting frames → cancel in-flight runs → every controller
//! loop exits and releases its resources → each writer flushes the
//! terminal `lifecycle_shutdown` marker → the process exits 0. A peer
//! node (a second core) pointed at the same working directory must
//! then read the session history without repairs.
//!
//! The same file guards the other half of that promise: the shutdown
//! budget bounds the **post-signal** drain, not the process's uptime,
//! so a node that was never signalled keeps serving.
//!
//! Unix-gated: SIGTERM has no `signal::unix` equivalent on the
//! non-unix builds this crate otherwise supports.

#![cfg(unix)]

use std::{
    io::BufReader,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use uva_app_server::rpc::framing::{
    DEFAULT_MAX_FRAME_SIZE,
    read_frame_blocking,
    write_frame_blocking,
};

/// SIGTERM's numeric value on every unix this test runs on.
const SIGTERM: i32 = 15;

/// How long the process may take to exit after SIGTERM.
const EXIT_DEADLINE: Duration = Duration::from_secs(15);

/// How long a node has to keep serving after boot. Comfortably past
/// the drain deadline (8 s), so a node that tore itself down on its
/// own is caught.
const UPTIME_WINDOW: Duration =
    Duration::from_secs(uva_app_server::shutdown::DRAIN_DEADLINE.as_secs() + 6);

/// A node: the spawned core plus its pipes.
struct Node {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Node {
    /// Start the core against `dir`.
    ///
    /// Hermetic: no ambient API keys (a developer shell may export
    /// real ones, and the provider fallback would then make this test
    /// hit the network), no inherited UVA_* overrides.
    fn start(dir: &std::path::Path) -> Node {
        let path = std::env::var_os("PATH").unwrap_or_default();
        let home = std::env::var_os("HOME").unwrap_or_default();
        let tmpdir = std::env::var_os("TMPDIR").unwrap_or_default();
        let mut child = Command::new(env!("CARGO_BIN_EXE_uva-app-server"))
            .arg("core")
            .arg("--directory")
            .arg(dir)
            .env_clear()
            .envs([
                ("PATH", path.to_str().unwrap_or("")),
                ("HOME", home.to_str().unwrap_or("")),
                ("TMPDIR", tmpdir.to_str().unwrap_or("")),
            ])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn the core");
        let stdin = child.stdin.take().expect("the core reads stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut node = Node {
            child,
            stdin,
            stdout,
            next_id: 1,
        };
        node.await_ready();
        node
    }

    /// Read frames until the `ready` notification.
    fn await_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            let frame = self.recv();
            if frame["method"] == "ready" {
                return;
            }
            assert!(
                Instant::now() < deadline,
                "the core never announced itself"
            );
        }
    }

    /// Write one Content-Length frame.
    fn send(&mut self, frame: &Value) {
        let payload = serde_json::to_vec(frame).expect("a frame");
        write_frame_blocking(&mut self.stdin, &payload)
            .expect("write the frame");
    }

    /// Read one Content-Length frame.
    fn recv(&mut self) -> Value {
        let payload =
            read_frame_blocking(&mut self.stdout, DEFAULT_MAX_FRAME_SIZE)
                .expect("read a frame");
        serde_json::from_slice(&payload).expect("the frame is JSON")
    }

    /// Send a request and wait for its answer.
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = format!("t{}", self.next_id);
        self.next_id += 1;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let frame = self.recv();
            if frame["id"] == id {
                return frame;
            }
            assert!(Instant::now() < deadline, "no answer for {method}");
        }
    }

    /// Whether the process is still alive.
    fn is_running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    /// Send SIGTERM, then assert the process exits 0 within the
    /// deadline.
    fn terminate_and_assert_clean_exit(mut self) {
        send_signal(self.child.id(), SIGTERM);
        let deadline = Instant::now() + EXIT_DEADLINE;
        loop {
            match self.child.try_wait().expect("wait on the child") {
                Some(status) => {
                    assert!(
                        status.success(),
                        "node must exit 0, got {status:?}"
                    );
                    return;
                }
                None => {
                    assert!(
                        Instant::now() < deadline,
                        "node did not exit within {EXIT_DEADLINE:?} of SIGTERM"
                    );
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
        }
    }
}

impl Drop for Node {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `kill(2)` through a direct extern: one signal, no libc crate.
fn send_signal(pid: u32, signal: i32) {
    unsafe extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    // SAFETY: `pid` is a live child (SIGTERM is sent before any wait),
    // and `signal` is SIGTERM.
    let _ = unsafe { kill(pid as i32, signal) };
}

/// The keyless demo deployment: `[providers.demo] type = "demo"`.
fn write_config(dir: &std::path::Path) {
    let layer = dir.join(".agents");
    std::fs::create_dir_all(&layer).expect("the .agents layer");
    std::fs::write(
        layer.join("config.toml"),
        r#"default_provider = "demo"
default_model = "demo"

[providers.demo]
type = "demo"
api_key_env = "DEMO_API_KEY"
default_model = "demo"
"#,
    )
    .expect("write .agents/config.toml");
}

/// Wait until `needle` shows up in the session's event log.
fn wait_for_history(dir: &std::path::Path, session: &str, needle: &str) {
    let log = dir
        .join("sessions")
        .join("dev")
        .join(session)
        .join("events.jsonl");
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Ok(body) = std::fs::read_to_string(&log)
            && body.contains(needle)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "history never contained {needle:?} (log: {})",
            log.display()
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
    let mut node_a = Node::start(dir.path());
    let created =
        node_a.call("chat/session/create", json!({ "session_id": SESSION }));
    assert!(created["result"].is_object(), "create failed: {created}");
    let sent = node_a.call(
        "chat/message/send",
        json!({ "session_id": SESSION, "text": "hello handover" }),
    );
    assert!(sent["result"].is_object(), "send failed: {sent}");
    wait_for_history(dir.path(), SESSION, "demo echo: hello handover");

    // SIGTERM: the process must exit 0 within the deadline.
    node_a.terminate_and_assert_clean_exit();

    // The drain really ran to completion: each controller's exit path
    // appends (and flushes) the terminal `lifecycle_shutdown` marker
    // row before the process may leave. A process merely dropped by
    // the runtime would leave no marker.
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
    let mut node_b = Node::start(dir.path());
    let detail = node_b.call("sessions/get", json!({ "id": SESSION }));
    let body = serde_json::to_string(&detail).expect("a payload");
    assert!(
        body.contains("hello handover") && body.contains("demo echo:"),
        "node B's history must carry the turn node A ran: {body}"
    );

    // Cleanup: node B exits the same graceful way.
    node_b.terminate_and_assert_clean_exit();
}

/// The other half of the shutdown promise: the budget bounds the
/// **post-signal** drain, not the process's uptime.
///
/// A regression once wrapped the serving future in the drain timeout,
/// which bounded the *uptime*: every node tore itself down
/// `DRAIN_DEADLINE` after boot with no signal sent. The handover test
/// above cannot see it — it signals within seconds of boot.
#[test]
fn the_core_keeps_serving_until_a_signal_arrives() {
    let dir = tempfile::TempDir::new().expect("temp workspace");
    write_config(dir.path());

    let mut node = Node::start(dir.path());

    // Well past the drain deadline: nothing has been signalled, so
    // nothing may have stopped the node.
    std::thread::sleep(UPTIME_WINDOW);
    assert!(
        node.is_running(),
        "the core exited on its own after {UPTIME_WINDOW:?} — the \
         shutdown budget must bound the post-signal drain, not the \
         uptime"
    );

    // Still serving: a request answers.
    let pong = node.call("health/ping", json!({}));
    assert_eq!(pong["result"]["ok"], true, "ping must answer: {pong}");

    // And a signal still ends it the graceful way.
    node.terminate_and_assert_clean_exit();
}
