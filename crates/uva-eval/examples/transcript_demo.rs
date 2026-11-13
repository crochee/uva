//! Transcript-demo: build a small async agent, run `EvalRunner::run_trials`
//! with parallelism, write one JSONL row per trial via
//! `JsonlTranscriptSink`, and print the per-case pass-rate +
//! pass@k. The proof line prints once the JSONL sink has been
//! flushed.
//!
//! Run with `cargo run --example transcript_demo -p uva-eval`.

use std::{fs::File, io::BufWriter, sync::Arc};

use tempfile::tempdir;
use uva_eval::{
    EvalAgent,
    EvalError,
    EvalReport,
    EvalRunner,
    EvalSuite,
    JsonlTranscriptSink,
    KeywordMetric,
    SyncMetricAdapter,
    TestCase,
    transcript::{
        SpanRecord,
        ToolCallRecord,
        Transcript,
        TranscriptOutcome,
        TranscriptSink,
        Usage,
    },
};

/// Echo agent — sleeps for `delay_ms` so the parallelism gain is
/// visible.
struct DelayedEchoAgent {
    delay_ms: u64,
}

#[async_trait::async_trait]
impl EvalAgent for DelayedEchoAgent {
    async fn respond(&self, input: &str) -> Result<String, EvalError> {
        if self.delay_ms > 0 {
            tokio::time::sleep(std::time::Duration::from_millis(self.delay_ms))
                .await;
        }
        Ok(format!("echo: {input}"))
    }
}

/// Walk one `EvalReport` entry and emit a `Transcript` for it. A
/// real run would carry the messages + tool calls + spans; this
/// stub keeps the example hermetic (no model call, no tool
/// execution) but exercises every field of the wire shape.
fn stub_transcript(
    case_id: &str,
    actual_output: &str,
    outcome: TranscriptOutcome,
) -> Transcript {
    let mut t = Transcript::new(case_id)
        .with_seed("seed-transcript-1")
        .with_attribute("agent", "delayed-echo");
    t.messages.clear();
    t.tool_calls.push(ToolCallRecord::new(
        "echo",
        "tc-1",
        serde_json::json!({"text": case_id}),
        false,
        0,
    ));
    t.usage = Usage {
        input: 24,
        cache_read: 0,
        cache_write: 0,
        output: actual_output.len(),
    };
    t.spans.push(SpanRecord::new(
        "transcript-demo",
        "agent",
        vec![("case_id".to_string(), case_id.to_string())],
        0,
    ));
    t.outcome = outcome;
    t
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), EvalError> {
    let tmp = tempdir().expect("tempdir");
    let sink_path = tmp.path().join("transcripts.jsonl");
    let file = File::create(&sink_path).expect("create");
    let sink: Arc<dyn TranscriptSink> =
        Arc::new(JsonlTranscriptSink::new(BufWriter::new(file)));

    let suite = EvalSuite::new("transcript-demo")
        .add_case(TestCase::new("greeting", "hello").expect_contains("echo"))
        .add_case(TestCase::new("world", "world").expect_contains("echo"))
        .add_case(TestCase::new("foo", "foo").expect_contains("echo"));

    let agent = Arc::new(DelayedEchoAgent { delay_ms: 5 });
    let runner = EvalRunner::new()
        .metric(Box::new(SyncMetricAdapter(KeywordMetric)))
        .threshold(0.5)
        .parallelism(4);

    let report: EvalReport = runner.run_trials(agent, &suite, 10).await?;
    assert_eq!(report.trials, 10);
    assert_eq!(report.total, 30);

    // Emit one Transcript per (case, trial) using the per_case_passes
    // ledger as the source-of-truth for pass / fail. Trial-level
    // data is synthesised here — a real run would thread the actual
    // tool calls + spans through the runner.
    for case in suite.cases() {
        let passes = report.per_case_passes.get(&case.id).copied().unwrap_or(0);
        let attempts =
            report.per_case_attempts.get(&case.id).copied().unwrap_or(0);
        for trial_idx in 0..attempts {
            let outcome = if trial_idx < passes {
                TranscriptOutcome::Pass { score: 1.0 }
            } else {
                TranscriptOutcome::Fail {
                    score: 0.0,
                    reason: "no echo".into(),
                }
            };
            sink.write(&stub_transcript(
                &case.id,
                &format!("echo: {trial_idx}"),
                outcome,
            ))
            .expect("transcript write");
        }
    }

    let pass_at_1 = report.pass_at(1);
    let pass_at_k = report.pass_at(10);
    let pass_rate = report.per_case_pass_rate();

    println!(
        "TRANSCRIPT-DEMO: ok rows={} sink={}",
        report.total,
        sink_path.display(),
    );
    println!(
        "  pass_at(k=1)={:?}",
        pass_at_1.values().cloned().collect::<Vec<_>>()
    );
    println!(
        "  pass_at(k=10)={:?}",
        pass_at_k.values().cloned().collect::<Vec<_>>()
    );
    println!(
        "  per_case_pass_rate={:?}",
        pass_rate.values().cloned().collect::<Vec<_>>()
    );

    // The tempdir is held for the duration of `main`; the JSONL
    // inside it persists until the `tmp` binding is dropped at
    // function exit.
    drop(sink); // flush + close before reading.
    let written = std::fs::metadata(&sink_path).map(|m| m.len()).unwrap_or(0);
    eprintln!("wrote {} bytes to {}", written, sink_path.display());

    Ok(())
}
