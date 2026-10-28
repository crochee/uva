//! The crate's retry vocabulary must be complete with no adapter
//! feature enabled.
//!
//! The tokio *dependency edge* is proven by
//! `cargo tree -p elysper-provider -e normal --no-default-features`
//! (a manifest property, not a runtime one). What that check cannot
//! see is whether the crate still hangs together in that
//! configuration: the classifier and the [`elysper_core::Error`]
//! vocabulary it reads must be nameable and usable without an
//! adapter. This file is that half.
//!
//! Run with `cargo test -p elysper-provider --no-default-features
//! --test tokio_edges`.

use elysper_core::Error;
use elysper_provider::retry::{RetryClass, classify_error, is_retryable_error};

#[test]
fn retry_classifiers_need_no_adapter() {
    assert!(is_retryable_error(429));
    assert!(!is_retryable_error(400));
    let _ = RetryClass::Transient;
}

#[test]
fn error_classification_works_without_an_adapter() {
    // `Error` is the crate's own error type (`elysper_core::Error`);
    // constructing it and classifying it must not require an
    // adapter feature.
    assert_eq!(
        classify_error(&Error::Timeout {
            message: "boom".into(),
        }),
        RetryClass::Transient
    );
    assert_eq!(
        classify_error(&Error::Internal {
            message: "boom".into(),
        }),
        RetryClass::Permanent
    );
}
