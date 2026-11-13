//! The command line's external contract.
//!
//! The TUI's stdout is the **terminal**, not a protocol channel, so
//! clap's conventional help-on-stdout is right here. What the test
//! pins is the exit code and the fact that the binary never touches
//! the terminal before the arguments are parsed.

use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_uva-tui"))
        .args(args)
        .output()
        .expect("the binary must be spawnable")
}

/// `--help` is a success path: exit 0, the text on stdout.
#[test]
fn help_exits_zero_with_the_text_on_stdout() {
    let output = run(&["--help"]);
    assert_eq!(output.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage"), "{stdout}");
    assert!(stdout.contains("--attach"), "{stdout}");
}

/// An unknown flag is a usage error: exit 2, the text on stderr.
#[test]
fn an_unknown_flag_exits_two_with_the_text_on_stderr() {
    let output = run(&["--nope"]);
    assert_eq!(output.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&output.stdout).is_empty(),
        "a usage error must not print on stdout"
    );
    assert!(!String::from_utf8_lossy(&output.stderr).is_empty());
}
