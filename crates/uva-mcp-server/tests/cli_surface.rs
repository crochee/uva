//! The command line's external contract.
//!
//! stdout is the JSON-RPC channel of this binary: a peer reads it as
//! frames, so **nothing else may land there** — not a help text, not
//! a usage error, not a version banner. These tests spawn the real
//! binary and assert the stream and the exit code, because that
//! contract is invisible to a unit test that only inspects the
//! parser's return value.

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_uva-mcp-server"))
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

/// `--help` is a success path: exit 0, the text on stderr, stdout
/// untouched.
#[test]
fn help_exits_zero_with_the_text_on_stderr() {
    let output = run(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    assert!(
        stdout_of(&output).is_empty(),
        "stdout must stay the JSON-RPC channel, got: {}",
        stdout_of(&output)
    );
    let stderr = stderr_of(&output);
    assert!(stderr.contains("Usage"), "{stderr}");
    assert!(stderr.contains("--workspace"), "{stderr}");
}

/// `--version` follows the same rule as `--help`.
#[test]
fn version_exits_zero_with_the_text_on_stderr() {
    let output = run(&["--version"]);
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

/// An unknown flag is a usage error: exit 2, the text on stderr.
#[test]
fn unknown_flag_exits_two_with_the_text_on_stderr() {
    let output = run(&["--nope"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stdout_of(&output).is_empty(),
        "stdout must stay the JSON-RPC channel, got: {}",
        stdout_of(&output)
    );
    assert!(!stderr_of(&output).is_empty());
}

/// A workspace that is not a directory is refused before the protocol
/// loop starts — exit 2, and the protocol never runs.
#[test]
fn a_workspace_that_is_not_a_directory_is_refused() {
    let file = std::env::temp_dir().join("uva-mcp-server-cli-surface");
    std::fs::write(&file, b"not a directory").unwrap();
    let output = run(&["--workspace", file.to_str().unwrap()]);
    let _ = std::fs::remove_file(&file);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        stdout_of(&output).is_empty(),
        "stdout must stay the JSON-RPC channel, got: {}",
        stdout_of(&output)
    );
    assert!(
        stderr_of(&output).contains("is not a directory"),
        "{}",
        stderr_of(&output)
    );
}
