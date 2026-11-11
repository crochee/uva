#![allow(dead_code, clippy::useless_conversion, clippy::collapsible_if)]
use std::{collections::VecDeque, path::PathBuf, sync::Mutex};

use super::*;

/// Recording fake: each call appends its argv to `recorded`
/// and pops the next canned outcome.
#[derive(Default)]
struct FakeRunner {
    recorded: Mutex<Vec<Vec<String>>>,
    outcomes: Mutex<VecDeque<CommandOutcome>>,
}

impl FakeRunner {
    fn with_outcomes(outcomes: Vec<CommandOutcome>) -> Self {
        let deque: VecDeque<CommandOutcome> = outcomes.into();
        // Test pushes outcomes in invocation order; pop_front
        // takes from the front, so we leave the deque as-is.
        Self {
            recorded: Mutex::new(Vec::new()),
            outcomes: Mutex::new(deque),
        }
    }

    fn recorded(&self) -> Vec<Vec<String>> {
        self.recorded.lock().expect("lock").clone()
    }
}
impl CommandRunner for FakeRunner {
    fn run(
        &self,
        argv: &[String],
        _cwd: &Path,
        _cancel: Arc<dyn CancelToken>,
    ) -> CommandOutcome {
        self.recorded.lock().expect("lock").push(argv.to_vec());
        self.outcomes
            .lock()
            .expect("lock")
            .pop_front()
            .unwrap_or_else(|| CommandOutcome {
                status: synthetic_failure_status(),
                stdout: String::new(),
                stderr: String::new(),
                reason: CommandStopReason::Exited,
            })
    }
}
struct StaticCancelToken {
    cancelled: std::sync::atomic::AtomicBool,
}

impl StaticCancelToken {
    fn new(cancelled: bool) -> Self {
        Self {
            cancelled: std::sync::atomic::AtomicBool::new(cancelled),
        }
    }
}

impl CancelToken for StaticCancelToken {
    fn is_cancelled(&self) -> bool {
        self.cancelled.load(std::sync::atomic::Ordering::Acquire)
    }

    fn cancel(&self) {
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
    }

    fn cancelled(&self) -> Pin<Box<dyn Future<Output = ()> + Send + '_>> {
        Box::pin(std::future::pending())
    }
}

fn never_cancel() -> Arc<dyn CancelToken> {
    Arc::new(StaticCancelToken::new(false))
}

fn pre_cancel() -> Arc<dyn CancelToken> {
    Arc::new(StaticCancelToken::new(true))
}

fn success_outcome() -> CommandOutcome {
    CommandOutcome {
        status: success_status(),
        stdout: "ok".to_string(),
        stderr: String::new(),
        reason: CommandStopReason::Exited,
    }
}

fn failure_outcome(code: i32) -> CommandOutcome {
    CommandOutcome {
        status: code_status(code),
        stdout: "out".to_string(),
        stderr: "boom".to_string(),
        reason: CommandStopReason::Exited,
    }
}

fn killed_outcome() -> CommandOutcome {
    CommandOutcome {
        status: synthetic_failure_status(),
        stdout: "partial".to_string(),
        stderr: String::new(),
        reason: CommandStopReason::Killed,
    }
}

fn temp_dir() -> PathBuf {
    std::env::temp_dir()
}

#[cfg(unix)]
fn success_status() -> ExitStatus {
    // WEXITED with exit code 0: `from_raw(0)`.
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(0)
}
#[cfg(not(unix))]
fn success_status() -> ExitStatus {
    ExitStatus::default()
}

#[cfg(unix)]
fn code_status(code: i32) -> ExitStatus {
    // WEXITED status on Unix: shift the exit code into bits 8-15
    // so WIFEXITED is true and `code()` returns `Some(code)`.
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw((code & 0xff) << 8)
}

#[test]
fn empty_command_is_rejected_before_spawn() {
    let runner = FakeRunner::default();
    let err = run_gate(
        &GateSpec {
            command: vec![],
            timeout: None,
        },
        &temp_dir(),
        &runner,
        never_cancel(),
    )
    .expect_err("empty argv must error");
    assert!(matches!(err, GateError::EmptyCommand));
    assert!(runner.recorded().is_empty());
}

#[test]
fn missing_working_directory_is_rejected_before_spawn() {
    let runner = FakeRunner::default();
    let bogus = PathBuf::from("/nonexistent/please/no/such/dir");
    let err = run_gate(&GateSpec::new("true"), &bogus, &runner, never_cancel())
        .expect_err("missing cwd must error");
    assert!(matches!(err, GateError::MissingWorkingDirectory { .. }));
    assert!(runner.recorded().is_empty());
}

#[test]
fn successful_exit_yields_pass_verdict() {
    let runner = FakeRunner::with_outcomes(vec![success_outcome()]);
    let verdict = run_gate(
        &GateSpec::new("cargo").arg("test"),
        &temp_dir(),
        &runner,
        never_cancel(),
    )
    .expect("run");
    assert!(verdict.is_pass());
    assert_eq!(
        runner.recorded(),
        vec![vec!["cargo".to_string(), "test".to_string()]]
    );
}

#[test]
fn non_zero_exit_yields_fail_with_captured_output() {
    let runner = FakeRunner::with_outcomes(vec![failure_outcome(2)]);
    let verdict =
        run_gate(&GateSpec::new("make"), &temp_dir(), &runner, never_cancel())
            .expect("run");
    let GateVerdict::Fail {
        stdout,
        stderr,
        status,
    } = verdict
    else {
        panic!("expected Fail");
    };
    assert_eq!(stdout, "out");
    assert_eq!(stderr, "boom");
    assert_eq!(status, 2);
}
#[test]
fn cancellation_yields_killed_not_pass() {
    let runner = FakeRunner::with_outcomes(vec![killed_outcome()]);
    let verdict =
        run_gate(&GateSpec::new("sleep"), &temp_dir(), &runner, pre_cancel())
            .expect("run");
    assert!(matches!(verdict, GateVerdict::Killed { .. }));
    assert!(verdict.is_failure());
    assert!(!verdict.is_pass(), "killed must NEVER read as passing");
}

#[test]
fn run_gate_produces_exactly_one_verdict() {
    let runner =
        FakeRunner::with_outcomes(vec![failure_outcome(3), success_outcome()]);
    let first =
        run_gate(&GateSpec::new("a"), &temp_dir(), &runner, never_cancel())
            .expect("first");
    let second =
        run_gate(&GateSpec::new("b"), &temp_dir(), &runner, never_cancel())
            .expect("second");
    assert!(matches!(first, GateVerdict::Fail { status: 3, .. }));
    assert!(second.is_pass());
}

#[test]
fn apply_gate_returns_child_text_on_pass() {
    let text = "child did the thing";
    let out = apply_gate_to_output(Some(text), &GateVerdict::Pass, "verify");
    assert!(!out.is_error.unwrap_or(false));
    let rendered = format!("{:?}", out.content);
    assert!(rendered.contains(text));
}

#[test]
fn apply_gate_marks_failure_as_error_with_prefix() {
    let verdict = GateVerdict::Fail {
        stdout: "out".to_string(),
        stderr: "boom".to_string(),
        status: 2,
    };
    let out = apply_gate_to_output(Some("child text"), &verdict, "verify");
    let rendered = format!("{:?}", out.content);
    assert!(rendered.contains("failed gate"));
    assert!(rendered.contains("exit 2"));
    assert!(rendered.contains("verify"));
}

#[test]
fn verdict_summary_names_variants() {
    assert_eq!(verdict_summary(&GateVerdict::Pass), "pass");
    assert_eq!(
        verdict_summary(&GateVerdict::Fail {
            stdout: String::new(),
            stderr: String::new(),
            status: 7
        }),
        "fail (exit 7)"
    );
    assert_eq!(
        verdict_summary(&GateVerdict::Timeout {
            stdout: String::new(),
            stderr: String::new()
        }),
        "timeout"
    );
    assert_eq!(
        verdict_summary(&GateVerdict::Killed {
            stdout: String::new(),
            stderr: String::new()
        }),
        "killed"
    );
}

#[test]
fn gate_spec_builder_chains_args_and_timeout() {
    let spec = GateSpec::new("cargo")
        .arg("test")
        .arg("-p")
        .arg("elysper-harness")
        .with_timeout(Duration::from_secs(30));
    assert_eq!(
        spec.command,
        vec![
            "cargo".to_string(),
            "test".to_string(),
            "-p".to_string(),
            "elysper-harness".to_string()
        ]
    );
    assert_eq!(spec.timeout, Some(Duration::from_secs(30)));
    assert_eq!(spec.label(), "cargo");
}

#[test]
fn from_string_slices_yields_one_arg_command() {
    let spec: GateSpec = "make".into();
    assert_eq!(spec.command, vec!["make".to_string()]);
    assert_eq!(spec.timeout, None);
}

#[test]
fn from_vec_string_preserves_argv() {
    let spec: GateSpec = vec![
        "cargo".to_string(),
        "fmt".to_string(),
        "--check".to_string(),
    ]
    .into();
    assert_eq!(
        spec.command,
        vec![
            "cargo".to_string(),
            "fmt".to_string(),
            "--check".to_string()
        ]
    );
    assert_eq!(spec.timeout, None);
}
