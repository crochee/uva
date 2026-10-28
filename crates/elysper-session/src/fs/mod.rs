//! The crate's one filesystem offload.
//!
//! `elysper-session` writes and reads `events.jsonl` with
//! `std::fs` — a blocking API. A caller on an async executor must
//! not run it on the thread that is driving futures, and the crate
//! must not name a runtime to move it: `tokio::task::spawn_blocking`
//! would make the whole crate tokio-only by construction.
//!
//! [`FsBackend`] states that offload once, over the [`Blocking`]
//! seam. The default implementation is [`ThreadBlocking`] (a std
//! thread per call), so the sink and the search index work with no
//! runtime at all; a host on a real runtime installs that runtime's
//! adapter instead (the tokio one ships as
//! `elysper_harness_tokio::TokioBlocking`).
//!
//! ## The write's durability contract lives here
//!
//! [`FsBackend::append_lines`] is the one write every append path
//! makes: the whole batch in **one** `write_all`, then **one**
//! `sync_all`, and on a failed write a `set_len` rollback to the
//! file's previous length — so `Ok(())` means "every byte is
//! durable", and an `Err` leaves no half-written row behind. Both
//! promises only hold because the batch reaches the file as one
//! call; a per-row loop would give neither.
//!
//! ## A refusal says what it left behind
//!
//! The two failures are not the same. A refused *write* is rolled
//! back: the file is as it was, and the batch was never written. A
//! refused *barrier* comes after the bytes reached the file: the
//! batch **is** in the file, only its durability is unproven.
//!
//! `AppendFailure` carries that difference, because the sink's
//! idempotency turns on it: a batch that is in the file must not be
//! written again, and a batch that is not must be written again.

use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

use futures::{FutureExt as _, channel::oneshot};
use elysper_core::blocking::{Blocking, ThreadBlocking};

/// A refused batch, and what it left behind.
///
/// The difference is the sink's idempotency: a batch whose bytes
/// reached the file is *written* — a retry must not write it again —
/// and a batch that was rolled back is *not* written, so the retry
/// must write it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct AppendFailure {
    /// What went wrong.
    pub message: String,
    /// Whether the batch's bytes reached the file.
    pub landed: bool,
}

impl AppendFailure {
    /// A batch that is **not** in the file: the write failed and the
    /// file was rolled back to its previous length, or the work never
    /// reported back at all.
    fn rolled_back(message: String) -> Self {
        Self {
            message,
            landed: false,
        }
    }

    /// A batch that **is** in the file: the bytes reached it, and
    /// only the durability barrier failed.
    fn landed(message: String) -> Self {
        Self {
            message,
            landed: true,
        }
    }
}
/// The fate of one dispatched batch write, recorded by the blocking
/// thread that performs the write.
///
/// The awaiting caller can be dropped at any await — a compaction
/// timeout does exactly that to a flush. The thread cannot be
/// cancelled, so it is the one place the write's outcome is known
/// for certain: it records the outcome here before it exits, and a
/// later caller reads it instead of trusting an ordinal mark the
/// dropped caller never got to advance.
///
/// The fate deliberately says nothing about whether the write's
/// closure *started*: a thread that was scheduled but has not run
/// its first instruction is indistinguishable here from one that
/// never will, and guessing the difference is how a reconcile
/// double-writes. The `done` signal (whose sender lives inside the
/// closure) is the arbiter: it resolves `Ok` when the thread
/// finished the write and `Err` when the closure was dropped before
/// it ever ran.
pub(crate) struct WriteFate {
    /// Whether the batch's bytes reached the file. Recorded by the
    /// write's closure; `false` until then, which is also the
    /// truth when the closure never ran.
    landed: AtomicBool,
    /// Whether the durability barrier succeeded too. Meaningful only
    /// when the bytes landed.
    barrier_ok: AtomicBool,
}

impl Default for WriteFate {
    fn default() -> Self {
        Self {
            landed: AtomicBool::new(false),
            barrier_ok: AtomicBool::new(false),
        }
    }
}

impl WriteFate {
    /// Record the write's outcome. Called by the write's closure,
    /// before it lights the `done` signal.
    fn record(&self, landed: bool, barrier_ok: bool) {
        self.landed.store(landed, Ordering::SeqCst);
        self.barrier_ok.store(barrier_ok, Ordering::SeqCst);
    }

    /// Whether the batch's bytes reached the file. Truthful only
    /// after the `done` signal resolved `Ok` (the closure finished);
    /// `false` before that, and for a closure that never ran.
    pub(crate) fn landed(&self) -> bool {
        self.landed.load(Ordering::SeqCst)
    }

    /// Whether the durability barrier succeeded. Meaningful only
    /// when the bytes landed.
    pub(crate) fn barrier_ok(&self) -> bool {
        self.barrier_ok.load(Ordering::SeqCst)
    }
}

/// One dispatched batch write: its [`WriteFate`] (shared with the
/// blocking thread), the `done` signal that thread lights on exit,
/// and the future a caller that still waits can await for the
/// write's result.
pub(crate) struct DispatchedAppend {
    /// The write's outcome, recorded by the blocking thread.
    pub(crate) fate: Arc<WriteFate>,
    /// Fires when the blocking thread exits — the waiter a later
    /// reconciler uses when the original awaiter was dropped
    /// mid-write.
    pub(crate) done: oneshot::Receiver<()>,
    /// The write's result, for a caller that still waits. A caller
    /// dropped at this await abandons it; the fate and `done` above
    /// carry the same information to whoever comes next.
    pub(crate) result: futures::future::BoxFuture<
        'static,
        Result<Result<(), AppendFailure>, String>,
    >,
}

/// Blocking filesystem work, off the caller's thread.
///
/// Cheap to clone — it is the shared [`Blocking`] seam plus nothing
/// else — so every sink and index that needs fs work holds one.
#[derive(Clone)]
pub struct FsBackend {
    blocking: Arc<dyn Blocking>,
}

impl Default for FsBackend {
    /// The runtime-free default: [`ThreadBlocking`].
    fn default() -> Self {
        Self::new(Arc::new(ThreadBlocking))
    }
}

impl FsBackend {
    /// Run this backend's fs work through `blocking`.
    pub fn new(blocking: Arc<dyn Blocking>) -> Self {
        Self { blocking }
    }

    /// Read a whole file.
    ///
    /// A missing file reads as empty rather than as an error: a
    /// session's log is read before its first row is written, and
    /// "no file yet" is that log's empty state, not a failure.
    pub async fn read_file(&self, path: &Path) -> Result<Vec<u8>, String> {
        let path = path.to_path_buf();
        self.run(move || match fs::read(&path) {
            Ok(bytes) => Ok(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Ok(Vec::new())
            }
            Err(error) => Err(format!("read {}: {error}", path.display())),
        })
        .await?
    }

    /// Append `lines` (preceded by `header`, if any) as one durable
    /// batch.
    ///
    /// See the module docs for the write/fsync/rollback contract.
    ///
    /// # Errors
    ///
    /// The failure's message. The message does not say whether the
    /// batch's bytes reached the file — a caller that retries needs
    /// that, and `Self::append_owned` is the seam that carries it.
    pub async fn append_lines(
        &self,
        path: &Path,
        header: Option<&str>,
        lines: &[String],
    ) -> Result<(), String> {
        self.append_owned(
            path.to_path_buf(),
            header.map(str::to_string),
            lines.to_vec(),
        )
        .await
        .map_err(|failure| failure.message)
    }

    /// [`Self::append_lines`] taking the batch by value.
    ///
    /// The work runs inside a `'static` closure, so the batch has to
    /// be owned by the time it gets there. A sink that already owns
    /// its lines (the write path serialises rows into fresh
    /// `String`s) hands them over instead of copying them a second
    /// time on the way in.
    ///
    /// # Errors
    ///
    /// The refusal, and what it left behind: a batch whose bytes
    /// reached the file is *written*, and a batch that was rolled
    /// back is not.
    pub(crate) async fn append_owned(
        &self,
        path: PathBuf,
        header: Option<String>,
        lines: Vec<String>,
    ) -> Result<(), AppendFailure> {
        self.dispatch_append_owned(path, header, lines)
            .result
            .await
            .map_err(AppendFailure::rolled_back)?
    }

    /// [`Self::append_owned`] split into "dispatched" and "awaited"
    /// halves, for the write path whose caller may be dropped at the
    /// await.
    ///
    /// The blocking thread that performs the write records its own
    /// fate into the shared [`WriteFate`] **before** it exits — a
    /// dropped awaiter cannot prevent that — and lights
    /// [`DispatchedAppend::done`] when it finishes. A caller that is
    /// still waiting uses [`DispatchedAppend::result`]; a caller that
    /// arrives after the first one was dropped reads the fate (and,
    /// if the thread is still writing, waits on `done`) instead of
    /// guessing at a file the mark may not describe.
    pub(crate) fn dispatch_append_owned(
        &self,
        path: PathBuf,
        header: Option<String>,
        lines: Vec<String>,
    ) -> DispatchedAppend {
        let fate = Arc::new(WriteFate::default());
        let writer_fate = Arc::clone(&fate);
        let (tx, done) = oneshot::channel::<()>();
        let (result_tx, result_rx) = oneshot::channel();
        let blocking = Arc::clone(&self.blocking);
        let result = blocking
            .run(Box::new(move || {
                let outcome = write_lines_sync(
                    &path,
                    header.as_deref(),
                    lines.iter().map(String::as_str),
                );
                match &outcome {
                    Ok(()) => writer_fate.record(true, true),
                    Err(failure) => {
                        writer_fate.record(failure.landed, false);
                    }
                }
                // The fate is recorded before the signals go out, so
                // whoever wakes on `done` — or on the result — reads
                // a fate that already describes the file.
                let _ = tx.send(());
                let _ = result_tx.send(outcome);
            }))
            .then(|()| async move {
                result_rx.await.map_err(|_abandoned| {
                    "the blocking task dropped its result".to_string()
                })
            });
        DispatchedAppend {
            fate,
            done,
            result: Box::pin(result),
        }
    }

    /// Read up to `max_bytes` bytes from the **end** of a file.
    ///
    /// The bound is the point: a reader that wants the last N rows of a
    /// log pays for those rows instead of the whole file, and the
    /// returned buffer can never exceed what it asked for. The chunk
    /// also reports where the read began, so the caller can tell a
    /// whole first line from a fragment. A missing file reads as empty,
    /// like [`Self::read_file`].
    ///
    /// # Errors
    ///
    /// The path's I/O error, formatted.
    pub async fn read_tail(
        &self,
        path: &Path,
        max_bytes: u64,
    ) -> Result<TailChunk, String> {
        let path = path.to_path_buf();
        self.run(move || {
            let (bytes, start) = read_bounded(&path, End::Tail, max_bytes)?;
            Ok(TailChunk { bytes, start })
        })
        .await?
    }

    /// Read up to `max_bytes` bytes from the **start** of a file —
    /// the counterpart of [`Self::read_tail`] for the few whole-log
    /// facts (a session's first row) that a tail read cannot answer.
    ///
    /// # Errors
    ///
    /// The path's I/O error, formatted.
    pub async fn read_head(
        &self,
        path: &Path,
        max_bytes: u64,
    ) -> Result<Vec<u8>, String> {
        let path = path.to_path_buf();
        self.run(move || read_bounded(&path, End::Head, max_bytes).map(|r| r.0))
            .await?
    }

    /// Re-establish the durability barrier over `path` — one
    /// `sync_all`, no bytes written.
    ///
    /// A batch whose write landed but whose barrier failed leaves
    /// the file written-but-unproven; a retry of that batch owes the
    /// barrier, and this is how it pays.
    ///
    /// # Errors
    ///
    /// The open's or the barrier's I/O error, formatted.
    pub(crate) async fn sync_file(&self, path: &Path) -> Result<(), String> {
        let path = path.to_path_buf();
        self.run(move || {
            let file = fs::OpenOptions::new()
                .append(true)
                .open(&path)
                .map_err(|e| format!("open {}: {e}", path.display()))?;
            file.sync_all().map_err(|e| format!("fsync: {e}"))
        })
        .await?
    }

    /// Run `f` on the blocking seam and bring its value back.
    ///
    /// `Blocking::run` erases the closure's return value, so the
    /// value travels over a oneshot of our own. A panicking (or
    /// otherwise dropped) closure drops the sender with it; the
    /// caller sees an error instead of a future that never resolves.
    pub(crate) async fn run<T>(
        &self,
        f: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, String>
    where
        T: Send + 'static,
    {
        let (tx, rx) = oneshot::channel();
        self.blocking
            .run(Box::new(move || {
                // The receiver is gone when the caller stopped
                // waiting; the work still ran and there is nobody
                // left to tell.
                let _ = tx.send(f());
            }))
            .await;
        rx.await
            .map_err(|_| "the blocking task dropped its result".to_string())
    }
}

/// A bounded read from the end of a file.
///
/// `start` is what makes the chunk interpretable: a read that begins at
/// byte 0 caught the file's first line whole, while a read that begins
/// mid-file starts inside a row and must drop its first fragment. A
/// byte budget alone cannot tell the two apart — a file exactly as long
/// as the budget starts at 0 *and* fills the budget.
#[derive(Debug, Clone)]
pub struct TailChunk {
    /// The bytes read, ending at the file's last byte.
    pub bytes: Vec<u8>,
    /// Offset the read started at; `0` means it reached the file's
    /// start.
    pub start: u64,
}

/// Which end of a file a bounded read starts from.
#[derive(Clone, Copy)]
enum End {
    /// From the first byte.
    Head,
    /// Ending at the last byte.
    Tail,
}

/// Read at most `max_bytes` bytes from the given end of `path`.
///
/// The read is bounded twice: it seeks straight to the start of the
/// range (so a tail read does not walk the file) and takes at most
/// `max_bytes` from there. A missing file is an empty read — the log
/// is read before its first row is written.
fn read_bounded(
    path: &Path,
    end: End,
    max_bytes: u64,
) -> Result<(Vec<u8>, u64), String> {
    use std::io::{Read as _, Seek as _, SeekFrom};

    let mut file = match fs::File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok((Vec::new(), 0));
        }
        Err(error) => return Err(format!("open {}: {error}", path.display())),
    };
    let len = file
        .metadata()
        .map_err(|error| format!("stat {}: {error}", path.display()))?
        .len();
    let start = match end {
        End::Head => 0,
        End::Tail => len.saturating_sub(max_bytes),
    };
    file.seek(SeekFrom::Start(start))
        .map_err(|error| format!("seek {}: {error}", path.display()))?;
    let mut bytes = Vec::with_capacity((len - start).min(max_bytes) as usize);
    file.take(max_bytes)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    Ok((bytes, start))
}

/// Synchronous batch write + single fsync. Concatenates every line
/// into one buffer and hands it to the file in one `write_all`
/// followed by one `sync_all`, so the whole batch is one durable
/// barrier instead of N.
///
/// The two failures are told apart: a refused write is rolled back
/// and leaves the file as it was, while a refused barrier comes after
/// the bytes reached the file.
fn write_lines_sync<'a>(
    path: &Path,
    header: Option<&str>,
    lines: impl IntoIterator<Item = &'a str>,
) -> Result<(), AppendFailure> {
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| {
            AppendFailure::rolled_back(format!("open {}: {e}", path.display()))
        })?;
    let mut buffer = String::new();
    if let Some(header) = header {
        buffer.push_str(header);
        buffer.push('\n');
    }
    for line in lines {
        buffer.push_str(line);
        buffer.push('\n');
    }
    // The file's length before the batch: a failed write is rolled
    // back to it, so the log keeps no half-written row.
    let previous_len = file.metadata().map(|meta| meta.len()).unwrap_or(0);
    if let Err(error) = file.write_all(buffer.as_bytes()) {
        // A rollback that itself fails leaves the file's state
        // unknown, so the batch is reported as landed — a retry
        // that rewrote it could put the same ordinals in twice.
        return Err(match file.set_len(previous_len) {
            Ok(()) => AppendFailure::rolled_back(format!("write: {error}")),
            Err(rollback) => AppendFailure::landed(format!(
                "write: {error}; the rollback to {previous_len} \
                 bytes failed too: {rollback} — the file's state is \
                 unknown, treat the batch as written",
            )),
        });
    }
    // The bytes are in the file; only their durability is unproven.
    file.sync_all()
        .map_err(|e| AppendFailure::landed(format!("fsync: {e}")))?;
    Ok(())
}

#[cfg(test)]
mod tests;
