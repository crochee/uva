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
/// `chat/message/stream` — one request, the turn's deltas back.
///
/// The headless core's native streaming door: the answer names the
/// turn it started, the turn's frames follow as `chat/turn/event`
/// notifications carrying that id, and the last frame is the turn's
/// end. The demo provider answers in one text chunk, so the proof is
/// the *shape* of the stream — the text frame, then the end frame,
/// all under one turn id, none of them the session's cumulative
/// snapshot.
///
/// The frames can precede the response: the turn starts when the
/// park is durable, and the demo provider answers before the
/// response's round trip completes. The test therefore collects the
/// turn's notifications while it waits for the answer — exactly what
/// a client that buffers notifications it cannot yet attribute has
/// to do.
#[test]
fn a_turn_streams_its_deltas() {
    let temp = workspace();
    let mut core = Core::spawn(temp.path());
    core.await_ready();
    let created = core.call(
        "c1",
        "chat/session/create",
        json!({ "title": "a streamed turn" }),
    );
    let session_id = created["result"]["session_id"]
        .as_str()
        .expect("the session id")
        .to_string();
    core.send(&json!({
        "jsonrpc": "2.0",
        "id": "c2",
        "method": "chat/message/stream",
        "params": { "session_id": session_id, "text": "hello stream" },
    }));
    // Collect the turn's frames while the answer is in flight: a
    // notification that arrives before its request's response is
    // still part of that request's stream.
    let mut frames: Vec<Value> = Vec::new();
    let answer = loop {
        let frame = core.recv();
        if frame.get("method").and_then(Value::as_str)
            == Some("chat/turn/event")
        {
            frames.push(frame);
            continue;
        }
        if frame.get("id") == Some(&json!("c2")) {
            break frame;
        }
    };
    assert!(answer["result"]["accepted"] == true, "{answer}");
    let turn_id = answer["result"]["turn_id"]
        .as_str()
        .expect("the turn id")
        .to_string();
    // Read the turn's own frames until its end. The demo provider
    // answers in one text chunk, so the stream is short — but it must
    // end with an `end` frame under the same turn id, and a text
    // frame must arrive before it.
    let mut saw_text = false;
    let mut seq = 0u64;
    let reason = loop {
        let frame = if frames.is_empty() {
            core.recv()
        } else {
            frames.remove(0)
        };
        if frame.get("method").and_then(Value::as_str)
            != Some("chat/turn/event")
        {
            continue;
        }
        let params = &frame["params"];
        assert_eq!(
            params["turn_id"].as_str(),
            Some(turn_id.as_str()),
            "{frame}"
        );
        let next = params["seq"].as_u64().expect("the frame's ordinal");
        assert_eq!(next, seq + 1, "the frames number themselves: {frame}");
        seq = next;
        match params["frame"]["kind"].as_str() {
            Some("text") => {
                saw_text = true;
                let text = params["frame"]["text"].as_str().unwrap_or("");
                assert!(
                    text.contains("hello stream"),
                    "the demo echo carries the prompt: {frame}"
                );
            }
            Some("end") => break params["frame"]["reason"].clone(),
            _ => {}
        }
    };
    assert_eq!(reason, json!("completed"), "{reason}");
    assert!(saw_text, "the stream carried the answer's text");
}

/// A client-owned tool executes on the client.
///
/// The reverse direction of the core face: the client registers a
/// tool it owns (`tools/register` with `owner: "client"`), the model
/// calls it, and the core *asks the client to run it* — a request
/// frame with an id the core minted, answered on the same pipe. The
/// proof is the whole loop: the `tools/execute` request arrives
/// while the turn is running, the client's answer becomes the tool
/// result, and the durable rows carry it.
#[test]
fn a_client_owned_tool_executes_on_the_client() {
    let temp = workspace();
    let mut core = Core::spawn(temp.path());
    core.await_ready();
    let created = core.call(
        "c1",
        "chat/session/create",
        json!({ "title": "a client tool" }),
    );
    let session_id = created["result"]["session_id"]
        .as_str()
        .expect("the session id")
        .to_string();
    let registered = core.call(
        "c2",
        "tools/register",
        json!({
            "name": "client_tool",
            "description": "runs on the client that registered it",
            "input_schema": { "type": "object" },
            "owner": "client",
            "session_id": session_id,
        }),
    );
    assert_eq!(registered["result"]["name"], "client_tool", "{registered}");

    core.send(&json!({
        "jsonrpc": "2.0",
        "id": "c3",
        "method": "chat/message/stream",
        "params": {
            "session_id": session_id,
            "text": "!tool client_tool {}",
        },
    }));
    let mut answered = None;
    let mut answer = None;
    let end;
    // One loop, three facts: the stream's answer, the core's
    // question — answered the moment it arrives, the turn is
    // waiting on it — and the turn's end frame. The answer comes
    // when the park is durable, the question when the model asks,
    // and the end frame only after the answer flowed back, so the
    // loop exits exactly when the whole round trip is done.
    loop {
        let frame = core.recv();
        if frame.get("method").and_then(Value::as_str) == Some("tools/execute")
        {
            assert_eq!(
                frame["params"]["name"], "client_tool",
                "the question names the tool: {frame}"
            );
            assert_eq!(
                frame["params"]["session_id"], session_id,
                "the question names the session it is about: {frame}"
            );
            assert!(
                frame["id"]
                    .as_str()
                    .is_some_and(|id| id.starts_with("core:internal:")),
                "the id is one the core minted: {frame}"
            );
            answered = Some(frame["id"].clone());
            core.send(&json!({
                "jsonrpc": "2.0",
                "id": frame["id"],
                "result": {
                    "content": "ran on the client",
                    "is_error": false,
                },
            }));
            continue;
        }
        if frame.get("id") == Some(&json!("c3")) && answer.is_none() {
            answer = Some(frame);
            continue;
        }
        if frame.get("method").and_then(Value::as_str)
            == Some("chat/turn/event")
            && frame["params"]["frame"]["kind"].as_str() == Some("end")
        {
            end = frame;
            break;
        }
    }
    let answer = answer.expect("the stream answered");
    assert!(answer["result"]["accepted"] == true, "{answer}");
    assert!(answered.is_some(), "the core asked the client to run it");
    let turn_id = answer["result"]["turn_id"].as_str().expect("the turn id");
    assert_eq!(
        end["params"]["turn_id"].as_str(),
        Some(turn_id),
        "the end frame belongs to the turn the answer named: {end}"
    );
    assert_eq!(end["params"]["frame"]["reason"], json!("completed"));

    // The durable rows carry the client's answer as the tool result.
    let events =
        core.call("c4", "sessions/events", json!({ "id": session_id }));
    let rows = serde_json::to_string(&events["result"]["typed"])
        .expect("the durable rows serialise");
    assert!(
        rows.contains("ran on the client"),
        "the client's answer is the tool result: {events}"
    );
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
