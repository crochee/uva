//! OS-level execution policy for confined tool runs.
//!
//! Adopted from `deepseek-harness`'s `sandbox` +
//! `sandbox-policy` packages. A run's *file-effect policy* is
//! resolved once per call, handed to a backend that returns the
//! argv to spawn **instead** of the tool's own, and a missing
//! backend fails closed instead of silently running unconfined.
//!
//! This module belongs to the `shell` plugin, not to the tool
//! paradigm: the shell is the one agent-facing tool that runs a
//! process, so process confinement is its own concern. A
//! consumer assembling the shell brick gets the policy seam with
//! it, and a consumer that never registers `shell` carries none
//! of it.
//!
//! Three pure pieces:
//!
//! - [`ExecutionPolicy`] — the boundary one run executes under:
//!   [`ReadOnly`](ExecutionPolicy::ReadOnly),
//!   [`WorkspaceWrite`](ExecutionPolicy::WorkspaceWrite), or
//!   [`DangerFullAccess`](ExecutionPolicy::DangerFullAccess).
//! - [`SandboxBackend`] — the confinement seam. [`BwrapBackend`]
//!   wraps a command in bubblewrap; [`NoopBackend`] is the
//!   explicit no-confinement bypass for `DangerFullAccess`.
//!   Neither spawns the wrapped command: [`SandboxBackend::confine`]
//!   is argv construction.
//! - [`effective_policy`] — the fold
//!   `grant > session mode > deployment default`, so a mode
//!   switch survives a restart by replaying the session log.
//!
//! [`ShellTool`](crate::shell::ShellTool) consumes all three: it holds
//! its own policy and backend (so the description it advertises
//! and the argv it spawns can never disagree), spawns the
//! resolved argv, and classifies a refused file effect as a
//! *policy denial* rather than a command failure.

use std::{
    fmt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::OnceLock,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Stable error code a fail-closed refusal carries (dsh
/// `SANDBOX_UNAVAILABLE`): callers can tell "no confinement is
/// available" from a command that ran and failed.
pub const SANDBOX_UNAVAILABLE: &str = "SANDBOX_UNAVAILABLE";

/// Every denial signature a confinement mechanism is known to
/// produce (dsh `DENIAL_SIGNATURES`): bubblewrap's read-only bind
/// mount speaks `EROFS`, Landlock `EACCES`, Seatbelt `EPERM`.
///
/// A backend narrows this to its own dialect through
/// [`SandboxBackend::denial_signatures`]. Classifying against the
/// union instead would claim denials a given backend never
/// produces, so this constant is documentation plus a source for
/// third-party backends — not a default.
pub const KNOWN_DENIAL_SIGNATURES: &[&str] = &[
    "read-only file system",
    "permission denied",
    "operation not permitted",
];

/// File-effect boundary one command executes under (dsh
/// `SandboxMode` parity).
///
/// Variants are ordered narrowest to widest, so [`Ord`] expresses
/// "wider than" when a caller picks the narrowest escalation that
/// suffices.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Serialize,
    Deserialize,
)]
#[serde(rename_all = "kebab-case")]
pub enum ExecutionPolicy {
    /// Reads are unrestricted; every file write is refused by the
    /// backend.
    ReadOnly,
    /// Reads are unrestricted; writes are confined to the
    /// workspace root (plus a private `/tmp`).
    WorkspaceWrite,
    /// No confinement — the command runs with the caller's own
    /// authority. The default, so callers that never opt in keep
    /// today's behaviour byte for byte.
    #[default]
    DangerFullAccess,
}

impl ExecutionPolicy {
    /// Wire name (`"read-only"` / `"workspace-write"` /
    /// `"danger-full-access"`), matching the serde form and the
    /// `mode` field of a `sandbox_mode` session event.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read-only",
            Self::WorkspaceWrite => "workspace-write",
            Self::DangerFullAccess => "danger-full-access",
        }
    }

    /// Parse a policy wire name.
    ///
    /// Returns `None` for anything else, so a log written by a
    /// newer build degrades to "no override" instead of failing.
    #[must_use]
    pub fn from_wire(raw: &str) -> Option<Self> {
        match raw {
            "read-only" => Some(Self::ReadOnly),
            "workspace-write" => Some(Self::WorkspaceWrite),
            "danger-full-access" => Some(Self::DangerFullAccess),
            _ => None,
        }
    }

    /// True when running under this policy needs a confining
    /// backend — i.e. everything but `DangerFullAccess`.
    #[must_use]
    pub const fn is_confined(self) -> bool {
        !matches!(self, Self::DangerFullAccess)
    }
}

/// Why a confined run could not be wrapped.
#[derive(Debug, Error)]
pub enum SandboxError {
    /// The policy needs confinement but no backend can enforce it.
    /// The run is refused, never executed unconfined (dsh
    /// `SANDBOX_UNAVAILABLE`).
    #[error(
        "execution policy \"{}\" needs a sandbox but no backend is usable on this host ({}); \
         refusing to run the command unconfined. Install bubblewrap, or run with the \
         danger-full-access policy. [{}]",
        .policy.as_str(),
        .detail,
        SANDBOX_UNAVAILABLE
    )]
    Unavailable {
        /// Policy that could not be enforced.
        policy: ExecutionPolicy,
        /// Why confinement is unavailable.
        detail: String,
    },
    /// A usable backend refused to build a wrap for this call —
    /// typically an unusable workspace root.
    #[error(
        "sandbox backend {backend} refused to confine the command: {detail}"
    )]
    Refused {
        /// Backend that refused.
        backend: String,
        /// Backend-supplied reason.
        detail: String,
    },
}

impl SandboxError {
    /// Fail-closed refusal of `policy`.
    #[must_use]
    pub fn unavailable(
        policy: ExecutionPolicy,
        detail: impl Into<String>,
    ) -> Self {
        Self::Unavailable {
            policy,
            detail: detail.into(),
        }
    }

    /// Stable machine-readable code: [`SANDBOX_UNAVAILABLE`] for a
    /// fail-closed refusal, `None` for a backend that was usable
    /// but rejected the wrap.
    #[must_use]
    pub const fn code(&self) -> Option<&'static str> {
        match self {
            Self::Unavailable { .. } => Some(SANDBOX_UNAVAILABLE),
            Self::Refused { .. } => None,
        }
    }
}

/// Whether a backend can enforce a policy on this host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendAvailability {
    /// The backend's runner is usable.
    Available,
    /// No usable runner: a confined policy must refuse, because
    /// running unconfined would be a silent boundary loss.
    Unavailable {
        /// Why the runner is unusable (probe output).
        reason: String,
    },
}

impl BackendAvailability {
    /// True for [`BackendAvailability::Available`].
    #[must_use]
    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

/// The argv to spawn **instead** of the caller's own
/// `sh -c <command>`.
///
/// `args` is the wrapper's argument list up to — but excluding —
/// the command payload: the wrapper already carries the `-c`, and
/// [`ConfinedCommand::argv_for`] appends the command itself. That
/// keeps the payload convention on this side of the seam, so a
/// caller cannot forget it or quote it twice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfinedCommand {
    /// Program to spawn — a runner such as `bwrap`, or `sh` when
    /// nothing confines the run.
    pub program: PathBuf,
    /// Wrapper arguments preceding the command payload.
    pub args: Vec<String>,
}

impl ConfinedCommand {
    /// The unconfined wrap: the command runs through `sh -c`,
    /// exactly as it would with no backend configured.
    #[must_use]
    pub fn unconfined() -> Self {
        Self {
            program: PathBuf::from("sh"),
            args: vec!["-c".to_string()],
        }
    }

    /// Complete the argv by appending the command payload.
    #[must_use]
    pub fn argv_for(&self, command: &str) -> Vec<String> {
        let mut argv = Vec::with_capacity(self.args.len() + 2);
        argv.push(self.program.to_string_lossy().into_owned());
        argv.extend(self.args.iter().cloned());
        argv.push(command.to_string());
        argv
    }
}

/// OS-level confinement seam.
///
/// `confine` must return an *enforcing* wrap or fail: silently
/// passing the caller's argv through unconfined is forbidden, and
/// so is claiming a policy a runner cannot enforce.
pub trait SandboxBackend: fmt::Debug + Send + Sync {
    /// Backend name used in refusal messages (`"bwrap"`).
    fn name(&self) -> &str;

    /// Whether this backend can confine on this host. Probes are
    /// the backend's business; callers only read the verdict.
    fn availability(&self) -> BackendAvailability;

    /// Wrap the `sh -c` payload for `policy`.
    ///
    /// # Errors
    ///
    /// [`SandboxError::Unavailable`] when the backend cannot
    /// enforce `policy` on this host, [`SandboxError::Refused`]
    /// when it can but this call's inputs are unusable.
    fn confine(
        &self,
        policy: ExecutionPolicy,
        workspace_root: &Path,
    ) -> Result<ConfinedCommand, SandboxError>;

    /// Denial dialect: the case-insensitive stderr substrings a
    /// file effect denied by *this* backend produces (bwrap's
    /// read-only bind mount prints `EROFS` text, Landlock
    /// `EACCES`, Seatbelt `EPERM`). Consumers classify a failed
    /// run against exactly these rather than a cross-backend
    /// union, which would claim denials this backend never
    /// produces.
    fn denial_signatures(&self) -> &'static [&'static str];
}

/// Denial dialect of a read-only bind mount: the kernel refuses
/// the write with `EROFS`, whose message text follows.
const BWRAP_DENIAL_SIGNATURES: &[&str] = &["read-only file system"];

/// bubblewrap backend — argv construction only.
///
/// The only process it ever spawns is its one-shot
/// `bwrap --version` availability probe (cached for the lifetime
/// of the backend); the wrapped command is spawned by the caller.
#[derive(Debug)]
pub struct BwrapBackend {
    program: PathBuf,
    name: String,
    probe: OnceLock<BackendAvailability>,
}

impl BwrapBackend {
    /// Backend over the `bwrap` found on `PATH`.
    #[must_use]
    pub fn new() -> Self {
        Self::with_program(PathBuf::from("bwrap"))
    }

    /// Backend over an explicit runner program (pinned installs,
    /// tests).
    #[must_use]
    pub fn with_program(program: PathBuf) -> Self {
        let name = program.display().to_string();
        Self {
            program,
            name,
            probe: OnceLock::new(),
        }
    }

    /// The runner program this backend probes and spawns.
    #[must_use]
    pub fn program(&self) -> &Path {
        &self.program
    }
}

impl Default for BwrapBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl SandboxBackend for BwrapBackend {
    fn name(&self) -> &str {
        &self.name
    }

    fn availability(&self) -> BackendAvailability {
        self.probe
            .get_or_init(|| probe_runner(&self.program))
            .clone()
    }

    fn confine(
        &self,
        policy: ExecutionPolicy,
        workspace_root: &Path,
    ) -> Result<ConfinedCommand, SandboxError> {
        if !policy.is_confined() {
            // Explicit bypass: `DangerFullAccess` never needs a
            // runner, not even a present one.
            return NoopBackend::new().confine(policy, workspace_root);
        }
        if let BackendAvailability::Unavailable { reason } = self.availability()
        {
            return Err(SandboxError::unavailable(
                policy,
                format!("{}: {reason}", self.name()),
            ));
        }
        Ok(ConfinedCommand {
            program: self.program.clone(),
            args: bwrap_profile_args(policy, workspace_root)?,
        })
    }

    fn denial_signatures(&self) -> &'static [&'static str] {
        BWRAP_DENIAL_SIGNATURES
    }
}

/// bubblewrap profile arguments for one policy, ending with the
/// payload's `sh -c` (pure: no probe, no filesystem access).
///
/// The wrap read-only-binds `/`, so reads stay unrestricted and a
/// write outside the grant fails with `EROFS`; adds a private
/// writable `/tmp` plus a read-write bind of `workspace_root` for
/// [`ExecutionPolicy::WorkspaceWrite`]; mounts `/dev` and
/// `/proc`; shares no PID namespace, and no network at all under
/// [`ExecutionPolicy::ReadOnly`]; and dies with its parent
/// (`--die-with-parent`). Under `ReadOnly` the workspace stays
/// read-only through the loopback bind of `/`, so no second mount
/// is emitted for it.
///
/// # Errors
///
/// [`SandboxError::Refused`] when `WorkspaceWrite` is asked to
/// bind a `workspace_root` that is not an absolute UTF-8 path — a
/// `--bind` of anything else would either fail at spawn time or
/// grant a different directory than the caller asked for.
pub fn bwrap_profile_args(
    policy: ExecutionPolicy,
    workspace_root: &Path,
) -> Result<Vec<String>, SandboxError> {
    let mut args = Vec::new();
    push_all(&mut args, &["--ro-bind", "/", "/"]);
    if policy == ExecutionPolicy::WorkspaceWrite {
        // dsh parity: a read-only `/tmp` breaks ordinary
        // commands (toolchains, editors, `patch`) that need
        // scratch space.
        let root = bind_root(workspace_root)?;
        push_all(&mut args, &["--tmpfs", "/tmp", "--bind", root, root]);
    }
    push_all(
        &mut args,
        &["--dev", "/dev", "--proc", "/proc", "--unshare-pid"],
    );
    if policy == ExecutionPolicy::ReadOnly {
        // A run that may not write does not need the network.
        push_all(&mut args, &["--unshare-net"]);
    }
    push_all(&mut args, &["--die-with-parent", "--", "sh", "-c"]);
    Ok(args)
}

/// The explicit no-confinement backend.
///
/// Installing it is not a way around a confined policy: it wraps
/// [`ExecutionPolicy::DangerFullAccess`] exactly like an
/// unconfigured tool, and refuses every other policy, so a
/// misconfigured caller fails closed instead of silently running
/// unconfined.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopBackend;

impl NoopBackend {
    /// Build the bypass backend.
    #[must_use]
    pub const fn new() -> Self {
        Self
    }
}

impl SandboxBackend for NoopBackend {
    fn name(&self) -> &str {
        "none"
    }

    fn availability(&self) -> BackendAvailability {
        BackendAvailability::Available
    }

    fn confine(
        &self,
        policy: ExecutionPolicy,
        _workspace_root: &Path,
    ) -> Result<ConfinedCommand, SandboxError> {
        if policy.is_confined() {
            return Err(SandboxError::unavailable(
                policy,
                "the no-op backend does not confine",
            ));
        }
        Ok(ConfinedCommand::unconfined())
    }

    fn denial_signatures(&self) -> &'static [&'static str] {
        &[]
    }
}

/// Resolve the argv wrapper for one run, failing closed.
///
/// `DangerFullAccess` bypasses the backend explicitly (through
/// [`NoopBackend`]); every other policy requires a backend, and a
/// missing one is a typed refusal — never a silent unconfined
/// run.
///
/// # Errors
///
/// [`SandboxError`] from the backend, or
/// [`SandboxError::Unavailable`] when a confined policy has no
/// backend at all.
pub fn resolve_confined_command(
    backend: Option<&dyn SandboxBackend>,
    policy: ExecutionPolicy,
    workspace_root: &Path,
) -> Result<ConfinedCommand, SandboxError> {
    if !policy.is_confined() {
        return NoopBackend::new().confine(policy, workspace_root);
    }
    match backend {
        Some(backend) => backend.confine(policy, workspace_root),
        None => Err(SandboxError::unavailable(
            policy,
            "no sandbox backend is configured for this call",
        )),
    }
}

/// Denial dialect of the backend that governs `policy`.
///
/// Empty when the run is unconfined or no backend is installed, so
/// an unconfined failure is never mislabelled as a policy denial.
#[must_use]
pub fn denial_dialect(
    backend: Option<&dyn SandboxBackend>,
    policy: ExecutionPolicy,
) -> &'static [&'static str] {
    if !policy.is_confined() {
        return &[];
    }
    match backend {
        Some(backend) => backend.denial_signatures(),
        None => &[],
    }
}

/// Classify a settled run against one backend's denial dialect.
///
/// A denial requires a plain non-zero exit — `None` means the run
/// was killed (timeout or signal), whose stderr is not evidence of
/// a refused file effect — plus a case-insensitive stderr match on
/// one of `signatures`.
#[must_use]
pub fn classify_denial(
    exit_code: Option<i32>,
    stderr: &str,
    signatures: &[&str],
) -> bool {
    if !matches!(exit_code, Some(code) if code != 0) {
        return false;
    }
    signatures
        .iter()
        .any(|signature| contains_ignore_ascii_case(stderr, signature))
}

/// Model-facing marker for a run the sandbox refused (dsh
/// `sandboxDenialMarker`), naming the mode so the model can tell a
/// policy boundary from a command that merely failed.
#[must_use]
pub fn denial_marker(policy: ExecutionPolicy) -> String {
    format!(
        "[sandbox: file access denied under {} mode]",
        policy.as_str()
    )
}

/// The model-facing paragraph describing one policy's write scope.
///
/// The shell tool appends this to its description (and the agent
/// loop may inject it into a prompt), so the model plans inside the
/// boundary instead of discovering it by failing.
#[must_use]
pub fn policy_prompt_block(policy: ExecutionPolicy) -> String {
    match policy {
        ExecutionPolicy::ReadOnly => {
            "Execution policy: read-only — commands may read any file, but every file write \
             is refused by the OS sandbox and surfaces as \
             `[sandbox: file access denied under read-only mode]`; that marker is a policy \
             denial, not a command failure."
        }
        ExecutionPolicy::WorkspaceWrite => {
            "Execution policy: workspace-write — commands may read any file and write inside \
             the workspace root (plus a private /tmp); writes elsewhere are refused by the OS \
             sandbox and surface as \
             `[sandbox: file access denied under workspace-write mode]`; that marker is a \
             policy denial, not a command failure."
        }
        ExecutionPolicy::DangerFullAccess => {
            "Execution policy: danger-full-access — commands run unconfined, so there is no \
             OS boundary on file effects and every write the user can make the command can \
             make."
        }
    }
    .to_string()
}

/// Resolve the policy one run executes under, by pure fold.
///
/// `modes` holds the policy wire names carried by a session's
/// `sandbox_mode` events, oldest first — the caller reads the
/// session log in order and extracts each row's mode
/// (`SessionEvent::sandbox_mode`). Taking names rather than
/// events is what keeps this crate independent of
/// `elysper-session`: the shell brick depends on the tool
/// paradigm and nothing else.
///
/// Precedence: the most recent entry of `grants` (an escalation the
/// user already sanctioned) > the last known mode in `modes` >
/// `default` (the deployment default). Reading the session log
/// *is* the state, so an override survives a restart with no
/// catch-up machinery, and a mode this build does not know is
/// skipped rather than treated as a switch to the default.
#[must_use]
pub fn effective_policy(
    grants: &[ExecutionPolicy],
    modes: &[&str],
    default: ExecutionPolicy,
) -> ExecutionPolicy {
    if let Some(grant) = grants.last() {
        return *grant;
    }
    for mode in modes.iter().rev() {
        if let Some(policy) = ExecutionPolicy::from_wire(mode) {
            return policy;
        }
    }
    default
}

/// Append `values` to `args`.
fn push_all(args: &mut Vec<String>, values: &[&str]) {
    for value in values {
        args.push((*value).to_string());
    }
}

/// Validate the read-write bind root: absolute and UTF-8 (the argv
/// is a `String` list, so a non-UTF-8 path cannot be expressed).
fn bind_root(workspace_root: &Path) -> Result<&str, SandboxError> {
    let refuse = |detail: String| SandboxError::Refused {
        backend: "bwrap".to_string(),
        detail,
    };
    let Some(root) = workspace_root.to_str() else {
        return Err(refuse(format!(
            "workspace root {} is not valid UTF-8",
            workspace_root.display()
        )));
    };
    if root.is_empty() || !workspace_root.is_absolute() {
        return Err(refuse(format!(
            "workspace root {root:?} must be an absolute path"
        )));
    }
    Ok(root)
}

/// One-shot runner probe: the program must exist and answer
/// `--version` with a successful exit.
fn probe_runner(program: &Path) -> BackendAvailability {
    let probe = Command::new(program)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    match probe {
        Ok(status) if status.success() => BackendAvailability::Available,
        Ok(status) => BackendAvailability::Unavailable {
            reason: format!(
                "`{} --version` exited with {status}",
                program.display()
            ),
        },
        Err(error) => BackendAvailability::Unavailable {
            reason: format!(
                "`{} --version` could not run: {error}",
                program.display()
            ),
        },
    }
}

/// Case-insensitive ASCII substring test with no allocation.
///
/// The denial dialects are ASCII, and a UTF-8 haystack cannot match
/// one of them across a multi-byte boundary: continuation bytes are
/// never ASCII.
fn contains_ignore_ascii_case(haystack: &str, needle: &str) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    let needle = needle.as_bytes();
    haystack
        .as_bytes()
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests;
