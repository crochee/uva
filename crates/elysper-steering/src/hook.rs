//! The debug-level lifecycle logger.

/// Debug-level lifecycle logger — the default observability hook.
///
/// A unit value: the harness implements its own `Hook` trait for it,
/// so installing the default policy is one `host.hook(Arc::new(
/// LoggingHook))` and the logger has no dependency on the host.
pub struct LoggingHook;
