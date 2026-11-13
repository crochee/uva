use super::*;

#[test]
fn classify_rate_limited_variant() {
    let e = Error::RateLimited { retry_after: None };
    assert_eq!(classify_error(&e), RetryClass::RateLimit);
}

/// `StreamAborted` must stay `Permanent`.
///
/// The streaming adapters consult the cancel token inside their
/// establishment closure and return `StreamAborted` to stop. That
/// works *only* because the class is terminal: `Permanent` is
/// `max_attempts: 1` with no sleep, so the retry exits at once.
/// Move this variant into the `Transient` arm above — a plausible
/// edit, since that arm already lists every other `Stream*`
/// variant — and a cancelled run would instead sit in the backoff
/// (1s + 2s for `Transient`) before noticing.
///
/// The cancel tests in `tests/provider_test.rs` would still pass
/// (still zero requests, because the guard fires first), so
/// nothing else catches that edit.
#[test]
fn classify_stream_aborted_is_permanent() {
    let class = classify_error(&Error::stream_aborted("cancelled"));
    assert_eq!(class, RetryClass::Permanent);
    assert_eq!(
        retry_config_for(class).max_attempts,
        1,
        "a cancelled stream must not be retried at all"
    );
    assert_eq!(
        retry_config_for(class).max_elapsed_ms,
        0,
        "and must not sleep through a backoff"
    );
}

#[test]
fn classify_request_failed_by_status() {
    let mk = |status: u16| Error::RequestFailed {
        status,
        message: "x".into(),
    };
    assert_eq!(classify_error(&mk(429)), RetryClass::RateLimit);
    assert_eq!(classify_error(&mk(500)), RetryClass::Overloaded);
    assert_eq!(classify_error(&mk(503)), RetryClass::Overloaded);
    assert_eq!(classify_error(&mk(400)), RetryClass::Permanent);
    assert_eq!(classify_error(&mk(401)), RetryClass::Permanent);
}

#[test]
fn classify_transient_variants() {
    assert_eq!(
        classify_error(&Error::Timeout {
            message: "t".into()
        }),
        RetryClass::Transient
    );
    assert_eq!(
        classify_error(&Error::Stream {
            message: "s".into()
        }),
        RetryClass::Transient
    );
}

#[test]
fn classify_provider_markers() {
    assert_eq!(
        classify_error(&Error::Provider {
            message: "anthropic overloaded_error".into()
        }),
        RetryClass::Overloaded
    );
    assert_eq!(
        classify_error(&Error::Provider {
            message: "rate_limit exceeded".into()
        }),
        RetryClass::RateLimit
    );
    assert_eq!(
        classify_error(&Error::Provider {
            message: "bad input".into()
        }),
        RetryClass::Permanent
    );
}

#[test]
fn classify_permanent_variants() {
    assert_eq!(
        classify_error(&Error::Unauthorized {
            message: "u".into()
        }),
        RetryClass::Permanent
    );
    assert_eq!(
        classify_error(&Error::ModelNotFound {
            message: "m".into()
        }),
        RetryClass::Permanent
    );
    assert_eq!(
        classify_error(&Error::Validation {
            message: "v".into()
        }),
        RetryClass::Permanent
    );
}

#[test]
fn classify_quota_markers() {
    // dsh `isQuotaExceededError` wording, in the shapes the
    // OpenAI/Anthropic-compatible providers actually emit.
    for message in [
        "insufficient_quota",
        "You exceeded your current quota, please check your plan",
        "billing hard limit reached",
        "usage limit exhausted",
        "insufficient balance",
        "your credit balance is out of credits",
    ] {
        assert_eq!(
            classify_error(&Error::Provider {
                message: message.into()
            }),
            RetryClass::Quota,
            "must classify as Quota: {message}"
        );
    }
}

#[test]
fn classify_quota_is_distinct_from_rate_limit() {
    // Request-rate wording stays RateLimit...
    for message in ["rate_limit exceeded", "Rate limit reached"] {
        assert_eq!(
            classify_error(&Error::Provider {
                message: message.into()
            }),
            RetryClass::RateLimit,
            "must stay RateLimit: {message}"
        );
    }
    // ...and a TEXT-ONLY body stays status-mapped (contract):
    // 429 is the request-rate status, 402 the account-quota one.
    // A *structured* body gets to override the status — see
    // `classify_openai_quota_body_overrides_the_429_status`.
    assert_eq!(
        classify_error(&Error::RequestFailed {
            status: 429,
            message: "insufficient_quota".into()
        }),
        RetryClass::RateLimit
    );
    assert_eq!(
        classify_error(&Error::RequestFailed {
            status: 402,
            message: "Insufficient Balance".into()
        }),
        RetryClass::Quota
    );
}

/// Real error bodies from both providers, classified from the
/// provider's own `type`/`code`/`message` rather than from the
/// status — and each landing in its own class, which is the whole
/// point of reading the body: OpenAI reports an exhausted account
/// (quota), a request-rate limit, a context overflow and a bad
/// credential all with overlapping statuses.
#[test]
fn classify_real_provider_error_bodies_into_distinct_classes() {
    let cases: [(u16, &str, RetryClass); 7] = [
        (
            429,
            r#"{"error":{"message":"You exceeded your current quota, please check your plan and billing details.","type":"insufficient_quota","param":null,"code":"insufficient_quota"}}"#,
            RetryClass::Quota,
        ),
        (
            429,
            r#"{"error":{"message":"Rate limit reached for gpt-4o in organization org-x on requests per min (RPM): Limit 500, Used 500.","type":"rate_limit_exceeded","param":null,"code":"rate_limit_exceeded"}}"#,
            RetryClass::RateLimit,
        ),
        (
            529,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            RetryClass::Overloaded,
        ),
        (
            400,
            r#"{"error":{"message":"This model's maximum context length is 128000 tokens. However, your messages resulted in 150000 tokens. Please reduce the length of the messages.","type":"invalid_request_error","param":"messages","code":"context_length_exceeded"}}"#,
            RetryClass::ContextOverflow,
        ),
        (
            400,
            r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 213000 tokens > 200000 maximum"}}"#,
            RetryClass::ContextOverflow,
        ),
        (
            401,
            r#"{"error":{"message":"Incorrect API key provided: sk-abc***. You can find your API key at https://platform.openai.com/account/api-keys.","type":"invalid_request_error","param":null,"code":"invalid_api_key"}}"#,
            RetryClass::Auth,
        ),
        (
            401,
            r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#,
            RetryClass::Auth,
        ),
    ];
    let classes: Vec<RetryClass> = cases
        .iter()
        .map(|(status, body, expected)| {
            let got = classify_error(&Error::request_failed(*status, *body));
            assert_eq!(got, *expected, "body: {body}");
            got
        })
        .collect();
    let distinct: std::collections::HashSet<RetryClass> =
        classes.iter().copied().collect();
    assert_eq!(
        distinct.len(),
        5,
        "quota, rate limit, capacity, overflow and auth must be \
         distinguishable, got {distinct:?}"
    );
}

/// The OpenAI 429 shape carries `insufficient_quota`: the body
/// outranks the status, so the failure is not retried like a rate
/// limit.
#[test]
fn classify_openai_quota_body_overrides_the_429_status() {
    let body = r#"{"error":{"message":"You exceeded your current quota","type":"insufficient_quota","code":"insufficient_quota"}}"#;
    assert_eq!(classify_provider_error_body(429, body), RetryClass::Quota);
    assert_eq!(retry_config_for(RetryClass::Quota).max_attempts, 1);
}

/// Regression (R33): a body the classifier cannot read as a
/// structured signal keeps the pre-R33 status mapping — text-only
/// bodies, HTML from a proxy, and JSON whose `type` is outside
/// the marker vocabulary alike.
#[test]
fn classify_unrecognized_bodies_keep_the_status_class() {
    let cases: [(u16, &str, RetryClass); 10] = [
        (400, "bad request", RetryClass::Permanent),
        (401, "no", RetryClass::Permanent),
        (402, "Insufficient Balance", RetryClass::Quota),
        (403, "<html>forbidden</html>", RetryClass::Permanent),
        // A bare marker word is not a structured body: it stays
        // status-mapped until the provider shapes it as JSON.
        (429, "insufficient_quota", RetryClass::RateLimit),
        (429, "rate limited", RetryClass::RateLimit),
        (500, "internal server error", RetryClass::Overloaded),
        (503, "<html>bad gateway</html>", RetryClass::Overloaded),
        (529, "Overloaded", RetryClass::Permanent),
        (
            400,
            r#"{"error":{"type":"invalid_request_error","message":"unknown field"}}"#,
            RetryClass::Permanent,
        ),
    ];
    for (status, body, expected) in cases {
        assert_eq!(
            classify_provider_error_body(status, body),
            expected,
            "status {status}, body {body}"
        );
        assert_eq!(
            classify_error(&Error::request_failed(status, body)),
            expected
        );
    }
}

#[test]
fn classify_empty_completion_marker() {
    let err =
        crate::validation::empty_response_error("test-model", Some("end_turn"));
    assert_eq!(classify_error(&err), RetryClass::EmptyResponse);
}

/// A 5xx is a capacity signal in its own right: body wording that
/// would otherwise read as auth must not turn a recoverable
/// overload into a caller-side failure and cost it its retry
/// budget. An explicit quota body still wins — waiting cannot
/// clear an exhausted account, whatever status carries it.
#[test]
fn classify_capacity_status_outranks_non_quota_body_wording() {
    let auth_wording = concat!(
        r#"{"error":{"type":"authentication_error","#,
        r#""message":"authentication service unavailable"}}"#
    );
    assert_eq!(
        classify_provider_error_body(503, auth_wording),
        RetryClass::Overloaded
    );
    assert_eq!(retry_config_for(RetryClass::Overloaded).max_attempts, 4);

    let quota = concat!(
        r#"{"error":{"type":"insufficient_quota","#,
        r#""message":"account quota exhausted"}}"#
    );
    assert_eq!(classify_provider_error_body(500, quota), RetryClass::Quota);
}

#[test]
fn retry_config_for_new_class_budgets() {
    let empty = retry_config_for(RetryClass::EmptyResponse);
    assert_eq!(empty.max_attempts, 2, "small re-ask budget");
    assert_eq!(empty.initial_interval_ms, 500);
    assert!(empty.max_interval_ms <= 10000);

    let quota = retry_config_for(RetryClass::Quota);
    assert_eq!(quota.max_attempts, 1, "quota is not retryable");
}

#[cfg(any(feature = "anthropic", feature = "openai"))]
#[tokio::test]
async fn retry_with_classification_gives_up_on_quota() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = AtomicUsize::new(0);
    let result: Result<(), Error> = retry_with_classification(|| {
        calls.fetch_add(1, Ordering::SeqCst);
        async {
            Err(Error::Provider {
                message: "insufficient_quota".into(),
            })
        }
    })
    .await;
    let Err(Error::RetryExhausted {
        attempts,
        last_error,
    }) = result
    else {
        panic!("quota must surface as RetryExhausted");
    };
    assert_eq!(attempts, 1, "not retried within a session");
    assert_eq!(classify_error(&last_error), RetryClass::Quota);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[cfg(any(feature = "anthropic", feature = "openai"))]
#[tokio::test]
async fn retry_with_classification_reasks_empty_completion() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = AtomicUsize::new(0);
    let result: Result<&'static str, Error> = retry_with_classification(|| {
        let n = calls.fetch_add(1, Ordering::SeqCst);
        async move {
            if n == 0 {
                Err(crate::validation::empty_response_error("m", Some("stop")))
            } else {
                Ok("recovered")
            }
        }
    })
    .await;
    assert_eq!(result.unwrap(), "recovered");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[cfg(any(feature = "anthropic", feature = "openai"))]
#[tokio::test]
async fn retry_with_classification_exhausts_empty_budget() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = AtomicUsize::new(0);
    let result: Result<(), Error> = retry_with_classification(|| {
        calls.fetch_add(1, Ordering::SeqCst);
        async {
            Err(crate::validation::empty_response_error("m", Some("stop")))
        }
    })
    .await;
    let Err(Error::RetryExhausted {
        attempts,
        last_error,
    }) = result
    else {
        panic!("empty completions must exhaust their budget");
    };
    assert_eq!(attempts, 2, "2-attempt budget");
    assert!(crate::validation::is_empty_response_error(&last_error));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn retry_config_for_class_budgets() {
    assert_eq!(retry_config_for(RetryClass::RateLimit).max_attempts, 6);
    assert_eq!(retry_config_for(RetryClass::Overloaded).max_attempts, 4);
    assert_eq!(retry_config_for(RetryClass::Transient).max_attempts, 3);
    assert_eq!(retry_config_for(RetryClass::Permanent).max_attempts, 1);
    // Neither a prompt that cannot fit nor a rejected credential
    // becomes viable by waiting.
    assert_eq!(
        retry_config_for(RetryClass::ContextOverflow).max_attempts,
        1
    );
    assert_eq!(retry_config_for(RetryClass::Auth).max_attempts, 1);
}

#[cfg(any(feature = "anthropic", feature = "openai"))]
#[tokio::test]
async fn retry_with_classification_gives_up_on_permanent() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = AtomicUsize::new(0);
    let result: Result<(), Error> = retry_with_classification(|| {
        calls.fetch_add(1, Ordering::SeqCst);
        async {
            Err(Error::Unauthorized {
                message: "no".into(),
            })
        }
    })
    .await;
    assert!(matches!(
        result,
        Err(Error::RetryExhausted { attempts: 1, .. })
    ));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[cfg(any(feature = "anthropic", feature = "openai"))]
#[tokio::test]
async fn retry_with_classification_succeeds_after_transient() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let calls = AtomicUsize::new(0);
    let result: Result<&'static str, Error> = retry_with_classification(|| {
        let n = calls.fetch_add(1, Ordering::SeqCst);
        async move {
            if n == 0 {
                Err(Error::Timeout {
                    message: "t".into(),
                })
            } else {
                Ok("ok")
            }
        }
    })
    .await;
    assert_eq!(result.unwrap(), "ok");
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn test_is_retryable_error() {
    assert!(is_retryable_error(429));
    assert!(is_retryable_error(500));
    assert!(is_retryable_error(503));
    assert!(!is_retryable_error(400));
    assert!(!is_retryable_error(401));
}

#[test]
fn test_retry_config_default() {
    let config = RetryConfig::default();
    assert_eq!(config.max_attempts, 3);
    assert_eq!(config.initial_interval_ms, 1000);
    assert_eq!(config.max_interval_ms, 10000);
}

#[test]
fn test_parse_retry_after_seconds() {
    let duration = parse_retry_after("30");
    assert_eq!(duration, Some(Duration::from_secs(30)));
}

#[test]
fn test_parse_retry_after_invalid() {
    let duration = parse_retry_after("not-a-number");
    assert!(duration.is_none());
}
