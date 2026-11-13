use std::time::Duration;

use uva_core::{Clock, Error};

use crate::{
    context_overflow::ContextOverflowDetector,
    error_body::{ProviderErrorBody, parse_provider_error_body},
};

#[derive(Debug, Clone, Default)]
pub enum RetryPolicy {
    #[default]
    Default,
    Aggressive,
    Conservative,
    Custom(RetryConfig),
}

impl RetryPolicy {
    pub fn config(&self) -> RetryConfig {
        match self {
            RetryPolicy::Default => RetryConfig::default(),
            RetryPolicy::Aggressive => RetryConfig {
                max_attempts: 5,
                initial_interval_ms: 500,
                max_interval_ms: 15000,
                max_elapsed_ms: 120000,
            },
            RetryPolicy::Conservative => RetryConfig {
                max_attempts: 1,
                initial_interval_ms: 5000,
                max_interval_ms: 30000,
                max_elapsed_ms: 60000,
            },
            RetryPolicy::Custom(config) => config.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct RetryConfig {
    pub max_attempts: u32,
    pub initial_interval_ms: u64,
    pub max_interval_ms: u64,
    pub max_elapsed_ms: u64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 3,
            initial_interval_ms: 1000,
            max_interval_ms: 10000,
            max_elapsed_ms: 60000,
        }
    }
}

pub fn is_retryable_error(status: u16) -> bool {
    matches!(status, 429 | 500 | 502 | 503 | 504)
}

/// Extract Retry-After duration from a rate limit response.
/// Supports both integer seconds and HTTP date formats.
///
/// The HTTP-date form needs "now" to turn a timestamp into a delay;
/// that read goes through [`uva_core::Clock`]
/// (see [`parse_retry_after_at`] for the injectable form), so a test
/// can pin the arithmetic instead of racing the wall clock.
pub fn parse_retry_after(header_value: &str) -> Option<Duration> {
    parse_retry_after_at(header_value, uva_core::SharedClock::system().now())
}

/// [`parse_retry_after`] against an explicit "now".
///
/// Integer-second headers ignore `now` entirely; the HTTP-date form
/// subtracts it.
pub fn parse_retry_after_at(
    header_value: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<Duration> {
    if let Ok(seconds) = header_value.parse::<u64>() {
        return Some(Duration::from_secs(seconds));
    }
    if let Ok(date) = chrono::DateTime::parse_from_rfc2822(header_value) {
        let diff = date.with_timezone(&chrono::Utc) - now;
        if diff.num_seconds() > 0 {
            return Some(Duration::from_secs(diff.num_seconds() as u64));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Typed retry classification (R4 Phase C.1 — dsh llm-retry semantics)
// ---------------------------------------------------------------------------
/// Failure class for a provider error, each with its own retry
/// budget and backoff shape (dsh `ResolvedRetryPolicy` semantics).
///
/// Classification is the retry decision: [`classify_error`]
/// inspects the [`Error`] variant (and, for provider-opaque
/// payloads, well-known message markers) and returns the class;
/// [`retry_config_for`] maps the class to a [`RetryConfig`].
/// `Permanent` and `Quota` map to a single attempt — no retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetryClass {
    /// Quota / throttling (HTTP 429, `RateLimited`). Retries
    /// with the largest budget — quota windows recover and the
    /// caller has already committed to waiting.
    RateLimit,
    /// Provider capacity (HTTP 5xx, Anthropic
    /// `overloaded_error`). Bounded retries with short initial
    /// delay — overload bursts usually clear in seconds.
    Overloaded,
    /// Network / timeout / stream hiccups. The pre-R4 default
    /// budget (3 attempts, 1s → 10s).
    Transient,
    /// Degenerate empty terminal completion: the provider
    /// reported a normal stop but produced no content at all
    /// (dsh `EMPTY_RESPONSE`). Retryable with a small budget —
    /// the failure is often a transient sampling artefact, and
    /// one cheap re-ask usually recovers it.
    EmptyResponse,
    /// Exhausted account quota, balance, credits, or billing
    /// (HTTP 402, dsh `QUOTA`). Distinct from `RateLimit`
    /// because the failure is NOT retryable within a session:
    /// one attempt, so callers can report "quota exhausted"
    /// instead of pretending a retry might succeed.
    Quota,
    /// Context-window overflow (`context_length_exceeded`,
    /// "prompt is too long"). One attempt: the same request cannot
    /// fit, so the caller must shrink the prompt instead of
    /// sleeping. Reported from a structured error body only —
    /// text-only overflow wording still classifies by status.
    ContextOverflow,
    /// Authentication / authorization failure (`invalid_api_key`,
    /// `permission_error`). One attempt: a credential problem needs
    /// a new key or a scope change, not a retry. Reported from a
    /// structured error body only — a text-only 401/403 stays
    /// `Permanent`.
    Auth,
    /// Auth, validation, model-not-found, and other caller-side
    /// failures. Retrying cannot succeed; exactly one attempt.
    Permanent,
}

/// Classify a provider failure into its [`RetryClass`].
///
/// Variant-driven first (`RateLimited`, `RequestFailed`, `Timeout`),
/// then marker sniffing for provider-opaque `Provider` payloads: the
/// typed empty-completion marker
/// ([`crate::validation::is_empty_response_error`] — dsh
/// `EMPTY_RESPONSE`), quota/billing wording (dsh `QUOTA`),
/// `"overloaded_error"` — Anthropic, `"rate_limit"` —
/// OpenAI/Anthropic message text. Everything else is
/// [`RetryClass::Permanent`]: the caller-side error taxonomy
/// (auth, validation, not-found) never benefits from a retry.
///
/// `RequestFailed` carries the status *and* the raw HTTP body, so it
/// is classified by [`classify_provider_error_body`]: a body the
/// provider shaped itself decides the class from its own
/// `type`/`code`/`message` (an OpenAI 429 with `insufficient_quota`
/// is [`RetryClass::Quota`], not [`RetryClass::RateLimit`]), while a
/// text-only body keeps the status mapping it had before R33 —
/// 429 `RateLimit`, 402 `Quota`, 5xx `Overloaded`, everything else
/// `Permanent`.
#[must_use]
pub fn classify_error(err: &Error) -> RetryClass {
    if crate::validation::is_empty_response_error(err) {
        return RetryClass::EmptyResponse;
    }
    match err {
        Error::RateLimited { .. } => RetryClass::RateLimit,
        Error::RequestFailed { status, message } => {
            classify_provider_error_body(*status, message)
        }
        Error::Timeout { .. }
        | Error::Io { .. }
        | Error::Stream { .. }
        | Error::StreamHttpFailure { .. }
        | Error::StreamProtocolError { .. }
        | Error::StreamInternal { .. } => RetryClass::Transient,
        Error::Provider { message } => {
            let normalized = normalize_markers(message);
            if is_quota_marker(&normalized) {
                RetryClass::Quota
            } else if normalized.contains("overloaded") {
                RetryClass::Overloaded
            } else if normalized.contains("rate limit") {
                RetryClass::RateLimit
            } else {
                RetryClass::Permanent
            }
        }
        _ => RetryClass::Permanent,
    }
}

/// Classify an HTTP failure from its status and raw body — the pair
/// the adapters already store in [`Error::RequestFailed`].
///
/// Precedence:
///
/// 1. a **quota/billing** body — the one signal that outranks the
///    status, because an exhausted account cannot recover by waiting
///    and OpenAI reports it with the same 429 it uses for
///    request-rate limits;
/// 2. a **capacity status** (5xx) — the status is its own signal, so
///    wording like "authentication service unavailable" must not
///    turn a recoverable overload into a caller-side failure;
/// 3. the rest of the body ladder (context overflow, auth, overload
///    wording, request rate);
/// 4. `status_class`, the pre-R33 mapping — a body with no
///    recognized signal (non-JSON, or a `type`/`code` outside the
///    marker vocabulary) classifies exactly as it did before R33.
#[must_use]
pub fn classify_provider_error_body(status: u16, raw: &str) -> RetryClass {
    let body = parse_provider_error_body(status, raw);
    let Some(markers) = body_markers(&body) else {
        return status_class(status);
    };
    if is_quota_marker(&markers) {
        return RetryClass::Quota;
    }
    if is_capacity_status(status) {
        return RetryClass::Overloaded;
    }
    classify_body_markers(&markers).unwrap_or(status_class(status))
}

/// Status-only mapping for a body that carries no structured signal.
fn status_class(status: u16) -> RetryClass {
    match status {
        429 => RetryClass::RateLimit,
        402 => RetryClass::Quota,
        _ if is_capacity_status(status) => RetryClass::Overloaded,
        _ => RetryClass::Permanent,
    }
}

/// True for the statuses that mean "the provider itself is strained".
fn is_capacity_status(status: u16) -> bool {
    matches!(status, 500 | 502 | 503 | 504)
}

/// Fold a body's structured signals into one normalized marker
/// string, or `None` when the body carried none at all (non-JSON
/// body, or a JSON object without `type`/`code`/`message`).
fn body_markers(body: &ProviderErrorBody) -> Option<String> {
    let parts: Vec<&str> = [
        body.kind.as_deref(),
        body.code.as_deref(),
        body.message.as_deref(),
    ]
    .into_iter()
    .flatten()
    .collect();
    if parts.is_empty() {
        return None;
    }
    Some(normalize_markers(&parts.join(" ")))
}

/// Body ladder for a non-capacity status. Precedence: an account that
/// ran out of context is reported with the same `invalid_request_error`
/// as any other bad request, so overflow is tested before auth, and
/// both before the rate wording every one of these messages carries.
/// `None` means "no recognized signal" — the caller falls back to the
/// status mapping.
fn classify_body_markers(normalized: &str) -> Option<RetryClass> {
    if ContextOverflowDetector::new().is_overflow(normalized) {
        return Some(RetryClass::ContextOverflow);
    }
    if is_auth_marker(normalized) {
        return Some(RetryClass::Auth);
    }
    if normalized.contains("overloaded") {
        return Some(RetryClass::Overloaded);
    }
    if normalized.contains("rate limit") {
        return Some(RetryClass::RateLimit);
    }
    None
}

/// Credential / permission wording: a new key or a scope change is
/// required, so no amount of retrying with the same credential helps.
/// Operates on [`normalize_markers`] output, so `invalid_api_key`
/// matches the same phrase as `invalid api key`.
fn is_auth_marker(normalized: &str) -> bool {
    normalized.contains("authentication")
        || normalized.contains("permission")
        || normalized.contains("unauthorized")
        || normalized.contains("forbidden")
        || normalized.contains("access denied")
        || normalized.contains("api key")
        || normalized.contains("invalid token")
        || normalized.contains("credential")
}

/// Lowercase a provider message and fold `_` / `-` separators into
/// spaces so one phrase list matches `insufficient_quota`,
/// `insufficient-quota`, and `insufficient quota` alike.
fn normalize_markers(message: &str) -> String {
    message.to_lowercase().replace(['_', '-'], " ")
}

/// Quota/billing-wording detector (dsh `isQuotaExceededError`
/// parity): exhausted account quota, balance, credits, budget, or
/// usage limits.
///
/// Operates on [`normalize_markers`] output, so all phrases are
/// space-separated.
fn is_quota_marker(normalized: &str) -> bool {
    let exhausted = normalized.contains("exceeded")
        || normalized.contains("exhausted")
        || normalized.contains("reached")
        || normalized.contains("depleted");
    normalized.contains("insufficient quota")
        || normalized.contains("insufficient balance")
        || normalized.contains("insufficient credit")
        || normalized.contains("out of credits")
        || normalized.contains("out of budget")
        || (exhausted
            && (normalized.contains("quota")
                || normalized.contains("usage limit")
                || normalized.contains("billing")))
}

/// The per-class retry budget (dsh defaults: 2 retries, 500ms →
/// 10s; tuned per class).
///
/// - `RateLimit` — 6 attempts, 1s → 30s: quota windows are the
///   longest-recovering failure and the Retry-After hint (when
///   present) already gates the first sleep.
/// - `Overloaded` — 4 attempts, 500ms → 10s: capacity bursts
///   clear fast; dsh's default shape.
/// - `Transient` — the pre-R4 [`RetryConfig::default`] (3
///   attempts, 1s → 10s), unchanged behaviour.
/// - `EmptyResponse` — 2 attempts, 500ms → 10s: dsh's default
///   bounded policy; one cheap re-ask recovers most degenerate
///   completions, and the budget must not turn into a spin.
/// - `Quota` — 1 attempt, no retry: exhausting an account quota
///   needs an operator (or a different credential), not a sleep.
/// - `ContextOverflow` — 1 attempt, no retry: the same request
///   cannot fit, the caller has to shrink the prompt.
/// - `Auth` — 1 attempt, no retry: a credential problem needs a new
///   key, not a sleep.
/// - `Permanent` — 1 attempt, no retry.
#[must_use]
pub fn retry_config_for(class: RetryClass) -> RetryConfig {
    match class {
        RetryClass::RateLimit => RetryConfig {
            max_attempts: 6,
            initial_interval_ms: 1000,
            max_interval_ms: 30000,
            max_elapsed_ms: 120000,
        },
        RetryClass::Overloaded => RetryConfig {
            max_attempts: 4,
            initial_interval_ms: 500,
            max_interval_ms: 10000,
            max_elapsed_ms: 30000,
        },
        RetryClass::Transient => RetryConfig::default(),
        RetryClass::EmptyResponse => RetryConfig {
            max_attempts: 2,
            initial_interval_ms: 500,
            max_interval_ms: 10000,
            max_elapsed_ms: 15000,
        },
        RetryClass::Quota
        | RetryClass::ContextOverflow
        | RetryClass::Auth
        | RetryClass::Permanent => RetryConfig {
            max_attempts: 1,
            initial_interval_ms: 0,
            max_interval_ms: 0,
            max_elapsed_ms: 0,
        },
    }
}

/// Retry with per-class backoff: each failure is classified and
/// the class's budget governs the next sleep.
///
/// The budget adapts per class: a 429 keeps retrying longer than
/// a timeout, an empty completion gets one cheap re-ask, and the
/// terminal classes (`Quota`, `ContextOverflow`, `Auth`,
/// `Permanent`) return after the first attempt without sleeping.
/// The elapsed-time budget resets per class — a run that starts
/// `Transient` and later trips a rate limit gets the full
/// `RateLimit` budget.
///
/// Adapter-only: the sleep is `tokio::time::sleep`, and the two
/// adapter features are the only code that calls this and are the
/// only configuration where tokio is a dependency.
#[cfg(any(feature = "anthropic", feature = "openai"))]
pub async fn retry_with_classification<F, Fut, T>(
    mut operation: F,
) -> Result<T, Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, Error>>,
{
    let mut attempt: u32 = 0;
    let start = std::time::Instant::now();
    loop {
        attempt += 1;
        match operation().await {
            Ok(value) => return Ok(value),
            Err(err) => {
                let class = classify_error(&err);
                let config = retry_config_for(class);
                // `Quota`, `ContextOverflow`, `Auth` and `Permanent`
                // are terminal: return the classifier output after the
                // first attempt without consulting the elapsed budget.
                let terminal = matches!(
                    class,
                    RetryClass::Quota
                        | RetryClass::ContextOverflow
                        | RetryClass::Auth
                        | RetryClass::Permanent
                );
                if terminal || attempt >= config.max_attempts {
                    return Err(Error::RetryExhausted {
                        attempts: attempt,
                        last_error: Box::new(err),
                    });
                }
                if config.max_elapsed_ms > 0
                    && start.elapsed().as_millis() as u64
                        >= config.max_elapsed_ms
                {
                    return Err(Error::RetryExhausted {
                        attempts: attempt,
                        last_error: Box::new(err),
                    });
                }
                let backoff_ms = config
                    .initial_interval_ms
                    .saturating_mul(1_u64 << (attempt - 1).min(16))
                    .min(config.max_interval_ms);
                tokio::time::sleep(Duration::from_millis(backoff_ms)).await;
            }
        }
    }
}

#[cfg(test)]
mod tests;
