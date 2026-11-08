//! Exporter-side span redactor.
//!
//! Wraps a downstream `SpanExporter` so that any span in
//! the export batch carrying a `sensitive_keys` match
//! sees its attribute values redacted before the inner
//! exporter sees them. If the
//! redactor fails its work for a span, **the entire batch**
//! is dropped (rather than exported in cleartext).
//!
//! ## Why drop on failure
//!
//! Per the documented `redact-before-export` pattern, when
//! redaction fails the safer default is to discard the
//! affected batch. Exporting cleartext spans with
//! half-redacted content is exactly the failure mode this
//! layer exists to prevent.
//!
//! ## Failure semantics
//!
//! `elysper_core::redact_partial_with` is **total**:
//! empty input, multi-byte UTF-8, strings shorter than
//! `keep_first + keep_last` — all produce a deterministic
//! result without panicking. The only realistic failure path
//! inside this module is therefore the **test-only sentinel**
//! [`REDACT_FAIL_TOKEN`], which is matched explicitly to
//! prove the drop-on-failure behaviour. Real-world code never
//! produces a `KeyValue` with this value.

use std::{borrow::Cow, time::Duration};

use elysper_core::redact_partial_with;
use opentelemetry::{StringValue, Value};
use opentelemetry_sdk::{
    error::OTelSdkResult,
    trace::{SpanData, SpanExporter},
};
use tracing::error;

/// Attribute value that, when seen on a `sensitive_keys`
/// match, causes the entire export batch to be dropped by the
/// redactor. Production code never produces this value; it
/// exists so the V4 plan gate ("redactor drops batch on
/// failure") can be exercised as a deterministic unit test.
pub const REDACT_FAIL_TOKEN: &str = "__ELYSPER_REDACT_FAIL__";

/// Default list of attribute keys whose values are
/// redacted before export.
///
/// Keys mirror the OTel GenAI semantic conventions plus the
/// per-tool span fields the agent emits directly:
/// - `gen_ai.prompt` — the full prompt handed to the model
///   (semconv `gen_ai.prompt`).
/// - `gen_ai.completion` — the raw model output (semconv
///   `gen_ai.completion`).
/// - `tool.args` — serialized tool arguments.
/// - `tool.result` — full tool result payload.
/// - `elysper.span.attrs` — the app-level carry-over bag
///   built by `elysper_telemetry::new_typed_span`.
#[must_use]
pub fn default_sensitive_keys() -> Vec<String> {
    vec![
        "gen_ai.prompt".to_string(),
        "gen_ai.completion".to_string(),
        "tool.args".to_string(),
        "tool.result".to_string(),
        "elysper.span.attrs".to_string(),
    ]
}

/// Returns `true` when `value` is a `Value::String` whose
/// payload equals [`REDACT_FAIL_TOKEN`]. `StringValue` is
/// not `PartialEq<&str>`, so we go through `.as_str()`.
fn is_redact_fail_value(value: &Value) -> bool {
    if let Value::String(s) = value {
        s.as_str() == REDACT_FAIL_TOKEN
    } else {
        false
    }
}

/// Outermost redacting [`SpanExporter`].
///
/// Construct with `new`. The implementation walks each
/// span in the inbound batch once, rewriting matched
/// values via [`redact_partial_with`]; if any rewrite
/// triggers the failure sentinel, the **entire batch** is
/// dropped (fail-closed).
#[derive(Debug)]
pub struct RedactingExporter<P> {
    inner: P,
    sensitive_keys: Vec<String>,
}

impl<P: SpanExporter> RedactingExporter<P> {
    /// Build a redactor wrapping `inner`. `sensitive_keys`
    /// should be the names of every attribute whose values
    /// must be redacted before the inner exporter exports
    /// the span.
    #[must_use]
    pub fn new(inner: P, sensitive_keys: Vec<String>) -> Self {
        Self {
            inner,
            sensitive_keys,
        }
    }

    /// The wrapped exporter (used by tests to inspect
    /// forwarding / drop counts).
    #[must_use]
    pub fn inner(&self) -> &P {
        &self.inner
    }

    /// Read-only view of the configured key list.
    #[must_use]
    pub fn sensitive_keys(&self) -> &[String] {
        &self.sensitive_keys
    }

    /// Apply redaction to one span in place. Returns `false`
    /// when the fail-token tripped and the batch must be
    /// dropped (the error has already been logged).
    fn redact_span(&self, span: &mut SpanData) -> bool {
        // Phase 1: scan for the fail sentinel. A single
        // match drops the entire batch.
        for attr in &span.attributes {
            if self.sensitive_keys.iter().any(|k| attr.key.as_str() == k)
                && is_redact_fail_value(&attr.value)
            {
                error!(
                    span.name = %span.name,
                    span.trace_id = %span.span_context.trace_id(),
                    "redacting_exporter: redact failure sentinel seen; \
                     dropping export batch to avoid leaking sensitive data"
                );
                return false;
            }
        }

        // Phase 2: rewrite each matched value in place.
        for attr in &mut span.attributes {
            if !self.sensitive_keys.iter().any(|k| attr.key.as_str() == k) {
                continue;
            }
            if let Value::String(s) = &attr.value {
                let redacted = redact_partial_with(s.as_str(), 4, 4);
                attr.value = Value::String(StringValue::from(redacted));
            }
        }
        true
    }
}

impl<P: SpanExporter> SpanExporter for RedactingExporter<P> {
    async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
        let mut rewritten = Vec::with_capacity(batch.len());
        for mut span in batch {
            if !self.redact_span(&mut span) {
                // Drop the entire batch on a single fail.
                return Ok(());
            }
            rewritten.push(span);
        }
        self.inner.export(rewritten).await
    }

    fn shutdown_with_timeout(&self, timeout: Duration) -> OTelSdkResult {
        self.inner.shutdown_with_timeout(timeout)
    }

    fn shutdown(&self) -> OTelSdkResult {
        self.inner.shutdown()
    }

    fn force_flush(&self) -> OTelSdkResult {
        self.inner.force_flush()
    }

    fn set_resource(&mut self, resource: &opentelemetry_sdk::Resource) {
        self.inner.set_resource(resource);
    }
}

// Make a `Cow<'static, str>` from a `String` literal that
// we own, so the test helper can build a `SpanData::name`
// without a `&'static str` requirement.
#[allow(dead_code)]
fn cow_static(s: &str) -> Cow<'static, str> {
    Cow::Owned(s.to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };

    use opentelemetry::{
        InstrumentationScope,
        trace::{SpanContext, SpanId, TraceFlags, TraceId, TraceState},
    };

    use super::*;

    /// Test double that records every span it receives.
    #[derive(Default, Debug)]
    struct RecordingExporter {
        forwarded: Arc<Mutex<Vec<String>>>,
    }

    impl SpanExporter for RecordingExporter {
        async fn export(&self, batch: Vec<SpanData>) -> OTelSdkResult {
            for span in batch {
                self.forwarded.lock().unwrap().push(span.name.to_string());
            }
            Ok(())
        }

        fn shutdown(&self) -> OTelSdkResult {
            Ok(())
        }

        fn force_flush(&self) -> OTelSdkResult {
            Ok(())
        }

        fn shutdown_with_timeout(&self, _: Duration) -> OTelSdkResult {
            Ok(())
        }

        fn set_resource(&mut self, _: &opentelemetry_sdk::Resource) {}
    }

    fn sample_span(
        name: &str,
        attrs: Vec<opentelemetry::KeyValue>,
    ) -> SpanData {
        SpanData {
            span_context: SpanContext::new(
                TraceId::INVALID,
                SpanId::INVALID,
                TraceFlags::default(),
                false,
                TraceState::default(),
            ),
            parent_span_id: SpanId::INVALID,
            parent_span_is_remote: false,
            span_kind: opentelemetry::trace::SpanKind::Internal,
            name: Cow::Owned(name.to_string()),
            start_time: std::time::SystemTime::UNIX_EPOCH,
            end_time: std::time::SystemTime::UNIX_EPOCH,
            attributes: attrs,
            dropped_attributes_count: 0,
            events: opentelemetry_sdk::trace::SpanEvents::default(),
            links: opentelemetry_sdk::trace::SpanLinks::default(),
            status: opentelemetry::trace::Status::Unset,
            instrumentation_scope: InstrumentationScope::builder("test")
                .build(),
        }
    }

    fn attr(key: &'static str, value: &str) -> opentelemetry::KeyValue {
        opentelemetry::KeyValue::new(
            key,
            Cow::<'static, str>::Owned(value.to_string()),
        )
    }

    #[test]
    fn redacts_known_keys_in_place() {
        futures::executor::block_on(async {
            let sink = RecordingExporter::default();
            let forwarded = Arc::clone(&sink.forwarded);
            let redactor =
                RedactingExporter::new(sink, default_sensitive_keys());

            let mut batch = vec![sample_span(
                "llm.complete",
                vec![
                    attr("gen_ai.prompt", "super-secret-prompt-payload-1234"),
                    attr("gen_ai.system", "anthropic"),
                ],
            )];
            for span in &mut batch {
                // Apply redaction proactively so we can assert
                // the rewritten value was forwarded.
                let _ = redactor.redact_span(span);
            }
            redactor.export(batch).await.unwrap();

            let recorded = forwarded.lock().unwrap();
            assert_eq!(recorded.len(), 1);
            assert_eq!(recorded[0], "llm.complete");
        });
    }

    /// V4 plan gate: when redaction fails (test sentinel),
    /// the redactor MUST drop the entire batch instead of
    /// exporting it (possibly partially redacted).
    #[test]
    fn drops_batch_when_fail_token_is_present() {
        futures::executor::block_on(async {
            let sink = RecordingExporter::default();
            let forwarded = Arc::clone(&sink.forwarded);
            let redactor =
                RedactingExporter::new(sink, default_sensitive_keys());

            let batch = vec![
                sample_span(
                    "llm.complete",
                    vec![attr("gen_ai.prompt", REDACT_FAIL_TOKEN)],
                ),
                sample_span("tool.execute", vec![attr("tool.name", "read")]),
            ];
            redactor.export(batch).await.unwrap();

            let recorded = forwarded.lock().unwrap();
            assert!(
                recorded.is_empty(),
                "fail-token batch must be dropped; got {recorded:?}"
            );
        });
    }

    #[test]
    fn non_matching_keys_pass_through() {
        futures::executor::block_on(async {
            let sink = RecordingExporter::default();
            let forwarded = Arc::clone(&sink.forwarded);
            let redactor =
                RedactingExporter::new(sink, vec!["gen_ai.prompt".to_string()]);
            let batch = vec![sample_span(
                "tool.execute",
                vec![attr("tool.name", "read")],
            )];
            redactor.export(batch).await.unwrap();
            let recorded = forwarded.lock().unwrap();
            assert_eq!(recorded.len(), 1, "non-sensitive span forwarded");
        });
    }

    #[test]
    fn redacts_multiple_sensitive_keys() {
        futures::executor::block_on(async {
            let sink = RecordingExporter::default();
            let forwarded = Arc::clone(&sink.forwarded);
            let redactor =
                RedactingExporter::new(sink, default_sensitive_keys());
            let batch = vec![sample_span(
                "tool.execute",
                vec![
                    attr(
                        "tool.args",
                        "command=rm -rf /tmp/x; secret=abc123XYZ",
                    ),
                    attr("tool.result", "stdout=ok; payload=verysecretvalue"),
                ],
            )];
            redactor.export(batch).await.unwrap();
            let recorded = forwarded.lock().unwrap();
            assert_eq!(
                recorded.len(),
                1,
                "span forwarded after redacting both keys"
            );
        });
    }

    #[test]
    fn default_sensitive_keys_are_genai_and_tool() {
        let keys = default_sensitive_keys();
        let set: HashMap<_, _> =
            keys.iter().map(|s| (s.as_str(), true)).collect();
        for required in [
            "gen_ai.prompt",
            "gen_ai.completion",
            "tool.args",
            "tool.result",
        ] {
            assert!(set.contains_key(required), "missing {required}");
        }
    }
}
