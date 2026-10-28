//! Self-contained `shell` tool plugin. It depends only on the
//! `elysper-tool` paradigm crate for the `Tool` trait, its context
//! and output types; process confinement lives in this crate's own
//! [`sandbox`] module, so a consumer that never registers `shell`
//! carries none of it.
//!
//! `shell` built-in tool — execute a shell command.
//!
//! Safety contract (per `mvp-shell-safety` spec):
//! - Default deny patterns block obviously destructive commands.
//! - The check can be disabled via [`ShellSafetyConfig::disabled`].
//!
//! Execution contract (dsh / deepseek-harness aligned):
//! - The command runs in `sh -c` with [`Context::workspace_root`]
//!   as its working directory, in its **own process group**, so a
//!   timeout can take down the whole spawned tree.
//! - `timeout_secs` defaults to 120 s and is clamped to
//!   `[1, 600]` — the model cannot ask for an unbounded run.
//! - On timeout the tree gets `SIGTERM`, a 3 s grace, then
//!   `SIGKILL` (no orphaned grandchildren).
//! - Environment is hardened against interactive/paged output
//!   (`NO_COLOR`, `TERM=dumb`, `PAGER=cat`, `GIT_PAGER=cat`).
//! - A **non-zero exit is a normal result**, not an error: the
//!   model needs failing output as data (failing tests, probe
//!   commands). The exit status is rendered as a parseable
//!   trailing marker, always LAST:
//!   `[exit code: N]` / `[timed out after Ns]` /
//!   `[killed by signal: N]`.
//!
//! Sandbox contract (R31, dsh parity) — owned by this crate:
//! - The tool holds its own [`ExecutionPolicy`] and, for a
//!   confined policy, the [`SandboxBackend`] that enforces it
//!   ([`ShellTool::with_policy`] / [`ShellTool::with_sandbox`]),
//!   so the description it shows the model and the argv a call
//!   spawns always come from one value.
//! - [`resolve_confined_command`] turns that pair into the argv:
//!   under [`ExecutionPolicy::DangerFullAccess`] it is `sh -c`
//!   exactly as before, under a confined policy the backend's
//!   wrap.
//! - A confined policy with no usable backend is a **typed
//!   refusal** (`SANDBOX_UNAVAILABLE`) — the tool never degrades
//!   into an unconfined run.
//! - A file effect the sandbox refused is rendered as the
//!   `[sandbox: file access denied under <mode> mode]` marker
//!   (still followed by the exit marker), so the model can tell a
//!   policy boundary from a command that merely failed.

// `tokio::process::Command::process_group` and `std::process::Stdio`
// are the unix-only spawn knobs this crate leans on (process-group
// kill escalation + piped stdout/stderr). Gating the import keeps
// the windows-gnu build warning-clean.
#[cfg(unix)]
use std::process::Stdio;
// `Mutex` is only used by `spawn_reader` (POSIX-only pipe drain);
// `Arc` is shared with the unix / non-unix `execute` stubs so it
// stays unconditional.
#[cfg(unix)]
use std::sync::Mutex;
use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use schemars_derive::JsonSchema;
use serde::Deserialize;
// POSIX-only: `signal_of` reads `ExitStatus::signal()`, which
// lives on `std::os::unix`. The whole `ShellTool::call` path that
// uses it (`execute`) is gated to `#[cfg(unix)]` below; the import
// rides along.
#[cfg(unix)]
use elysper_tool::signal_of;
use elysper_tool::{
    Context,
    ExecutionMode,
    RenderKind,
    Risk,
    Tool,
    ToolAnnotations,
    ToolOutput,
    ToolOutputDefinition,
};
#[cfg(unix)]
use tokio::io::AsyncReadExt;
// `process::Command` is the unix-only handle we wrap with
// `process_group(0)` / `kill_on_drop(true)`. Non-unix builds get
// the typed-refusal `execute` stub above. `AsyncReadExt` rides
// along with `spawn_reader` / `finish_reader`, both also
// unix-only.
#[cfg(unix)]
use tokio::process::Command;

pub mod sandbox;

mod risk;
pub use sandbox::{
    BackendAvailability,
    BwrapBackend,
    ConfinedCommand,
    ExecutionPolicy,
    KNOWN_DENIAL_SIGNATURES,
    NoopBackend,
    SANDBOX_UNAVAILABLE,
    SandboxBackend,
    SandboxError,
    bwrap_profile_args,
    classify_denial,
    denial_dialect,
    denial_marker,
    effective_policy,
    policy_prompt_block,
    resolve_confined_command,
};

/// `shell`'s contract text, before the execution-policy paragraph
/// [`policy_prompt_block`] appends.
const BASE_DESCRIPTION: &str = "Execute a shell command and return its stdout and stderr \
     with a trailing exit-status marker. Non-zero exits are \
     normal results — inspect the `[exit code: N]` marker.";

/// Default deny-pattern list. Substring match (case-sensitive).
pub const DEFAULT_DENY_PATTERNS: &[&str] = &[
    "rm -rf",
    "mkfs",
    "dd if=",
    "chmod -R 777",
    "chmod -R 7777",
    "sudo ",
    "curl ",
    "wget ",
    ">/dev/sd",
    ":(){:|:&};:",
];

/// Default execution timeout (dsh parity: 120 s).
pub const DEFAULT_TIMEOUT_SECS: u64 = 120;

/// Hard upper bound on a model-requested timeout (dsh parity:
/// 600 s). Larger values are clamped, never rejected — the model
/// should not be able to wedge a session for hours.
pub const MAX_TIMEOUT_SECS: u64 = 600;

/// Grace period between `SIGTERM` and `SIGKILL` when a command
/// overruns its timeout (dsh parity: 3 s). POSIX-only: the
/// grace window is between `kill(-pgid, SIGTERM)` and
/// `kill(-pgid, SIGKILL)`, both unix primitives; on non-unix the
/// shell tool returns a typed refusal before this constant is
/// ever consulted.
#[cfg(unix)]
const KILL_GRACE_SECS: u64 = 3;

#[derive(Debug, Clone, Default)]
pub struct ShellSafetyConfig {
    pub disabled: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
struct ShellArgs {
    #[schemars(
        description = "The shell command to execute (passed to `sh -c`)."
    )]
    command: String,
    #[serde(default)]
    #[schemars(
        range(min = 1),
        extend("default" = 120),
        description = "Maximum execution time in seconds. Default: 120, \
                       capped at 600."
    )]
    timeout_secs: Option<u64>,
}

#[derive(Debug)]
pub struct ShellTool {
    config: ShellSafetyConfig,
    /// File-effect boundary every call runs under. The description
    /// shown to the model is derived from this same value, so the
    /// advertised policy and the enforced one cannot disagree.
    policy: ExecutionPolicy,
    /// Backend that enforces [`ShellTool::policy`] when it is not
    /// `DangerFullAccess`. `None` with a confined policy is a
    /// fail-closed refusal at call time, never a silent unconfined
    /// run.
    sandbox: Option<Arc<dyn SandboxBackend>>,
    description: String,
}

impl ShellTool {
    /// Shell tool with the default safety config, running under
    /// the default (unconfined) execution policy.
    pub fn new() -> Self {
        Self::with_config(ShellSafetyConfig::default())
    }

    /// Shell tool with `config` under the default policy.
    pub fn with_config(config: ShellSafetyConfig) -> Self {
        Self::with_config_and_policy(config, ExecutionPolicy::default())
    }

    /// Shell tool running under `policy`.
    ///
    /// The policy is the tool's own configuration: the description
    /// the model is shown and the argv a call spawns both come
    /// from it. A confined policy needs
    /// [`ShellTool::with_sandbox`]; without a backend the call is
    /// refused (`SANDBOX_UNAVAILABLE`) rather than run unconfined.
    pub fn with_policy(policy: ExecutionPolicy) -> Self {
        Self::with_config_and_policy(ShellSafetyConfig::default(), policy)
    }

    /// Shell tool with both a safety config and a policy.
    pub fn with_config_and_policy(
        config: ShellSafetyConfig,
        policy: ExecutionPolicy,
    ) -> Self {
        let description =
            format!("{BASE_DESCRIPTION}\n\n{}", policy_prompt_block(policy));
        Self {
            config,
            policy,
            sandbox: None,
            description,
        }
    }

    /// Attach the backend that enforces this tool's policy.
    ///
    /// The backend is consulted only under a confined policy; under
    /// the default (`danger-full-access`) the run is unconfined by
    /// construction and no backend is needed.
    pub fn with_sandbox(mut self, sandbox: Arc<dyn SandboxBackend>) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    fn is_denied(&self, command: &str) -> bool {
        if self.config.disabled {
            return false;
        }
        DEFAULT_DENY_PATTERNS
            .iter()
            .any(|pattern| command.contains(pattern))
    }
}

impl Default for ShellTool {
    fn default() -> Self {
        Self::new()
    }
}

// Programs that only read their inputs. A command is low risk only
// when every segment it contains is on this list (or matches a
// read-only special case below); one unknown segment makes the whole
// command unknown, and one high-risk segment makes it high.
const READ_ONLY_COMMANDS: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "pwd",
    "echo",
    "printf",
    "grep",
    "rg",
    "wc",
    "sort",
    "uniq",
    "cut",
    "tr",
    "stat",
    "file",
    "du",
    "df",
    "date",
    "whoami",
    "id",
    "uname",
    "hostname",
    "which",
    "printenv",
    "true",
    "false",
    "test",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "tree",
    "ps",
    "diff",
    "cmp",
    "jq",
    "yq",
    "column",
    "less",
    "more",
    "nl",
    "od",
    "hexdump",
    "sha256sum",
    "md5sum",
    "base64",
    "sleep",
    "history",
    "alias",
    "type",
    "locate",
];

// Programs whose ordinary invocation writes, deletes, escalates or
// reaches the network. `find`, `sed` and `sort` are argument-sensitive
// and classified separately.
const HIGH_RISK_COMMANDS: &[&str] = &[
    "rm",
    "rmdir",
    "mv",
    "cp",
    "touch",
    "mkdir",
    "ln",
    "chmod",
    "chown",
    "chgrp",
    "install",
    "dd",
    "truncate",
    "tee",
    "shred",
    "kill",
    "pkill",
    "killall",
    "reboot",
    "shutdown",
    "halt",
    "poweroff",
    "mount",
    "umount",
    "sudo",
    "su",
    "doas",
    "passwd",
    "useradd",
    "usermod",
    "userdel",
    "groupadd",
    "crontab",
    "systemctl",
    "service",
    "pip",
    "pip3",
    "npm",
    "pnpm",
    "yarn",
    "gem",
    "apt",
    "apt-get",
    "yum",
    "dnf",
    "apk",
    "brew",
    "docker",
    "podman",
    "kubectl",
    "helm",
    "terraform",
    "curl",
    "wget",
    "nc",
    "ncat",
    "netcat",
    "ssh",
    "scp",
    "sftp",
    "rsync",
    "ftp",
    "telnet",
    "ping",
    "sh",
    "bash",
    "zsh",
    "fish",
    "dash",
    "ksh",
    "python",
    "python3",
    "perl",
    "ruby",
    "node",
    "deno",
    "bun",
    "php",
    "lua",
    "xargs",
    "eval",
    "awk",
    "gawk",
    "mawk",
    "nawk",
    "source",
];

fn classify_segment_risk(segment: &str) -> Risk {
    let words: Vec<&str> = segment.split_whitespace().collect();
    let Some(idx) = command_index(&words) else {
        return Risk::Unknown;
    };
    let program = words[idx];
    let name = program.rsplit('/').next().unwrap_or(program);
    let args = &words[idx + 1..];
    match name {
        "git" => classify_git_risk(args),
        "find" => {
            if args.iter().any(|arg| {
                matches!(
                    *arg,
                    "-delete"
                        | "-exec"
                        | "-execdir"
                        | "-ok"
                        | "-okdir"
                        | "-fprintf"
                        | "-fprint"
                        | "-fprint0"
                        | "-fls"
                )
            }) {
                Risk::High
            } else {
                Risk::Low
            }
        }
        "sed" => {
            if args.iter().any(|arg| is_sed_in_place(arg)) {
                Risk::High
            } else {
                Risk::Low
            }
        }
        "sort" => {
            if args
                .iter()
                .any(|arg| *arg == "-o" || arg.starts_with("--output"))
            {
                Risk::High
            } else {
                Risk::Low
            }
        }
        _ if name.starts_with("mkfs") => Risk::High,
        _ if HIGH_RISK_COMMANDS.contains(&name) => Risk::High,
        _ if READ_ONLY_COMMANDS.contains(&name) => Risk::Low,
        _ => Risk::Unknown,
    }
}

// The index of the program a segment runs: environment assignments,
// reserved words and transparent wrappers are skipped. A wrapper's
// own options (and its leading operands, e.g. `timeout`'s duration)
// are consumed so the index lands on the command it wraps.
fn command_index(words: &[&str]) -> Option<usize> {
    let mut idx = 0;
    loop {
        while idx < words.len() {
            if is_env_assignment(words[idx]) {
                idx += 1;
                continue;
            }
            // These words separate one command from the next; they
            // are never the program a segment runs.
            if matches!(
                words[idx],
                "then" | "do" | "else" | "elif" | "fi" | "done" | "{" | "}"
            ) {
                idx += 1;
                continue;
            }
            break;
        }
        if idx >= words.len() {
            return None;
        }
        match words[idx] {
            "command" | "builtin" | "exec" => idx += 1,
            "env" => {
                idx += 1;
                idx = skip_options(
                    words,
                    idx,
                    &["-u", "--unset", "-C", "--chdir"],
                );
            }
            // Wrappers run their arguments as the real command; their
            // own options and operands are not the program.
            "timeout" => {
                idx = skip_options(
                    words,
                    idx + 1,
                    &["-s", "--signal", "-k", "--kill-after"],
                );
                // DURATION is the wrapper's one operand.
                idx += usize::from(idx < words.len());
            }
            "nice" => {
                idx = skip_options(words, idx + 1, &["-n", "--adjustment"]);
            }
            "nohup" => idx += 1,
            "time" => {
                idx = skip_options(
                    words,
                    idx + 1,
                    &["-o", "--output", "-f", "--format"],
                );
            }
            "stdbuf" => {
                idx = skip_options(
                    words,
                    idx + 1,
                    &["-i", "--input", "-o", "--output", "-e", "--error"],
                );
            }
            "ionice" => {
                idx = skip_options(
                    words,
                    idx + 1,
                    &[
                        "-c",
                        "--class",
                        "-n",
                        "--classdata",
                        "-p",
                        "--pid",
                        "-P",
                        "--pgid",
                        "-u",
                        "--uid",
                    ],
                );
            }
            "chroot" => {
                idx = skip_options(words, idx + 1, &["--userspec", "--groups"]);
                // NEWROOT is the wrapper's one operand.
                idx += usize::from(idx < words.len());
            }
            _ => return Some(idx),
        }
    }
}

// Skip a wrapper's option words starting at `idx`, consuming a value
// for each option named in `takes_value` (and for `--name=value`
// spellings, which carry their own value).
fn skip_options(words: &[&str], mut idx: usize, takes_value: &[&str]) -> usize {
    while idx < words.len() && words[idx].starts_with('-') && words[idx] != "-"
    {
        let word = words[idx];
        let option = word.split_once('=').map(|(name, _)| name).unwrap_or(word);
        let consumes_next = word == option && takes_value.contains(&option);
        idx += if consumes_next { 2 } else { 1 };
    }
    idx
}

// Words and characters that mark a compound shell construct this
// analyser does not model. A segment that resolves to neither a known
// program nor a known wrapper, and still carries one, is not a
// read-only command.
fn has_shell_control_syntax(segment: &str) -> bool {
    segment.split_whitespace().any(|word| {
        matches!(
            word,
            "if" | "then"
                | "else"
                | "elif"
                | "fi"
                | "for"
                | "while"
                | "until"
                | "do"
                | "done"
                | "case"
                | "esac"
                | "select"
                | "in"
                | "function"
                | "!"
        )
    }) || segment
        .chars()
        .any(|ch| matches!(ch, '{' | '}' | '(' | ')'))
}

// `sed -Ei`, `sed -ni`, `sed -i.bak` and `--in-place` all write.
fn is_sed_in_place(arg: &str) -> bool {
    arg.starts_with("--in-place")
        || (arg.starts_with('-')
            && !arg.starts_with("--")
            && arg[1..].contains('i'))
}

fn is_env_assignment(word: &str) -> bool {
    let Some((key, _)) = word.split_once('=') else {
        return false;
    };
    !key.is_empty()
        && key
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn classify_git_risk(args: &[&str]) -> Risk {
    // `--output=<file>` writes a file even under a read-only
    // subcommand such as `log`.
    if args.iter().any(|arg| arg.starts_with("--output")) {
        return Risk::High;
    }
    // Skip the global options that precede the subcommand; `-C` and
    // `-c` consume a value.
    let mut idx = 0;
    while idx < args.len() && args[idx].starts_with('-') {
        let takes_value =
            matches!(args[idx], "-C" | "-c" | "--git-dir" | "--work-tree");
        idx += if takes_value { 2 } else { 1 };
    }
    let Some(subcommand) = args.get(idx) else {
        return Risk::Unknown;
    };
    let rest = &args[idx + 1..];
    match *subcommand {
        "status" | "log" | "diff" | "show" | "rev-parse" | "describe"
        | "ls-files" | "ls-tree" | "grep" | "blame" | "cat-file"
        | "shortlog" | "rev-list" | "whatchanged" | "reflog" | "help"
        | "version" => Risk::Low,
        "commit" | "push" | "pull" | "fetch" | "checkout" | "switch"
        | "reset" | "clean" | "rm" | "mv" | "add" | "merge" | "rebase"
        | "cherry-pick" | "revert" | "apply" | "am" | "init" | "clone"
        | "submodule" | "worktree" | "bisect" | "gc" | "prune"
        | "filter-branch" => Risk::High,
        "branch" | "tag" => {
            let mutating = rest.iter().any(|arg| {
                matches!(
                    *arg,
                    "-d" | "-D"
                        | "-m"
                        | "-M"
                        | "-c"
                        | "-C"
                        | "-f"
                        | "--delete"
                        | "--move"
                        | "--copy"
                        | "--force"
                        | "--edit-description"
                )
            });
            if mutating { Risk::High } else { Risk::Low }
        }
        "remote" => {
            let mutating = rest.iter().any(|arg| {
                matches!(
                    *arg,
                    "add"
                        | "remove"
                        | "rm"
                        | "rename"
                        | "set-url"
                        | "set-head"
                        | "prune"
                        | "update"
                )
            });
            if mutating { Risk::High } else { Risk::Low }
        }
        "config" => {
            let reads = rest.iter().any(|arg| {
                matches!(*arg, "--get" | "--get-all" | "--list" | "-l")
            });
            if reads || rest.is_empty() {
                Risk::Low
            } else {
                Risk::High
            }
        }
        "stash" => {
            if matches!(rest.first().copied(), Some("list" | "show")) {
                Risk::Low
            } else {
                Risk::High
            }
        }
        _ => Risk::Unknown,
    }
}

/// A completed shell run: collected streams plus the cause-of-death
/// classification. `timed_out` and `signal` are mutually exclusive
/// with a plain exit code (first-cause wins, dsh parity).
struct ShellRun {
    stdout: String,
    stderr: String,
    exit_code: Option<i32>,
    signal: Option<i32>,
    timed_out: bool,
    /// Set when the sandbox refused a file effect — the exit code
    /// stays authoritative, the marker only names the boundary.
    denied_by_policy: Option<ExecutionPolicy>,
}

impl ShellRun {
    /// Render the wire body: stdout, stderr, then the status
    /// marker — always LAST so `parse_exit_status`-style consumers
    /// can anchor on the tail.
    fn render(&self, timeout_secs: u64) -> String {
        let mut body = String::new();
        if !self.stdout.is_empty() {
            body.push_str("stdout:\n");
            body.push_str(&self.stdout);
        }
        if !self.stderr.is_empty() {
            if !body.is_empty() {
                body.push_str("\n\n");
            }
            body.push_str("stderr:\n");
            body.push_str(&self.stderr);
        }
        if body.is_empty() {
            body.push_str("(no output)");
        }
        body.push_str("\n\n");
        if let Some(policy) = self.denied_by_policy {
            // A refused file effect is a policy fact, not a command
            // failure: name the boundary, then let the exit marker
            // stay the tail.
            body.push_str(&denial_marker(policy));
            body.push('\n');
        }
        if self.timed_out {
            // The timeout marker wins even when the command
            // trapped SIGTERM and exited 0 — the run was cut
            // short, and the model must know.
            body.push_str(&format!("[timed out after {timeout_secs}s]"));
        } else if let Some(signal) = self.signal {
            body.push_str(&format!("[killed by signal: {signal}]"));
        } else {
            body.push_str(&format!(
                "[exit code: {}]",
                self.exit_code.unwrap_or(-1)
            ));
        }
        body
    }
}

/// Signal an entire process group (`-pgid`), falling back to the
/// direct child when the group is already gone. POSIX-only — Windows
/// has no process group kill primitive; the timeout escalation on
/// non-unix platforms uses `kill_on_drop(true)` and waits for the
/// child to exit instead.
#[cfg(unix)]
fn signal_group(pgid: i32, sig: i32) {
    if pgid > 0 && unsafe { libc::kill(-pgid, sig) } == -1 {
        // Group already reaped: best-effort direct signal.
        let _ = unsafe { libc::kill(pgid, sig) };
    }
}

/// Spawn a background task draining one pipe into a shared
/// buffer. Concurrent draining is mandatory: a full pipe buffer
/// would otherwise deadlock the child. POSIX-only — only the
/// unix `execute` path uses this (it `tokio::process::Command`'s
/// piped stdout/stderr); non-unix builds serve the typed-refusal
/// stub from `execute` instead.
#[cfg(unix)]
fn spawn_reader<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    mut pipe: R,
) -> (Arc<Mutex<Vec<u8>>>, tokio::task::JoinHandle<()>) {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&buffer);
    let handle = tokio::spawn(async move {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut buf) = sink.lock() {
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
    });
    (buffer, handle)
}

/// Bounded wait for the reader tasks so a surviving grandchild
/// holding the pipe cannot stall the tool result. POSIX-only —
/// only `spawn_reader` (unix) consumes this.
#[cfg(unix)]
async fn finish_reader(
    buffer: &Arc<Mutex<Vec<u8>>>,
    handle: tokio::task::JoinHandle<()>,
) -> String {
    let _ =
        tokio::time::timeout(Duration::from_secs(KILL_GRACE_SECS + 1), handle)
            .await;
    buffer
        .lock()
        .map(|buf| String::from_utf8_lossy(&buf).into_owned())
        .unwrap_or_default()
}

#[async_trait]
impl Tool for ShellTool {
    fn name(&self) -> &str {
        "shell"
    }

    fn description(&self) -> &str {
        &self.description
    }

    fn parameters(&self) -> serde_json::Value {
        // Schema is generated from `ShellArgs` via `schemars`,
        // so the type and the LLM-facing schema cannot drift —
        // including `additionalProperties: false` and the
        // `timeout_secs` default, all declared inline via
        // `#[schemars(extend(...))]`.
        serde_json::to_value(schemars::schema_for!(ShellArgs))
            .expect("ShellArgs schema is always serializable")
    }

    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    /// Shell risk is per call, not per tool: `ls` and `rm -rf` share
    /// this tool but not this risk. Read-only argv is low; a write,
    /// redirect, delete, privilege change or network reach is high;
    /// anything the classifier does not recognise is unknown, so the
    /// approval policy — not a guess — decides.
    fn risk(&self, input: &serde_json::Value) -> Risk {
        let command = input
            .get("command")
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        risk::classify_command_risk(command)
    }

    /// R124: MCP-native descriptor hints. `shell` may mutate the
    /// workspace (or the wider system under a permissive policy);
    /// the `destructive` hint is the inverse of the current
    /// `ExecutionPolicy::is_confined()` so a confined policy
    /// (e.g. `WorkspaceReadOnly`) reports the model-side
    /// restriction as part of the descriptor. No outbound network
    /// (HTTP fetch lives in `elysper-tool-web`).
    fn annotations(&self) -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(self.policy.is_confined()),
            destructive_hint: Some(!self.policy.is_confined()),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
        })
    }

    /// R17: shell output carries the exit-status marker
    /// (`[exit code: N]` / `[timed out after Ns]` /
    /// `[killed by signal: N]`) as its last line; the render kind
    /// tells a UI to give the block a terminal chrome.
    fn output_definition(&self) -> ToolOutputDefinition {
        ToolOutputDefinition::passthrough("shell")
            .with_kind(RenderKind::Shell)
            .with_title("Shell")
            .with_presentation(
                "exit_marker",
                serde_json::json!("[exit code: N]"),
            )
    }

    async fn call(
        &self,
        input: serde_json::Value,
        context: &Context,
    ) -> ToolOutput {
        let args: ShellArgs = match serde_json::from_value(input) {
            Ok(a) => a,
            Err(e) => {
                return ToolOutput::error(format!("Invalid arguments: {}", e));
            }
        };

        if self.is_denied(&args.command) {
            return ToolOutput::error(format!(
                "Command denied by safety policy (matches default deny pattern): {}",
                args.command
            ));
        }

        let timeout_secs = args
            .timeout_secs
            .unwrap_or(DEFAULT_TIMEOUT_SECS)
            .clamp(1, MAX_TIMEOUT_SECS);

        // Resolve the argv before running anything: a policy the
        // host cannot enforce refuses here, so the command never
        // starts unconfined.
        let confined = match resolve_confined_command(
            self.sandbox.as_deref(),
            self.policy,
            &context.workspace_root,
        ) {
            Ok(confined) => confined,
            Err(error) => return ToolOutput::error(error.to_string()),
        };
        let dialect = denial_dialect(self.sandbox.as_deref(), self.policy);

        let run = execute(
            &args.command,
            context,
            Duration::from_secs(timeout_secs),
            &confined,
        )
        .await;

        match run {
            Ok(mut run) => {
                // A refusal by the sandbox is a policy boundary,
                // not a broken command: keep the run's data and
                // add the marker the model is told to expect.
                if classify_denial(run.exit_code, &run.stderr, dialect) {
                    run.denied_by_policy = Some(self.policy);
                }
                ToolOutput::text(run.render(timeout_secs))
            }
            Err(message) => ToolOutput::error(message),
        }
    }
}

/// Spawn and supervise one command. `Ok` carries the collected
/// run (including timeouts — rendered as markers); `Err` is
/// reserved for infrastructure failures (spawn errors).
///
/// The argv comes from `confined` rather than being built here:
/// under the default policy that is `sh -c <command>` verbatim,
/// under a confined policy it is the backend's wrap with the same
/// payload at the tail.
///
/// POSIX-only. Uses `cmd.process_group(0)` (unix-only tokio API),
/// `libc::kill(-pgid, SIGTERM/SIGKILL)` for timeout escalation, and
/// `ExitStatus::signal()` (unix-only std extension) for the
/// `[killed by signal: N]` marker. On non-unix targets this entire
/// body is replaced by a typed refusal below — the `ShellTool`
/// struct / `Tool` impl / arg schema stay available so the
/// registry / model wiring compiles; only the actual execution
/// fails cleanly.
#[cfg(unix)]
async fn execute(
    command: &str,
    context: &Context,
    timeout: Duration,
    confined: &ConfinedCommand,
) -> Result<ShellRun, String> {
    let argv = confined.argv_for(command);
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    // Run inside the workspace (dsh session-cwd semantics); the
    // agent loop feeds `workspace_root` on every dispatch.
    if !context.workspace_root.as_os_str().is_empty() {
        cmd.current_dir(&context.workspace_root);
    }
    // Interactive/paged output is noise for a model: suppress
    // colour, prompts, and pagers.
    cmd.env("NO_COLOR", "1")
        .env("TERM", "dumb")
        .env("PAGER", "cat")
        .env("GIT_PAGER", "cat");
    // Own process group → the whole spawned tree is addressable
    // as `-pgid` when the timeout fires.
    cmd.process_group(0);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);

    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Failed to spawn shell: {e}"))?;
    let pgid = child.id().unwrap_or(0) as i32;

    let (stdout_buf, stdout_task) =
        spawn_reader(child.stdout.take().expect("piped stdout"));
    let (stderr_buf, stderr_task) =
        spawn_reader(child.stderr.take().expect("piped stderr"));

    // First cause wins: natural exit vs. deadline.
    let timed_out;
    let status = tokio::select! {
        biased;
        status = child.wait() => {
            timed_out = false;
            Some(status.map_err(|e| format!("wait failed: {e}"))?)
        }
        _ = tokio::time::sleep(timeout) => {
            timed_out = true;
            None
        }
    };

    if timed_out {
        // SIGTERM → grace → SIGKILL on the whole group. The
        // grace timer is NOT cleared when the leader exits: the
        // tree may outlive it.
        signal_group(pgid, libc::SIGTERM);
        let _ = tokio::time::timeout(
            Duration::from_secs(KILL_GRACE_SECS),
            child.wait(),
        )
        .await;
        signal_group(pgid, libc::SIGKILL);
        let _ = child.wait().await;
    }

    let stdout = finish_reader(&stdout_buf, stdout_task).await;
    let stderr = finish_reader(&stderr_buf, stderr_task).await;

    let (exit_code, signal) = match status {
        Some(status) => {
            let code = status.code();
            let signal = if timed_out { None } else { signal_of(&status) };
            (code, signal)
        }
        None => (None, None),
    };

    Ok(ShellRun {
        stdout,
        stderr,
        exit_code,
        signal,
        timed_out,
        denied_by_policy: None,
    })
}

/// Non-unix stub for `execute`. The `ShellTool` itself stays
/// registered (so the schema / harness wiring still type-checks),
/// but every call surfaces a typed refusal so a Windows consumer
/// gets a clear "not supported" message instead of a panic at the
/// first `process_group(0)` call. Native `cmd.exe` integration is a
/// separate project; this stub keeps the cross-build green.
#[cfg(not(unix))]
async fn execute(
    _command: &str,
    _context: &Context,
    _timeout: Duration,
    _confined: &ConfinedCommand,
) -> Result<ShellRun, String> {
    Err(
        "shell tool is not implemented on non-unix targets in this build \
         (process-group kill escalation and `signal_of` are POSIX-only)"
            .to_string(),
    )
}

#[cfg(test)]
mod tests;
