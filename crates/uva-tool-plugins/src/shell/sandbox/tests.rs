use std::path::Path;

use super::*;

/// Backend that always refuses, and says so in its dialect:
/// proves the `DangerFullAccess` bypass never consults it.
#[derive(Debug)]
struct RefusingBackend;

impl SandboxBackend for RefusingBackend {
    fn name(&self) -> &str {
        "refusing"
    }

    fn availability(&self) -> BackendAvailability {
        BackendAvailability::Unavailable {
            reason: "test backend".to_string(),
        }
    }

    fn confine(
        &self,
        policy: ExecutionPolicy,
        _workspace_root: &Path,
    ) -> Result<ConfinedCommand, SandboxError> {
        Err(SandboxError::unavailable(policy, "test backend refuses"))
    }

    fn denial_signatures(&self) -> &'static [&'static str] {
        &["denied by the test backend"]
    }
}

/// Write an executable stand-in runner that exits with `code`.
#[cfg(unix)]
fn fake_runner(dir: &Path, code: i32) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;

    let path = dir.join(format!("fake-bwrap-{code}"));
    std::fs::write(&path, format!("#!/bin/sh\nexit {code}\n"))
        .expect("write runner");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755))
        .expect("chmod runner");
    path
}

#[test]
fn read_only_profile_never_binds_the_workspace_writable() {
    let args = bwrap_profile_args(ExecutionPolicy::ReadOnly, Path::new("/ws"))
        .expect("profile");
    assert_eq!(
        args,
        vec![
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--unshare-pid",
            "--unshare-net",
            "--die-with-parent",
            "--",
            "sh",
            "-c",
        ]
    );
    assert!(!args.contains(&"--bind".to_string()));
    // The complete argv ends with the payload the caller
    // spawns: `sh -c <command>`.
    let confined = ConfinedCommand {
        program: PathBuf::from("bwrap"),
        args,
    };
    assert_eq!(
        confined.argv_for("ls /"),
        vec![
            "bwrap",
            "--ro-bind",
            "/",
            "/",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--unshare-pid",
            "--unshare-net",
            "--die-with-parent",
            "--",
            "sh",
            "-c",
            "ls /",
        ]
    );
}

#[test]
fn workspace_write_profile_binds_the_root_read_write() {
    let args =
        bwrap_profile_args(ExecutionPolicy::WorkspaceWrite, Path::new("/ws"))
            .expect("profile");
    assert_eq!(
        args,
        vec![
            "--ro-bind",
            "/",
            "/",
            "--tmpfs",
            "/tmp",
            "--bind",
            "/ws",
            "/ws",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--unshare-pid",
            "--die-with-parent",
            "--",
            "sh",
            "-c",
        ]
    );
    // Only read-only mode gives up the network.
    assert!(!args.contains(&"--unshare-net".to_string()));
}

#[test]
fn workspace_write_refuses_a_relative_root() {
    let error =
        bwrap_profile_args(ExecutionPolicy::WorkspaceWrite, Path::new("ws"))
            .expect_err("relative root must be refused");
    assert!(
        matches!(error, SandboxError::Refused { .. }),
        "got {error:?}"
    );
    assert!(error.code().is_none());
    assert!(error.to_string().contains("absolute"));
}

#[test]
fn missing_runner_fails_closed() {
    let backend = BwrapBackend::with_program(PathBuf::from(
        "/nonexistent/uva-test-bwrap",
    ));
    assert!(!backend.availability().is_available());

    let error = backend
        .confine(ExecutionPolicy::ReadOnly, Path::new("/ws"))
        .expect_err("a confined policy must refuse");
    assert_eq!(error.code(), Some(SANDBOX_UNAVAILABLE));
    assert!(error.to_string().contains("refusing to run"));
    assert!(error.to_string().contains("danger-full-access"));

    // `DangerFullAccess` is the documented bypass: the missing
    // runner is irrelevant.
    let confined = backend
        .confine(ExecutionPolicy::DangerFullAccess, Path::new("/ws"))
        .expect("bypass");
    assert_eq!(confined.argv_for("echo hi"), vec!["sh", "-c", "echo hi"]);
}

#[cfg(unix)]
#[test]
fn usable_runner_wraps_the_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = fake_runner(dir.path(), 0);
    let backend = BwrapBackend::with_program(runner.clone());
    assert!(backend.availability().is_available());

    let confined = backend
        .confine(ExecutionPolicy::WorkspaceWrite, Path::new("/ws"))
        .expect("confines");
    assert_eq!(confined.program, runner);
    let runner = runner.display().to_string();
    assert_eq!(
        confined.argv_for("echo hi"),
        vec![
            runner.as_str(),
            "--ro-bind",
            "/",
            "/",
            "--tmpfs",
            "/tmp",
            "--bind",
            "/ws",
            "/ws",
            "--dev",
            "/dev",
            "--proc",
            "/proc",
            "--unshare-pid",
            "--die-with-parent",
            "--",
            "sh",
            "-c",
            "echo hi",
        ]
    );
    assert_eq!(backend.denial_signatures(), &["read-only file system"]);
}

#[cfg(unix)]
#[test]
fn availability_verdict_is_cached() {
    let dir = tempfile::tempdir().expect("tempdir");
    let runner = fake_runner(dir.path(), 7);
    let backend = BwrapBackend::with_program(runner.clone());
    let first = backend.availability();
    assert!(!first.is_available());

    std::fs::remove_file(&runner).expect("remove runner");
    // Cached: a re-probe would now report "could not run"
    // instead of the original exit status.
    assert_eq!(backend.availability(), first);
}

#[test]
fn only_transient_spawn_failures_are_retried() {
    use super::is_transient_spawn_error;

    // "not now": the kernel is busy, the caller may try again.
    for errno in [4, 11, 12] {
        let error = std::io::Error::from_raw_os_error(errno);
        assert!(
            is_transient_spawn_error(&error),
            "errno {errno} is transient"
        );
    }
    // "not ever": the program is missing or not runnable.
    for errno in [2, 13] {
        let error = std::io::Error::from_raw_os_error(errno);
        assert!(
            !is_transient_spawn_error(&error),
            "errno {errno} is permanent"
        );
    }
}

#[test]
fn a_program_that_cannot_be_run_is_unavailable() {
    let dir = tempfile::tempdir().expect("tempdir");
    let backend =
        BwrapBackend::with_program(dir.path().join("no-such-program"));
    assert!(!backend.availability().is_available());
}

#[test]
fn noop_backend_refuses_confined_policies() {
    let backend = NoopBackend::new();
    assert!(backend.availability().is_available());
    let error = NoopBackend
        .confine(ExecutionPolicy::WorkspaceWrite, Path::new("/ws"))
        .expect_err("the bypass must not launder a confined policy");
    assert_eq!(error.code(), Some(SANDBOX_UNAVAILABLE));
    assert!(NoopBackend.denial_signatures().is_empty());
}

#[test]
fn resolve_without_a_backend_refuses_confined_policies() {
    for policy in [ExecutionPolicy::ReadOnly, ExecutionPolicy::WorkspaceWrite] {
        let error = resolve_confined_command(None, policy, Path::new("/ws"))
            .expect_err("must fail closed");
        assert_eq!(error.code(), Some(SANDBOX_UNAVAILABLE));
    }
}

#[test]
fn resolve_bypasses_the_backend_for_danger_full_access() {
    let confined = resolve_confined_command(
        Some(&RefusingBackend),
        ExecutionPolicy::DangerFullAccess,
        Path::new("/ws"),
    )
    .expect("bypass ignores the backend");
    assert_eq!(confined.argv_for("echo hi"), vec!["sh", "-c", "echo hi"]);
    assert!(
        denial_dialect(
            Some(&RefusingBackend),
            ExecutionPolicy::DangerFullAccess
        )
        .is_empty()
    );
    assert_eq!(
        denial_dialect(Some(&RefusingBackend), ExecutionPolicy::ReadOnly),
        &["denied by the test backend"]
    );
}

#[test]
fn denial_needs_a_nonzero_exit_and_a_dialect_match() {
    let dialect = BwrapBackend::new().denial_signatures();
    assert!(classify_denial(
        Some(1),
        "touch: cannot touch '/etc/x': Read-only file system",
        dialect
    ));
    assert!(!classify_denial(
        Some(0),
        "touch: cannot touch '/etc/x': Read-only file system",
        dialect
    ));
    assert!(!classify_denial(
        None,
        "touch: cannot touch '/etc/x': Read-only file system",
        dialect
    ));
    assert!(!classify_denial(
        Some(2),
        "make: *** [Makefile:3: all] Error 2",
        dialect
    ));
    assert!(!classify_denial(Some(1), "permission denied", dialect));
    // A backend with no declared dialect never claims a denial.
    assert!(!classify_denial(Some(1), "Read-only file system", &[]));
}

#[test]
fn denial_marker_names_the_mode() {
    assert_eq!(
        denial_marker(ExecutionPolicy::ReadOnly),
        "[sandbox: file access denied under read-only mode]"
    );
    assert_eq!(
        denial_marker(ExecutionPolicy::WorkspaceWrite),
        "[sandbox: file access denied under workspace-write mode]"
    );
}

#[test]
fn policy_prompt_block_names_each_write_scope() {
    let read_only = policy_prompt_block(ExecutionPolicy::ReadOnly);
    assert!(read_only.contains("read-only"));
    assert!(read_only.contains("refused"));
    assert!(read_only.contains("not a command failure"));

    let workspace = policy_prompt_block(ExecutionPolicy::WorkspaceWrite);
    assert!(workspace.contains("workspace root"));
    assert!(workspace.contains("workspace-write mode"));

    let danger = policy_prompt_block(ExecutionPolicy::DangerFullAccess);
    assert!(danger.contains("unconfined"));
    assert!(!danger.contains("denied"));
}

#[test]
fn wire_names_round_trip_through_serde() {
    for policy in [
        ExecutionPolicy::ReadOnly,
        ExecutionPolicy::WorkspaceWrite,
        ExecutionPolicy::DangerFullAccess,
    ] {
        let wire = policy.as_str();
        assert_eq!(
            serde_json::to_string(&policy).expect("serializes"),
            format!("\"{wire}\"")
        );
        assert_eq!(ExecutionPolicy::from_wire(wire), Some(policy));
    }
    assert_eq!(
        ExecutionPolicy::default(),
        ExecutionPolicy::DangerFullAccess
    );
    assert!(!ExecutionPolicy::DangerFullAccess.is_confined());
    assert!(ExecutionPolicy::ReadOnly.is_confined());
    assert_eq!(ExecutionPolicy::from_wire("future-mode"), None);
    assert!(
        ExecutionPolicy::ReadOnly < ExecutionPolicy::DangerFullAccess,
        "narrower policies order first"
    );
}

#[test]
fn effective_policy_prefers_the_most_recent_grant() {
    let modes = [
        ExecutionPolicy::ReadOnly.as_str(),
        ExecutionPolicy::WorkspaceWrite.as_str(),
    ];
    assert_eq!(
        effective_policy(
            &[ExecutionPolicy::DangerFullAccess],
            &modes,
            ExecutionPolicy::ReadOnly,
        ),
        ExecutionPolicy::DangerFullAccess
    );
    assert_eq!(
        effective_policy(
            &[
                ExecutionPolicy::WorkspaceWrite,
                ExecutionPolicy::DangerFullAccess,
            ],
            &modes,
            ExecutionPolicy::ReadOnly,
        ),
        ExecutionPolicy::DangerFullAccess,
        "the last sanctioned grant wins"
    );
}

#[test]
fn effective_policy_folds_the_last_session_override() {
    // `modes` is the session's `sandbox_mode` names in log
    // order: a later switch supersedes an earlier one.
    let modes = [
        ExecutionPolicy::ReadOnly.as_str(),
        ExecutionPolicy::WorkspaceWrite.as_str(),
    ];
    assert_eq!(
        effective_policy(&[], &modes, ExecutionPolicy::DangerFullAccess),
        ExecutionPolicy::WorkspaceWrite
    );
    // No override at all → the deployment default.
    assert_eq!(
        effective_policy(&[], &[], ExecutionPolicy::WorkspaceWrite),
        ExecutionPolicy::WorkspaceWrite
    );
}

#[test]
fn effective_policy_ignores_modes_it_cannot_interpret() {
    let modes = [ExecutionPolicy::WorkspaceWrite.as_str(), "future-mode"];
    assert_eq!(
        effective_policy(&[], &modes, ExecutionPolicy::ReadOnly),
        ExecutionPolicy::WorkspaceWrite,
        "an unknown mode is skipped, not read as a switch back"
    );
}
