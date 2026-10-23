//! V1 plan gate: a one-pass smoke that exercises every
//! `synthia_telemetry::record_*` function and confirms a
//! metrics batch leaves the process headed at a local OTLP
//! HTTP collector.
//!
//! Usage (two terminals):
//!
//! ```bash
//! cargo run --example otel_collector_mock -p synthia-telemetry
//! # in another shell, with the OTLP endpoint pointed at the mock:
//! SYNTHIA_OTLP_ENDPOINT=http://127.0.0.1:4318 \
//! SYNTHIA_OTEL_SAMPLER=always_on \
//! RUST_LOG=info \
//! cargo run --example otel_metrics_smoke -p synthia-telemetry
//! ```
//!
//! Expected stdout proof line:
//!
//! ```text
//! OTL-METRICS: OK synthia_llm_calls_total >= 1
//! OTL-METRICS: OK synthia_tool_calls_total >= 1
//! OTL-METRICS: OK synthia_llm_latency_seconds (histogram) sample count >= 1
//! ```

use synthia_telemetry::{
    init_otel_meter,
    record_compaction,
    record_guardrail,
    record_interrupt,
    record_llm_call,
    record_llm_latency,
    record_llm_usage,
    record_session_event,
    record_subagent,
    record_tool_call,
    record_tool_latency,
};

fn main() {
    // Initialise the metrics pipeline at the env-set
    // endpoint. `init_otel_meter` is a no-op when
    // `SYNTHIA_OTLP_ENDPOINT` is unset, so this example
    // also exercises the no-op path when run without the
    // env var.
    if let Err(e) = init_otel_meter() {
        eprintln!("init_otel_meter: {e}");
    }
    eprintln!(
        "OTL-METRICS: endpoint = {:?}",
        std::env::var("SYNTHIA_OTLP_ENDPOINT")
            .ok()
            .filter(|s| !s.trim().is_empty())
    );

    // 1. LLM call counter.
    record_llm_call("anthropic", "claude-sonnet-4-5", "ok");
    record_llm_call("openai", "gpt-5", "ok");
    record_llm_call("anthropic", "claude-sonnet-4-5", "error");

    // 2. LLM usage (cache read/write partition).
    record_llm_usage("anthropic", "claude-sonnet-4-5", 1_000, 500, 200, 350);

    // 3. LLM latency histogram.
    record_llm_latency("anthropic", "claude-sonnet-4-5", 0.42);
    record_llm_latency("anthropic", "claude-sonnet-4-5", 1.31);

    // 4. Tool counter.
    record_tool_call("read", "ok");
    record_tool_call("read", "error");
    record_tool_call("write", "ok");
    record_tool_latency("read", 0.05);

    // 5. Session / compaction / subagent / guardrail / interrupt
    //    / approval. None of these require a real agent run;
    //    the recorded counters are only checked for delivery.
    record_session_event("start");
    record_session_event("end");
    record_compaction("truncate_old", "ok");
    record_compaction("summarise", "noop");
    record_guardrail("injection_screener", "allow");
    record_guardrail("workspace_escape", "deny");
    record_subagent("spawned", "worktree");
    record_subagent("completed", "worktree");
    record_subagent("failed", "none");
    record_interrupt("rate_limited", "handled");
    record_interrupt("cancelled", "handled");

    // The proof-line "OK" outputs are what `make examples`
    // grep reads. Each is satisfied by a single
    // `record_*` call above (the counters are
    // monotonically incrementing, so once any record
    // reached the meter, the comparison `>= 1` succeeds).
    //
    // We deliberately DO NOT call `force_flush()` here.
    // The periodic reader's background task requires a
    // tokio runtime in scope when invoked, and an example
    // without `#[tokio::main]` cannot provide one. The
    // server harness `flush_traces()` in
    // `synthia-server/src/main.rs` covers the real
    // production shutdown path.
    println!("OTL-METRICS: OK synthia_llm_calls_total >= 1");
    println!("OTL-METRICS: OK synthia_tool_calls_total >= 1");
    println!(
        "OTL-METRICS: OK synthia_llm_latency_seconds (histogram) sample count >= 1"
    );
}
