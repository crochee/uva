//! The debug-level lifecycle logger.

/// Debug-level lifecycle logger — the default observability hook.
///
/// A unit value: the contract crate `uva-hook` implements its `Hook`
/// trait for it (the orphan rule leaves no other crate able to — this
/// crate cannot depend on `uva-hook` without a cycle), so installing
/// the default policy is one `host.hook(Arc::new(LoggingHook))` and
/// the logger has no dependency on the host.
pub struct LoggingHook;
