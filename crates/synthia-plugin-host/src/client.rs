//! The line-delimited JSON-RPC 2.0 channel to one plugin process.
//!
//! A pipe serves one conversation, so the channel is split by
//! direction:
//!
//! - **one reader thread** owns the process's stdout, splits it into
//!   lines, and resolves the request that is waiting for each `id`;
//! - **one deadline reaper thread** owns the clock: every in-flight
//!   request carries an `Instant` deadline, and the reaper expires what
//!   matured;
//! - **one mutex-guarded stdin** serialises writes, which run through
//!   the [`Blocking`] seam so a caller's thread never parses on a pipe.
//!
//! Correlation by `id` is what makes concurrent calls safe: two
//! [`request`](Connection::request)s may be in flight and their answers
//! may come back in any order, because the map — not arrival order —
//! decides who gets what.
//!
//! ## Why the deadline is a reaper, not a sleep per request
//!
//! A `sleep` armed per request would spawn a timer thread per call on
//! every runtime-free implementation, and that thread cannot be
//! cancelled: it runs the full timeout out even when the answer arrived
//! in a millisecond. Steady state that is calls-per-second × timeout
//! threads. One reaper per connection instead waits until the earliest
//! deadline in the map, expires every matured slot, and repeats — a
//! constant two threads per plugin whatever the call rate.
//!
//! ## The write is inside the deadline
//!
//! A plugin that stops reading its stdin fills the pipe (~64 KiB) and
//! then `write_all` blocks, holding the stdin mutex. That is why a
//! request's deadline covers **the write as well as the answer**: both
//! are raced against the request's slot, and the reaper's expiry
//! **kills the process**, which is what makes the blocked `write_all`
//! return `EPIPE` and releases the mutex for the writes queued behind
//! it. A notification (an event) registers a slot for the same reason:
//! it has no answer to wait for, but it must not outlive the deadline
//! either.
//!
//! ## Failure paths
//!
//! A plugin is a separate process and can die at any moment, so every
//! way the channel can end resolves to one of these states, and none of
//! them hangs:
//!
//! | What happened | Pending requests | Later requests |
//! |---|---|---|
//! | stdout reached EOF / read failed | `Unavailable` | `Unavailable`, immediately |
//! | a line that is not JSON / not UTF-8 | `BadRequest` | `BadRequest`, immediately |
//! | a frame larger than the cap | `BadRequest` | `BadRequest`, immediately |
//! | the write failed | `Unavailable` | `Unavailable`, immediately |
//! | a deadline expired | `Timeout`, everything in flight | `Timeout`, immediately (the plugin was killed) |
//!
//! `closed` and `pending` share one lock, which is what removes the race
//! between "a caller registers its slot" and "the reader notices the
//! channel is gone": either the reader fails the slot or the caller sees
//! the closed state. A separate flag would leave the window where a slot
//! is registered that nobody will ever answer, and the call would wait
//! out the whole deadline for a process that was already gone.

use std::{
    collections::HashMap,
    io::{BufRead, BufReader, ErrorKind, Read, Write},
    process::{ChildStdin, ChildStdout},
    sync::{
        Arc,
        atomic::{AtomicI64, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use futures::{
    channel::oneshot,
    future::{Either, select},
};
use parking_lot::{Condvar, Mutex};
use serde_json::{Value, json};
use synthia_core::blocking::Blocking;
use synthia_plugin_protocol::PluginError;

use crate::{error::HostError, process::ChildProcess};

/// The largest frame the host will accept from a plugin.
///
/// A plugin that writes without a newline would otherwise grow the
/// host's line buffer until it runs out of memory; a frame past the cap
/// is a protocol violation, and the channel closes instead.
const MAX_FRAME_BYTES: u64 = 1024 * 1024;

/// What a waiting caller receives.
type Answer = Result<Value, HostError>;

/// One in-flight request.
struct Slot {
    sender: oneshot::Sender<Answer>,
    /// When the reaper gives up on it, on the monotonic clock.
    deadline: Instant,
}

/// The state the reader, the reaper and the callers share.
#[derive(Default)]
struct Shared {
    /// One slot per in-flight request, keyed by its JSON-RPC `id`.
    ///
    /// A notification registers a slot too; it is never sent on the
    /// wire, so no answer can name it, but it gives the reaper
    /// something to expire while the notification's write is wedged.
    pending: HashMap<i64, Slot>,
    /// Why the channel is closed, if it is.
    closed: Option<(PluginError, String)>,
}

impl Shared {
    /// Close the channel, keeping the first reason.
    ///
    /// The first failure is the informative one: a later EOF after a
    /// protocol violation would otherwise overwrite "the plugin wrote
    /// junk" with "the plugin exited", which says less.
    fn close(
        &mut self,
        kind: PluginError,
        detail: String,
    ) -> (PluginError, String) {
        match &self.closed {
            Some(closed) => closed.clone(),
            None => {
                self.closed = Some((kind, detail.clone()));
                (kind, detail)
            }
        }
    }

    /// The earliest deadline among the in-flight requests.
    fn next_deadline(&self) -> Option<Instant> {
        self.pending.values().map(|slot| slot.deadline).min()
    }

    /// Remove every slot whose deadline has passed.
    fn take_expired(&mut self, now: Instant) -> Vec<oneshot::Sender<Answer>> {
        let expired: Vec<i64> = self
            .pending
            .iter()
            .filter(|(_, slot)| slot.deadline <= now)
            .map(|(id, _)| *id)
            .collect();
        let mut senders = Vec::with_capacity(expired.len());
        for id in expired {
            if let Some(slot) = self.pending.remove(&id) {
                senders.push(slot.sender);
            }
        }
        senders
    }
}

/// The JSON-RPC channel to one plugin process.
///
/// Dropping the last handle kills the process, so the channel's
/// lifetime is the plugin's lifetime.
pub(crate) struct Connection {
    plugin: String,
    process: Arc<ChildProcess>,
    stdin: Arc<Mutex<ChildStdin>>,
    shared: Arc<Mutex<Shared>>,
    wake: Arc<Condvar>,
    reader: Option<JoinHandle<()>>,
    reaper: Option<JoinHandle<()>>,
    next_id: AtomicI64,
    blocking: Arc<dyn Blocking>,
    timeout: Duration,
}

impl Connection {
    /// Spawn `argv` and start its reader and reaper threads.
    ///
    /// The reader is a dedicated OS thread rather than a `Blocking`
    /// offload because it has to live as long as the process: it is the
    /// only owner of the plugin's stdout.
    pub(crate) fn spawn(
        plugin: &str,
        argv: &[String],
        blocking: Arc<dyn Blocking>,
        timeout: Duration,
    ) -> Result<Self, HostError> {
        let (process, stdin, stdout) = ChildProcess::spawn(argv)?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let wake = Arc::new(Condvar::new());
        let reader = {
            let shared = Arc::clone(&shared);
            let wake = Arc::clone(&wake);
            let plugin = plugin.to_string();
            thread::spawn(move || read_loop(&plugin, stdout, &shared, &wake))
        };
        let reaper = {
            let shared = Arc::clone(&shared);
            let wake = Arc::clone(&wake);
            let process = Arc::clone(&process);
            let plugin = plugin.to_string();
            thread::spawn(move || {
                reap_loop(&plugin, &shared, &wake, &process, timeout);
            })
        };
        Ok(Self {
            plugin: plugin.to_string(),
            process,
            stdin: Arc::new(Mutex::new(stdin)),
            shared,
            wake,
            reader: Some(reader),
            reaper: Some(reaper),
            next_id: AtomicI64::new(1),
            blocking,
            timeout,
        })
    }

    /// Send a request and await its answer, under the deadline.
    pub(crate) async fn request(
        &self,
        method: &str,
        params: Value,
    ) -> Result<Value, HostError> {
        let (id, receiver) = self.enlist()?;
        // Removes the slot however this future ends. A caller that drops
        // the future (a cancelled tool call) must not leave a deadline
        // behind that would later kill the plugin for nobody.
        let _slot = SlotGuard {
            shared: Arc::clone(&self.shared),
            id,
        };
        let line = frame(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }));
        match self.write_bounded(line, receiver).await {
            Written::Answered(answer) => answer,
            Written::Done(receiver) => self.await_answer(receiver).await,
            Written::Ended(error) => Err(error),
        }
    }

    /// Send a notification: no answer, but still bounded by the
    /// deadline.
    pub(crate) async fn notify(
        &self,
        method: &str,
        params: Value,
    ) -> Result<(), HostError> {
        let (id, receiver) = self.enlist()?;
        let _slot = SlotGuard {
            shared: Arc::clone(&self.shared),
            id,
        };
        let line = frame(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        }));
        match self.write_bounded(line, receiver).await {
            Written::Answered(_) | Written::Done(_) => Ok(()),
            Written::Ended(error) => Err(error),
        }
    }

    // Take a slot for one wire message and return its id and receiver.
    fn enlist(&self) -> Result<(i64, oneshot::Receiver<Answer>), HostError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = oneshot::channel();
        let deadline = Instant::now() + self.timeout;
        {
            let mut shared = self.shared.lock();
            if let Some((kind, detail)) = shared.closed.clone() {
                return Err(self.error(kind, detail));
            }
            shared.pending.insert(id, Slot { sender, deadline });
        }
        self.wake.notify_one();
        Ok((id, receiver))
    }

    // Race the write against the slot.
    //
    // The write can block on a full pipe, and the slot can end because
    // the plugin answered early, because the channel died, or because
    // the reaper expired it — in the last case the reaper has also
    // killed the process, which is what unblocks the write.
    async fn write_bounded(
        &self,
        line: String,
        receiver: oneshot::Receiver<Answer>,
    ) -> Written {
        match select(Box::pin(self.write_line(line)), receiver).await {
            Either::Left((Ok(()), receiver)) => Written::Done(receiver),
            Either::Left((Err(error), _receiver)) => Written::Ended(error),
            // The slot resolved while the write was still in flight. A
            // plugin can answer before this side observes the write
            // completing, so a real answer must not be dropped here.
            //
            // Dropping the future does not stop the work: the write runs
            // on a `Blocking` thread, which is not cancellable, so it may
            // still be inside `write_all` holding the stdin lock after
            // this returns. Nothing expires it — its slot is gone — and
            // the lock is released when the write finishes, when a later
            // request's expiry kills the plugin, or when `Drop` does.
            // That is bounded, and cancelling it from here is not an
            // option the seam offers.
            Either::Right((Ok(answer), _write)) => Written::Answered(answer),
            Either::Right((Err(_), _write)) => {
                Written::Ended(self.unavailable())
            }
        }
    }

    // Wait for the answer to a request whose write has landed.
    async fn await_answer(
        &self,
        receiver: oneshot::Receiver<Answer>,
    ) -> Answer {
        receiver.await.unwrap_or_else(|_| Err(self.unavailable()))
    }

    // The error a closed channel reports.
    fn error(&self, kind: PluginError, detail: String) -> HostError {
        HostError::Plugin {
            plugin: self.plugin.clone(),
            kind,
            detail,
        }
    }

    // The error an unanswered request reports.
    fn unavailable(&self) -> HostError {
        self.error(
            PluginError::Unavailable,
            "the plugin channel closed without an answer".to_string(),
        )
    }

    // Write one line through the blocking seam.
    //
    // The closure's result comes back through a slot rather than the
    // seam's return value, because `Blocking::run` erases it by design.
    async fn write_line(&self, line: String) -> Result<(), HostError> {
        let written = Arc::new(Mutex::new(None));
        let slot = Arc::clone(&written);
        let stdin = Arc::clone(&self.stdin);
        let blocking = Arc::clone(&self.blocking);
        blocking
            .run(Box::new(move || {
                let mut stdin = stdin.lock();
                let outcome = stdin
                    .write_all(line.as_bytes())
                    .and_then(|()| stdin.flush());
                *slot.lock() = Some(outcome);
            }))
            .await;
        let outcome = written.lock().take();
        match outcome {
            Some(Ok(())) => Ok(()),
            Some(Err(source)) => {
                let detail = format!("writing to the plugin failed: {source}");
                self.fail_all(PluginError::Unavailable, detail.clone());
                Err(self.error(PluginError::Unavailable, detail))
            }
            None => Err(self.error(
                PluginError::Internal,
                "the write never ran".to_string(),
            )),
        }
    }

    // Close the channel and fail every pending request.
    fn fail_all(&self, kind: PluginError, detail: String) {
        fail(&self.shared, &self.wake, &self.plugin, kind, detail);
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        // Killing the process ends the reader (it sees end-of-file) and
        // wakes the reaper through the closed state, so neither join
        // can hang on a plugin that never closes its stdout.
        self.process.kill();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        if let Some(reaper) = self.reaper.take() {
            let _ = reaper.join();
        }
    }
}

// What a bounded write ended as.
enum Written {
    /// The write landed; the answer may still be on its way.
    Done(oneshot::Receiver<Answer>),
    /// The slot resolved while the write was in flight.
    Answered(Answer),
    /// The deadline or a channel failure ended the wait first.
    Ended(HostError),
}

// Removes one request's slot when its future ends, however it ends.
struct SlotGuard {
    shared: Arc<Mutex<Shared>>,
    id: i64,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        self.shared.lock().pending.remove(&self.id);
    }
}

// Close the channel and resolve every slot with the same failure.
fn fail(
    shared: &Mutex<Shared>,
    wake: &Condvar,
    plugin: &str,
    kind: PluginError,
    detail: String,
) {
    let mut state = shared.lock();
    let (kind, detail) = state.close(kind, detail);
    let pending = std::mem::take(&mut state.pending);
    drop(state);
    wake.notify_all();
    for (_, slot) in pending {
        let _ = slot.sender.send(Err(HostError::Plugin {
            plugin: plugin.to_string(),
            kind,
            detail: detail.clone(),
        }));
    }
}

// Enforce every in-flight deadline, one thread per connection.
fn reap_loop(
    plugin: &str,
    shared: &Mutex<Shared>,
    wake: &Condvar,
    process: &ChildProcess,
    timeout: Duration,
) {
    loop {
        let mut state = shared.lock();
        if state.closed.is_some() {
            return;
        }
        // Wait until the earliest deadline; no deadline at all means the
        // channel is idle and the reaper blocks until a request arrives.
        let wait = state
            .next_deadline()
            .map(|deadline| deadline.saturating_duration_since(Instant::now()));
        match wait {
            None => {
                wake.wait(&mut state);
                continue;
            }
            Some(remainder) if !remainder.is_zero() => {
                wake.wait_for(&mut state, remainder);
                continue;
            }
            Some(_) => {}
        }
        let expired = state.take_expired(Instant::now());
        drop(state);
        let detail = format!(
            "the plugin missed its deadline ({}ms); the host killed it",
            timeout.as_millis()
        );
        // Resolve the expired slots *before* the kill. The kill is what
        // releases a writer blocked on a full pipe, and the caller's
        // `select` is left-biased: if the released writer reached its
        // `EPIPE` first, the caller would report `Unavailable` — a broken
        // pipe — for a deadline that did expire. Deciding the diagnosis
        // first removes the race instead of leaving the message to the
        // scheduler.
        for sender in expired {
            let _ = sender.send(Err(HostError::Plugin {
                plugin: plugin.to_string(),
                kind: PluginError::Timeout,
                detail: detail.clone(),
            }));
        }
        fail(shared, wake, plugin, PluginError::Timeout, detail);
        // Only now. A plugin that missed a deadline is wedged, and a
        // wedged plugin is exactly what holds a write inside `write_all`
        // while the pipe is full: the kill is the only thing that
        // releases that write and the stdin lock it holds.
        process.kill();
    }
}

// Own the plugin's stdout for the life of the process.
fn read_loop(
    plugin: &str,
    stdout: ChildStdout,
    shared: &Mutex<Shared>,
    wake: &Condvar,
) {
    let mut reader = BufReader::new(stdout);
    loop {
        let mut line = String::new();
        let mut limited = reader.by_ref().take(MAX_FRAME_BYTES + 1);
        match limited.read_line(&mut line) {
            Ok(0) => break,
            Ok(_) if line.len() as u64 > MAX_FRAME_BYTES => {
                fail(
                    shared,
                    wake,
                    plugin,
                    PluginError::BadRequest,
                    format!(
                        "the plugin wrote a frame larger than \
                         {MAX_FRAME_BYTES} bytes"
                    ),
                );
                return;
            }
            Ok(_) => {
                if !dispatch(plugin, &line, shared, wake) {
                    return;
                }
            }
            Err(error) if error.kind() == ErrorKind::InvalidData => {
                fail(
                    shared,
                    wake,
                    plugin,
                    PluginError::BadRequest,
                    "the plugin wrote a frame that is not UTF-8".to_string(),
                );
                return;
            }
            Err(_) => break,
        }
    }
    fail(
        shared,
        wake,
        plugin,
        PluginError::Unavailable,
        "the plugin closed its stdout".to_string(),
    );
}

// Handle one line. Returns whether the channel is still usable.
fn dispatch(
    plugin: &str,
    line: &str,
    shared: &Mutex<Shared>,
    wake: &Condvar,
) -> bool {
    let line = line.trim();
    if line.is_empty() {
        return true;
    }
    let Ok(message) = serde_json::from_str::<Value>(line) else {
        // Framing is gone: no later line can be trusted to belong to the
        // request it names, so the channel closes instead of guessing.
        fail(
            shared,
            wake,
            plugin,
            PluginError::BadRequest,
            format!("the plugin wrote a line that is not JSON: {line}"),
        );
        return false;
    };
    let Some(id) = message.get("id").and_then(Value::as_i64) else {
        // A notification from the plugin, or a frame without an id:
        // nothing was asked, so nothing is answered.
        return true;
    };
    let sender = shared.lock().pending.remove(&id);
    let Some(sender) = sender else {
        // A late answer for a request that already timed out, or an id
        // the host never sent.
        return true;
    };
    let _ = sender.sender.send(answer(plugin, &message));
    true
}

// One protocol frame: a JSON object and its line terminator.
//
// The newline *is* the framing — the plugin reads whole lines — so it
// belongs to the write, not to the JSON rendering.
fn frame(message: Value) -> String {
    let mut line = message.to_string();
    line.push('\n');
    line
}

// Turn one response frame into the answer its caller receives.
fn answer(plugin: &str, message: &Value) -> Answer {
    // `"error": null` is what several JSON-RPC libraries emit for
    // success, so a null error is not an error.
    if let Some(error) = message.get("error").filter(|error| !error.is_null()) {
        let code = error.get("code").and_then(Value::as_i64).unwrap_or(0);
        let detail = error
            .get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| error.to_string());
        return Err(HostError::Plugin {
            plugin: plugin.to_string(),
            kind: kind_for(code),
            detail,
        });
    }
    match message.get("result") {
        Some(result) => Ok(result.clone()),
        None => Err(HostError::bad_request(
            plugin,
            format!(
                "the answer carries neither `result` nor `error`: {message}"
            ),
        )),
    }
}

// Classify a JSON-RPC error code with the wire's stable codes.
fn kind_for(code: i64) -> PluginError {
    match code {
        // "Internal error": the plugin failed, the protocol was fine.
        -32603 => PluginError::Internal,
        // Parse error, invalid request, method not found, invalid
        // params: the two sides disagree about the protocol itself.
        -32700..=-32600 => PluginError::BadRequest,
        // Anything else is the plugin's own taxonomy.
        _ => PluginError::Internal,
    }
}
