//! The stdio pump: Content-Length frames in, Content-Length frames
//! out.
//!
//! stdout carries the protocol and nothing else — the log stream goes
//! to stderr (see [`uva::telemetry::LogWriter::Stderr`]), so a
//! peer reading stdout never sees a log line.
//!
//! # Concurrency
//!
//! The read loop spawns a task per frame, so a slow request does not
//! hold the queue: two requests may be in flight at once and their
//! responses may arrive in either order. A client matches on `id`.
//!
//! Every envelope the process emits — responses and notifications
//! alike — goes through one channel, so one writer owns stdout and
//! two frames never interleave.
//!
//! # Cancellation and deadlines
//!
//! Each answered frame is registered in the [`Inflight`] table with a
//! cancel token and (if configured) a deadline. A `$/cancelRequest`
//! for its id, or its deadline arriving, removes the entry; a task
//! whose entry is gone writes no response. See [`super::inflight`].
use std::{
    io::{BufRead, ErrorKind, Read},
    sync::Arc,
    time::{Duration, Instant},
};

use serde_json::{Value, json};
use tokio::{io::AsyncWrite, sync::mpsc};
use uva::core::CancelToken;

use super::{
    APPLICATION_ERROR,
    INVALID_REQUEST,
    Outbound,
    PARSE_ERROR,
    RpcRouter,
    error_frame,
    framing::{
        DEFAULT_MAX_FRAME_SIZE,
        FrameError,
        read_frame_blocking,
        write_frame,
    },
    inflight::Inflight,
};
use crate::state::AppState;

/// How long a shutdown waits for in-flight request handlers before
/// aborting them.
///
/// stdin closing is a normal peer exit, but a handler can be waiting
/// on a provider that never answers; without a bound the process
/// would never reach its terminal drain (the session marker that
/// proves the controllers flushed). Two seconds covers a handler that
/// is merely between polls; anything longer is abandoned.
pub const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

/// How the pump reads and writes.
pub struct StioOptions {
    /// The largest frame the core accepts from its peer.
    pub max_frame_size: usize,
    /// How long one request may run before it is answered with a
    /// `timeout` error and removed from the in-flight table.
    ///
    /// `None` (the default) means no deadline; cancellation still
    /// works.
    pub request_timeout: Option<Duration>,
    /// How long the pump waits for in-flight handlers after stdin
    /// closes before aborting them.
    ///
    /// See [`DEFAULT_SHUTDOWN_GRACE`]; the default is two seconds.
    pub shutdown_grace: Duration,
}

impl Default for StioOptions {
    fn default() -> Self {
        Self {
            max_frame_size: DEFAULT_MAX_FRAME_SIZE,
            request_timeout: None,
            shutdown_grace: DEFAULT_SHUTDOWN_GRACE,
        }
    }
}

/// One frame handed from the reader thread to the async read loop.
enum Inbound {
    /// A decoded JSON text frame, ready to dispatch.
    Frame(String),
    /// A frame the codec rejected before it could be parsed. The
    /// envelope is written straight to the peer; there is no `id` to
    /// correlate because the payload was not decoded.
    Rejected(Value),
}

/// The stdio transport.
pub struct StioPump {
    state: Arc<AppState>,
    router: Arc<RpcRouter>,
    outbound_rx: mpsc::UnboundedReceiver<Value>,
    options: StioOptions,
    inflight: Arc<Inflight>,
}

impl StioPump {
    /// Build the pump with the default options.
    ///
    /// `outbound_rx` is the receiving half of the channel the
    /// process's [`Outbound`] handle writes to; whoever builds the
    /// channel hands the sender to [`crate::state::AppState`] and the
    /// receiver here.
    pub fn new(
        state: Arc<AppState>,
        router: Arc<RpcRouter>,
        outbound_rx: mpsc::UnboundedReceiver<Value>,
    ) -> Self {
        Self::with_options(state, router, outbound_rx, StioOptions::default())
    }

    /// Build the pump with explicit framing and deadline options.
    pub fn with_options(
        state: Arc<AppState>,
        router: Arc<RpcRouter>,
        outbound_rx: mpsc::UnboundedReceiver<Value>,
        options: StioOptions,
    ) -> Self {
        let inflight = Arc::new(Inflight::new(options.request_timeout));
        Self {
            state,
            router,
            outbound_rx,
            options,
            inflight,
        }
    }

    /// Run until stdin closes.
    pub async fn run(self, outbound: Outbound) -> std::io::Result<()> {
        self.run_with(|| std::io::stdin().lock(), tokio::io::stdout(), outbound)
            .await
    }

    /// Run against an injected reader and writer.
    ///
    /// `open_input` runs **inside** the dedicated reader thread: the
    /// process stdin is locked there, not on the caller's thread, so
    /// the pump never parks a blocking-pool task (see the comment in
    /// [`Self::run`]). Tests pass a cursor and a duplex here.
    pub(crate) async fn run_with<R, W, F>(
        self,
        open_input: F,
        mut output: W,
        outbound: Outbound,
    ) -> std::io::Result<()>
    where
        F: FnOnce() -> R + Send + 'static,
        R: BufRead + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let StioPump {
            state,
            router,
            mut outbound_rx,
            options,
            inflight,
        } = self;
        // One writer owns stdout. The shutdown signal exists because
        // the channel does not close on its own: `AppState` holds an
        // `Outbound` sender for notifications, so waiting for every
        // sender to drop would keep the pump alive after the peer
        // closed stdin. On the signal the writer flushes what is
        // already queued and stops.
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
        let mut writer = tokio::spawn(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = &mut shutdown_rx => {
                        while let Ok(envelope) = outbound_rx.try_recv() {
                            if !write_envelope(&mut output, &envelope).await {
                                return;
                            }
                        }
                        return;
                    }
                    envelope = outbound_rx.recv() => {
                        let Some(envelope) = envelope else {
                            return;
                        };
                        if !write_envelope(&mut output, &envelope).await {
                            return;
                        }
                    }
                }
            }
        });
        // The readiness announcement. A peer waits for it before it
        // sends anything: the boot (providers, tools, skills, the
        // session registry) has finished by the time it appears.
        outbound.notify(
            "ready",
            json!({
                "version": crate::build_info::VERSION,
                "workspace_root": state.workspace_root.to_string_lossy(),
            }),
        );
        // stdin is read on a dedicated OS thread, not
        // `tokio::io::stdin()`: the latter parks a *blocking-pool*
        // task for as long as the peer holds the pipe open, and a
        // runtime shutdown waits for blocking tasks — so a SIGTERM
        // would log "core shutting down" and then hang until the peer
        // closed stdin. A plain thread is not waited on; the process
        // exit ends it.
        let (inbound_tx, mut inbound_rx) =
            tokio::sync::mpsc::unbounded_channel::<Inbound>();
        let max_frame_size = options.max_frame_size;
        std::thread::spawn(move || {
            read_frames(open_input(), max_frame_size, &inbound_tx);
        });
        let mut tasks = tokio::task::JoinSet::new();
        while let Some(inbound) = inbound_rx.recv().await {
            match inbound {
                Inbound::Rejected(envelope) => outbound.send(envelope),
                Inbound::Frame(text) => {
                    handle_frame(
                        &state, &router, &outbound, &inflight, &mut tasks,
                        &text,
                    );
                }
            }
        }
        // stdin closed. A client may have half-closed its write side
        // after sending requests and still be reading: let the
        // in-flight requests finish so their answers are queued
        // before the writer stops — but only for the shutdown grace.
        // A handler stuck on a dead provider must not hold the
        // process past that, or the terminal session drain never
        // runs.
        let grace = options.shutdown_grace;
        let joined = tokio::time::timeout(grace, async {
            while let Some(outcome) = tasks.join_next().await {
                if let Err(error) = outcome {
                    tracing::error!(%error, "a request task failed");
                }
            }
        })
        .await;
        if joined.is_err() {
            let in_flight = tasks.len();
            tracing::warn!(
                in_flight,
                grace_seconds = grace.as_secs_f64(),
                "shutdown grace expired; aborting in-flight requests"
            );
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
        }
        // The process keeps an `Outbound` handle alive for
        // notifications, so the channel does not close on its own;
        // the signal is what ends the writer after it drains what is
        // already queued.
        drop(outbound);
        drop(state);
        let _ = shutdown_tx.send(());
        if tokio::time::timeout(grace, &mut writer).await.is_err() {
            tracing::warn!("the frame writer did not drain; aborting it");
            writer.abort();
            let _ = writer.await;
        }
        Ok(())
    }
}

/// Encode one envelope and write it as a frame.
///
/// Returns `false` when stdout is gone, which ends the writer task;
/// an encode failure is logged and skipped (the stream is still
/// aligned).
async fn write_envelope<W: AsyncWrite + Unpin>(
    output: &mut W,
    envelope: &Value,
) -> bool {
    match serde_json::to_vec(envelope) {
        Ok(payload) => write_frame(output, &payload).await.is_ok(),
        Err(error) => {
            tracing::error!(%error, "could not encode an outbound frame");
            true
        }
    }
}

/// Decode frames on the reader thread until the stream ends.
fn read_frames<R: BufRead>(
    mut reader: R,
    max_frame_size: usize,
    inbound_tx: &mpsc::UnboundedSender<Inbound>,
) {
    loop {
        match read_frame_blocking(&mut reader, max_frame_size) {
            Ok(bytes) => {
                let Ok(text) = String::from_utf8(bytes) else {
                    let envelope = error_frame(
                        Value::Null,
                        PARSE_ERROR,
                        "the frame is not UTF-8",
                    );
                    if inbound_tx.send(Inbound::Rejected(envelope)).is_err() {
                        return;
                    }
                    continue;
                };
                if inbound_tx.send(Inbound::Frame(text)).is_err() {
                    return;
                }
            }
            Err(error @ FrameError::TooLarge { length, .. }) => {
                // The payload was never read; discard it so the next
                // frame starts at a boundary, then keep serving. A
                // peer that closes or stalls during the discard ends
                // the transport.
                if !discard(&mut reader, length) {
                    return;
                }
                if inbound_tx
                    .send(Inbound::Rejected(framing_error_frame(&error)))
                    .is_err()
                {
                    return;
                }
            }
            Err(error) if error.is_end_of_stream() => return,
            Err(error) => {
                // A missing or malformed header leaves no boundary to
                // resynchronise on, so the transport closes — but the
                // peer still gets one stable error frame first.
                tracing::error!(
                    kind = error.kind(),
                    %error,
                    "unreadable frame; closing the stdio transport"
                );
                let envelope = framing_error_frame(&error);
                let _ = inbound_tx.send(Inbound::Rejected(envelope));
                return;
            }
        }
    }
}

/// A protocol error the peer can read before the transport closes.
///
/// The stable kind (`missing_header`, `bad_header`,
/// `invalid_content_length`, `too_large`) travels in `error.data.code`
/// so a client can branch on it without parsing the message.
fn framing_error_frame(error: &FrameError) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": Value::Null,
        "error": {
            "code": INVALID_REQUEST,
            "message": format!("framing error: {error}"),
            "data": { "code": error.kind() },
        },
    })
}

/// Read and drop `remaining` bytes of a rejected frame.
fn discard<R: Read>(reader: &mut R, mut remaining: usize) -> bool {
    let mut scratch = [0u8; 8 * 1024];
    while remaining > 0 {
        let want = remaining.min(scratch.len());
        match reader.read(&mut scratch[..want]) {
            Ok(0) => return false,
            Ok(read) => remaining -= read,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(_) => return false,
        }
    }
    true
}

/// Route one decoded frame: cancellation first, then dispatch.
fn handle_frame(
    state: &Arc<AppState>,
    router: &Arc<RpcRouter>,
    outbound: &Outbound,
    inflight: &Arc<Inflight>,
    tasks: &mut tokio::task::JoinSet<()>,
    text: &str,
) {
    let frame: Value = match serde_json::from_str(text) {
        Ok(frame) => frame,
        Err(error) => {
            outbound.send(error_frame(
                Value::Null,
                PARSE_ERROR,
                &format!("parse error: {error}"),
            ));
            return;
        }
    };
    if handle_cancel(&frame, inflight, outbound) {
        return;
    }
    // Register before spawning: a `$/cancelRequest` for this id may be
    // the very next frame, and it must find the entry (or the token
    // race would let the request run as if it were never cancelled).
    let registration = frame.get("id").cloned().map(|id| {
        let token = inflight.register(&id);
        let deadline = inflight.deadline(&id);
        Registration {
            id,
            token,
            deadline,
        }
    });
    tasks.spawn(dispatch_frame(
        Arc::clone(state),
        Arc::clone(router),
        outbound.clone(),
        Arc::clone(inflight),
        frame,
        registration,
    ));
}

/// One request's in-flight entry, created before its task is spawned.
struct Registration {
    id: Value,
    token: Arc<dyn CancelToken>,
    deadline: Option<Instant>,
}

/// Answer a `$/cancelRequest` frame, reporting whether it was one.
fn handle_cancel(
    frame: &Value,
    inflight: &Inflight,
    outbound: &Outbound,
) -> bool {
    let Some(object) = frame.as_object() else {
        return false;
    };
    if object.get("method").and_then(Value::as_str) != Some("$/cancelRequest") {
        return false;
    }
    let target = object
        .get("params")
        .and_then(|params| params.get("id"))
        .cloned();
    let cancelled = target.as_ref().is_some_and(|id| inflight.cancel(id));
    if let Some(request_id) = object.get("id").cloned() {
        let response = if cancelled {
            json!({
                "jsonrpc": "2.0",
                "id": request_id,
                "result": { "cancelled": true },
            })
        } else {
            not_found_frame(request_id, target.as_ref())
        };
        outbound.send(response);
    } else if !cancelled {
        tracing::debug!(?target, "cancel for a request that is not in flight");
    }
    true
}

fn not_found_frame(id: Value, target: Option<&Value>) -> Value {
    let target = target
        .map(|value| value.to_string())
        .unwrap_or_else(|| "null".to_string());
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": APPLICATION_ERROR,
            "message": format!("not found: request {target} is not in flight"),
            "data": { "code": "not_found" },
        },
    })
}

/// Dispatch one frame and write its response unless it was cancelled
/// or timed out first.
async fn dispatch_frame(
    state: Arc<AppState>,
    router: Arc<RpcRouter>,
    outbound: Outbound,
    inflight: Arc<Inflight>,
    frame: Value,
    registration: Option<Registration>,
) {
    let Some(Registration {
        id,
        token,
        deadline,
    }) = registration
    else {
        // Notifications get no entry and no response; a frame with no
        // id that is not a valid notification still gets its protocol
        // error, as it did before the frame table existed.
        if let Some(response) = router.dispatch_value(state, frame).await {
            outbound.send(response);
        }
        return;
    };
    let response = match deadline {
        Some(deadline) => {
            tokio::select! {
                biased;
                () = token.cancelled() => None,
                () = sleep_until(deadline) => Some(timeout_frame(&id)),
                response = router.dispatch_value(state, frame) => response,
            }
        }
        None => {
            tokio::select! {
                biased;
                () = token.cancelled() => None,
                response = router.dispatch_value(state, frame) => response,
            }
        }
    };
    if !inflight.finish(&id) {
        tracing::debug!(
            %id,
            "request was cancelled; its response is suppressed"
        );
        return;
    }
    if let Some(response) = response {
        outbound.send(response);
    }
}

async fn sleep_until(deadline: Instant) {
    tokio::time::sleep_until(tokio::time::Instant::from_std(deadline)).await;
}

fn timeout_frame(id: &Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": APPLICATION_ERROR,
            "message": "request timed out",
            "data": { "code": "timeout" },
        },
    })
}
