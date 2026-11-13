use super::*;

// -- Display (thiserror-generated) -------------------------------

/// Every variant's Display format is pinned verbatim — the
/// HTTP layer copies Display into the response envelope's
/// `message` field, so wording changes are wire-visible.
#[test]
fn display_formats_are_pinned() {
    let io: Error = std::io::Error::other("disk full").into();
    assert_eq!(io.to_string(), "I/O error: disk full");
    assert_eq!(Error::not_found("widget").to_string(), "not found: widget");
    assert_eq!(
        Error::already_exists("skill 'x'").to_string(),
        "already exists: skill 'x'"
    );
    assert_eq!(
        Error::invalid_item("bad label").to_string(),
        "invalid item: bad label"
    );
    assert_eq!(
        Error::validation("bad").to_string(),
        "validation error: bad"
    );
    assert_eq!(Error::internal("boom").to_string(), "internal error: boom");
    assert_eq!(
        Error::rate_limited(None).to_string(),
        "rate limited, retry after None"
    );
    assert_eq!(
        Error::edit_conflict("/tmp/f", 1, 2).to_string(),
        "edit conflict on /tmp/f: original_hash=1, current_hash=2"
    );
    assert_eq!(
        Error::context_overflow(100, 200).to_string(),
        "context overflow: 200 tokens used, limit is 100"
    );
    assert_eq!(
        Error::doom_loop("bash", 5).to_string(),
        "doom loop detected on 'bash' after 5 iterations"
    );
    assert_eq!(
        Error::prompt_injection("user_input", "ignore previous").to_string(),
        "prompt injection detected in user_input: pattern \
         'ignore previous'"
    );
    assert_eq!(
        Error::StreamInternal {
            message: "x".into()
        }
        .to_string(),
        "stream error: (internal): x"
    );
    assert_eq!(
        Error::StreamHttpFailure {
            message: "x".into()
        }
        .to_string(),
        "stream error: (http_failure): x"
    );
    assert_eq!(
        Error::StreamAborted {
            message: "x".into()
        }
        .to_string(),
        "stream error: (aborted): x"
    );
    assert_eq!(
        Error::retry_exhausted(3, Error::timeout("upstream")).to_string(),
        "retry exhausted after 3 attempts: timeout: upstream"
    );
}

// -- helper constructors ------------------------------------------

/// Each helper constructor MUST produce its specific variant
/// (not a sibling variant with the same fields). The
/// `assert_variant!` macro expands to one `matches!`
/// invocation so the assertion line itself stays a single
/// statement — clippy counts a `matches!` macro call + the
/// arm match as separate cognitive points, so each helper
/// would otherwise add ~2 to the enclosing test.
///
/// The 35 helpers are split into per-domain tests so each
/// function stays well under the 20-point cognitive
/// complexity threshold.
macro_rules! assert_variant {
    ($ctor:expr, $($variant:tt)+) => {
        assert!(matches!($ctor, $($variant)+));
    };
}

#[test]
fn helpers_for_lookup_and_validation() {
    assert_variant!(Error::not_found("x"), Error::NotFound { .. });
    assert_variant!(Error::already_exists("x"), Error::AlreadyExists { .. });
    assert_variant!(Error::invalid_item("x"), Error::InvalidItem { .. });
    assert_variant!(Error::validation("x"), Error::Validation { .. });
}

#[test]
fn helpers_for_provider_and_protocol() {
    assert_variant!(Error::internal("x"), Error::Internal { .. });
    assert_variant!(
        Error::Unauthorized {
            message: "x".into()
        },
        Error::Unauthorized { .. }
    );
    assert_variant!(Error::forbidden("x"), Error::Forbidden { .. });
    assert_variant!(Error::parse("x"), Error::Parse { .. });
    assert_variant!(
        Error::ToolExecution {
            message: "x".into()
        },
        Error::ToolExecution { .. }
    );
    assert_variant!(Error::provider("x"), Error::Provider { .. });
    assert_variant!(Error::timeout("x"), Error::Timeout { .. });
}

#[test]
fn helpers_for_stream_variants() {
    assert_variant!(Error::stream("x"), Error::Stream { .. });
    assert_variant!(
        Error::StreamInternal {
            message: "x".into()
        },
        Error::StreamInternal { .. }
    );
    assert_variant!(
        Error::StreamHttpFailure {
            message: "x".into()
        },
        Error::StreamHttpFailure { .. }
    );
    assert_variant!(
        Error::StreamAborted {
            message: "x".into()
        },
        Error::StreamAborted { .. }
    );
    assert_variant!(
        Error::StreamProtocolError {
            message: "x".into()
        },
        Error::StreamProtocolError { .. }
    );
}

#[test]
fn helpers_for_auth_and_config() {
    assert_variant!(Error::config("x"), Error::Config { .. });
    assert_variant!(
        Error::Router {
            message: "x".into()
        },
        Error::Router { .. }
    );
    assert_variant!(
        Error::Context {
            message: "x".into()
        },
        Error::Context { .. }
    );
    assert_variant!(Error::telemetry("x"), Error::Telemetry { .. });
    assert_variant!(
        Error::Multiagent {
            message: "x".into()
        },
        Error::Multiagent { .. }
    );
    assert_variant!(
        Error::Evaluation {
            message: "x".into()
        },
        Error::Evaluation { .. }
    );
}

#[test]
fn helpers_for_lifecycle_and_memory() {
    assert_variant!(Error::session("x"), Error::Session { .. });
    assert_variant!(
        Error::Skill {
            message: "x".into()
        },
        Error::Skill { .. }
    );
    assert_variant!(
        Error::Memory {
            message: "x".into()
        },
        Error::Memory { .. }
    );
    assert_variant!(
        Error::GuardianViolation {
            message: "x".into()
        },
        Error::GuardianViolation { .. }
    );
}

#[test]
fn helpers_for_model_and_rate_limit() {
    assert_variant!(
        Error::ModelNotFound {
            message: "x".into()
        },
        Error::ModelNotFound { .. }
    );
    assert_variant!(
        Error::ModelUnavailable {
            message: "x".into()
        },
        Error::ModelUnavailable { .. }
    );
    assert_variant!(Error::rate_limited(None), Error::RateLimited { .. });
    assert_variant!(
        Error::request_failed(500, "x"),
        Error::RequestFailed { .. }
    );
}

#[test]
fn helpers_for_edit_and_retry() {
    assert_variant!(
        Error::edit_conflict("p", 0, 0),
        Error::EditConflict { .. }
    );
    assert_variant!(
        Error::retry_exhausted(1, Error::internal("x")),
        Error::RetryExhausted { .. }
    );
    assert_variant!(
        Error::context_overflow(1, 2),
        Error::ContextOverflow { .. }
    );
    assert_variant!(Error::doom_loop("t", 1), Error::DoomLoop { .. });
    assert_variant!(
        Error::prompt_injection("s", "p"),
        Error::PromptInjection { .. }
    );
}

#[test]
fn from_std_io_produces_io_variant() {
    assert_variant!(std::io::Error::other("x"), std::io::Error { .. });
}

// -- payload round-trips ------------------------------------------

#[test]
fn rate_limited_constructor_stores_retry_after() {
    let e = Error::rate_limited(Some(Duration::from_secs(7)));
    assert!(e.is_rate_limited());
    let Error::RateLimited { retry_after, .. } = e else {
        panic!("expected RateLimited variant");
    };
    assert_eq!(retry_after, Some(Duration::from_secs(7)));

    let Error::RateLimited { retry_after, .. } = Error::rate_limited(None)
    else {
        panic!("expected RateLimited variant");
    };
    assert_eq!(retry_after, None);
}

#[test]
fn request_failed_constructor_stores_status_and_message() {
    let Error::RequestFailed {
        status, message, ..
    } = Error::request_failed(503, "service unavailable")
    else {
        panic!("expected RequestFailed variant");
    };
    assert_eq!(status, 503);
    assert_eq!(message, "service unavailable");
}

#[test]
fn edit_conflict_constructor_stores_path_and_hashes() {
    let path = PathBuf::from("/etc/config.toml");
    let Error::EditConflict {
        path: p,
        original_hash,
        current_hash,
    } = Error::edit_conflict(path.clone(), 0xDEAD_BEEF, 0xCAFE_BABE)
    else {
        panic!("expected EditConflict variant");
    };
    assert_eq!(p, path);
    assert_eq!(original_hash, 0xDEAD_BEEF);
    assert_eq!(current_hash, 0xCAFE_BABE);
}

#[test]
fn context_overflow_constructor_stores_token_counts() {
    let Error::ContextOverflow {
        limit_tokens,
        actual_tokens,
        ..
    } = Error::context_overflow(100_000, 150_000)
    else {
        panic!("expected ContextOverflow variant");
    };
    assert_eq!(limit_tokens, 100_000);
    assert_eq!(actual_tokens, 150_000);
}

#[test]
fn doom_loop_constructor_stores_tool_and_iterations() {
    let Error::DoomLoop {
        tool_name,
        iterations,
        ..
    } = Error::doom_loop("bash", 42)
    else {
        panic!("expected DoomLoop variant");
    };
    assert_eq!(tool_name, "bash");
    assert_eq!(iterations, 42);
}

#[test]
fn prompt_injection_constructor_stores_source_and_pattern() {
    let Error::PromptInjection {
        input_source,
        pattern,
        ..
    } = Error::prompt_injection("user_input", "ignore previous")
    else {
        panic!("expected PromptInjection variant");
    };
    assert_eq!(input_source, "user_input");
    assert_eq!(pattern, "ignore previous");
}

#[test]
fn retry_exhausted_constructor_stores_attempts_and_inner_error() {
    let inner = Error::timeout("upstream");
    let Error::RetryExhausted { attempts, .. } =
        Error::retry_exhausted(3, inner)
    else {
        panic!("expected RetryExhausted variant");
    };
    assert_eq!(attempts, 3);
}

// -- retryability --------------------------------------------------

#[test]
fn retryability_classification() {
    assert!(Error::request_failed(429, "x").is_retryable());
    assert!(Error::request_failed(500, "x").is_retryable());
    assert!(Error::request_failed(503, "x").is_retryable());
    assert!(!Error::request_failed(400, "x").is_retryable());
    assert!(!Error::request_failed(401, "x").is_retryable());
    assert!(Error::stream("x").is_retryable());
    assert!(Error::stream_http_failure("x").is_retryable());
    assert!(
        Error::StreamProtocolError {
            message: "x".into()
        }
        .is_retryable()
    );
    assert!(
        Error::StreamInternal {
            message: "x".into()
        }
        .is_retryable()
    );
    assert!(!Error::stream_aborted("x").is_retryable());
    assert!(!Error::provider("x").is_retryable());
    assert!(!Error::validation("x").is_retryable());
    assert!(!Error::internal("x").is_retryable());
}

// -- source chain ---------------------------------------------------

/// `source()` MUST route through the `#[from]` / `#[source]`
/// payload fields so the original cause stays reachable
/// (the attachment-style `source` chain for logging).
#[test]
fn source_routes_through_payload_fields() {
    use std::error::Error as _;

    let io: Error = std::io::Error::other("inner").into();
    assert_eq!(
        io.source().map(|s| s.to_string()),
        Some("inner".to_string())
    );

    let e = Error::retry_exhausted(2, Error::timeout("upstream"));
    assert_eq!(
        e.source().map(|s| s.to_string()),
        Some("timeout: upstream".to_string())
    );

    assert!(Error::not_found("x").source().is_none());
}

// -- From impls ------------------------------------------------------

#[test]
fn from_io_error_produces_io_variant() {
    let io_err =
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "no access");
    let Error::Io { inner, .. } = io_err.into() else {
        panic!("expected Io variant from io::Error");
    };
    assert_eq!(inner.kind(), std::io::ErrorKind::PermissionDenied);
}

#[test]
fn io_variant_only_via_from_io() {
    let e: Error = std::io::Error::other("x").into();
    assert!(matches!(e, Error::Io { .. }));
}

#[test]
fn from_serde_json_error_produces_parse_variant() {
    let json_err: serde_json::Error =
        serde_json::from_str::<serde_json::Value>("{bad").unwrap_err();
    let Error::Parse { message, .. } = json_err.into() else {
        panic!("expected Parse variant from JSON error");
    };
    assert!(!message.is_empty(), "message must be populated");
}

#[test]
fn from_serde_yaml_error_produces_parse_variant_with_prefix() {
    let yaml_err: serde_yaml::Error =
        serde_yaml::from_str::<serde_yaml::Value>("k: : :").unwrap_err();
    let Error::Parse { message, .. } = yaml_err.into() else {
        panic!("expected Parse variant from YAML error");
    };
    assert!(
        message.starts_with("yaml error: "),
        "yaml prefix required (got {message:?})"
    );
}
