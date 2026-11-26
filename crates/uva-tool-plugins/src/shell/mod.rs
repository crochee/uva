//! Self-contained `shell` tool plugin. It depends only on the
//! `uva-tool` paradigm crate for the `Tool` trait, its context
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
use std::{pin::Pin, sync::Arc, time::Duration};

use async_trait::async_trait;
#[cfg(unix)]
use exec::{finish_reader, spawn_reader};
use schemars_derive::JsonSchema;
use serde::Deserialize;
// `process::Command` is the unix-only handle we wrap with
// `process_group(0)` / `kill_on_drop(true)`. Non-unix builds get
// the typed-refusal `execute` stub below.
#[cfg(unix)]
use tokio::process::Command;
// POSIX-only: `signal_of` reads `ExitStatus::signal()`, which
// lives on `std::os::unix`. The whole `ShellTool::call` path that
// uses it (`execute`) is gated to `#[cfg(unix)]` below; the import
// rides along.
#[cfg(unix)]
use uva_tool::signal_of;
use uva_tool::{
    Context,
    ExecutionMode,
    RenderKind,
    Risk,
    StreamOutput,
    Tool,
    ToolAnnotations,
    ToolOutput,
    ToolOutputDefinition,
};

pub mod sandbox;

/// The shell crate's native kernel plugins: the `sandbox` service and
/// the `shell` tool brick.
pub mod plugin;

/// The pipe readers: concurrent draining plus the optional progress
/// stream. POSIX-only, like the execution path that uses it.
#[cfg(unix)]
mod exec;
mod risk;
/// The `stream` half of the `Tool` impl: `Progress` per pipe chunk,
/// one terminal `Result`.
mod stream;
/// The terminal seam: who runs a session's commands when the client
/// owns the terminal.
pub mod terminal;
pub use plugin::{
    KEY_SANDBOX,
    SandboxBackendKind,
    SandboxPlugin,
    SandboxPluginConfig,
    SandboxService,
    ShellToolPlugin,
};
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
pub use terminal::{LocalTerminals, TerminalError, TerminalOwner, TerminalRun};

/// `shell`'s contract text, before the execution-policy paragraph
/// [`policy_prompt_block`] appends.
const BASE_DESCRIPTION: &str = "Execute a shell command and return its stdout and stderr \
     with a trailing exit-status marker. Non-zero exits are \
     normal results — inspect the `[exit code: N]` marker.";

/// The block that says where a command runs when the session's client
/// owns the terminal.
///
/// The tool's description is static — one text for every session — so
/// it has to name both paths truthfully: the policy above holds for
/// the sessions that run on this machine, and the client's own rules
/// hold for the sessions whose terminal the client owns. A model that
/// believes the sandbox is in force while the command runs in the
/// user's terminal would read a policy denial that never comes.
const TERMINAL_OWNER_PROMPT: &str = "Where the command runs: on this deployment's own \
     machine, under the execution policy above — unless the session's client owns the \
     terminal, in which case the command runs in the client's terminal (the pane the \
     user watches) and the client's own rules apply there instead of the policy above. \
     A command refused by the client's terminal comes back as that client's refusal.";

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
    /// Who runs a session's commands when the client owns the
    /// terminal. The default owns nothing: every session runs on this
    /// machine.
    terminals: Arc<dyn TerminalOwner>,
    description: String,
}

impl std::fmt::Debug for ShellTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The terminal owner is an opaque collaborator — the client
        // that owns it is not the tool's business to print.
        f.debug_struct("ShellTool")
            .field("config", &self.config)
            .field("policy", &self.policy)
            .field("sandbox", &self.sandbox.is_some())
            .finish_non_exhaustive()
    }
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
        let description = format!(
            "{BASE_DESCRIPTION}\n\n{}\n\n{}",
            policy_prompt_block(policy),
            TERMINAL_OWNER_PROMPT,
        );
        Self {
            config,
            policy,
            sandbox: None,
            terminals: Arc::new(LocalTerminals),
            description,
        }
    }

    /// Attach the owner that runs a client-owned session's commands.
    ///
    /// The owner answers per session: a session it claims runs in the
    /// client's terminal, every other session keeps running on this
    /// machine under this tool's own policy.
    pub fn with_terminals(mut self, terminals: Arc<dyn TerminalOwner>) -> Self {
        self.terminals = terminals;
        self
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

/// Where one reader task announces the chunk it just read.
///
/// [`ShellTool::stream`] installs one; [`Tool::call`] passes none.
/// The send is never awaited: a reader that blocked on a slow
/// consumer would stop draining its pipe, and a full pipe deadlocks
/// the child. A dropped receiver is legal too — the stream ended and
/// nobody is listening any more.
type ProgressSink = futures::channel::mpsc::UnboundedSender<ToolOutput>;

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
    /// (HTTP fetch lives in `uva-tool-web`).
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
        self.run(input, context, None).await
    }

    /// Stream one command: each pipe chunk is a `Progress` item, the
    /// rendered run is the single `Result`.
    ///
    /// The state machine lives in the private `stream` module; `call`
    /// runs the same command without a progress sink.
    fn stream<'a>(
        &'a self,
        input: serde_json::Value,
        context: &'a Context,
    ) -> Pin<Box<dyn futures::Stream<Item = StreamOutput> + Send + 'a>> {
        stream::run(self, input, context)
    }
}

impl ShellTool {
    /// Run one command in the session's client-owned terminal.
    ///
    /// The run is the client's: its pane holds the output, its rules
    /// hold the command. The tool renders the answer in the same
    /// shape the local path renders — one body, the status marker
    /// last — so the model's view of a run does not depend on where
    /// it ran.
    async fn run_in_client_terminal(
        &self,
        command: &str,
        context: &Context,
        timeout: Duration,
    ) -> ToolOutput {
        let run = self
            .terminals
            .run(
                &context.session_id,
                command,
                &context.workspace_root,
                timeout,
            )
            .await;
        match run {
            Ok(run) => {
                let run = ShellRun {
                    stdout: run.output,
                    stderr: String::new(),
                    exit_code: run.exit_code,
                    signal: None,
                    timed_out: run.timed_out,
                    denied_by_policy: None,
                };
                ToolOutput::text(run.render(timeout.as_secs()))
            }
            Err(error) => ToolOutput::error(error.to_string()),
        }
    }

    /// Execute one call, announcing pipe chunks to `progress` when it
    /// is installed.
    async fn run(
        &self,
        input: serde_json::Value,
        context: &Context,
        progress: Option<ProgressSink>,
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

        // A session whose client owns the terminal runs there: the
        // client's pane, the client's rules. The deployment's own
        // sandbox does not reach that far — the description says so
        // for every session.
        if self.terminals.owns(&context.session_id) {
            return self
                .run_in_client_terminal(
                    &args.command,
                    context,
                    Duration::from_secs(timeout_secs),
                )
                .await;
        }

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
            progress,
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
    progress: Option<ProgressSink>,
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

    let (stdout_buf, stdout_task) = spawn_reader(
        child.stdout.take().expect("piped stdout"),
        progress.clone(),
    );
    let (stderr_buf, stderr_task) =
        spawn_reader(child.stderr.take().expect("piped stderr"), progress);

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
    _progress: Option<ProgressSink>,
) -> Result<ShellRun, String> {
    Err(
        "shell tool is not implemented on non-unix targets in this build \
         (process-group kill escalation and `signal_of` are POSIX-only)"
            .to_string(),
    )
}

#[cfg(test)]
mod tests;
