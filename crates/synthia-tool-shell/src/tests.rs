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
            synthia_provider::types::ContentPart::Text(t) => {
                Some(t.text.clone())
            }
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
/// proven in [`crate::sandbox`], so stubbing the runner keeps
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
