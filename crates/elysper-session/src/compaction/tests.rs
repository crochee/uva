//! Compaction seam tests: the trigger, the balanced cut, the
//! summary's provenance, and the pruner's order.

use std::{
    future::Future,
    sync::{
        Arc,
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
};

use async_trait::async_trait;
use elysper_core::{AtomicCancelToken, CancelToken, Error};
use elysper_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    Message,
    ModelConfig,
    ModelProvider,
    ProviderConfig,
    Role,
    TokenUsage,
    ToolUse,
};

use super::*;

// --- fixtures --------------------------------------------------------

struct RecordingProvider {
    reply: String,
    requests: Mutex<Vec<CompletionRequest>>,
}

impl RecordingProvider {
    fn new(reply: &str) -> Self {
        Self {
            reply: reply.to_string(),
            requests: Mutex::new(Vec::new()),
        }
    }

    fn last_request(&self) -> CompletionRequest {
        self.requests
            .lock()
            .expect("requests lock")
            .last()
            .cloned()
            .expect("the summariser must have called the provider")
    }
}

#[async_trait]
impl ModelProvider for RecordingProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "recording"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "recording".into(),
            provider: "test".into(),
            context_window: 128_000,
            max_output_tokens: 4096,
            supports_tools: false,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        self.requests.lock().expect("requests lock").push(request);
        Ok(CompletionResponse {
            id: "recording".into(),
            model: "recording".into(),
            content: Content::text(self.reply.clone()),
            usage: TokenUsage::default(),
            cached: false,
            replay_state: None,
            stop_reason: None,
        })
    }
}

struct PanickingProvider {
    called: Arc<AtomicBool>,
}

#[async_trait]
impl ModelProvider for PanickingProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "panicking"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "panicking".into(),
            provider: "test".into(),
            context_window: 128_000,
            max_output_tokens: 4096,
            supports_tools: false,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        self.called.store(true, Ordering::SeqCst);
        unreachable!("a cancelled summary must not reach the provider")
    }
}

fn big_user(index: usize, chars: usize) -> Message {
    Message::user(format!("turn {index}: {}", "x".repeat(chars)))
}

fn assistant_call(id: &str) -> Message {
    Message::new(
        Role::Assistant,
        Content::Single(ContentPart::ToolUse(ToolUse {
            id: id.to_string(),
            name: "echo".to_string(),
            input: serde_json::json!({"text": "hi"}),
        })),
    )
}

fn tool_result(id: &str, text: impl Into<String>) -> Message {
    Message::tool(Content::text(text.into()), id)
}

fn compactor() -> (SummaryCompactor, Arc<RecordingProvider>) {
    let provider = Arc::new(RecordingProvider::new("a compact summary"));
    let compactor = SummaryCompactor::new(provider.clone(), "summary-model");
    (compactor, provider)
}

// --- tests -----------------------------------------------------------

#[test]
fn no_plan_when_history_is_under_budget() {
    let (compactor, _provider) = compactor();
    let history = vec![Message::user("hello"), Message::assistant("hi")];
    let budget = TokenBudget {
        window: 10_000,
        reserve: 1_000,
    };
    assert!(
        compactor.plan(&history, budget).is_none(),
        "a history that fits the window must not be compacted"
    );
}

#[test]
fn plan_keeps_the_recent_tail_and_balances_tool_pairs() {
    let (compactor, _provider) = compactor();
    // The tail budget would keep the tool result but drop the
    // assistant call that produced it; the plan must move the cut
    // back so the pair stays whole.
    let history = vec![
        big_user(0, 2_000),
        assistant_call("c1"),
        tool_result("c1", "ok"),
        Message::assistant("done"),
    ];
    let plan = compactor
        .plan(
            &history,
            TokenBudget {
                window: 8,
                reserve: 0,
            },
        )
        .expect("an oversized history must plan a compaction");

    assert_eq!(
        plan.range.end, 1,
        "the cut must move back to the assistant call, not the result"
    );
    assert_eq!(plan.keep_recent, history.len() - plan.range.end);
    assert!(crate::tool_pairing::balanced_message_cut(
        &history,
        plan.range.end
    ));
    assert_eq!(history[plan.range.end].role, Role::Assistant);
    assert!(
        plan.shadowed
            .iter()
            .all(|message| message.role != Role::Tool),
        "a balanced cut never shadows a lone tool result"
    );
    assert_eq!(plan.shadowed.len(), plan.range.len());
    assert_eq!(
        plan.shadowed_tokens,
        elysper_provider::estimate_messages_token_count(&plan.shadowed),
        "the outcome must price exactly the shadowed span, before pruning"
    );
    assert!(plan.shadowed_tokens > 400);
}

#[test]
fn outcome_carries_every_shadowed_seq() {
    let (compactor, _provider) = compactor();
    let history = vec![big_user(0, 2_000), Message::assistant("kept")];
    let mut plan = compactor
        .plan(
            &history,
            TokenBudget {
                window: 8,
                reserve: 0,
            },
        )
        .expect("plan");
    // The harness owns the seq mapping; the summary echoes it.
    plan.shadowed_seqs = vec![3, 7, 11];

    let outcome = block_on_summary(&compactor, &plan);
    assert_eq!(outcome.shadowed_seqs, vec![3, 7, 11]);
    assert_eq!(outcome.shadowed_tokens, plan.shadowed_tokens);
    assert_eq!(outcome.summary, "a compact summary");
}

#[test]
fn outcome_names_the_summariser_that_answered() {
    let (compactor, _provider) = compactor();
    let history = vec![big_user(0, 2_000), Message::assistant("kept")];
    let plan = compactor
        .plan(
            &history,
            TokenBudget {
                window: 8,
                reserve: 0,
            },
        )
        .expect("plan");

    let outcome = block_on_summary(&compactor, &plan);
    assert_eq!(outcome.provider.as_deref(), Some("recording"));
    assert_eq!(outcome.model.as_deref(), Some("summary-model"));
    assert_eq!(
        outcome.usage.map(|usage| usage.total_tokens),
        Some(0),
        "the default compactor records the provider's usage report"
    );
}

#[test]
fn plan_trigger_holds_at_the_window_boundaries() {
    // No window: no reserve, and a zero trigger means never compact.
    assert_eq!(
        TokenBudget::for_window(0),
        TokenBudget {
            window: 0,
            reserve: 0
        }
    );
    // A window that exceeds the default reserve keeps the default.
    let roomy = TokenBudget::for_window(1_000_000);
    assert_eq!(roomy.reserve, 16_384);
    assert_eq!(roomy.trigger(), 1_000_000 - 16_384);
    // A small window reserves a quarter, so the trigger stays above
    // zero for any window >= 4.
    let small = TokenBudget::for_window(100);
    assert_eq!(small.reserve, 25);
    assert_eq!(small.trigger(), 75);
    // Below 4 the reserve floors at zero instead of underflowing.
    assert_eq!(
        TokenBudget::for_window(3),
        TokenBudget {
            window: 3,
            reserve: 0
        }
    );
}

#[test]
fn pruner_runs_before_the_summary_call() {
    let (compactor, provider) = compactor();
    let noise = "n".repeat(5_000);
    let history = vec![
        big_user(0, 2_000),
        tool_result("c1", format!("TOOL-RAW-{noise}")),
        Message::assistant("done"),
    ];
    let plan = compactor
        .plan(
            &history,
            TokenBudget {
                window: 8,
                reserve: 0,
            },
        )
        .expect("plan");
    assert!(
        plan.shadowed
            .iter()
            .any(|message| message.role == Role::Tool),
        "the fixture must shadow a tool result"
    );

    let _ = block_on_summary(&compactor, &plan);
    let request = provider.last_request();
    let prompt = request
        .messages
        .iter()
        .filter_map(|message| message.content.extract_text())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        prompt.contains(PRUNED_TOOL_RESULT),
        "the summariser must see the pruned tool result: {prompt}"
    );
    assert!(
        !prompt.contains("TOOL-RAW-"),
        "the raw tool result must not reach the summariser"
    );
}

#[test]
fn summariser_failure_is_reported_not_swallowed() {
    struct Failing;

    #[async_trait]
    impl ModelProvider for Failing {
        async fn initialize(
            &mut self,
            _config: ProviderConfig,
        ) -> Result<(), Error> {
            Ok(())
        }

        fn name(&self) -> &str {
            "failing"
        }

        fn model_config(&self) -> ModelConfig {
            ModelConfig {
                name: "failing".into(),
                provider: "test".into(),
                context_window: 128_000,
                max_output_tokens: 4096,
                supports_tools: false,
                supports_streaming: false,
                supports_reasoning: false,
            }
        }

        async fn complete(
            &self,
            _request: CompletionRequest,
        ) -> Result<CompletionResponse, Error> {
            Err(Error::provider("nope"))
        }
    }

    let compactor = SummaryCompactor::new(Arc::new(Failing), "summary-model");
    let history = vec![big_user(0, 2_000), Message::assistant("kept")];
    let plan = compactor
        .plan(
            &history,
            TokenBudget {
                window: 8,
                reserve: 0,
            },
        )
        .expect("plan");
    let error = block_on_fallible(&compactor, &plan)
        .expect_err("a provider failure must surface");
    assert!(matches!(error, CompactionError::Provider(_)), "{error:?}");
}

#[test]
fn cancelled_summary_never_reaches_the_provider() {
    let called = Arc::new(AtomicBool::new(false));
    let provider = Arc::new(PanickingProvider {
        called: Arc::clone(&called),
    });
    let compactor = SummaryCompactor::new(provider, "summary-model");
    let history = vec![big_user(0, 2_000), Message::assistant("kept")];
    let plan = compactor
        .plan(
            &history,
            TokenBudget {
                window: 8,
                reserve: 0,
            },
        )
        .expect("plan");
    let cancel = Arc::new(AtomicCancelToken::new());
    cancel.cancel();
    let error = block_on_future(compactor.summarize(&plan, cancel))
        .expect_err("a cancelled summary must fail");
    assert_eq!(error, CompactionError::Cancelled);
    assert!(!called.load(Ordering::SeqCst));
}

// --- blocking helpers ------------------------------------------------

fn block_on_future<F: Future>(future: F) -> F::Output {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime");
    runtime.block_on(future)
}

fn block_on_fallible(
    compactor: &SummaryCompactor,
    plan: &CompactionPlan,
) -> Result<CompactionOutcome, CompactionError> {
    block_on_future(compactor.summarize(
        plan,
        Arc::new(AtomicCancelToken::new()) as Arc<dyn CancelToken>,
    ))
}

fn block_on_summary(
    compactor: &SummaryCompactor,
    plan: &CompactionPlan,
) -> CompactionOutcome {
    block_on_fallible(compactor, plan).expect("summary")
}

// --- bind_plan: the provenance the compactor cannot know --------------

fn unbound_plan(range: std::ops::Range<usize>) -> CompactionPlan {
    CompactionPlan {
        range,
        shadowed_seqs: Vec::new(),
        keep_recent: 0,
        shadowed: Vec::new(),
        shadowed_tokens: 0,
    }
}

#[test]
fn bind_plan_clamps_the_range_to_the_fold() {
    let history = vec![
        Message::user("a"),
        Message::assistant("b"),
        Message::assistant("c"),
    ];
    let seqs = vec![7, 8, 9];
    let oversized =
        bind_plan(unbound_plan(0..99), &history, &seqs).expect("clamped");
    assert_eq!(oversized.range, 0..3);
    assert_eq!(oversized.shadowed_seqs, seqs);
    assert_eq!(oversized.keep_recent, 0);
    assert_eq!(
        oversized.shadowed_tokens,
        elysper_provider::estimate_messages_token_count(&history)
    );

    let past_the_seqs =
        bind_plan(unbound_plan(1..3), &history, &seqs[..2]).expect("clamped");
    assert_eq!(past_the_seqs.range, 1..2);
}

#[test]
fn bind_plan_refuses_an_empty_span() {
    let history = vec![Message::user("a"), Message::assistant("b")];
    let seqs = vec![1, 2];
    assert!(bind_plan(unbound_plan(1..1), &history, &seqs).is_none());
    assert!(bind_plan(unbound_plan(5..6), &history, &seqs).is_none());
    assert!(bind_plan(unbound_plan(0..2), &[], &seqs).is_none());
}

#[test]
fn bind_plan_never_shadows_the_system_prompt() {
    let history = vec![
        Message::system("system"),
        Message::user("a"),
        Message::assistant("b"),
    ];
    let seqs = vec![1, 2, 3];
    let start_at_zero =
        bind_plan(unbound_plan(0..3), &history, &seqs).expect("bound");
    assert_eq!(
        start_at_zero.range,
        1..3,
        "the cached prefix is not shadowed"
    );

    // Only the system message is shadowable after clamping: refuse.
    assert!(bind_plan(unbound_plan(0..1), &history, &seqs).is_none());
    assert_eq!(history[0].role, Role::System);
}

// --- plan_compaction: outcome classification -------------------------

struct FixedPlan(CompactionPlan);

impl Compactor for FixedPlan {
    fn plan(
        &self,
        _history: &[Message],
        _budget: TokenBudget,
    ) -> Option<CompactionPlan> {
        Some(self.0.clone())
    }

    fn summarize(
        &self,
        _plan: &CompactionPlan,
        _cancel: Arc<dyn CancelToken>,
    ) -> BoxFuture<'static, Result<CompactionOutcome, CompactionError>> {
        unimplemented!("planning never summarises")
    }
}

struct NeverPlans;

impl Compactor for NeverPlans {
    fn plan(
        &self,
        _history: &[Message],
        _budget: TokenBudget,
    ) -> Option<CompactionPlan> {
        None
    }

    fn summarize(
        &self,
        _plan: &CompactionPlan,
        _cancel: Arc<dyn CancelToken>,
    ) -> BoxFuture<'static, Result<CompactionOutcome, CompactionError>> {
        unimplemented!("a declined plan is never summarised")
    }
}

fn stamped(payload: serde_json::Value, seq: u64) -> serde_json::Value {
    let mut row =
        serde_json::to_value(crate::user_message(payload, None)).expect("row");
    row["seq"] = serde_json::Value::from(seq);
    row
}

fn system_only_rows() -> Vec<serde_json::Value> {
    vec![stamped(
        serde_json::to_value(Message::system("system")).expect("message"),
        1,
    )]
}

#[test]
fn plan_compaction_reports_an_empty_span_without_an_error() {
    let rows = system_only_rows();
    let budget = TokenBudget::for_window(1_000);
    // The only candidate is the cached system prompt: the fold clamps
    // the span to nothing, which is a skipped pass, not a broken log.
    assert!(matches!(
        plan_compaction_from_rows(
            &rows,
            &FixedPlan(unbound_plan(0..1)),
            budget
        ),
        Ok(PlannedCompaction::EmptySpan)
    ));
    // A compactor that declines is likewise a skip, not an error.
    assert!(matches!(
        plan_compaction_from_rows(&rows, &NeverPlans, budget),
        Ok(PlannedCompaction::Declined)
    ));
}

#[test]
fn plan_compaction_binds_a_real_span() {
    let payloads = [
        serde_json::to_value(Message::system("system")).expect("message"),
        serde_json::to_value(Message::user("go")).expect("message"),
        serde_json::to_value(Message::assistant("answer")).expect("message"),
    ];
    let rows: Vec<_> = payloads
        .into_iter()
        .enumerate()
        .map(|(index, payload)| stamped(payload, index as u64 + 1))
        .collect();
    let budget = TokenBudget::for_window(1_000);
    match plan_compaction_from_rows(
        &rows,
        &FixedPlan(unbound_plan(0..9)),
        budget,
    ) {
        Ok(PlannedCompaction::Plan(plan)) => {
            assert_eq!(plan.range, 1..3, "the system prompt is never shadowed");
            assert_eq!(plan.shadowed_seqs, vec![2, 3]);
        }
        other => panic!("expected a bound plan, got {other:?}"),
    }
}

struct HangingProvider;

#[async_trait]
impl ModelProvider for HangingProvider {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), Error> {
        Ok(())
    }

    fn name(&self) -> &str {
        "hanging"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "hanging".into(),
            provider: "test".into(),
            context_window: 128_000,
            max_output_tokens: 4096,
            supports_tools: false,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, Error> {
        std::future::pending().await
    }
}

#[tokio::test]
async fn a_hung_summariser_times_out() {
    let compactor = SummaryCompactor::new(Arc::new(HangingProvider), "hanging")
        .with_timeout(std::time::Duration::from_millis(20));
    let history =
        vec![Message::user("a".repeat(5_000)), Message::assistant("done")];
    let plan = compactor
        .plan(
            &history,
            TokenBudget {
                window: 8,
                reserve: 0,
            },
        )
        .expect("an oversized history must plan");
    let error = compactor
        .summarize(&plan, Arc::new(AtomicCancelToken::new()))
        .await
        .expect_err("a hung provider must time out");
    assert!(
        matches!(error, CompactionError::Timeout(timeout) if timeout == std::time::Duration::from_millis(20)),
        "got {error:?}"
    );
}
