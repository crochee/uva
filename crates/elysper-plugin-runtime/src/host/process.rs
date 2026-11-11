//! The process half of a stdio plugin: spawn it, hand out its pipes,
//! reap it.
//!
//! `std::process::Command`, not a runtime's process API: a plugin's
//! lifecycle is an OS fact, and a library crate here must not name a
//! runtime to observe one.
//!
//! The child lives behind a `Mutex` because killing it is not only
//! `Drop`'s job: the deadline reaper has to kill a wedged plugin from a
//! shared `&self` (see `client`'s module docs).

use std::{
    io,
    process::{Child, ChildStdin, ChildStdout, Command, Stdio},
    sync::Arc,
};

use parking_lot::Mutex;

use crate::host::error::HostError;

/// A spawned plugin process.
///
/// The host owns the plugin's lifetime, so [`kill`](Self::kill) is
/// called on drop as well: an uninstalled plugin does not linger, and a
/// plugin that ignored its stdout being closed does not leak.
///
/// ## Grandchildren
///
/// A plugin's own children inherit its stdout. If one of them keeps the
/// pipe open after the plugin exits, the host's reader thread sees no
/// end-of-file and `Connection::drop`'s join waits for it. The host
/// cannot fix that from here — it would need a process group — so the
/// requirement is on the plugin: either it waits for its children, or
/// they inherit nothing from stdin/stdout.
pub(crate) struct ChildProcess {
    child: Mutex<Child>,
}

impl ChildProcess {
    /// Spawn `argv` with piped stdin and stdout.
    ///
    /// `argv[0]` is the program and the rest are its arguments — an
    /// argv vector rather than a shell string, so a plugin's command
    /// line has no quoting rules to get wrong.
    ///
    /// stderr is inherited, not piped: a plugin's diagnostics belong on
    /// the host's stderr, and a pipe nobody drains could fill up and
    /// block the plugin mid-answer.
    pub(crate) fn spawn(
        argv: &[String],
        env: Option<&std::collections::HashMap<String, String>>,
    ) -> Result<(Arc<Self>, ChildStdin, ChildStdout), HostError> {
        let Some((program, arguments)) = argv.split_first() else {
            return Err(HostError::EmptyCommand);
        };
        let mut child = Command::new(program);
        if let Some(env) = env {
            child.envs(env);
        }
        let mut child = child
            .args(arguments)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .map_err(|source| HostError::Spawn {
                program: program.clone(),
                source,
            })?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| pipe_missing(program, "stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| pipe_missing(program, "stdout"))?;
        Ok((
            Arc::new(Self {
                child: Mutex::new(child),
            }),
            stdin,
            stdout,
        ))
    }

    /// Kill and reap the process.
    ///
    /// Idempotent: the extra `kill` on an already-reaped child fails and
    /// is ignored, which is what a second call needs.
    pub(crate) fn kill(&self) {
        let mut child = self.child.lock();
        let _ = child.kill();
        let _ = child.wait();
    }
}

impl Drop for ChildProcess {
    fn drop(&mut self) {
        self.kill();
    }
}

// A piped stdio handle that `Command` did not hand back. Only reachable
// if the OS refuses the pipe, which spawn would already have reported;
// it is here so the code has no `expect`.
fn pipe_missing(program: &str, pipe: &str) -> HostError {
    HostError::Spawn {
        program: program.to_string(),
        source: io::Error::other(format!("the {pipe} pipe is missing")),
    }
}
