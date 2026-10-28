//! Panic isolation for the loop's pluggable seams: [`caught`] for the synchronous seams, [`catching_panics`] for the `async` ones, and [`isolate`] layering the common "report it and carry on" recovery on top.
use futures::FutureExt;
use tracing::warn;

use crate::{agent::sink::EventSink, events::WarningKind};

/// Convert a panic in `fut` into `Err(message)` — a seam panic must
/// not kill the run's task silently. `AssertUnwindSafe` is sound:
/// callers report the error, they never resume past the panic.
pub(super) async fn catching_panics<F, T>(fut: F) -> Result<T, String>
where
    F: Future<Output = T>,
{
    match std::panic::AssertUnwindSafe(fut).catch_unwind().await {
        Ok(value) => Ok(value),
        // By value: see `elysper_core::panic_message`'s docs for why a
        // reference parameter would let the message be silently lost.
        Err(payload) => Err(elysper_core::panic_message(payload)),
    }
}

/// The synchronous half of [`catching_panics`] — same recovery,
/// different calling convention.
pub(super) fn caught<T>(f: impl FnOnce() -> T) -> Result<T, String> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(value) => Ok(value),
        Err(payload) => Err(elysper_core::panic_message(payload)),
    }
}

/// Run a **synchronous** closure whose panic costs only its own contribution: log it, surface it as a [`WarningKind`]-classified
/// event, report `None`. The shape every loop-internal seam shares.
pub(super) fn isolate<T>(
    sink: &EventSink,
    warn_kind: WarningKind,
    iteration: Option<usize>,
    label: &str,
    f: impl FnOnce() -> T,
) -> Option<T> {
    match caught(f) {
        Ok(value) => Some(value),
        Err(message) => {
            warn!(panic = %message, "{label} panicked");
            sink.warn(
                warn_kind,
                iteration,
                format!("{label} panicked: {message}"),
            );
            None
        }
    }
}
