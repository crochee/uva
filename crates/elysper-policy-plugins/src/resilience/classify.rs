//! Default [`ClassifyFn`] — maps a `elysper_core::Error` to an
//! [`Outcome`].
//!
//! The default classifier is **conservative**: rate-limited /
//! overloaded / transient / empty-response errors are retryable;
//! auth, validation, model-not-found, and quota errors are
//! give-up. Reuses [`elysper_provider::retry::classify_error`] so
//! the resilience wrapper agrees with the transport-level retry
//! classifier on which errors are retryable.
//!
//! [`ClassifyFn`]: crate::resilience::classify::ClassifyFn

use std::sync::Arc;

use elysper_core::Error;

/// Outcome of classifying an inner error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Retry the call (subject to backoff and the breaker).
    Retry,
    /// Refuse the call. The wrapper surfaces the error to the
    /// caller without retrying.
    GiveUp,
}

/// Classifier: takes the inner error and returns what to do.
///
/// `Arc<dyn Fn>` so the resilient wrapper can hold one without
/// monomorphising on it; tests can swap in a custom classifier
/// without rebuilding the policy type.
pub type ClassifyFn = Arc<dyn Fn(&Error) -> Outcome + Send + Sync>;

/// Default classifier.
pub struct DefaultClassifier;

impl DefaultClassifier {
    /// Returns `Retry` for rate-limit / overload / transient /
    /// empty-response errors; `GiveUp` otherwise.
    #[must_use]
    pub fn classify(err: &Error) -> Outcome {
        default_classifier(err)
    }
}

/// Default classification function. Uses the provider crate's
/// `classify_error` to keep resilience and transport-level retry
/// in agreement.
#[must_use]
pub fn default_classifier(err: &Error) -> Outcome {
    use elysper_provider::retry::{RetryClass, classify_error};
    match classify_error(err) {
        RetryClass::RateLimit
        | RetryClass::Overloaded
        | RetryClass::Transient
        | RetryClass::EmptyResponse => Outcome::Retry,
        RetryClass::Quota
        | RetryClass::ContextOverflow
        | RetryClass::Auth
        | RetryClass::Permanent => Outcome::GiveUp,
    }
}
