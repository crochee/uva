//! ACP over the core's own JSON-RPC channel.
//!
//! The core's stdio channel carries two vocabularies: the core's own
//! methods and ACP's. A client that speaks ACP — an editor or a
//! terminal program reaching the deployment through the gateway's
//! WebSocket — sends ACP frames on that channel, and the core answers
//! them with the ACP adapter rather than with the core's router.
//!
//! What it proves:
//!
//! - the handshake answers with the protocol version and the
//!   capabilities, on the same channel the core's own methods use;
//! - a session opens and a turn runs, streaming its answer as
//!   `session/update` notifications before the prompt's answer;
//! - the core's own methods still answer on the same channel.
use std::{
    io::{BufRead, BufReader, Read as _, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde_json::{Value, json};

/// The core, driven over its pipes.
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
        for _ in 0..256 {
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
    fn await_ready(&mut self) {
        for _ in 0..16 {
            let frame = self.recv();
            if frame.get("method").and_then(Value::as_str) == Some("ready") {
                return;
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

/// The core answers ACP's handshake on its own channel.
#[test]
fn the_core_answers_the_acp_handshake() {
    let temp = workspace();
    let mut core = Core::spawn(temp.path());
    core.await_ready();
    let answer = core.call(
        "c1",
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    let result = &answer["result"];
    assert_eq!(result["protocolVersion"], 1, "{answer}");
    assert_eq!(result["agentCapabilities"]["loadSession"], true, "{answer}");
    assert_eq!(result["agentInfo"]["name"], "uva", "{answer}");
}

/// The core's own methods and ACP's share the channel: one does not
/// hide the other.
#[test]
fn the_core_answers_its_own_methods_beside_acp() {
    let temp = workspace();
    let mut core = Core::spawn(temp.path());
    core.await_ready();
    core.call(
        "c1",
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    let ping = core.call("c2", "health/ping", json!({}));
    assert_eq!(ping["result"]["ok"], true, "{ping}");
}

/// A prompt over the core's channel streams its answer as ACP's
/// session updates and answers with the stop reason.
#[test]
fn a_prompt_over_the_core_channel_streams_acp_updates() {
    let temp = workspace();
    let mut core = Core::spawn(temp.path());
    core.await_ready();
    core.call(
        "c1",
        "initialize",
        json!({"protocolVersion": 1, "clientCapabilities": {}}),
    );
    let opened = core.call(
        "c2",
        "session/new",
        json!({"cwd": "/tmp", "mcpServers": []}),
    );
    let session_id = opened["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("the session id: {opened}"))
        .to_string();
    core.send(&json!({
        "jsonrpc": "2.0",
        "id": "c3",
        "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": "hello over rpc"}],
        },
    }));
    let mut updates: Vec<Value> = Vec::new();
    let mut answer = None;
    for _ in 0..256 {
        let frame = core.recv();
        if frame.get("method").and_then(Value::as_str) == Some("session/update")
        {
            updates.push(frame);
            continue;
        }
        if frame.get("id").and_then(Value::as_str) == Some("c3") {
            answer = Some(frame);
            break;
        }
    }
    let answer = answer.expect("the prompt is answered");
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    let text: String = updates
        .iter()
        .filter_map(|update| {
            update["params"]["update"]["content"]["text"].as_str()
        })
        .collect();
    assert!(text.contains("hello over rpc"), "{text}");
}
