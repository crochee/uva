//! End-to-end acceptance for the profile lifecycle cut.
//!
//! Spawns the real core binary over a temp workspace whose
//! `.agents/profile.toml` — the project's configuration layer —
//! switches the `tool-todo` row off. Drives the four acceptance
//! behaviours over the stdio wire: the off row is visible with a
//! reason, turning it on mounts the capability and broadcasts
//! `profile/changed`, a row that cannot mount reports `failed` and
//! leaves the document byte-identical, and turning it off removes the
//! capability again.

use std::{
    io::{BufRead, BufReader, Read as _, Write},
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde_json::{Value, json};

/// The built-in native rows, in the boot's declaration order.
const NATIVE_IDS: &[&str] = &[
    "tool-registry",
    "tool-read",
    "tool-write",
    "tool-shell",
    "tool-todo",
    "tool-web",
    "model-adapter",
    "memory",
    "search",
    "session-store",
    "sandbox",
    "tool-task",
    "context-compactor",
    "approval",
    "skills",
    "hooks",
    "agent-loop",
    "mcp",
    "eval",
    "scheduler",
    "schedule-tool",
    "attachments",
];

struct Core {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// Notifications read while waiting for a response.
    queued: Vec<Value>,
}

impl Core {
    fn spawn(workspace: &std::path::Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_uva-app-server"))
            .arg("core")
            .arg("--directory")
            .arg(workspace)
            // Isolate the global layout: no developer home-patch or
            // user skills leak into the acceptance run. The home
            // lives beside the workspace, not inside it — the
            // workspace's own configuration layer holds the profile
            // document, and the two layers must stay distinct.
            .env("HOME", workspace.join("home"))
            .env_remove("UVA_HOME_PROFILE")
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
            queued: Vec::new(),
        }
    }

    fn send(&mut self, frame: &Value) {
        let payload = serde_json::to_vec(frame).expect("the frame serialises");
        write!(self.stdin, "Content-Length: {}\r\n\r\n", payload.len())
            .expect("the header is written");
        self.stdin
            .write_all(&payload)
            .expect("the frame is written");
        self.stdin.flush().expect("the frame is flushed");
    }

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

    fn next_unless(&mut self, matches: impl Fn(&Value) -> bool) -> Value {
        for _ in 0..64 {
            let frame = self.recv();
            if matches(&frame) {
                return frame;
            }
            self.queued.push(frame);
        }
        panic!("the expected frame never arrived");
    }

    fn call(&mut self, id: &str, method: &str, params: Value) -> Value {
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        self.next_unless(|frame| {
            frame.get("id").and_then(Value::as_str) == Some(id)
        })
    }

    fn await_ready(&mut self) -> Value {
        self.next_unless(|frame| {
            frame.get("method").and_then(Value::as_str) == Some("ready")
        })
    }

    fn await_notification(&mut self, method: &str) -> Value {
        if let Some(index) = self.queued.iter().position(|frame| {
            frame.get("method").and_then(Value::as_str) == Some(method)
        }) {
            return self.queued.remove(index);
        }
        self.next_unless(|frame| {
            frame.get("method").and_then(Value::as_str) == Some(method)
        })
    }
}

impl Drop for Core {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

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
    let mut rows = String::new();
    for id in NATIVE_IDS {
        match *id {
            "tool-todo" => rows.push_str(
                "  { id = \"tool-todo\", name = \"tool-todo\", disabled = true },\n",
            ),
            other => rows.push_str(&format!(
                "  {{ id = \"{other}\", name = \"{other}\" }},\n"
            )),
        }
    }
    std::fs::write(
        agents.join("profile.toml"),
        format!("hmr = false\n\n[core]\nentries = [\n{rows}]\n"),
    )
    .expect("the profile document");
    temp
}

fn row<'a>(rows: &'a Value, id: &str) -> &'a Value {
    rows.as_array()
        .expect("a row array")
        .iter()
        .find(|row| row["id"] == id)
        .unwrap_or_else(|| panic!("row `{id}` missing from {rows}"))
}

fn todo_tool(registry: &Value) -> bool {
    registry["result"]["data"]
        .as_array()
        .expect("a tool list")
        .iter()
        .any(|tool| tool["name"] == "TodoWrite")
}

#[test]
fn the_profile_transaction_survives_the_stdio_wire() {
    let temp = workspace();
    let doc = temp.path().join(".agents").join("profile.toml");
    let mut core = Core::spawn(temp.path());
    core.await_ready();

    // 1. The off row is visible, with the effective status the
    //    settings page renders.
    let listed = core.call("p1", "profile/list", json!({}));
    let rows = &listed["result"];
    assert!(
        row(rows, "tool-todo")["status"]
            .as_str()
            .expect("a status")
            .starts_with("off:"),
        "the disabled tool-todo row is reported off: {}",
        row(rows, "tool-todo")
    );
    assert_eq!(row(rows, "tool-todo")["disabled"], json!(true));
    assert_eq!(row(rows, "eval")["status"], "mounted");
    let tools = core.call("p2", "tools/list", json!({}));
    assert!(
        !todo_tool(&tools),
        "a disabled row contributes no tool: {tools}"
    );

    // 2. Turning it on applies, mounts the capability, and broadcasts.
    let applied = core.call(
        "p3",
        "profile/set",
        json!({ "id": "tool-todo", "disabled": false }),
    );
    assert_eq!(applied["result"]["state"], "applied", "{applied}");
    let tools = core.call("p4", "tools/list", json!({}));
    assert!(
        todo_tool(&tools),
        "the enabled tool-todo row registers `TodoWrite`: {tools}"
    );
    let changed = core.await_notification("profile/changed");
    assert_eq!(changed["params"]["ids"], json!(["tool-todo"]));
    assert_eq!(changed["params"]["state"], "applied");

    // 3. A row that cannot mount reports failed and leaves the
    //    document byte-identical.
    let before = std::fs::read(&doc).expect("the document exists");
    let failed = core.call(
        "p5",
        "profile/set",
        json!({ "id": "model-adapter", "config": { "providers": "not-an-object" } }),
    );
    assert_eq!(failed["result"]["state"], "failed", "{failed}");
    assert_eq!(
        std::fs::read(&doc).expect("the document exists"),
        before,
        "a failed transaction never rewrites the document"
    );

    // 4. Switching it off again removes the capability and broadcasts
    //    a second time.
    let off = core.call(
        "p6",
        "profile/set",
        json!({ "id": "tool-todo", "disabled": true }),
    );
    assert_eq!(off["result"]["state"], "applied", "{off}");
    let tools = core.call("p7", "tools/list", json!({}));
    assert!(
        !todo_tool(&tools),
        "the row's capability disappears with it: {tools}"
    );
    let changed = core.await_notification("profile/changed");
    assert_eq!(changed["params"]["ids"], json!(["tool-todo"]));
}
