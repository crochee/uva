//! The ACP face, driven over its pipes.
//!
//! The ACP mode speaks the Agent Client Protocol on stdin/stdout, one
//! JSON-RPC message per line. The test spawns the binary, writes
//! messages, and reads the answers — the same conversation an editor
//! (Zed, a JetBrains IDE, any ACP client) has with it.
//!
//! What it proves:
//!
//! - the handshake answers with the protocol version, the
//!   capabilities and the agent's identity;
//! - an unknown method answers the standard code;
//! - a malformed line answers the parse code and the conversation
//!   continues;
//! - a prompt turn streams its answer as session updates and answers
//!   with the stop reason;
//! - a session reload replays the transcript.
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde_json::{Value, json};

/// The ACP face, driven over its pipes.
struct Acp {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl Acp {
    /// Spawn the ACP face against a workspace directory.
    fn spawn(workspace: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_uva-app-server"))
            .arg("acp")
            .arg("--directory")
            .arg(workspace)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("the ACP binary spawns");
        let stdin = child.stdin.take().expect("the face reads stdin");
        let stdout = BufReader::new(
            child.stdout.take().expect("the face writes stdout"),
        );
        Self {
            child,
            stdin,
            stdout,
        }
    }

    /// Write one message.
    fn send(&mut self, message: &Value) {
        let payload =
            serde_json::to_vec(message).expect("the message serialises");
        self.stdin
            .write_all(&payload)
            .expect("the message is written");
        self.stdin.write_all(b"\n").expect("the newline is written");
        self.stdin.flush().expect("the message is flushed");
    }

    /// Write one raw line.
    fn send_line(&mut self, line: &str) {
        self.stdin
            .write_all(line.as_bytes())
            .expect("the line is written");
        self.stdin.write_all(b"\n").expect("the newline is written");
        self.stdin.flush().expect("the line is flushed");
    }

    /// Read one message.
    fn recv(&mut self) -> Value {
        let mut line = String::new();
        let read = self
            .stdout
            .read_line(&mut line)
            .expect("the message is read");
        assert!(read > 0, "the face closed stdout");
        serde_json::from_str(line.trim_end_matches(['\r', '\n']))
            .expect("the message is JSON")
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

    /// Read messages until the one with the given id arrives.
    fn recv_id(&mut self, id: &str) -> Value {
        for _ in 0..256 {
            let message = self.recv();
            if message.get("id").and_then(Value::as_str) == Some(id) {
                return message;
            }
        }
        panic!("no answer for {id}");
    }

    /// The session updates a request's answer was preceded by.
    ///
    /// The updates may arrive before the answer: the turn starts when
    /// the prompt is parked, and the provider answers before the
    /// request's round trip completes.
    fn call_with_updates(
        &mut self,
        id: &str,
        method: &str,
        params: Value,
    ) -> (Value, Vec<Value>) {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        let mut updates = Vec::new();
        loop {
            let message = self.recv();
            if message.get("method").and_then(Value::as_str)
                == Some("session/update")
            {
                updates.push(message);
                continue;
            }
            if message.get("id").and_then(Value::as_str) == Some(id) {
                return (message, updates);
            }
        }
    }
}

impl Drop for Acp {
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

/// The handshake answers with the protocol version and the
/// capabilities.
#[test]
fn the_handshake_answers_the_protocol_and_the_capabilities() {
    let temp = workspace();
    let mut acp = Acp::spawn(temp.path());
    let answer = acp.call(
        "c1",
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {},
        }),
    );
    let result = &answer["result"];
    assert_eq!(result["protocolVersion"], 1, "{answer}");
    assert_eq!(result["agentCapabilities"]["loadSession"], true, "{answer}");
    assert_eq!(
        result["agentCapabilities"]["promptCapabilities"]["image"], true,
        "{answer}"
    );
    assert_eq!(result["agentInfo"]["name"], "uva", "{answer}");
    assert!(result["authMethods"].is_array(), "{answer}");
}

/// An unknown method answers the standard code.
#[test]
fn an_unknown_method_answers_the_standard_code() {
    let temp = workspace();
    let mut acp = Acp::spawn(temp.path());
    acp.call("c1", "initialize", json!({"protocolVersion": 1}));
    let answer = acp.call("c2", "no/such/method", json!({}));
    assert_eq!(answer["error"]["code"], -32601, "{answer}");
}

/// A malformed line answers the parse code, and the conversation
/// continues.
#[test]
fn a_malformed_line_answers_the_parse_code() {
    let temp = workspace();
    let mut acp = Acp::spawn(temp.path());
    acp.send_line("{not json");
    let answer = acp.recv();
    assert_eq!(answer["error"]["code"], -32700, "{answer}");
    let handshake = acp.call("c1", "initialize", json!({"protocolVersion": 1}));
    assert_eq!(handshake["result"]["protocolVersion"], 1, "{handshake}");
}

/// A prompt turn streams the answer as session updates and answers
/// with the stop reason.
#[test]
fn a_prompt_turn_streams_its_answer() {
    let temp = workspace();
    let mut acp = Acp::spawn(temp.path());
    acp.call("c1", "initialize", json!({"protocolVersion": 1}));
    let opened = acp.call(
        "c2",
        "session/new",
        json!({"cwd": "/tmp", "mcpServers": []}),
    );
    let session_id = opened["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("the session id: {opened}"))
        .to_string();
    let (answer, updates) = acp.call_with_updates(
        "c3",
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": "hello ACP"}],
        }),
    );
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    let chunks: Vec<&Value> = updates
        .iter()
        .filter(|update| {
            update["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
        })
        .collect();
    assert!(!chunks.is_empty(), "the answer streamed: {updates:?}");
    let text: String = chunks
        .iter()
        .map(|chunk| {
            chunk["params"]["update"]["content"]["text"]
                .as_str()
                .unwrap_or_default()
        })
        .collect();
    assert!(text.contains("hello ACP"), "{text}");
}

/// A session reload replays the transcript.
#[test]
fn a_reload_replays_the_transcript() {
    let temp = workspace();
    let mut acp = Acp::spawn(temp.path());
    acp.call("c1", "initialize", json!({"protocolVersion": 1}));
    let opened = acp.call(
        "c2",
        "session/new",
        json!({"cwd": "/tmp", "mcpServers": []}),
    );
    let session_id = opened["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("the session id: {opened}"))
        .to_string();
    let (answer, _) = acp.call_with_updates(
        "c3",
        "session/prompt",
        json!({
            "sessionId": session_id,
            "prompt": [{"type": "text", "text": "remember this"}],
        }),
    );
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    let (loaded, updates) = acp.call_with_updates(
        "c4",
        "session/load",
        json!({"sessionId": session_id, "cwd": "/tmp", "mcpServers": []}),
    );
    assert!(loaded["result"].is_object(), "{loaded}");
    let replayed: Vec<&Value> = updates
        .iter()
        .filter(|update| {
            update["params"]["update"]["sessionUpdate"] == "agent_message_chunk"
        })
        .collect();
    assert!(!replayed.is_empty(), "the transcript replayed: {updates:?}");
}

/// A client that offers the ACP file methods owns its session's
/// files: the `read` tool asks the client instead of touching the
/// deployment's disk.
///
/// The client advertises `fs/readTextFile` in `initialize`, opens a
/// session with its own directory, and prompts a turn that reads a
/// file. The agent sends `fs/read_text_file` back to the client, the
/// client answers with its own bytes, and the model sees those bytes
/// — not whatever this disk holds at that path.
#[test]
fn a_client_who_offers_the_file_methods_reads_its_own_files() {
    let temp = workspace();
    let mut acp = Acp::spawn(temp.path());
    acp.call(
        "c1",
        "initialize",
        json!({
            "protocolVersion": 1,
            "clientCapabilities": {
                "fs": {"readTextFile": true, "writeTextFile": false},
            },
        }),
    );
    let opened = acp.call(
        "c2",
        "session/new",
        json!({"cwd": "/client", "mcpServers": []}),
    );
    let session_id = opened["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("the session id: {opened}"))
        .to_string();
    acp.send(&json!({
        "jsonrpc": "2.0",
        "id": "c3",
        "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [{
                "type": "text",
                "text": r#"!tool read {"file_path": "notes.txt"}"#,
            }],
        },
    }));
    // The turn asks the client to read the file. The question is a
    // request with its own id: the client answers it.
    let mut read_request = None;
    let mut updates: Vec<Value> = Vec::new();
    for _ in 0..64 {
        let message = acp.recv();
        if message.get("method").and_then(Value::as_str)
            == Some("session/update")
        {
            updates.push(message);
            continue;
        }
        if message.get("method").and_then(Value::as_str)
            == Some("fs/read_text_file")
        {
            read_request = Some(message);
            break;
        }
    }
    let read_request =
        read_request.expect("the agent asked the client for the file");
    assert_eq!(
        read_request["params"]["sessionId"], session_id,
        "the read names the session"
    );
    assert_eq!(
        read_request["params"]["path"], "/client/notes.txt",
        "the read names the client's own path: {read_request}"
    );
    acp.send(&json!({
        "jsonrpc": "2.0",
        "id": read_request["id"].clone(),
        "result": {"content": "from the client"},
    }));
    // The turn continues: the updates that follow the answer carry
    // the tool result, and the prompt answers with its stop reason.
    let mut answer = None;
    for _ in 0..256 {
        let message = acp.recv();
        if message.get("method").and_then(Value::as_str)
            == Some("session/update")
        {
            updates.push(message);
            continue;
        }
        if message.get("id").and_then(Value::as_str) == Some("c3") {
            answer = Some(message);
            break;
        }
    }
    let answer = answer.expect("the prompt answers");
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    // The tool result the model saw is the client's own bytes, in the
    // line-numbered form the read tool renders.
    let rendered = updates.iter().any(|update| {
        let text = update["params"]["update"]["content"].to_string();
        text.contains("from the client")
    });
    assert!(rendered, "the model saw the client's bytes: {updates:?}");
}

/// A held call asks the operator, and the answer continues the turn.
///
/// The demo provider scripts a `shell` call whose command redirects
/// output — a high-risk call, so the deployment's approval gate holds
/// it. The face turns the hold into a permission question, the client
/// answers, and the turn continues: the refused call becomes the
/// model's error result and the loop answers with text.
#[test]
fn a_held_call_asks_the_operator_and_the_turn_continues() {
    let temp = workspace();
    let mut acp = Acp::spawn(temp.path());
    acp.call("c1", "initialize", json!({"protocolVersion": 1}));
    let opened = acp.call(
        "c2",
        "session/new",
        json!({"cwd": "/tmp", "mcpServers": []}),
    );
    let session_id = opened["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("the session id: {opened}"))
        .to_string();
    acp.send(&json!({
        "jsonrpc": "2.0",
        "id": "c3",
        "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [{
                "type": "text",
                "text": r#"!tool shell {"command": "echo held > /tmp/held.txt"}"#,
            }],
        },
    }));
    // The turn streams the call, then the face asks the operator. The
    // question is a request with its own id: the client answers it.
    let mut question = None;
    let mut updates: Vec<Value> = Vec::new();
    for _ in 0..64 {
        let message = acp.recv();
        if message.get("method").and_then(Value::as_str)
            == Some("session/update")
        {
            updates.push(message);
            continue;
        }
        if message.get("method").and_then(Value::as_str)
            == Some("session/request_permission")
        {
            question = Some(message);
            break;
        }
    }
    let question = question.expect("the held call asks the operator");
    let request_id = question["id"].clone();
    assert!(request_id.is_string(), "the question's id: {question}");
    assert_eq!(
        question["params"]["sessionId"], session_id,
        "the question names the session"
    );
    assert!(
        question["params"]["options"].is_array(),
        "the question offers options: {question}"
    );
    // The call itself was reported before the question.
    let called = updates.iter().any(|update| {
        update["params"]["update"]["sessionUpdate"] == "tool_call"
    });
    assert!(called, "the call was reported: {updates:?}");
    // The operator refuses: the call must not run.
    acp.send(&json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "result": {
            "outcome": {"outcome": "selected", "optionId": "reject-once"},
        },
    }));

    let answer = acp.recv_id("c3");
    assert_eq!(answer["result"]["stopReason"], "end_turn", "{answer}");
    assert!(
        !std::path::Path::new("/tmp/held.txt").exists(),
        "the refused call did not run"
    );
}

/// A cancelled turn answers with the cancelled stop reason.
///
/// The client cancels while the permission question is open — the
/// protocol's own rule is that a cancelled turn's pending question is
/// answered with the `cancelled` outcome — and the prompt request
/// answers `cancelled` instead of waiting for an answer that will
/// not come.
#[test]
fn a_cancelled_turn_answers_the_cancelled_stop_reason() {
    let temp = workspace();
    let mut acp = Acp::spawn(temp.path());
    acp.call("c1", "initialize", json!({"protocolVersion": 1}));
    let opened = acp.call(
        "c2",
        "session/new",
        json!({"cwd": "/tmp", "mcpServers": []}),
    );
    let session_id = opened["result"]["sessionId"]
        .as_str()
        .unwrap_or_else(|| panic!("the session id: {opened}"))
        .to_string();
    acp.send(&json!({
        "jsonrpc": "2.0",
        "id": "c3",
        "method": "session/prompt",
        "params": {
            "sessionId": session_id,
            "prompt": [{
                "type": "text",
                "text": r#"!tool shell {"command": "echo held > /tmp/cancelled.txt"}"#,
            }],
        },
    }));
    let mut question = None;
    for _ in 0..64 {
        let message = acp.recv();
        if message.get("method").and_then(Value::as_str)
            == Some("session/request_permission")
        {
            question = Some(message);
            break;
        }
    }
    let question = question.expect("the held call asks the operator");
    let request_id = question["id"].clone();
    // The client cancels the turn, then answers the open question the
    // way the protocol says a cancelled turn is answered.
    acp.send(&json!({
        "jsonrpc": "2.0",
        "method": "session/cancel",
        "params": {"sessionId": session_id},
    }));
    acp.send(&json!({
        "jsonrpc": "2.0",
        "id": request_id,
        "result": {"outcome": {"outcome": "cancelled"}},
    }));
    let answer = acp.recv_id("c3");
    assert_eq!(answer["result"]["stopReason"], "cancelled", "{answer}");
    assert!(
        !std::path::Path::new("/tmp/cancelled.txt").exists(),
        "the cancelled call did not run"
    );
}
