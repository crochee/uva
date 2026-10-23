//! OpenTelemetry metrics path.
//!
//! Parallel to [`crate::metrics`] (Prometheus), but the meter
//! is sent over OTLP through the same endpoint configured for
//! trace export (`SYNTHIA_OTLP_ENDPOINT`). Together with the
//! typed-span layer in [`crate::spans`], this is the
//! observability ground floor that R-OPT-2 / R-OPT-3 / R-OPT-4
//! land typed payloads on.
//!
//! Entirely behind the `otlp` feature. With the feature off,
//! the module is gated out of `lib.rs` and the
//! `record_*` functions live only as documentation snippets;
//! the upstream callers all gate on `cfg(feature = "otlp")`
//! (or via the `synthia` facade `telemetry` feature) so a
//! build without `otlp` pays zero cost.
//!
//! ## Fallback semantics
//!
//! When `SYNTHIA_OTLP_ENDPOINT` is unset or empty,
//! [`init_otel_meter`] returns `Ok(())` without installing a
//! `MeterProvider`. Calls to [`meter`] then route through
//! `opentelemetry::global::meter_provider()`, which the OTel
//! SDK ships as a no-op default. Each `record_*` becomes a
//! one-shot attribute-less vector add against an inert meter
//! — equivalent in cost to a no-op.

use std::{sync::LazyLock, time::Duration};

use opentelemetry::{
    KeyValue,
    global,
    metrics::{Counter, Histogram, Meter},
};
use opentelemetry_otlp::{MetricExporter, WithExportConfig};
use opentelemetry_sdk::{Resource, metrics::SdkMeterProvider};
use synthia_core::Error;

use crate::tracer::{SYNTHIA_OTLP_ENDPOINT_ENV, detect_protocol};

/// Meter scope name. Visible on every emitted metric
/// (`synthia_*` instruments are then namespaced under it).
const SCOPE_NAME: &str = "synthia";

/// Per-attempt OTLP transport timeout.
const EXPORT_TIMEOUT: Duration = Duration::from_secs(5);

/// Process-wide [`Meter`] handle.
///
/// Resolved lazily on first use. When the OTLP pipeline is not
/// configured (see module docs), `global::meter_provider()`
/// returns the OTel SDK's no-op default and every
/// `record_*` call is silently discarded.
///
/// `LazyLock` is the right primitive here: the initializer
/// depends only on `global::meter_provider()` (no runtime
/// input), so the cell + initializer live together.
static METER: LazyLock<Meter> =
    LazyLock::new(|| global::meter_provider().meter(SCOPE_NAME));

/// Resolve the lazy [`Meter`]. Identical to the LazyLock's
/// deref, exposed for symmetry with the `meter()` API used
/// inside `lazy_static!`-based `metrics.rs`.
#[must_use]
pub fn meter() -> &'static Meter {
    &METER
}

/// Process-wide meter provider (the OTel pipeline's
/// [`SdkMeterProvider`]). Held so [`force_flush`] can pull the
/// last batch through before exit.
///
/// `OnceLock` — not `LazyLock` — because the initializer
/// depends on runtime input (`SYNTHIA_OTLP_ENDPOINT`, the
/// resolved protocol, and the constructed [`MetricExporter`])
/// and is set explicitly by [`init_otel_meter`].
static PROVIDER: std::sync::OnceLock<SdkMeterProvider> =
    std::sync::OnceLock::new();

/// Initialize the OTel metrics pipeline.
///
/// Idempotent: subsequent calls short-circuit on the
/// `OnceLock`. When `SYNTHIA_OTLP_ENDPOINT` is unset or empty,
/// returns `Ok(())` without installing a meter provider; all
/// `record_*` calls then become no-ops via the global no-op
/// meter (see module docs).
///
/// Errors are returned only when the SDK reports a transport
/// configuration failure. A missing endpoint is **not** an
/// error — it's the documented "console-only" path.
pub fn init_otel_meter() -> Result<(), Error> {
    if PROVIDER.get().is_some() {
        return Ok(());
    }

    let Some(endpoint) = std::env::var(SYNTHIA_OTLP_ENDPOINT_ENV)
        .ok()
        .filter(|s| !s.trim().is_empty())
    else {
        return Ok(());
    };

    let resource = Resource::builder().with_service_name("synthia").build();
    let exporter = build_exporter(&endpoint)?;
    let provider = SdkMeterProvider::builder()
        .with_resource(resource)
        .with_periodic_exporter(exporter)
        .build();

    global::set_meter_provider(provider.clone());
    let _ = PROVIDER.set(provider);
    Ok(())
}

fn build_exporter(endpoint: &str) -> Result<MetricExporter, Error> {
    match detect_protocol(endpoint) {
        crate::tracer::OtlpProtocol::Grpc => MetricExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint.to_string())
            .with_timeout(EXPORT_TIMEOUT)
            .build()
            .map_err(|e| {
                Error::telemetry(format!(
                    "Failed to build OTLP gRPC metric exporter: {e}"
                ))
            }),
        crate::tracer::OtlpProtocol::Http => MetricExporter::builder()
            .with_http()
            .with_endpoint(endpoint.to_string())
            .with_timeout(EXPORT_TIMEOUT)
            .build()
            .map_err(|e| {
                Error::telemetry(format!(
                    "Failed to build OTLP HTTP metric exporter: {e}"
                ))
            }),
    }
}

/// Force the periodic reader to flush pending metrics.
///
/// Idempotent. Safe to call from a shutdown handler without a
/// matching `init_otel_meter` — when no provider was
/// installed, this is a no-op.
pub fn force_flush() -> Result<(), Error> {
    if let Some(provider) = PROVIDER.get() {
        provider
            .force_flush()
            .map_err(|e| Error::telemetry(format!("metric flush: {e}")))?;
    }
    Ok(())
}

// --- Instruments ---

/// Instrument identity markers used by the `record_*`
/// families. Distinct
/// from `SpanKind` because metrics and traces name different
/// facets of the system. Closed enum keeps the wire name
/// (`synthia_*`) stable.
#[derive(Debug, Clone, Copy)]
enum Metric {
    LlmCalls,
    LlmInputTokens,
    LlmCacheReadTokens,
    LlmCacheWriteTokens,
    LlmOutputTokens,
    LlmLatencySeconds,
    ToolCalls,
    ToolLatencySeconds,
    SessionEvents,
    Compaction,
    Guardrail,
    Subagent,
    Interrupt,
    Approval,
}

impl Metric {
    const fn name(self) -> &'static str {
        match self {
            Self::LlmCalls => "synthia_llm_calls_total",
            Self::LlmInputTokens => "synthia_llm_input_tokens_total",
            Self::LlmCacheReadTokens => "synthia_llm_cache_read_tokens_total",
            Self::LlmCacheWriteTokens => "synthia_llm_cache_write_tokens_total",
            Self::LlmOutputTokens => "synthia_llm_output_tokens_total",
            Self::LlmLatencySeconds => "synthia_llm_latency_seconds",
            Self::ToolCalls => "synthia_tool_calls_total",
            Self::ToolLatencySeconds => "synthia_tool_latency_seconds",
            Self::SessionEvents => "synthia_session_events_total",
            Self::Compaction => "synthia_compaction_total",
            Self::Guardrail => "synthia_guardrail_total",
            Self::Subagent => "synthia_subagent_total",
            Self::Interrupt => "synthia_interrupt_total",
            Self::Approval => "synthia_approval_total",
        }
    }
}

// Instrument handles. The SDK permits registering the same
// instrument many times; documenting best practice is to keep
// a single handle per name. `LazyLock` keeps the initializer
// next to the static and removes the `get_or_init` drumbeat.
static LLM_CALLS: LazyLock<Counter<u64>> = LazyLock::new(|| {
    meter()
        .u64_counter(Metric::LlmCalls.name())
        .with_description(
            "Count of provider completion calls (success / cancel / error)",
        )
        .build()
});
static LLM_INPUT_TOKENS: LazyLock<Counter<u64>> = LazyLock::new(|| {
    meter()
        .u64_counter(Metric::LlmInputTokens.name())
        .with_description("Sum of input tokens billed across completions")
        .build()
});
static LLM_CACHE_READ_TOKENS: LazyLock<Counter<u64>> = LazyLock::new(|| {
    meter()
        .u64_counter(Metric::LlmCacheReadTokens.name())
        .with_description("Sum of prompt-cache read tokens across completions")
        .build()
});
static LLM_CACHE_WRITE_TOKENS: LazyLock<Counter<u64>> = LazyLock::new(|| {
    meter()
        .u64_counter(Metric::LlmCacheWriteTokens.name())
        .with_description("Sum of prompt-cache write tokens across completions")
        .build()
});
static LLM_OUTPUT_TOKENS: LazyLock<Counter<u64>> = LazyLock::new(|| {
    meter()
        .u64_counter(Metric::LlmOutputTokens.name())
        .with_description("Sum of output tokens produced across completions")
        .build()
});
static LLM_LATENCY_SECONDS: LazyLock<Histogram<f64>> = LazyLock::new(|| {
    meter()
        .f64_histogram(Metric::LlmLatencySeconds.name())
        .with_description(
            "End-to-end provider latency between request and final \
             completion (or first stream chunk)",
        )
        .with_unit("s")
        .build()
});
static TOOL_CALLS: LazyLock<Counter<u64>> = LazyLock::new(|| {
    meter()
        .u64_counter(Metric::ToolCalls.name())
        .with_description(
            "Count of tool executions grouped by tool name + status",
        )
        .build()
});
static TOOL_LATENCY_SECONDS: LazyLock<Histogram<f64>> = LazyLock::new(|| {
    meter()
        .f64_histogram(Metric::ToolLatencySeconds.name())
        .with_description(
            "End-to-end tool execution latency from dispatch to \
             terminal `Result`",
        )
        .with_unit("s")
        .build()
});
static SESSION_EVENTS: LazyLock<Counter<u64>> = LazyLock::new(|| {
    meter()
        .u64_counter(Metric::SessionEvents.name())
        .with_description("Count of session-level events (start / end / abort)")
        .build()
});
static COMPACTION: LazyLock<Counter<u64>> = LazyLock::new(|| {
    meter()
        .u64_counter(Metric::Compaction.name())
        .with_description(
            "Count of context-compaction events (strategy × outcome)",
        )
        .build()
});
static GUARDRAIL: LazyLock<Counter<u64>> = LazyLock::new(|| {
    meter()
        .u64_counter(Metric::Guardrail.name())
        .with_description(
            "Count of pre-execution guard verdicts (guard × verdict)",
        )
        .build()
});
static SUBAGENT: LazyLock<Counter<u64>> = LazyLock::new(|| {
    meter()
        .u64_counter(Metric::Subagent.name())
        .with_description(
            "Count of sub-agent lifecycle events (outcome × isolation)",
        )
        .build()
});
static INTERRUPT: LazyLock<Counter<u64>> = LazyLock::new(|| {
    meter()
        .u64_counter(Metric::Interrupt.name())
        .with_description(
            "Count of in-flight interruption events (kind × resolution)",
        )
        .build()
});
static APPROVAL: LazyLock<Counter<u64>> = LazyLock::new(|| {
    meter()
        .u64_counter(Metric::Approval.name())
        .with_description(
            "Count of human-approval verdicts (approve / deny / amend)",
        )
        .build()
});

/// Record one LLM completion. `status` is `"ok"` / `"error"` /
/// `"cancelled"` — a free-form label kept small to bound the
/// cardinality.
pub fn record_llm_call(provider: &str, model: &str, status: &str) {
    LLM_CALLS.add(
        1,
        &[
            KeyValue::new("provider", provider.to_string()),
            KeyValue::new("model", model.to_string()),
            KeyValue::new("status", status.to_string()),
        ],
    );
}

/// Record per-call token usage. Cache read / cache write are
/// separated from raw input tokens because Anthropic prompt
/// caching reports them on distinct fields and a dashboard
/// that conflates them misleads cost analysis.
///
/// `usize` matches the collection-indexing idiom used by
/// every provider's wire type and avoids forcing
/// `try_into().unwrap()` at every call site; the OTel counter
/// coerce-converts internally.
#[allow(clippy::too_many_arguments)]
pub fn record_llm_usage(
    provider: &str,
    model: &str,
    input: usize,
    cache_read: usize,
    cache_write: usize,
    output: usize,
) {
    let attrs = &[
        KeyValue::new("provider", provider.to_string()),
        KeyValue::new("model", model.to_string()),
    ];
    if input > 0 {
        LLM_INPUT_TOKENS.add(input as u64, attrs);
    }
    if cache_read > 0 {
        LLM_CACHE_READ_TOKENS.add(cache_read as u64, attrs);
    }
    if cache_write > 0 {
        LLM_CACHE_WRITE_TOKENS.add(cache_write as u64, attrs);
    }
    if output > 0 {
        LLM_OUTPUT_TOKENS.add(output as u64, attrs);
    }
}

/// Record end-to-end LLM call latency (`secs`). One sample
/// per call.
pub fn record_llm_latency(provider: &str, model: &str, secs: f64) {
    LLM_LATENCY_SECONDS.record(
        secs,
        &[
            KeyValue::new("provider", provider.to_string()),
            KeyValue::new("model", model.to_string()),
        ],
    );
}

/// Record one tool execution at its end. `status` is
/// `"ok"` / `"error"` / `"panicked"` (mirrors `metrics.rs`).
pub fn record_tool_call(tool_name: &str, status: &str) {
    TOOL_CALLS.add(
        1,
        &[
            KeyValue::new("tool", tool_name.to_string()),
            KeyValue::new("status", status.to_string()),
        ],
    );
}

/// Record tool-execution latency.
pub fn record_tool_latency(tool_name: &str, secs: f64) {
    TOOL_LATENCY_SECONDS
        .record(secs, &[KeyValue::new("tool", tool_name.to_string())]);
}

/// Record one session-level event. `event_type` is free-form
/// for now; switch to a closed enum when the event set
/// stabilises.
pub fn record_session_event(event_type: &str) {
    SESSION_EVENTS
        .add(1, &[KeyValue::new("event_type", event_type.to_string())]);
}

/// Record one compaction. `strategy` is the strategy name,
/// `outcome` is `"ok"` / `"noop"` / `"error"`.
pub fn record_compaction(strategy: &str, outcome: &str) {
    COMPACTION.add(
        1,
        &[
            KeyValue::new("strategy", strategy.to_string()),
            KeyValue::new("outcome", outcome.to_string()),
        ],
    );
}

/// Record one guardrail verdict. `verdict` is
/// `"allow"` / `"deny"` / `"sanitize"` / `"panic"`.
pub fn record_guardrail(name: &str, verdict: &str) {
    GUARDRAIL.add(
        1,
        &[
            KeyValue::new("guard", name.to_string()),
            KeyValue::new("verdict", verdict.to_string()),
        ],
    );
}

/// Record one sub-agent spawn event. `outcome` is
/// `"spawned"` / `"completed"` / `"failed"` /
/// `"depth_exceeded"`. `isolation` is `"worktree"` / `"none"`.
pub fn record_subagent(outcome: &str, isolation: &str) {
    SUBAGENT.add(
        1,
        &[
            KeyValue::new("outcome", outcome.to_string()),
            KeyValue::new("isolation", isolation.to_string()),
        ],
    );
}

/// Record one in-flight interruption. `kind` is the
/// interruption class (e.g. `"escalated_approval"`,
/// `"rate_limited"`, `"cancelled"`). `resolved` is
/// `"handled"` / `"aborted"`.
pub fn record_interrupt(kind: &str, resolved: &str) {
    INTERRUPT.add(
        1,
        &[
            KeyValue::new("kind", kind.to_string()),
            KeyValue::new("resolved", resolved.to_string()),
        ],
    );
}

/// Record one approval decision. `verdict` is `"approve"` /
/// `"deny"` / `"amend"`.
pub fn record_approval(verdict: &str) {
    APPROVAL.add(1, &[KeyValue::new("verdict", verdict.to_string())]);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn clear_endpoint() {
        // `std::env::remove_var` is `unsafe` from 2024; the
        // telemetry tests run single-threaded under
        // `cargo test` by default. The guard below pins the
        // env to empty so the no-op fallback path is exercised.
        // SAFETY: tests run single-threaded; no other thread
        // reads `SYNTHIA_OTLP_ENDPOINT` between the unset
        // and the assertion below.
        unsafe {
            std::env::remove_var(SYNTHIA_OTLP_ENDPOINT_ENV);
        }
    }

    #[test]
    fn init_otel_meter_is_noop_without_endpoint() {
        clear_endpoint();
        // First call after `clear_endpoint` may have already
        // been cached by a sibling test; either way, the
        // function returns Ok and does not panic.
        init_otel_meter().expect("absent endpoint is not an error");
        record_llm_call("anthropic", "claude-sonnet-4-5", "ok");
        record_tool_call("read", "ok");
        record_llm_latency("anthropic", "claude-sonnet-4-5", 0.42);
        record_tool_latency("read", 0.05);
        record_session_event("start");
        record_compaction("truncate_old", "ok");
        record_guardrail("noop", "allow");
        record_subagent("spawned", "worktree");
        record_interrupt("cancelled", "handled");
        record_approval("approve");
        force_flush().expect("force_flush with no provider is a no-op");
    }

    #[test]
    fn record_llm_usage_partitions_tokens() {
        clear_endpoint();
        // Input + cache-read + cache-write + output all
        // branch into different counters; only the non-zero
        // partitions are emitted. Guards the no-op fallback
        // (zero tokens is a quietly-skipped branch).
        record_llm_usage("openai", "gpt-5", 0, 0, 0, 0);
        record_llm_usage("openai", "gpt-5", 100, 50, 30, 80);
    }
}
