use std::path::PathBuf;

use serde_json::json;

use super::*;

fn make_context() -> Context {
    Context::new("s1".to_string(), PathBuf::from("/tmp"))
}

fn text_of(out: &ToolOutput) -> String {
    out.content
        .iter()
        .find_map(|c| match c {
            uva_provider::types::ContentPart::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn ls_is_low_risk_and_rm_is_high_risk() {
    let tool = ShellTool::new();
    for command in [
        "ls -la",
        "cat README.md",
        "git status",
        "git log --oneline -5",
        "rg TODO crates",
        "pwd",
    ] {
        assert_eq!(
            tool.risk(&json!({ "command": command })),
            Risk::Low,
            "{command} is read-only"
        );
    }
    for command in [
        "rm -rf target",
        "mv a b",
        "echo hi > out.txt",
        "chmod +x run.sh",
        "sudo apt install foo",
        "git push origin main",
        "curl https://example.com",
    ] {
        assert_eq!(
            tool.risk(&json!({ "command": command })),
            Risk::High,
            "{command} mutates or reaches the network"
        );
    }
    assert_eq!(
        tool.risk(&json!({ "command": "cargo build" })),
        Risk::Unknown,
        "an unclassified command is not assumed safe or dangerous"
    );
}

// The scheduler asks per call, not per tool: a read-only command is
// safe beside another call, anything it cannot prove read-only is not.
#[test]
fn a_read_only_command_may_overlap_and_anything_else_may_not() {
    let tool = ShellTool::new();
    for command in ["ls -la", "cat README.md", "rg TODO crates", "pwd"] {
        assert_eq!(
            tool.mode_for(&json!({ "command": command })),
            ExecutionMode::Parallel,
            "{command} is read-only, so it may overlap"
        );
    }
    for command in [
        "rm -rf target",
        "echo hi > out.txt",
        "cargo build",
        "git push origin main",
    ] {
        assert_eq!(
            tool.mode_for(&json!({ "command": command })),
            ExecutionMode::Sequential,
            "{command} is not proven read-only, so it runs alone"
        );
    }
    assert_eq!(
        tool.mode_for(&json!({})),
        ExecutionMode::Sequential,
        "a call without a command is not classified parallel"
    );
}

#[test]
fn mutating_wrappers_and_flags_are_high_risk() {
    let tool = ShellTool::new();
    for command in [
        "env rm -rf target",
        "env -i rm -rf target",
        "awk 'BEGIN{system(\"rm -r target\")}'",
        "gawk 'BEGIN{system(\"rm -r target\")}'",
        "sed -Ei 's/a/b/' file",
        "sed -ni 's/a/b/p' file",
        "find . -fprintf out '%p'",
        "find . -fprint out",
        "find . -fprint0 out",
        "find . -fls out",
        "git log --output=file",
    ] {
        assert_eq!(
            tool.risk(&json!({ "command": command })),
            Risk::High,
            "{command} is not read-only"
        );
    }
    for command in ["env ls -la", "env -i ls -la", "sed -n 's/a/b/p' f"] {
        assert_eq!(
            tool.risk(&json!({ "command": command })),
            Risk::Low,
            "{command} is still read-only"
        );
    }
}

#[test]
fn compound_shell_constructs_are_high_risk() {
    let tool = ShellTool::new();
    for command in [
        "for f in target build; do rm -r \"$f\"; done",
        "if test -f target; then rm -r x; fi",
        "{ rm -r x; }",
        "while true; do sleep 1; done",
        "if ls; then cat x; fi",
    ] {
        assert_eq!(
            tool.risk(&json!({ "command": command })),
            Risk::High,
            "{command} is a compound construct the classifier does not model"
        );
    }
}

#[test]
fn transparent_wrappers_are_classified_by_the_command_they_run() {
    let tool = ShellTool::new();
    for command in [
        "timeout 5 rm -r x",
        "timeout -s KILL 5 rm -r x",
        "nice rm -r x",
        "nice -n 10 rm -r x",
        "nohup rm -r x",
        "time rm -r x",
        "stdbuf -oL rm -r x",
        "ionice -c3 rm -r x",
        "chroot /tmp rm -r x",
    ] {
        assert_eq!(
            tool.risk(&json!({ "command": command })),
            Risk::High,
            "{command} wraps a destructive command"
        );
    }
    for command in [
        "timeout 5 ls -la",
        "nice -n 10 cat README.md",
        "nohup git status",
        "time ls",
        "stdbuf -oL rg TODO crates",
        "ionice -c3 cat f",
        "chroot /tmp ls",
    ] {
        assert_eq!(
            tool.risk(&json!({ "command": command })),
            Risk::Low,
            "{command} wraps a read-only command"
        );
    }
}

#[test]
fn find_okdir_is_high_risk() {
    let tool = ShellTool::new();
    assert_eq!(
        tool.risk(&json!({ "command": "find . -okdir rm {} \\;" })),
        Risk::High,
        "-okdir executes a command"
    );
}

#[test]
fn is_denied_blocks_rm_rf() {
    let tool = ShellTool::new();
    assert!(tool.is_denied("rm -rf /tmp/foo"));
}

#[test]
fn is_denied_blocks_mkfs() {
    let tool = ShellTool::new();
    assert!(tool.is_denied("mkfs.ext4 /dev/sda1"));
}

#[test]
fn is_denied_blocks_fork_bomb() {
    let tool = ShellTool::new();
    assert!(tool.is_denied(":(){:|:&};:"));
}

#[test]
fn is_denied_allows_safe_commands() {
    let tool = ShellTool::new();
    assert!(!tool.is_denied("ls -la /tmp"));
    assert!(!tool.is_denied("cat /etc/hostname"));
    assert!(!tool.is_denied("echo hello"));
}

#[test]
fn is_denied_disabled_allows_dangerous_commands() {
    let tool = ShellTool::with_config(ShellSafetyConfig { disabled: true });
    assert!(!tool.is_denied("rm -rf /"));
}

#[tokio::test]
async fn safe_command_returns_text_output() {
    let tool = ShellTool::new();
    let out = tool
        .call(json!({"command": "echo hello"}), &make_context())
        .await;
    let text = text_of(&out);
    assert!(text.contains("hello"));
    assert!(text.trim_end().ends_with("[exit code: 0]"));
    assert!(out.is_error.is_none() || !out.is_error.unwrap_or(false));
}

#[tokio::test]
async fn denied_command_returns_error() {
    let tool = ShellTool::new();
    let out = tool
        .call(json!({"command": "rm -rf /tmp"}), &make_context())
        .await;
    assert!(out.is_error.unwrap_or(false));
}

/// dsh alignment: a non-zero exit is DATA, not an error. The
/// marker is rendered last and parseable.
#[tokio::test]
async fn non_zero_exit_is_a_normal_result_with_marker() {
    let tool = ShellTool::new();
    let out = tool
        .call(json!({"command": "echo boom >&2; exit 3"}), &make_context())
        .await;
    assert_ne!(out.is_error, Some(true));
    let text = text_of(&out);
    assert!(text.contains("boom"), "stderr should be captured: {text}");
    assert!(
        text.trim_end().ends_with("[exit code: 3]"),
        "exit marker must be last, got: {text}"
    );
}

/// The command MUST run inside `Context::workspace_root`.
#[tokio::test]
async fn command_runs_in_workspace_root() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = Context::new("s1".to_string(), dir.path().to_path_buf());
    let tool = ShellTool::new();
    let out = tool.call(json!({"command": "pwd"}), &ctx).await;
    let text = text_of(&out);
    assert!(
        text.contains(dir.path().as_os_str().to_string_lossy().as_ref()),
        "pwd should report the workspace root, got: {text}"
    );
}

/// Timeout MUST kill the tree, render the timeout marker (even
/// when the child ignores SIGTERM), and still return a normal
/// result with whatever output was produced.
#[tokio::test]
async fn timeout_kills_tree_and_renders_marker() {
    let tool = ShellTool::new();
    // Child traps SIGTERM and keeps spawning descendants that
    // also ignore it — only the SIGKILL escalation stops it.
    let out = tool
        .call(
            json!({
                "command": "trap '' TERM; echo started; sleep 30 & wait",
                "timeout_secs": 1
            }),
            &make_context(),
        )
        .await;
    let text = text_of(&out);
    assert!(text.contains("started"), "partial output kept: {text}");
    assert!(
        text.trim_end().ends_with("[timed out after 1s]"),
        "timeout marker must be last, got: {text}"
    );
    // A normal result, not an error: the model adapts.
    assert_ne!(out.is_error, Some(true));
}

/// A model-requested timeout above the cap MUST be clamped,
/// not rejected.
#[tokio::test]
async fn timeout_is_clamped_to_cap() {
    let tool = ShellTool::new();
    let out = tool
        .call(
            json!({"command": "true", "timeout_secs": 999_999}),
            &make_context(),
        )
        .await;
    assert_ne!(out.is_error, Some(true));
}

/// Environment hardening MUST reach the child.
#[tokio::test]
async fn env_hardening_reaches_child() {
    let tool = ShellTool::new();
    let out = tool
        .call(
            json!({"command": "echo \"$NO_COLOR/$TERM/$PAGER/$GIT_PAGER\""}),
            &make_context(),
        )
        .await;
    let text = text_of(&out);
    assert!(
        text.contains("1/dumb/cat/cat"),
        "hardened env expected, got: {text}"
    );
}

/// Backend that hands the payload straight to `sh` while
/// declaring bubblewrap's denial dialect: the wrap shape is
/// proven in [`crate::shell::sandbox`], so stubbing the runner keeps
/// this test hermetic.
#[derive(Debug)]
struct ShBackend;

impl SandboxBackend for ShBackend {
    fn name(&self) -> &str {
        "sh-stub"
    }

    fn availability(&self) -> BackendAvailability {
        BackendAvailability::Available
    }

    fn confine(
        &self,
        _policy: ExecutionPolicy,
        _workspace_root: &std::path::Path,
    ) -> std::result::Result<ConfinedCommand, SandboxError> {
        Ok(ConfinedCommand::unconfined())
    }

    fn denial_signatures(&self) -> &'static [&'static str] {
        &["read-only file system"]
    }
}

/// The model is told which boundary it plans inside.
#[test]
fn description_advertises_the_execution_policy() {
    let danger = ShellTool::new();
    assert!(danger.description().contains("danger-full-access"));

    let read_only = ShellTool::with_policy(ExecutionPolicy::ReadOnly);
    assert!(read_only.description().contains("read-only"));
    assert!(read_only.description().contains("not a command failure"));
    // The pre-existing contract text survives the paragraph.
    assert!(read_only.description().contains("[exit code: N]"));
}

/// R124: MCP-native descriptor hints flip with the policy —
/// confined policies report `read_only_hint: true` and
/// `destructive_hint: false`; permissive policies report the
/// inverse. The shape is the same; the booleans move.
#[test]
fn annotations_flip_with_the_execution_policy() {
    let danger = ShellTool::new();
    let a = danger
        .annotations()
        .expect("shell declares MCP-style annotations");
    assert_eq!(a.read_only_hint, Some(false));
    assert_eq!(a.destructive_hint, Some(true));
    assert_eq!(a.idempotent_hint, Some(false));
    assert_eq!(a.open_world_hint, Some(false));

    let confined = ShellTool::with_policy(ExecutionPolicy::ReadOnly);
    let a = confined
        .annotations()
        .expect("shell declares MCP-style annotations");
    assert_eq!(a.read_only_hint, Some(true));
    assert_eq!(a.destructive_hint, Some(false));
}

/// Fail closed: a confined policy with no backend refuses the
/// command instead of running it unconfined.
#[tokio::test]
async fn confined_policy_without_a_backend_refuses() {
    let tool = ShellTool::with_policy(ExecutionPolicy::ReadOnly);
    let out = tool
        .call(json!({"command": "echo ran"}), &make_context())
        .await;
    assert_eq!(out.is_error, Some(true));
    let text = text_of(&out);
    assert!(text.contains(SANDBOX_UNAVAILABLE), "got: {text}");
    assert!(text.contains("danger-full-access"), "got: {text}");
    assert!(
        !text.contains("stdout:"),
        "the command must never run: {text}"
    );
}

/// A sandbox refusal is a policy fact, not a broken command:
/// the run keeps its data and gains the marker, and the exit
/// marker stays last.
#[tokio::test]
async fn policy_denial_is_marked_and_is_not_an_error() {
    let tool = ShellTool::with_policy(ExecutionPolicy::ReadOnly)
        .with_sandbox(Arc::new(ShBackend));
    let out = tool
        .call(
            json!({
                "command": "echo \"touch: cannot touch '/x': Read-only file system\" >&2; exit 1"
            }),
            &make_context(),
        )
        .await;
    assert_ne!(out.is_error, Some(true), "a denial is data, not an error");
    let text = text_of(&out);
    assert!(
        text.contains("[sandbox: file access denied under read-only mode]"),
        "got: {text}"
    );
    assert!(
        text.trim_end().ends_with("[exit code: 1]"),
        "exit marker stays last: {text}"
    );
}

/// The default policy never claims a denial — even when the
/// command's own stderr uses the denial vocabulary.
#[tokio::test]
async fn danger_full_access_never_claims_a_policy_denial() {
    let tool = ShellTool::new();
    let out = tool
        .call(
            json!({
                "command": "echo 'Read-only file system' >&2; exit 1"
            }),
            &make_context(),
        )
        .await;
    assert_ne!(out.is_error, Some(true));
    let text = text_of(&out);
    assert!(!text.contains("[sandbox:"), "got: {text}");
    assert!(text.trim_end().ends_with("[exit code: 1]"));
}

/// Pin the JSON-Schema shape for `shell` so future drift in
/// either the schema, the typed `ShellArgs`, or the runtime
/// `#[serde(default)]` semantics breaks here instead of at
/// the LLM boundary.
#[test]
fn parameters_schema_is_self_consistent() {
    let tool = ShellTool::new();
    let params = tool.parameters();
    assert_eq!(params["type"], "object");
    let required: Vec<&str> = params["required"]
        .as_array()
        .expect("required")
        .iter()
        .filter_map(|v| v.as_str())
        .collect();
    assert_eq!(required, vec!["command"]);

    let props = params["properties"].as_object().expect("properties");
    let cmd = props["command"].as_object().expect("command");
    assert_eq!(cmd["type"], "string");
    assert!(cmd["description"].as_str().is_some());

    let timeout = props["timeout_secs"].as_object().expect("timeout_secs");
    let ty = &timeout["type"];
    assert!(
        ty == "integer"
            || ty.as_array().is_some_and(|arr| {
                arr.iter().any(|v| v == "integer")
                    && arr.iter().any(|v| v == "null")
            }),
        "timeout_secs type should be integer or [integer, null], got: {ty}"
    );
    assert_eq!(
        timeout["minimum"].as_f64().unwrap() as u64,
        1,
        "timeout_secs must require a positive integer"
    );
    assert_eq!(
        timeout["default"], 120,
        "timeout_secs schema default must match runtime default"
    );

    assert_eq!(
        params["additionalProperties"], false,
        "additional fields must be rejected to match serde_json::from_value"
    );
}

// ---------------------------------------------------------------------------
// Streaming: a long command's output reaches the client while it runs
// ---------------------------------------------------------------------------

/// Drain one tool stream into the chunks it announced and the one
/// output it settled with.
///
/// The contract under test is `Tool::stream`'s: any number of
/// `Progress` items, then exactly one `Result`, never the other way
/// round. Yielding the result after progress is what makes the
/// progress a preview rather than a second answer.
async fn drain<S>(mut stream: S) -> (Vec<String>, Vec<ToolOutput>)
where
    S: futures::Stream<Item = StreamOutput> + Unpin,
{
    use futures::StreamExt;
    let mut chunks = Vec::new();
    let mut results = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            StreamOutput::Progress(out) => {
                assert!(
                    results.is_empty(),
                    "a tool stream yielded progress after its result"
                );
                chunks.push(text_of(&out));
            }
            StreamOutput::Result(out) => results.push(out),
        }
    }
    (chunks, results)
}

/// A command whose output exceeds one pipe read streams in chunks:
/// the 8 KiB reader blocks are the natural slices, so a 18 KiB
/// output is announced more than once before the command exits.
#[tokio::test]
async fn a_long_command_streams_chunks_before_its_result() {
    let tool = ShellTool::new();
    let ctx = make_context();
    let stream = Box::pin(
        tool.stream(json!({"command": "seq -f 'line-%04g' 1 1500"}), &ctx),
    );
    let (chunks, results) = drain(stream).await;

    assert!(
        chunks.len() >= 2,
        "15 KiB of output must arrive in more than one chunk, got {}: {chunks:?}",
        chunks.len()
    );
    assert_eq!(results.len(), 1, "exactly one terminal output");
    let streamed = chunks.concat();
    assert_eq!(
        streamed.lines().count(),
        1500,
        "the chunks are the command's stdout, nothing dropped: {:?} .. {:?} ({} chunks, {} bytes)",
        streamed.lines().next(),
        streamed.lines().last(),
        chunks.len(),
        streamed.len()
    );
    assert_eq!(streamed.lines().next(), Some("line-0001"));
    assert!(
        streamed.trim_end().lines().last() == Some("line-1500"),
        "the last line arrives before the command settles: {streamed:?}"
    );
    let rendered = text_of(&results[0]);
    assert!(
        rendered.contains(&streamed),
        "the settled output carries the streamed stdout: {rendered}"
    );
    assert!(
        rendered.trim_end().ends_with("[exit code: 0]"),
        "the settled output still carries the exit marker: {rendered}"
    );
}

/// Stderr streams too: a command that reports progress on stderr is
/// not silent until it exits.
#[tokio::test]
async fn stderr_chunks_reach_the_stream() {
    let tool = ShellTool::new();
    let ctx = make_context();
    let stream = Box::pin(
        tool.stream(json!({"command": "seq -f 'line-%04g' 1 1500 >&2"}), &ctx),
    );
    let (chunks, results) = drain(stream).await;

    assert!(
        chunks.len() >= 2,
        "stderr streams in more than one chunk, got {}: {chunks:?}",
        chunks.len()
    );
    assert!(
        chunks.iter().any(|chunk| chunk.contains("line-1500")),
        "stderr is part of the streamed output: {chunks:?}"
    );
    assert_eq!(results.len(), 1);
    assert!(text_of(&results[0]).contains("line-1500"));
}

/// A silent command still settles exactly once — with no progress at
/// all. The dispatcher must never wait for a chunk that will not come.
#[tokio::test]
async fn a_silent_command_streams_no_chunks_and_one_result() {
    let tool = ShellTool::new();
    let ctx = make_context();
    let stream = Box::pin(tool.stream(json!({"command": "true"}), &ctx));
    let (chunks, results) = drain(stream).await;

    assert!(chunks.is_empty(), "no output, no chunks: {chunks:?}");
    assert_eq!(results.len(), 1);
    assert!(text_of(&results[0]).trim_end().ends_with("[exit code: 0]"));
}

/// Streaming does not change what a failing command means: a
/// non-zero exit is data, its marker is last, and the result is not
/// an error.
#[tokio::test]
async fn a_failing_command_streams_and_settles_with_its_marker() {
    let tool = ShellTool::new();
    let ctx = make_context();
    let stream = Box::pin(
        tool.stream(json!({"command": "echo boom >&2; exit 3"}), &ctx),
    );
    let (chunks, results) = drain(stream).await;

    assert!(
        chunks.iter().any(|chunk| chunk.contains("boom")),
        "the failure's output streams as it happens: {chunks:?}"
    );
    assert_eq!(results.len(), 1);
    assert_ne!(results[0].is_error, Some(true));
    assert!(
        text_of(&results[0]).trim_end().ends_with("[exit code: 3]"),
        "the marker stays last: {}",
        text_of(&results[0])
    );
}

/// A client that owns one session's terminal.
struct FakeTerminals {
    /// The sessions this client claims.
    claimed: Vec<String>,
    /// The commands that reached the client, in order.
    ran: std::sync::Mutex<Vec<(String, String)>>,
}

impl FakeTerminals {
    fn new(claimed: &[&str]) -> Self {
        Self {
            claimed: claimed.iter().map(|s| s.to_string()).collect(),
            ran: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl TerminalOwner for FakeTerminals {
    fn owns(&self, session_id: &str) -> bool {
        self.claimed.iter().any(|claimed| claimed == session_id)
    }

    async fn run(
        &self,
        session_id: &str,
        command: &str,
        cwd: &std::path::Path,
        _timeout: std::time::Duration,
    ) -> Result<TerminalRun, TerminalError> {
        self.ran
            .lock()
            .expect("the fake's own lock")
            .push((session_id.to_string(), command.to_string()));
        Ok(TerminalRun {
            output: format!("the client's pane ran it in {}\n", cwd.display()),
            exit_code: Some(3),
            timed_out: false,
        })
    }
}

#[tokio::test]
async fn a_client_owned_session_runs_its_command_in_the_client_terminal() {
    let terminals = Arc::new(FakeTerminals::new(&["s1"]));
    let tool = ShellTool::new().with_terminals(terminals.clone());

    let out = tool
        .call(json!({ "command": "echo hi" }), &make_context())
        .await;

    let text = text_of(&out);
    assert!(
        text.contains("the client's pane ran it in /tmp"),
        "the client's own output reaches the model: {text}"
    );
    assert!(
        text.contains("[exit code: 3]"),
        "the client's exit status is the run's status: {text}"
    );
    let ran = terminals.ran.lock().expect("the fake's own lock");
    assert_eq!(
        ran.as_slice(),
        [("s1".to_string(), "echo hi".to_string())],
        "the command reached the client, not this machine"
    );
}

#[tokio::test]
async fn a_session_the_client_does_not_own_runs_on_this_machine() {
    let terminals = Arc::new(FakeTerminals::new(&["other"]));
    let tool = ShellTool::new().with_terminals(terminals.clone());

    let out = tool
        .call(json!({ "command": "echo local" }), &make_context())
        .await;

    let text = text_of(&out);
    assert!(
        text.contains("local"),
        "the local path ran the command: {text}"
    );
    assert!(
        !text.contains("the client's pane"),
        "the local path did not ask the client: {text}"
    );
    assert!(
        terminals
            .ran
            .lock()
            .expect("the fake's own lock")
            .is_empty(),
        "the client was never asked"
    );
}

#[test]
fn the_description_names_the_client_terminal_path() {
    let description = ShellTool::new().description().to_string();
    assert!(
        description.contains("client's terminal"),
        "the model is told where a command can run: {description}"
    );
    assert!(
        description.contains("execution policy"),
        "the policy still names the local path: {description}"
    );
}
