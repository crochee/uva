//! # synthia-telemetry
//!
//! Observability, in two independently switchable halves:
//!
//! | Half | Feature | What it brings |
//! |---|---|---|
//! | console + file logging | *always on* | `tracing` / `tracing-subscriber` |
//! | Prometheus vectors | `metrics` | `prometheus` + `lazy_static` |
//! | OTLP trace export | `otlp` | the `opentelemetry*` family, `tracing-opentelemetry`, gRPC and HTTP transports |
//!
//! `default = ["metrics", "otlp"]` — the full stack, which is what a
//! deployment (`synthia-server`) wants. A consumer that only needs the
//! tool metrics from [`metrics`] takes
//! `synthia-telemetry = { version = "0.1", default-features = false, features = ["metrics"] }`
//! and never compiles an exporter; a consumer that only wants logs
//! takes `default-features = false` and gets
//! [`init_tracing`] → [`TracingInit::Console`].
//!
//! The `otlp` feature gates [`propagation`] and [`tracer`]
//! together: W3C `traceparent` extraction only means something when
//! there is an SDK to hand the span context to.
//!
//! See the crate README for the environment variables
//! (`SYNTHIA_OTLP_ENDPOINT`, `SYNTHIA_OTEL_SAMPLER`,
//! `SYNTHIA_LOG_DIR`) and the protocol-selection rules.

// Allow `result_large_err` for the whole file: P1b added 4 hidden
// fields to every struct-form variant (frames, backtrace, source,
// and the synthetic source chain), so every `Result<_, Error>` is
// at least 128 bytes. Boxing the error would force every call site
// to `.map_err(|e| *e)` (or accept the allocation), and the existing
// API has no `Box<Error>` in the public surface. Accept the size
// cost; revisit if profiling shows it matters.
#![allow(clippy::result_large_err)]

pub mod console;
#[cfg(feature = "metrics")]
pub mod metrics;
#[cfg(feature = "otlp")]
pub mod otel_metrics;
#[cfg(feature = "otlp")]
pub mod propagation;
#[cfg(all(feature = "otlp", feature = "redact"))]
pub mod redaction;
pub mod spans;
#[cfg(feature = "otlp")]
pub mod tracer;

// Only the two env-var names and `TracingInit` are the crate's
// public entry contract here; the tracer module's protocol and
// sampler helpers are internal plumbing reached through
// `init_tracing`.
pub use console::SYNTHIA_LOG_DIR_ENV;
#[cfg(feature = "metrics")]
pub use metrics::{
    HTTP_REQUESTS_DURATION_SECONDS,
    HTTP_REQUESTS_TOTAL,
    TEXT_EXPOSITION_CONTENT_TYPE,
    gather_text,
    record_tool_duration,
    record_tool_outcome_metric,
    record_tool_panic,
    record_tool_truncation,
};
#[cfg(feature = "otlp")]
pub use otel_metrics::{
    force_flush,
    init_otel_meter,
    record_approval,
    record_compaction,
    record_guardrail,
    record_interrupt,
    record_llm_usage,
    record_session_event,
    record_subagent,
    record_tool_latency,
};

/// Always-available wrapper for OTel metrics emission. Real
/// when the `otlp` feature is compiled in, no-op otherwise —
/// so harness callers don't need their own `cfg(feature = ...)`
/// guards.
///
/// `status` is `"ok"` / `"error"` / `"cancelled"` / `"panicked"`
/// (free-form, kept small to bound metric cardinality).
#[allow(unused_variables)]
pub fn record_llm_call(provider: &str, model: &str, status: &str) {
    #[cfg(feature = "otlp")]
    otel_metrics::record_llm_call(provider, model, status);
}

/// Always-available wrapper for OTel metrics emission (LLM call
/// wall-clock latency, seconds). Real when `otlp` is on,
/// no-op otherwise.
#[allow(unused_variables)]
pub fn record_llm_latency(provider: &str, model: &str, secs: f64) {
    #[cfg(feature = "otlp")]
    otel_metrics::record_llm_latency(provider, model, secs);
}

/// Always-available wrapper for OTel metrics emission (tool
/// call counter). Real when `otlp` is on, no-op otherwise.
#[allow(unused_variables)]
pub fn record_tool_call(tool_name: &str, status: &str) {
    #[cfg(feature = "otlp")]
    otel_metrics::record_tool_call(tool_name, status);
}
#[cfg(feature = "otlp")]
pub use propagation::{
    ExtractedTraceContext,
    InjectedTraceContext,
    TRACEPARENT_HEADER,
    TRACESTATE_HEADER,
    extract_trace_context,
    format_span_id,
    format_trace_id,
    inject_trace_context,
};
pub use spans::{SpanKind, new_typed_span};
use synthia_core::Error;
#[cfg(feature = "otlp")]
pub use tracer::{SYNTHIA_OTEL_SAMPLER_ENV, SYNTHIA_OTLP_ENDPOINT_ENV};

#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    pub service_name: String,
    pub log_level: String,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            service_name: "synthia".to_string(),
            log_level: "info".to_string(),
        }
    }
}

/// What [`init_tracing`] installed.
///
/// The `Otlp` variant exists only with the `otlp` feature; without it
/// the enum is a single-variant [`TracingInit::Console`], so a caller
/// that matches on it keeps compiling whichever way the crate was
/// built (the exporter's `SdkTracerProvider` is only nameable when it
/// was compiled in).
pub enum TracingInit {
    /// The OTLP pipeline is installed. The provider is returned so the
    /// caller can keep it alive for the process's lifetime and
    /// `shutdown()` it on exit to flush pending spans.
    #[cfg(feature = "otlp")]
    Otlp(opentelemetry_sdk::trace::SdkTracerProvider),
    /// Console tracing only — either no OTLP endpoint was configured or
    /// the crate was built without the `otlp` feature.
    Console,
}

/// Initialize tracing: console (plus an optional file layer) always,
/// OTLP when it is configured *and* compiled in.
///
/// ## OTLP selection
///
/// With the `otlp` feature, `SYNTHIA_OTLP_ENDPOINT` decides:
/// - set: installs the OTLP pipeline (gRPC or HTTP transport
///   auto-detected from the endpoint URL scheme);
/// - unset: falls back to console tracing.
///
/// Without the feature the OTLP branch does not exist; setting
/// `SYNTHIA_OTLP_ENDPOINT` has no effect.
///
/// ## File logging
///
/// When `SYNTHIA_LOG_DIR` is set, a file logging layer is composed
/// alongside the console fmt layer, writing to
/// `{SYNTHIA_LOG_DIR}/synthia.log` in append mode with ANSI codes
/// disabled.
///
/// Composition limitation: if both `SYNTHIA_LOG_DIR` and
/// `SYNTHIA_OTLP_ENDPOINT` are set, the file layer wins and OTLP is
/// skipped — the OTLP pipeline installs its own subscriber via
/// `try_init`, which cannot be composed with an additional layer in a
/// single global subscriber.
pub fn init_tracing(config: &TelemetryConfig) -> Result<TracingInit, Error> {
    // Try to install a file layer if SYNTHIA_LOG_DIR is set. On failure we
    // warn and proceed without file logging (existing behavior).
    let file_layer: Option<
        Box<
            dyn tracing_subscriber::Layer<tracing_subscriber::Registry>
                + Send
                + Sync,
        >,
    > = std::env::var(SYNTHIA_LOG_DIR_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
        .and_then(|dir| {
            let path = std::path::PathBuf::from(dir);
            match console::make_file_layer(&path) {
                Ok(layer) => Some(layer),
                Err(e) => {
                    eprintln!("Warning: file logging init failed: {}", e);
                    None
                }
            }
        });

    match file_layer {
        Some(fl) => {
            // File layer present → console + file. The OTLP pipeline can't
            // share a global subscriber, so we skip it even if configured.
            #[cfg(feature = "otlp")]
            if std::env::var_os(tracer::SYNTHIA_OTLP_ENDPOINT_ENV).is_some() {
                eprintln!(
                    "Warning: SYNTHIA_LOG_DIR and SYNTHIA_OTLP_ENDPOINT both set; \
                     OTLP tracing skipped (Phase 0 limitation)"
                );
            }
            console::init_console_with_file(config, fl)?;
            Ok(TracingInit::Console)
        }
        // No file layer → the OTLP pipeline when it is compiled in,
        // console otherwise.
        None => init_without_file_layer(config),
    }
}

/// The no-file-layer path: OTLP when compiled in, console when not.
#[cfg(feature = "otlp")]
fn init_without_file_layer(
    config: &TelemetryConfig,
) -> Result<TracingInit, Error> {
    tracer::init_otlp_tracing(config)
}

/// The no-file-layer path without the `otlp` feature: console only.
#[cfg(not(feature = "otlp"))]
fn init_without_file_layer(
    config: &TelemetryConfig,
) -> Result<TracingInit, Error> {
    console::init_console_tracing(config)?;
    Ok(TracingInit::Console)
}

/// Flush all pending OTLP spans and metrics to the configured
/// endpoint, then shut down the global tracer provider so no
/// further spans are recorded.
///
/// Idempotent: subsequent calls are no-ops (the global tracer
/// provider slot is `take()`-n on the first shutdown, and the
/// metrics [`force_flush`] short-circuits when no provider
/// was installed). Equivalent to OpenAI Agents SDK's
/// `flush_traces()`. Wired from the server's
/// graceful-shutdown path so SIGTERM / Ctrl-C does not lose
/// the last batch.
#[cfg(feature = "otlp")]
pub fn flush_traces() -> Result<(), Error> {
    let provider = TRACE_PROVIDER.lock().take();
    if let Some(provider) = provider {
        provider
            .shutdown()
            .map_err(|e| Error::telemetry(format!("tracer shutdown: {e}")))?;
    }
    crate::otel_metrics::force_flush()
}

/// Process-global SdkTracerProvider, set once by
/// [`init_otlp_tracing`](crate::tracer::init_otlp_tracing).
/// `flush_traces` calls `take()` on the slot via a
/// `parking_lot::Mutex<Option<_>>` to shut it down exactly
/// once even when invoked from a panic path. We need a
/// `Mutex` (not `OnceLock`) because `OnceLock` has no
/// `take()` API. `parking_lot` instead of `std::sync`
/// because the lock is held for at most one assignment /
/// one take across the process lifetime, never across an
/// `.await`, and the unwrap boilerplate has no value here.
#[cfg(feature = "otlp")]
static TRACE_PROVIDER: parking_lot::Mutex<
    Option<opentelemetry_sdk::trace::SdkTracerProvider>,
> = parking_lot::Mutex::new(None);

#[cfg(feature = "otlp")]
pub(crate) fn store_tracer_provider(
    provider: opentelemetry_sdk::trace::SdkTracerProvider,
) {
    *TRACE_PROVIDER.lock() = Some(provider);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_telemetry_config_default() {
        let config = TelemetryConfig::default();
        assert_eq!(config.service_name, "synthia");
        assert_eq!(config.log_level, "info");
    }

    /// V3 plan gate: `flush_traces` MUST be safe to call
    /// multiple times in a row, since the server's
    /// graceful-shutdown path and test teardowns both want to
    /// invoke it. With no collector configured, the function
    /// just routes through the no-op global meter / unset
    /// tracer provider; both calls must therefore return `Ok`
    /// without panicking.
    #[cfg(feature = "otlp")]
    #[test]
    fn flush_traces_is_idempotent() {
        // SAFETY: tests run single-threaded. No other thread
        // inspects the env between the unset and the assertion.
        unsafe {
            std::env::remove_var(crate::tracer::SYNTHIA_OTLP_ENDPOINT_ENV);
        }
        flush_traces().expect("first flush must succeed");
        flush_traces().expect("second flush must also succeed");
    }
}
