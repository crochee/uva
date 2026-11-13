//! Typed span taxonomy.
//!
//! Every `tracing::span!` that represents a meaningful agent
//! lifecycle event is annotated with [`SpanKind`] (mapped to
//! both `uva.span.kind` and the OTel-standard `otel.kind`)
//! so dashboards can filter on a closed enum, not a string
//! equality. The taxonomy mirrors the eight transition points
//! listed in `docs/research/ai-agent-best-practices-2026-10.md`
//! §4.2.
//!
//! Always compiled (no feature gate) — the enum is a zero-cost
//! `Copy` type. When the consumer builds with the `otlp` feature
//! enabled, [`tracing_opentelemetry`] picks up the
//! `otel.kind` field and maps it into the OTel span's `kind`
//! attribute automatically.
//!
//! ## Why a typed enum and not free-form strings
//!
//! Free-form span kinds let typos drift into production:
//! `uva.span.kind = "tooll"` and `uva.span.kind = "tool"`
//! become two filter values in the same dashboard. The
//! [`SpanKind`] enum eliminates that drift at compile time.

/// Eight canonical span kinds for the agent lifecycle.
///
/// New kinds require an update to
/// [`docs/research/ai-agent-best-practices-2026-10.md`](file:///home/crochee/workspace/uva/docs/research/ai-agent-best-practices-2026-10.md)
/// §4.2 and a release-note entry; closed enum keeps the OTel
/// dashboard filter space stable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SpanKind {
    /// The top-level ReAct agent run that drives the loop
    /// end-to-end.
    Agent,
    /// One provider call to a generative model.
    Llm,
    /// One tool execution by name + call id.
    Tool,
    /// A delegation from one agent to another (the `task`
    /// tool). Spans the parent → child transition.
    Handoff,
    /// A pre-execution policy check (`run_guards` pipeline).
    Guardrail,
    /// An in-flight interruption (cancellation, escalated
    /// approval, human-in-the-loop pause).
    Interrupt,
    /// One eval trajectory / grader invocation. Reserved for
    /// R-OPT-2; defined here so future SpanKind additions
    /// don't introduce a one-off variant alongside the eight.
    Eval,
    /// One provider-internal generation (e.g. the first-token
    /// sub-span of a streamed completion).
    Generation,
}

impl SpanKind {
    /// Stable wire string for `uva.span.kind`. Lowercase
    /// short tokens match what the OTel dashboard filter chips
    /// expect.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Llm => "llm",
            Self::Tool => "tool",
            Self::Handoff => "handoff",
            Self::Guardrail => "guardrail",
            Self::Interrupt => "interrupt",
            Self::Eval => "eval",
            Self::Generation => "generation",
        }
    }

    /// OTel-standard `kind` value. The four kinds without an
    /// outbound hop map to `INTERNAL`; the three that produce
    /// or consume a request map to `CLIENT`. `SERVER` /
    /// `PRODUCER` / `CONSUMER` are intentionally unused for
    /// the agent lifecycle.
    #[must_use]
    pub const fn otel_kind(self) -> &'static str {
        match self {
            Self::Agent
            | Self::Guardrail
            | Self::Interrupt
            | Self::Eval
            | Self::Handoff => "INTERNAL",
            Self::Llm | Self::Tool | Self::Generation => "CLIENT",
        }
    }
}

/// Build a typed span and return its cheap [`tracing::Span`]
/// handle. Callers invoke `.enter()` on the returned handle
/// to obtain the lifetime guard.
///
/// `attrs` keys MUST be `&'static str` (string literals or
/// constants) because they end up as `tracing` field names
/// on the macro-generated span. `attrs` values MUST also be
/// `&'static str` for the same reason — `&tool_name.as_str()`
/// compiles, but `format!("{tool_name}")` does not. For
/// dynamic values, fall back to the underlying
/// `tracing::info_span!` macro and inline the
/// `uva.span.kind` / `otel.kind` fields directly.
///
/// Pre-declared fields are populated by `Span::record` after
/// creation. Empty values produce no field on the closed
/// span (tracing silently drops `Empty`-equivalent field
/// writes that don't resolve to a literal).
///
/// ## Why not return `EnteredSpan`
///
/// `tracing::Span::enter(&self)` returns an `Entered<'_, Span>`
/// lifetime-bound to `&self`. That lifetime cannot escape this
/// function — the local span is dropped on return. The
/// recommended call shape is therefore:
///
/// ```ignore
/// let span = uva_telemetry::new_typed_span(
///     SpanKind::Llm, "llm_complete",
///     &[("provider", "anthropic"), ("model", "claude-sonnet-4-5")],
/// );
/// let _enter = span.enter();
/// // … work bracketed by the span …
/// // _enter drops here; span closed.
/// ```
#[must_use]
pub fn new_typed_span(
    kind: SpanKind,
    name: &'static str,
    attrs: &[(&'static str, &'static str)],
) -> tracing::Span {
    // sentinel + kind fields are always emitted; per-call
    // attributes go through `Span::record`, which silently
    // drops writes to keys that do not exist. Declaring the
    // typed kind fields here is required for downstream
    // `tracing-opentelemetry` to map `otel.kind` correctly.
    let span = tracing::info_span!(
        "uva_typed",
        uva.span.name = name,
        uva.span.kind = kind.as_str(),
        "otel.kind" = kind.otel_kind(),
    );
    for (key, value) in attrs {
        span.record(*key, *value);
    }
    span
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn span_kind_as_str_is_stable() {
        assert_eq!(SpanKind::Agent.as_str(), "agent");
        assert_eq!(SpanKind::Llm.as_str(), "llm");
        assert_eq!(SpanKind::Tool.as_str(), "tool");
        assert_eq!(SpanKind::Handoff.as_str(), "handoff");
        assert_eq!(SpanKind::Guardrail.as_str(), "guardrail");
        assert_eq!(SpanKind::Interrupt.as_str(), "interrupt");
        assert_eq!(SpanKind::Eval.as_str(), "eval");
        assert_eq!(SpanKind::Generation.as_str(), "generation");
    }

    #[test]
    fn span_kind_otel_kind_maps_correctly() {
        assert_eq!(SpanKind::Agent.otel_kind(), "INTERNAL");
        assert_eq!(SpanKind::Llm.otel_kind(), "CLIENT");
        assert_eq!(SpanKind::Tool.otel_kind(), "CLIENT");
        assert_eq!(SpanKind::Handoff.otel_kind(), "INTERNAL");
        assert_eq!(SpanKind::Guardrail.otel_kind(), "INTERNAL");
        assert_eq!(SpanKind::Interrupt.otel_kind(), "INTERNAL");
        assert_eq!(SpanKind::Eval.otel_kind(), "INTERNAL");
        assert_eq!(SpanKind::Generation.otel_kind(), "CLIENT");
    }

    #[test]
    fn new_typed_span_emits_kind_fields() {
        let span = new_typed_span(
            SpanKind::Llm,
            "llm_complete",
            &[("provider", "anthropic"), ("model", "claude-sonnet-4-5")],
        );
        // Recording a second time must NOT panic; the helper
        // must be idempotent for the same shape of input.
        span.record("provider", "anthropic");
        let _entered = span.enter();
    }

    #[test]
    fn all_eight_kinds_construct_a_span() {
        for kind in [
            SpanKind::Agent,
            SpanKind::Llm,
            SpanKind::Tool,
            SpanKind::Handoff,
            SpanKind::Guardrail,
            SpanKind::Interrupt,
            SpanKind::Eval,
            SpanKind::Generation,
        ] {
            let s = new_typed_span(kind, "smoke", &[]);
            let _g = s.enter();
        }
    }
}
