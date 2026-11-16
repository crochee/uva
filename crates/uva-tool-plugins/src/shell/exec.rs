//! Pipe draining for the `shell` tool.
//!
//! A command's stdout and stderr are drained concurrently — a full
//! pipe buffer would deadlock the child — into one shared buffer per
//! pipe, and (when the caller streams) announced chunk by chunk so the
//! client sees the output grow while the command runs.
//!
//! POSIX-only: the buffers are fed by the unix `execute` path
//! (`tokio::process::Command`'s piped stdout / stderr). The non-unix
//! build refuses the call before spawning anything, so nothing here is
//! reachable there.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use tokio::io::AsyncReadExt;
use uva_tool::ToolOutput;

use super::ProgressSink;

/// How long a reader task may outlive its command before the result is
/// rendered anyway: a surviving grandchild holding the pipe open must
/// not stall the tool result.
const READER_GRACE_SECS: u64 = super::KILL_GRACE_SECS + 1;

/// Spawn a background task draining one pipe into a shared buffer.
///
/// Concurrent draining is mandatory: a full pipe buffer would
/// otherwise deadlock the child. With a `progress` sink each read is
/// announced as one chunk before it is buffered, so a long command's
/// output reaches the client while it runs. The chunk is decoded on its
/// own: a multi-byte character split across two 8 KiB reads shows its
/// replacement character in that chunk only, while the settled output —
/// decoded once from the whole buffer — stays intact.
pub(super) fn spawn_reader<R: tokio::io::AsyncRead + Unpin + Send + 'static>(
    mut pipe: R,
    progress: Option<ProgressSink>,
) -> (Arc<Mutex<Vec<u8>>>, tokio::task::JoinHandle<()>) {
    let buffer = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&buffer);
    let handle = tokio::spawn(async move {
        let mut chunk = [0u8; 8192];
        loop {
            match pipe.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Some(progress) = &progress {
                        let text = String::from_utf8_lossy(&chunk[..n]);
                        // A closed channel is the stream's own
                        // business: the run still needs its buffer.
                        let _ = progress.unbounded_send(ToolOutput::text(
                            text.into_owned(),
                        ));
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
pub(super) async fn finish_reader(
    buffer: &Arc<Mutex<Vec<u8>>>,
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
