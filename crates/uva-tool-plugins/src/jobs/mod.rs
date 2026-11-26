//! The `job` tool: a command that keeps running after the call ends.
//!
//! The `shell` tool runs a command in the foreground and gives up at
//! its deadline (600 s, the cap). A build, a test suite or a long
//! migration does not fit in that window — so the model starts it in
//! the **background** and comes back to it later.
//!
//! The reference harnesses agree on the shape:
//!
//! - **DeepSeek Harness**: `job_output`, `job_list` and `job_kill`
//!   over a job controller, with `job_output` waiting for the job
//!   (bounded) before it answers
//!   (`packages/jobs/tool-jobs/src/index.ts:1-60`).
//! - **omp**: background jobs plus a `wait` tool; the job's output is
//!   collected when the model asks for it
//!   (`packages/coding-agent/src/tools/wait.ts`).
//!
//! This module is the job controller and the tool that reaches it:
//! one deployment's jobs live in one registry, so a job started in
//! one call is still there in the next one. The registry is owned by
//! the tool — the tool is a deployment-level singleton, so the jobs
//! live exactly as long as the deployment does.
//!
//! What is **not** here yet: waking the owner when a job finishes.
//! dsh delivers a completion into the owner's next step (or opens a
//! turn on an idle one); this tool asks instead — the model polls
//! with `action = "output"`. The delivery is a harness-level change
//! (the turn loop must accept an out-of-band event), not a tool
//! change, and it is recorded as the open half in the ledger.
//!
//! The command runs under the same execution policy the `shell` tool
//! enforces: a confined policy without a backend refuses here too —
//! a background job must not be the way around the sandbox.

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use schemars_derive::JsonSchema;
use serde::Deserialize;
use uva_core::{IdGen, SharedIdGen};
use uva_tool::{
    Context,
    ExecutionMode,
    Tool,
    ToolAnnotations,
    ToolOutput,
    ToolOutputDefinition,
};

use crate::shell::sandbox::{
    ExecutionPolicy,
    SandboxBackend,
    resolve_confined_command,
};

/// The `job` tool brick as a native kernel plugin.
pub mod plugin;

pub use plugin::JobToolPlugin;

#[cfg(test)]
mod tests;

/// What a job is doing right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JobState {
    /// The command is still running.
    Running,
    /// The command finished on its own.
    Exited {
        /// The exit code, when the process reported one.
        code: Option<i32>,
        /// The signal that killed it, when one did.
        signal: Option<i32>,
    },
    /// The job was killed through the tool.
    Killed,
    /// The command could not be started.
    Failed(String),
}

impl JobState {
    /// The one-word state a listing shows.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Exited { .. } => "exited",
            Self::Killed => "killed",
            Self::Failed(_) => "failed",
        }
    }

    /// Whether the job will not produce anything more.
    pub fn is_finished(&self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// One background command.
pub struct Job {
    /// The job's id: what the model passes back.
    id: String,
    /// The session that started it.
    session_id: String,
    /// The command line, as the model wrote it.
    command: String,
    /// The job's state.
    state: Arc<Mutex<JobState>>,
    /// The command's stdout, readable while it runs.
    stdout: crate::process::Pipe,
    /// The command's stderr, readable while it runs.
    stderr: crate::process::Pipe,
    /// The process group, for the kill.
    pgid: i32,
}

impl std::fmt::Debug for Job {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Job")
            .field("id", &self.id)
            .field("command", &self.command)
            .field("state", &self.state.lock().map(|s| s.label()).ok())
            .finish_non_exhaustive()
    }
}

impl Job {
    /// The job's id.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The session that started the job.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// The command the job runs.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// The job's state.
    pub fn state(&self) -> JobState {
        self.state
            .lock()
            .map(|state| state.clone())
            .unwrap_or_else(|_| {
                JobState::Failed("the job's state is poisoned".to_string())
            })
    }

    /// The command's stdout so far.
    pub fn stdout(&self) -> String {
        crate::process::pipe_text(&self.stdout)
    }

    /// The command's stderr so far.
    pub fn stderr(&self) -> String {
        crate::process::pipe_text(&self.stderr)
    }

    /// Whether the job has finished.
    pub fn is_finished(&self) -> bool {
        self.state().is_finished()
    }
}

/// The deployment's background jobs.
pub struct JobRegistry {
    jobs: parking_lot::RwLock<HashMap<String, Arc<Job>>>,
    ids: SharedIdGen,
}

impl Default for JobRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for JobRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobRegistry")
            .field("jobs", &self.jobs.read().len())
            .finish_non_exhaustive()
    }
}

impl JobRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            jobs: parking_lot::RwLock::new(HashMap::new()),
            ids: SharedIdGen::ulid(),
        }
    }

    /// The job with that id.
    #[must_use]
    pub fn get(&self, id: &str) -> Option<Arc<Job>> {
        self.jobs.read().get(id).cloned()
    }

    /// Every job, oldest first.
    #[must_use]
    pub fn list(&self) -> Vec<Arc<Job>> {
        self.jobs.read().values().cloned().collect()
    }

    /// A fresh job id.
    fn next_id(&self) -> String {
        self.ids.next_id()
    }
}

/// What the tool is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum JobAction {
    /// Start a command in the background.
    Start,
    /// The job's output so far (optionally waiting for it).
    Output,
    /// Every job this deployment knows.
    List,
    /// Kill a running job.
    Kill,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
struct JobRequest {
    #[schemars(
        description = "What to do: `start` runs a command in the background, `output` reads a job's output (and can wait for it), `list` shows every job, `kill` stops a running job."
    )]
    action: JobAction,
    #[schemars(
        description = "The command to run. Required for `action = \"start\"`."
    )]
    #[serde(default)]
    command: Option<String>,
    #[schemars(
        description = "The job's id. Required for `action = \"output\"` and `action = \"kill\"`."
    )]
    #[serde(default)]
    job_id: Option<String>,
    #[schemars(
        extend("default" = 0),
        description = "How long `action = \"output\"` waits for the job to finish, in seconds (0 = do not wait). Capped at 600."
    )]
    #[serde(default)]
    wait_secs: Option<u64>,
}

/// The longest an `output` call waits for a job.
const MAX_WAIT_SECS: u64 = 600;

/// `job` — run a command in the background and come back to it.
pub struct JobTool {
    /// The deployment's jobs.
    registry: Arc<JobRegistry>,
    /// The policy every job runs under.
    policy: ExecutionPolicy,
    /// The backend that enforces the policy, when it is confined.
    sandbox: Option<Arc<dyn SandboxBackend>>,
}

impl Default for JobTool {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for JobTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("JobTool")
            .field("policy", &self.policy)
            .field("sandbox", &self.sandbox.is_some())
            .finish_non_exhaustive()
    }
}

impl JobTool {
    /// The tool over a fresh registry, under the default policy.
    #[must_use]
    pub fn new() -> Self {
        Self::with_registry(Arc::new(JobRegistry::new()))
    }

    /// The tool over `registry`: the jobs live there, so a job started
    /// in one call is still there in the next.
    #[must_use]
    pub fn with_registry(registry: Arc<JobRegistry>) -> Self {
        Self {
            registry,
            policy: ExecutionPolicy::default(),
            sandbox: None,
        }
    }

    /// The tool running under `policy`.
    #[must_use]
    pub fn with_policy(mut self, policy: ExecutionPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Attach the backend that enforces this tool's policy.
    #[must_use]
    pub fn with_sandbox(mut self, sandbox: Arc<dyn SandboxBackend>) -> Self {
        self.sandbox = Some(sandbox);
        self
    }

    /// The deployment's jobs.
    #[must_use]
    pub fn registry(&self) -> &Arc<JobRegistry> {
        &self.registry
    }
}

#[async_trait]
impl Tool for JobTool {
    fn name(&self) -> &str {
        "job"
    }

    fn description(&self) -> &str {
        "Run a command in the background and come back to it later. \
         `action = \"start\"` runs `command` and answers with a job id; \
         `action = \"output\"` reads that job's output and can wait \
         (`wait_secs`) for it to finish; `action = \"list\"` shows every \
         job; `action = \"kill\"` stops a running job. Use this for work \
         that outlasts a single command — a build, a test suite, a \
         migration — and keep working while it runs."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(JobRequest))
            .expect("JobRequest schema is always serializable")
    }

    fn output_definition(&self) -> ToolOutputDefinition {
        ToolOutputDefinition::passthrough("job")
            .with_kind(uva_tool::output::RenderKind::Shell)
            .with_title("Job")
    }

    /// A job starts a process: the loop must not run two of these at
    /// once, or two jobs would race for the same registry.
    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    /// MCP-native descriptor hints for permission gates.
    ///
    /// A job runs a command: it mutates whatever the command touches,
    /// it is not idempotent (a second `start` is a second process),
    /// and it reaches no network of its own.
    fn annotations(&self) -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(true),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
        })
    }

    async fn call(
        &self,
        input: serde_json::Value,
        context: &Context,
    ) -> ToolOutput {
        let request: JobRequest = match serde_json::from_value(input) {
            Ok(r) => r,
            Err(e) => {
                return ToolOutput::error(format!("Invalid arguments: {e}"));
            }
        };
        match request.action {
            JobAction::Start => self.start(&request, context).await,
            JobAction::Output => self.output(&request).await,
            JobAction::List => self.list(),
            JobAction::Kill => self.kill(&request),
        }
    }
}

impl JobTool {
    /// Start a command in the background.
    async fn start(
        &self,
        request: &JobRequest,
        context: &Context,
    ) -> ToolOutput {
        let command = match request.command.as_deref() {
            Some(command) if !command.trim().is_empty() => command,
            _ => {
                return ToolOutput::error(
                    "`command` is required for `action = \"start\"`.",
                );
            }
        };

        // The same confinement the shell tool resolves: a policy this
        // host cannot enforce refuses here too, so a background job
        // is never the way around the sandbox.
        let confined = match resolve_confined_command(
            self.sandbox.as_deref(),
            self.policy,
            &context.workspace_root,
        ) {
            Ok(confined) => confined,
            Err(error) => return ToolOutput::error(error.to_string()),
        };

        let id = self.registry.next_id();
        match spawn_job(
            &id,
            &context.session_id,
            command,
            &confined,
            &context.workspace_root,
        )
        .await
        {
            Ok(job) => {
                self.registry.jobs.write().insert(id.clone(), Arc::new(job));
                ToolOutput::text(format!(
                    "Started job `{id}`: {command}\nIt runs in the \
                     background; read it with `action = \"output\"` and \
                     `job_id = \"{id}\"`."
                ))
            }
            Err(message) => ToolOutput::error(message),
        }
    }

    /// The job's output, optionally waiting for it.
    async fn output(&self, request: &JobRequest) -> ToolOutput {
        let id = match request.job_id.as_deref() {
            Some(id) => id,
            None => {
                return ToolOutput::error(
                    "`job_id` is required for `action = \"output\"`.",
                );
            }
        };
        let job = match self.registry.get(id) {
            Some(job) => job,
            None => {
                return ToolOutput::error(format!(
                    "No job `{id}`. Use `action = \"list\"` to see the \
                     jobs this deployment knows."
                ));
            }
        };

        let wait = Duration::from_secs(
            request.wait_secs.unwrap_or(0).min(MAX_WAIT_SECS),
        );
        if !wait.is_zero() && !job.is_finished() {
            // Wait for the job, bounded: a job that outlives the wait
            // answers with what it has produced so far.
            let deadline = std::time::Instant::now() + wait;
            while !job.is_finished() && std::time::Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        }

        ToolOutput::text(render_job(&job))
    }

    /// Every job this deployment knows.
    fn list(&self) -> ToolOutput {
        let jobs = self.registry.list();
        if jobs.is_empty() {
            return ToolOutput::text(
                "No jobs. Start one with `action = \"start\"`.",
            );
        }
        let mut body = String::new();
        for job in jobs {
            let state = job.state();
            body.push_str(&format!(
                "{} [{}] {}\n",
                job.id(),
                state.label(),
                job.command()
            ));
        }
        ToolOutput::text(body.trim_end())
    }

    /// Kill a running job.
    fn kill(&self, request: &JobRequest) -> ToolOutput {
        let id = match request.job_id.as_deref() {
            Some(id) => id,
            None => {
                return ToolOutput::error(
                    "`job_id` is required for `action = \"kill\"`.",
                );
            }
        };
        let job = match self.registry.get(id) {
            Some(job) => job,
            None => {
                return ToolOutput::error(format!(
                    "No job `{id}`. Use `action = \"list\"` to see the \
                     jobs this deployment knows."
                ));
            }
        };
        if job.is_finished() {
            return ToolOutput::error(format!(
                "The job `{id}` already finished; there is nothing to \
                 kill."
            ));
        }
        kill_job(&job);
        ToolOutput::text(format!("Killed job `{id}`."))
    }
}

/// The job's answer: its output so far, then its state.
fn render_job(job: &Job) -> String {
    let mut body = String::new();
    let stdout = job.stdout();
    let stderr = job.stderr();
    if !stdout.is_empty() {
        body.push_str("stdout:\n");
        body.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !body.is_empty() {
            body.push_str("\n\n");
        }
        body.push_str("stderr:\n");
        body.push_str(&stderr);
    }
    if body.is_empty() {
        body.push_str("(no output yet)");
    }
    body.push_str("\n\n");
    let state = job.state();
    match state {
        JobState::Running => {
            body.push_str("[still running]");
        }
        JobState::Exited { code, signal } => {
            if let Some(signal) = signal {
                body.push_str(&format!("[killed by signal: {signal}]"));
            } else {
                body.push_str(&format!("[exit code: {}]", code.unwrap_or(-1)));
            }
        }
        JobState::Killed => body.push_str("[killed]"),
        JobState::Failed(message) => {
            body.push_str(&format!("[failed: {message}]"));
        }
    }
    body
}

/// Start the command in the background.
#[cfg(unix)]
async fn spawn_job(
    id: &str,
    session_id: &str,
    command: &str,
    confined: &crate::shell::sandbox::ConfinedCommand,
    cwd: &std::path::Path,
) -> Result<Job, String> {
    let argv = confined.argv_for(command);
    let cmd = crate::process::prepare(&argv, cwd);
    let spawned = crate::process::spawn(cmd)
        .map_err(|error| format!("Failed to spawn the command: {error}"))?;
    let mut child = spawned.child;
    let pgid = spawned.pgid;

    let (stdout, stdout_task) = crate::process::spawn_reader(
        child.stdout.take().expect("piped stdout"),
        None,
    );
    let (stderr, stderr_task) = crate::process::spawn_reader(
        child.stderr.take().expect("piped stderr"),
        None,
    );

    // The waiter owns the child: it records the exit status when the
    // command finishes. The kill path does not need the handle — it
    // signals the process group, which reaches the whole tree.
    let state = Arc::new(Mutex::new(JobState::Running));
    let waiter_state = Arc::clone(&state);
    tokio::spawn(async move {
        let status = child.wait().await;
        let next = match status {
            Ok(status) => JobState::Exited {
                code: status.code(),
                signal: crate::process::signal_of(&status),
            },
            Err(error) => JobState::Failed(format!("wait failed: {error}")),
        };
        if let Ok(mut slot) = waiter_state.lock() {
            *slot = next;
        }
        // The readers finish with the pipes; let them settle.
        let _ = stdout_task.await;
        let _ = stderr_task.await;
    });

    Ok(Job {
        id: id.to_string(),
        session_id: session_id.to_string(),
        command: command.to_string(),
        state,
        stdout,
        stderr,
        pgid,
    })
}

/// Start the command in the background: the non-unix build refuses.
#[cfg(not(unix))]
async fn spawn_job(
    _id: &str,
    _session_id: &str,
    _command: &str,
    _confined: &crate::shell::sandbox::ConfinedCommand,
    _cwd: &std::path::Path,
) -> Result<Job, String> {
    Err(
        "the job tool is not implemented on non-unix targets in this \
         build (the process group and its kill are POSIX-only)"
            .to_string(),
    )
}

/// Kill a running job: SIGTERM to the group, then SIGKILL.
#[cfg(unix)]
fn kill_job(job: &Job) {
    crate::process::signal_group(job.pgid, libc::SIGTERM);
    let pgid = job.pgid;
    // The escalation runs on the process's own thread: the kill is
    // answered immediately and the group dies a moment later.
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(crate::shell::KILL_GRACE_SECS));
        crate::process::signal_group(pgid, libc::SIGKILL);
    });
    if let Ok(mut slot) = job.state.lock() {
        *slot = JobState::Killed;
    }
}

/// Kill a running job: the non-unix build has nothing to signal.
#[cfg(not(unix))]
fn kill_job(_job: &Job) {}
