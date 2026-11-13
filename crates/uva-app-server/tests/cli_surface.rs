//! The command line's external contract.
//!
//! The `core` mode speaks JSON-RPC 2.0 on stdout, so **nothing else
//! may land there** — not a help text, not a version banner, not a
//! usage error. These tests spawn the real binary and assert the
//! stream and the exit code, because that contract is invisible to a
//! unit test that only inspects the parser's return value.
//!
//! The other modes are an ordinary CLI: their help goes to stdout,
//! the way every other command-line tool does it.

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_uva-app-server"))
        .args(args)
        .output()
        .expect("the binary must be spawnable")
}

fn stdout_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr_of(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// The core's help is a success path: exit 0, the text on stderr,
/// stdout untouched.
#[test]
fn the_core_help_exits_zero_with_the_text_on_stderr() {
    let output = run(&["core", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        stdout_of(&output).is_empty(),
        "stdout must stay the JSON-RPC channel, got: {}",
        stdout_of(&output)
    );
    let stderr = stderr_of(&output);
    assert!(stderr.contains("Usage"), "{stderr}");
    assert!(stderr.contains("--directory"), "{stderr}");
}

/// The core's version follows the same rule as its help.
#[test]
fn the_core_version_exits_zero_with_the_text_on_stderr() {
    let output = run(&["core", "--version"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        stdout_of(&output).is_empty(),
        "stdout must stay the JSON-RPC channel, got: {}",
        stdout_of(&output)
    );
    assert!(
        stderr_of(&output).contains(env!("CARGO_PKG_VERSION")),
        "{}",
        stderr_of(&output)
    );
}

/// An unknown flag is a usage error: exit 2, nothing on stdout.
#[test]
fn an_unknown_flag_exits_two() {
    let output = run(&["--nope"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stdout_of(&output).is_empty(),
        "stdout must stay the JSON-RPC channel, got: {}",
        stdout_of(&output)
    );
    assert!(!stderr_of(&output).is_empty());
}

/// The gateway is an ordinary CLI: its help goes to stdout.
#[test]
fn the_gateway_help_goes_to_stdout() {
    let output = run(&["serve", "--help"]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = stdout_of(&output);
    assert!(stdout.contains("Usage"), "{stdout}");
    assert!(stdout.contains("--bind"), "{stdout}");
}

/// The top-level help describes the CLI surface, so it goes to stdout
/// like any other command-line tool's.
#[test]
fn the_top_level_help_goes_to_stdout() {
    let output = run(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = stdout_of(&output);
    assert!(stdout.contains("Usage"), "{stdout}");
    assert!(stdout.contains("core"), "{stdout}");
}
