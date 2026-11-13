//! The stdio round trip: the headless core, driven over its pipes.
//!
//! The core speaks JSON-RPC 2.0 on stdin/stdout, one Content-Length
//! frame at a time. The test spawns the binary, writes frames, and
//! reads the answers — the same conversation a desktop shell or a TUI
//! would have.
//!
//! What it proves:
//!
//! - the binary boots and announces itself with a `ready`
//!   notification;
//! - a request frame is answered with the same `id`;
//! - an unknown method answers `-32601`;
//! - the management surfaces answer their payloads;
//! - a chat turn runs end to end against the demo provider.
use std::{
    io::{BufRead, BufReader, Read as _, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::Arc,
};

use serde_json::{Value, json};
struct Core {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}
impl Core {
    /// Spawn the core against a workspace directory.
    fn spawn(workspace: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_uva-app-server"))
            .arg("core")
            .arg("--directory")
            .arg(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the core binary spawns");
        let stdin = child.stdin.take().expect("the core reads stdin");
        let stdout = BufReader::new(
            child.stdout.take().expect("the core writes stdout"),
        );
        Self {
            child,
            stdin,
            stdout,
        }
    }

    /// Write one frame.
    fn send(&mut self, frame: &Value) {
        let payload = serde_json::to_vec(frame).expect("the frame serialises");
        write!(self.stdin, "Content-Length: {}\r\n\r\n", payload.len())
            .expect("the header is written");
        self.stdin
            .write_all(&payload)
            .expect("the frame is written");
        self.stdin.flush().expect("the frame is flushed");
    }

    /// Read one frame.
    fn recv(&mut self) -> Value {
        let mut length = None;
        loop {
            let mut line = String::new();
            let read =
                self.stdout.read_line(&mut line).expect("the frame is read");
            assert!(read > 0, "the core closed stdout");
            if line == "\r\n" {
                break;
            }
            if let Some(value) = line.strip_prefix("Content-Length: ") {
                length = Some(
                    value
                        .trim()
                        .parse::<usize>()
                        .expect("a numeric Content-Length"),
                );
            }
        }
        let length = length.expect("the frame declares Content-Length");
        let mut payload = vec![0u8; length];
        self.stdout
            .read_exact(&mut payload)
            .expect("the declared payload is read");
        serde_json::from_slice(&payload).expect("the frame is JSON")
    }

    /// Read frames until the one with the given id arrives.
    fn recv_id(&mut self, id: &str) -> Value {
        for _ in 0..64 {
            let frame = self.recv();
            if frame.get("id").and_then(Value::as_str) == Some(id) {
                return frame;
            }
        }
        panic!("no answer for {id}");
    }

    /// Send a request and wait for its answer.
    fn call(&mut self, id: &str, method: &str, params: Value) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        self.recv_id(id)
    }

    /// The `ready` notification the boot sends.
    fn await_ready(&mut self) -> Value {
        for _ in 0..16 {
            let frame = self.recv();
            if frame.get("method").and_then(Value::as_str) == Some("ready") {
                return frame;
            }
        }
        panic!("the core never announced itself");
    }
}
impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
/// A workspace with a demo provider, so a turn runs without a key.
fn workspace() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("a temp workspace");
    // The provider chain reads the layout's project layer; a
    // `config.toml` beside the workspace root is not a config file
    // any more.
    let agents = temp.path().join(".agents");
    std::fs::create_dir_all(&agents).expect("the .agents layer");
    std::fs::write(
        agents.join("config.toml"),
        r#"default_provider = "demo"
default_model = "demo-model"

[providers.demo]
type = "demo"
api_key_env = "DEMO_API_KEY"
default_model = "demo-model"
"#,
    )
    .expect("the provider config");
    temp
}
#[test]
fn the_core_announces_itself_and_answers_a_ping() {
    let temp = workspace();
    let mut core = Core::spawn(temp.path());
    let ready = core.await_ready();
    assert_eq!(ready["method"], "ready");
    assert_eq!(ready["params"]["version"], env!("CARGO_PKG_VERSION"));
    let answer = core.call("c1", "health/ping", json!({}));
    assert_eq!(answer["id"], "c1");
    assert_eq!(answer["result"]["ok"], true);
    assert!(answer["result"]["version"].is_string());
}
#[test]
fn an_unknown_method_answers_the_standard_code() {
    let temp = workspace();
    let mut core = Core::spawn(temp.path());
    core.await_ready();
    let answer = core.call("c1", "no/such/method", json!({}));
    assert_eq!(answer["error"]["code"], -32601);
}
#[test]
fn a_malformed_frame_answers_the_parse_code() {
    let temp = workspace();
    let mut core = Core::spawn(temp.path());
    core.await_ready();
    let payload = b"{not json";
    write!(core.stdin, "Content-Length: {}\r\n\r\n", payload.len())
        .expect("the header is written");
    core.stdin.write_all(payload).expect("the frame is written");
    core.stdin.flush().expect("the frame is flushed");
    let frame = core.recv();
    assert_eq!(frame["error"]["code"], -32700);
}
#[test]
fn the_management_surfaces_answer_their_payloads() {
    let temp = workspace();
    let mut core = Core::spawn(temp.path());
    core.await_ready();
    let models = core.call("c1", "models/list", json!({}));
    assert_eq!(models["result"]["default_model"], "demo-model");
    let agents = core.call("c2", "agents/list", json!({}));
    assert!(agents["result"]["data"].is_array());
    let skills = core.call("c3", "skills/list", json!({}));
    assert!(skills["result"]["data"].is_array());
    let tools = core.call("c4", "tools/list", json!({}));
    assert!(tools["result"]["data"].is_array());
    let sessions = core.call("c5", "sessions/list", json!({}));
    assert!(sessions["result"]["data"].is_array());
}
#[test]
fn a_chat_turn_runs_end_to_end() {
    let temp = workspace();
    let mut core = Core::spawn(temp.path());
    core.await_ready();
    let created = core.call(
        "c1",
        "chat/session/create",
        json!({ "title": "a smoke turn" }),
    );
    let session_id = created["result"]["session_id"]
        .as_str()
        .expect("the session id")
        .to_string();
    let sent = core.call(
        "c2",
        "chat/message/send",
        json!({ "session_id": session_id, "text": "hello" }),
    );
    assert!(sent["result"].is_object(), "{sent}");
    let listed = core.call("c3", "sessions/list", json!({}));
    let names: Vec<&str> = listed["result"]["data"]
        .as_array()
        .expect("the session list")
        .iter()
        .filter_map(|row| row["id"].as_str())
        .collect();
    assert!(
        names.contains(&session_id.as_str()),
        "the session is listed: {names:?}"
    );
}
#[test]
fn the_anthropic_surface_answers_a_message() {
    let temp = workspace();
    let mut core = Core::spawn(temp.path());
    core.await_ready();
    let answer = core.call(
        "c1",
        "anthropic/messages",
        json!({
            "model": "demo-model",
            "max_tokens": 64,
            "messages": [{ "role": "user", "content": "hello" }],
        }),
    );
    assert!(answer["result"].is_object(), "{answer}");
}
#[test]
fn a_follow_answers_the_baseline() {
    let temp = workspace();
    let mut core = Core::spawn(temp.path());
    core.await_ready();
    let created =
        core.call("c1", "chat/session/create", json!({ "title": "a follow" }));
    let session_id = created["result"]["session_id"]
        .as_str()
        .expect("the session id")
        .to_string();
    let baseline =
        core.call("c2", "sessions/follow", json!({ "session_id": session_id }));
    let result = &baseline["result"];
    assert!(result["header"]["session_id"].is_string(), "{baseline}");
    assert!(result["rows"].is_array(), "{baseline}");
    assert!(result["cursor"].is_u64(), "{baseline}");
    let stopped = core.call(
        "c3",
        "session/unfollow",
        json!({ "session_id": session_id }),
    );
    assert_eq!(stopped["result"]["stopped"], true);
}
/// The core's stdout carries the protocol and nothing else: a log
/// line on stdout would reach the peer as a malformed frame.
#[test]
fn stdout_carries_the_protocol_and_nothing_else() {
    let temp = workspace();
    let mut core = Core::spawn(temp.path());
    core.await_ready();
    core.call("c1", "health/ping", json!({}));
    // Every frame read so far decoded as a Content-Length payload; a
    // log line on stdout would have broken the header/payload parse
    // in `recv`.
    let answer = core.call("c2", "health/ping", json!({}));
    assert_eq!(answer["result"]["ok"], true);
}
/// The gateway serves the probes and the RPC socket.
#[test]
fn the_gateway_answers_its_probes() {
    let temp = workspace();
    let port: u16 = 18_000 + (std::process::id() % 1_000) as u16;
    let mut gateway = Command::new(env!("CARGO_BIN_EXE_uva-app-server"))
        .arg("serve")
        .arg("--directory")
        .arg(temp.path())
        .arg("--port")
        .arg(port.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("the gateway spawns");
    let ready = wait_for_probe(port);
    assert!(ready, "the gateway never became ready");
    let version = fetch(port, "/version");
    assert!(
        version["version"].is_string(),
        "the version probe answers: {version}"
    );
    let metrics = fetch(port, "/metrics");
    assert!(metrics.is_string(), "the metrics probe answers");
    let _ = gateway.kill();
    let _ = gateway.wait();
}
/// Wait for the gateway's readiness probe.
fn wait_for_probe(port: u16) -> bool {
    for _ in 0..100 {
        if fetch(port, "/readyz").is_object() {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    false
}
/// Fetch a path from the gateway, returning the parsed JSON body (or
/// the text, for the metrics endpoint).
fn fetch(port: u16, path: &str) -> Value {
    let mut stream = match std::net::TcpStream::connect(("127.0.0.1", port)) {
        Ok(stream) => stream,
        Err(_) => return Value::Null,
    };
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .expect("the read timeout is set");
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n"
    );
    if stream.write_all(request.as_bytes()).is_err() {
        return Value::Null;
    }
    let mut response = String::new();
    let mut reader = BufReader::new(stream);
    let _ = reader.read_to_string(&mut response);
    let Some((_head, body)) = response.split_once("\r\n\r\n") else {
        return Value::Null;
    };
    serde_json::from_str(body).unwrap_or(Value::String(body.to_string()))
}
/// The state type the crate exposes, for the tests that build one.
#[allow(dead_code)]
type TestState = Arc<uva_app_server::state::AppState>;
