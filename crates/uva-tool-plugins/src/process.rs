//! The one way this crate spawns a process.
//!
//! The `shell` tool runs a command in the foreground, the `job` tool
//! runs one in the background — and both must spawn it the **same**
//! way: the same argv, the same working directory, the same
//! environment, the same process group. A model told "the command
//! runs in the workspace" must get exactly that, whichever tool ran
//! it, so the setup lives here once.
//!
//! POSIX-only: the process group (`cmd.process_group(0)`), the group
//! signal (`libc::kill(-pgid, …)`) and the pipe draining are unix
//! primitives. The non-unix build keeps the module's shape and
//! refuses to spawn — the tools stay registered and answer with a
//! typed refusal instead of failing at the first unix call.

use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::process::Command;

/// How long a reader task may outlive its command before the result is
/// rendered anyway: a surviving grandchild holding the pipe open must
/// not stall the tool result.
const READER_GRACE_SECS: u64 = crate::shell::KILL_GRACE_SECS + 1;

/// The process group a spawned command leads.
///
/// The group is the addressable unit: a timeout or a kill reaches the
/// whole tree, not just the leader.
pub(crate) struct Spawned {
    /// The child process.
    pub(crate) child: tokio::process::Child,
    /// The process group id (`-pgid`).
    pub(crate) pgid: i32,
}

/// Prepare the command a run executes.
///
/// The argv comes from the caller (the shell's confined command, or
/// the job's plain `sh -c`), and this adds the three things every
/// run shares: the working directory, the environment a model does
/// not want to see, and its own process group.
pub(crate) fn prepare(argv: &[String], cwd: &Path) -> Command {
    let mut cmd = Command::new(&argv[0]);
    cmd.args(&argv[1..]);
    // Run inside the workspace (dsh session-cwd semantics); the
    // agent loop feeds `workspace_root` on every dispatch.
    if !cwd.as_os_str().is_empty() {
        cmd.current_dir(cwd);
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
    cmd.stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    cmd
}

/// Spawn the prepared command.
pub(crate) fn spawn(mut cmd: Command) -> std::io::Result<Spawned> {
    let child = cmd.spawn()?;
    let pgid = child.id().unwrap_or(0) as i32;
    Ok(Spawned { child, pgid })
}

/// Signal an entire process group (`-pgid`), falling back to the
/// direct child when the group is already gone. POSIX-only — Windows
/// has no process group kill primitive; the timeout escalation on
/// non-unix platforms uses `kill_on_drop(true)` and waits for the
/// child to exit instead.
pub(crate) fn signal_group(pgid: i32, sig: i32) {
    if pgid <= 0 {
        return;
    }
    // SAFETY: `kill` with a negative pid signals the process group;
    // the pid is the one this crate spawned, so the group is ours.
    unsafe {
        libc::kill(-pgid, sig);
    }
}

/// The exit status's signal, when the process died by one.
pub(crate) fn signal_of(status: &std::process::ExitStatus) -> Option<i32> {
    std::os::unix::process::ExitStatusExt::signal(status)
}

/// A pipe's collected bytes, readable while the command runs.
pub(crate) type Pipe = Arc<Mutex<Vec<u8>>>;

/// Spawn a background task draining one pipe into a shared buffer.
///
/// Concurrent draining is mandatory: a full pipe buffer would
/// otherwise deadlock the child. The chunk is decoded on its own: a
/// multi-byte character split across two 8 KiB reads shows its
/// replacement character in that chunk only, while the settled
/// output — decoded once from the whole buffer — stays intact.
pub(crate) fn spawn_reader<R>(
    mut pipe: R,
    on_chunk: Option<Box<dyn Fn(String) + Send + Sync>>,
) -> (Pipe, tokio::task::JoinHandle<()>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let buffer: Pipe = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&buffer);
    let handle = tokio::spawn(async move {
        let mut chunk = [0u8; 8192];
        loop {
            match tokio::io::AsyncReadExt::read(&mut pipe, &mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Some(on_chunk) = &on_chunk {
                        let text = String::from_utf8_lossy(&chunk[..n]);
                        // A closed channel is the stream's own
                        // business: the run still needs its buffer.
                        on_chunk(text.into_owned());
                    }
                    if let Ok(mut buf) = sink.lock() {
                        buf.extend_from_slice(&chunk[..n]);
                    }
                }
            }
        }
    });
    (buffer, handle)
}

/// Bounded wait for a reader task, then the whole buffer as text.
pub(crate) async fn finish_reader(
    buffer: &Pipe,
    handle: tokio::task::JoinHandle<()>,
) -> String {
    let _ =
        tokio::time::timeout(Duration::from_secs(READER_GRACE_SECS), handle)
            .await;
    buffer
        .lock()
        .map(|buf| String::from_utf8_lossy(&buf).into_owned())
        .unwrap_or_default()
}

/// The bytes a pipe has collected so far.
pub(crate) fn pipe_text(buffer: &Pipe) -> String {
    buffer
        .lock()
        .map(|buf| String::from_utf8_lossy(&buf).into_owned())
        .unwrap_or_default()
}
