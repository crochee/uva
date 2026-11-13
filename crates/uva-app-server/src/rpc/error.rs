//! The JSON-RPC error model.
//!
//! One boundary error type for the whole process: [`RpcError`]. It
//! carries the full source chain (an `anyhow::Error` wrapping the
//! original `uva::core::Error`), a stable **machine code**
//! (snake_case, e.g. `"not_found"`), a human-facing message, and an
//! optional KV context map (operator-facing; it never crosses the
//! wire).
//!
//! # Wire shape
//!
//! An application failure is reported with the JSON-RPC
//! server-error code [`APPLICATION_ERROR`] and the machine code in
//! `data`:
//!
//! ```json
//! { "error": { "code": -32000, "message": "not found: session 'x'",
//!              "data": { "code": "not_found" } } }
//! ```
//!
//! A protocol-level failure (unparseable frame, unknown method,
//! malformed params) uses the standard JSON-RPC codes and carries no
//! `data.code` — there is no domain failure to name.
//!
//! # Where the machine codes come from
//!
//! The variant → code mapping is inherited verbatim from the HTTP
//! surface this crate used to expose, so a client that already
//! knows the codes keeps working. [`From<Error>`] owns that table;
//! the crate defines no error variants of its own.
use std::collections::HashMap;

use serde_json::{Value, json};
use uva::core::Error;

/// JSON-RPC server error code for an application-level failure.
pub const APPLICATION_ERROR: i64 = -32000;
/// JSON-RPC code for an unparseable frame.
pub const PARSE_ERROR: i64 = -32700;
/// JSON-RPC code for a frame that is not a well-formed request.
pub const INVALID_REQUEST: i64 = -32600;
/// JSON-RPC code for a method the router does not know.
pub const METHOD_NOT_FOUND: i64 = -32601;
/// JSON-RPC code for params that do not deserialize into the
/// method's request type.
pub const INVALID_PARAMS: i64 = -32602;
/// JSON-RPC code for an unexpected internal failure.
pub const INTERNAL_ERROR: i64 = -32603;
/// The boundary error type every RPC method returns.
///
/// `code` is the machine code a client branches on; `message` is the
/// human-readable text. Both are derived from the single domain
/// error type [`uva::core::Error`].
#[derive(Debug)]
pub struct RpcError {
    /// Stable wire code (snake_case, e.g. `"not_found"`).
    pub code: &'static str,
    /// The JSON-RPC wire code this failure answers with:
    /// [`APPLICATION_ERROR`] for application failures, or a
    /// standard protocol code for protocol-level failures.
    wire_code: i64,
    /// Human-facing message that crosses the wire.
    pub message: String,
    /// Original error chain (wraps the original
    /// `uva::core::Error` for core failures, or any foreign
    /// error for `From<std::io::Error>` / etc.).
    pub source: anyhow::Error,
    /// Operator-facing KV context (request id, subject, etc.).
    /// Never crosses the wire — logged via `tracing::error!`.
    pub context: HashMap<String, String>,
}
impl RpcError {
    /// Wrap an error source, defaulting the machine code to
    /// `"internal_server_error"`.
    pub fn new(source: impl Into<anyhow::Error>) -> Self {
        let source = source.into();
        let message = source.to_string();
        Self {
            code: "internal_server_error",
            message,
            source,
            context: HashMap::new(),
            wire_code: APPLICATION_ERROR,
        }
    }

    /// Override the machine code. Returns `self` for fluent chaining.
    #[must_use]
    pub fn with_code(mut self, code: &'static str) -> Self {
        self.code = code;
        self
    }

    /// An application failure with no underlying source — a rejected
    /// request the handler decided on.
    pub fn application(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            source: anyhow::anyhow!("{code}"),
            context: HashMap::new(),
            wire_code: APPLICATION_ERROR,
        }
    }

    /// A protocol-level failure (parse / unknown method / bad
    /// params). Carries no machine code: `data.code` is omitted.
    pub fn protocol(code: i64, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            code: "protocol_error",
            source: anyhow::anyhow!("{message}"),
            message,
            context: HashMap::new(),
            wire_code: code,
        }
    }

    /// Override the message. Returns `self` for fluent chaining.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    /// Add a single KV entry to the operator-facing context map.
    /// Returns `self` for fluent chaining.
    #[must_use]
    pub fn with_context(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
        self.context.insert(key.into(), value.into());
        self
    }

    /// Merge another KV map into the operator-facing context.
    /// Existing keys are overwritten (last writer wins).
    #[must_use]
    pub fn merge_context(mut self, other: HashMap<String, String>) -> Self {
        self.context.extend(other);
        self
    }

    /// The JSON-RPC `error` member for this failure.
    ///
    /// Application failures carry `data.code` (the machine code);
    /// protocol failures carry the standard code and no `data`.
    pub fn to_error_object(&self) -> Value {
        let mut error = json!({
            "code": self.wire_code,
            "message": self.message,
        });
        if self.code != "protocol_error" {
            error["data"] = json!({ "code": self.code });
        }
        error
    }

    /// Log the failure the way the HTTP boundary used to: the full
    /// detail goes to the operator, the wire body carries only the
    /// code and the message.
    pub fn log(&self) {
        tracing::error!(
            code = self.code,
            message = %self.message,
            context = ?self.context,
            source = %self.source,
            "rpc call failed"
        );
    }
}
impl From<Error> for RpcError {
    /// Wrap a domain error. The variant → machine-code mapping is
    /// inherited verbatim from the HTTP boundary this crate used to
    /// expose, so clients that already branch on those codes keep
    /// working. The human-facing message comes from the
    /// thiserror-generated variant-appropriate prefix; payload fields
    /// (resource name, token counts, …) are folded into the
    /// operator-facing `context` map so they show up in the
    /// `tracing::error!` log line without going on the wire.
    fn from(err: Error) -> Self {
        let message = err.to_string();
        let mut context: HashMap<String, String> = HashMap::new();
        let code: &'static str = match &err {
            Error::NotFound { item } => {
                context.insert("item".into(), item.clone());
                "not_found"
            }
            Error::AlreadyExists { item } => {
                context.insert("item".into(), item.clone());
                "already_exists"
            }
            Error::InvalidItem { item } => {
                context.insert("item".into(), item.clone());
                "invalid_item"
            }
            Error::Parse { .. } => "parse",
            Error::Internal { .. } => "internal_server_error",
            Error::Unauthorized { .. } => "unauthorized",
            Error::Forbidden { .. } => "forbidden",
            Error::Validation { .. } => "validation",
            Error::ToolExecution { .. } => "tool_execution",
            Error::Provider { .. } => "request_failed",
            Error::RequestFailed { status, .. } => {
                context.insert("status".into(), status.to_string());
                "request_failed"
            }
            Error::Session { .. } => "session",
            Error::Skill { .. } => "skill",
            Error::Memory { .. } => "memory",
            Error::GuardianViolation { .. } => "guardian_violation",
            Error::EditConflict {
                path,
                original_hash,
                current_hash,
            } => {
                context.insert("path".into(), path.display().to_string());
                context
                    .insert("original_hash".into(), original_hash.to_string());
                context.insert("current_hash".into(), current_hash.to_string());
                "edit_conflict"
            }
            Error::RateLimited { retry_after } => {
                if let Some(d) = retry_after {
                    context.insert(
                        "retry_after_secs".into(),
                        d.as_secs().to_string(),
                    );
                }
                "rate_limited"
            }
            Error::Stream { .. } => "stream",
            Error::StreamHttpFailure { .. } => "stream_http_failure",
            Error::StreamProtocolError { .. } => "stream_protocol_error",
            Error::StreamAborted { .. } => "stream_aborted",
            Error::StreamInternal { .. } => "stream_internal",
            Error::Timeout { .. } => "timeout",
            Error::RetryExhausted { attempts, .. } => {
                context.insert("attempts".into(), attempts.to_string());
                "retry_exhausted"
            }
            Error::ModelNotFound { .. } => "model_not_found",
            Error::ModelUnavailable { .. } => "model_unavailable",
            Error::Config { .. } => "config",
            Error::Router { .. } => "router",
            Error::Context { .. } => "context",
            Error::Telemetry { .. } => "telemetry",
            Error::Multiagent { .. } => "multiagent",
            Error::Evaluation { .. } => "evaluation",
            Error::ContextOverflow {
                limit_tokens,
                actual_tokens,
            } => {
                context.insert("limit_tokens".into(), limit_tokens.to_string());
                context
                    .insert("actual_tokens".into(), actual_tokens.to_string());
                "context_overflow"
            }
            Error::DoomLoop {
                tool_name,
                iterations,
            } => {
                context.insert("tool_name".into(), tool_name.clone());
                context.insert("iterations".into(), iterations.to_string());
                "doom_loop"
            }
            Error::PromptInjection {
                input_source,
                pattern,
            } => {
                context.insert("input_source".into(), input_source.clone());
                context.insert("pattern".into(), pattern.clone());
                "prompt_injection"
            }
            Error::Io { inner } => {
                context.insert("io_kind".into(), format!("{:?}", inner.kind()));
                "io"
            }
            _ => "internal_server_error",
        };
        Self {
            code,
            message,
            source: anyhow::Error::new(err),
            context,
            wire_code: APPLICATION_ERROR,
        }
    }
}
impl From<std::io::Error> for RpcError {
    /// Foreign `io::Error` — the machine code names the failure
    /// class so a client can tell a missing file from a broken pipe.
    fn from(err: std::io::Error) -> Self {
        let msg = err.to_string();
        let code = match err.kind() {
            std::io::ErrorKind::NotFound => "not_found",
            std::io::ErrorKind::PermissionDenied => "forbidden",
            std::io::ErrorKind::TimedOut => "timeout",
            _ => "io_error",
        };
        Self::new(err)
            .with_code(code)
            .with_message(format!("I/O error: {msg}"))
    }
}
impl From<serde_json::Error> for RpcError {
    fn from(err: serde_json::Error) -> Self {
        let msg = err.to_string();
        Self::new(err)
            .with_code("parse")
            .with_message(format!("JSON error: {msg}"))
    }
}
impl From<serde_yaml::Error> for RpcError {
    fn from(err: serde_yaml::Error) -> Self {
        let msg = err.to_string();
        Self::new(err)
            .with_code("parse")
            .with_message(format!("YAML error: {msg}"))
    }
}
/// Schema-level validation failure from the `validator` crate
/// (the `#[validate(...)]` attributes on request DTOs). The RPC
/// parameter parser calls `Validate::validate` after
/// deserialization, so this maps the failure onto the wire shape:
/// the `validation` machine code plus a flattened
/// `field: message` summary.
impl From<validator::ValidationErrors> for RpcError {
    fn from(err: validator::ValidationErrors) -> Self {
        let message = err
            .field_errors()
            .iter()
            .map(|(field, errors)| {
                let reasons: Vec<String> = errors
                    .iter()
                    .map(|e| {
                        e.message
                            .clone()
                            .unwrap_or_else(|| e.code.to_string().into())
                            .to_string()
                    })
                    .collect();
                format!("{field}: {}", reasons.join(", "))
            })
            .collect::<Vec<_>>()
            .join("; ");
        Error::validation(message).into()
    }
}
#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;
    #[test]
    fn from_core_error_carries_machine_code_and_message() {
        let err =
            RpcError::from(Error::validation("limit must be greater than 0"));
        assert_eq!(err.code, "validation");
        assert_eq!(
            err.message,
            "validation error: limit must be greater than 0"
        );
        assert!(err.context.is_empty());
    }
    #[test]
    fn context_populated_for_variant_payloads() {
        let err = RpcError::from(Error::not_found("session-123"));
        assert_eq!(
            err.context.get("item").map(String::as_str),
            Some("session-123")
        );
        let err =
            RpcError::from(Error::rate_limited(Some(Duration::from_secs(42))));
        assert_eq!(
            err.context.get("retry_after_secs").map(String::as_str),
            Some("42")
        );
        let path = std::path::PathBuf::from("/etc/config.toml");
        let err = RpcError::from(Error::edit_conflict(path, 0xDEAD, 0xCAFE));
        assert_eq!(
            err.context.get("path").map(String::as_str),
            Some("/etc/config.toml")
        );
        assert_eq!(
            err.context.get("original_hash").map(String::as_str),
            Some("57005")
        );
        assert_eq!(
            err.context.get("current_hash").map(String::as_str),
            Some("51966")
        );
        let err = RpcError::from(Error::context_overflow(100, 200));
        assert_eq!(
            err.context.get("limit_tokens").map(String::as_str),
            Some("100")
        );
        assert_eq!(
            err.context.get("actual_tokens").map(String::as_str),
            Some("200")
        );
        let err = RpcError::from(Error::doom_loop("bash", 5));
        assert_eq!(
            err.context.get("tool_name").map(String::as_str),
            Some("bash")
        );
        assert_eq!(
            err.context.get("iterations").map(String::as_str),
            Some("5")
        );
    }
    #[test]
    fn builder_methods_are_chainable() {
        let err = RpcError::new(anyhow::anyhow!("base"))
            .with_code("internal_server_error")
            .with_message("human text")
            .with_context("request_id", "abc-123")
            .with_context("subject", "user:42");
        assert_eq!(err.code, "internal_server_error");
        assert_eq!(err.message, "human text");
        assert_eq!(
            err.context.get("request_id").map(String::as_str),
            Some("abc-123")
        );
        assert_eq!(
            err.context.get("subject").map(String::as_str),
            Some("user:42")
        );
    }
    #[test]
    fn merge_context_overwrites() {
        let mut base = HashMap::new();
        base.insert("k".to_string(), "first".to_string());
        let extra = HashMap::from([
            ("k".to_string(), "second".to_string()),
            ("new".to_string(), "v".to_string()),
        ]);
        let err = RpcError::new(anyhow::anyhow!("x"))
            .merge_context(base)
            .merge_context(extra);
        assert_eq!(err.context.get("k").map(String::as_str), Some("second"));
        assert_eq!(err.context.get("new").map(String::as_str), Some("v"));
    }
    #[test]
    fn application_error_serializes_machine_code_in_data() {
        let err = RpcError::from(Error::not_found("session 'abc'"));
        let body = err.to_error_object();
        assert_eq!(body["code"], APPLICATION_ERROR);
        assert_eq!(body["message"], "not found: session 'abc'");
        assert_eq!(body["data"]["code"], "not_found");
    }
    #[test]
    fn internal_error_uses_core_wire_name() {
        let err = RpcError::from(Error::internal("boom"));
        let body = err.to_error_object();
        assert_eq!(body["code"], APPLICATION_ERROR);
        assert_eq!(body["message"], "internal error: boom");
        assert_eq!(body["data"]["code"], "internal_server_error");
    }
    #[test]
    fn protocol_error_carries_no_machine_code() {
        let err = RpcError::protocol(METHOD_NOT_FOUND, "no such method");
        let body = err.to_error_object();
        assert_eq!(body["code"], METHOD_NOT_FOUND);
        assert_eq!(body["message"], "no such method");
        assert!(body.get("data").is_none());
    }
    #[test]
    fn source_downcasts_back_to_core_error() {
        let inner = Error::timeout("upstream");
        let err = RpcError::from(inner);
        assert!(
            err.source.downcast_ref::<uva::core::Error>().is_some(),
            "downcast_ref must recover the original Error"
        );
    }
    #[test]
    fn validation_errors_map_to_the_validation_code() {
        use validator::Validate;
        #[derive(validator::Validate)]
        struct Sample {
            #[validate(length(min = 3, message = "too short"))]
            name: String,
        }
        let err = Sample { name: "ab".into() }.validate().unwrap_err();
        let err = RpcError::from(err);
        assert_eq!(err.code, "validation");
        assert!(err.message.contains("name"));
        assert!(err.message.contains("too short"));
    }
}
