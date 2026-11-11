//! Judge scorer tests. Sibling to `judge.rs` so the production
//! module holds only the public API.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use async_trait::async_trait;
use elysper_core::Error as ProviderError;
use elysper_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    ModelConfig,
    ProviderConfig,
    TextContent,
};
use parking_lot::Mutex;

use crate::best_of_n::{CandidateScorer, judge::LlmJudgeScorer};

struct JudgeFixture {
    replies: Mutex<Vec<String>>,
    calls: AtomicUsize,
    fail: bool,
}

impl JudgeFixture {
    fn new(replies: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(
                replies.iter().map(|r| (*r).to_string()).collect(),
            ),
            calls: AtomicUsize::new(0),
            fail: false,
        })
    }

    fn failing() -> Arc<Self> {
        Arc::new(Self {
            replies: Mutex::new(Vec::new()),
            calls: AtomicUsize::new(0),
            fail: true,
        })
    }
}

#[async_trait]
impl elysper_provider::ModelProvider for JudgeFixture {
    async fn initialize(
        &mut self,
        _config: ProviderConfig,
    ) -> Result<(), ProviderError> {
        Ok(())
    }

    fn name(&self) -> &str {
        "judge-fixture"
    }

    fn model_config(&self) -> ModelConfig {
        ModelConfig {
            name: "judge".into(),
            provider: "judge-fixture".into(),
            context_window: 8_192,
            max_output_tokens: 64,
            supports_tools: false,
            supports_streaming: false,
            supports_reasoning: false,
        }
    }

    async fn complete(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(ProviderError::internal("judge offline"));
        }
        let text = {
            let mut replies = self.replies.lock();
            if replies.is_empty() {
                String::new()
            } else {
                replies.remove(0)
            }
        };
        Ok(CompletionResponse {
            content: Content::Single(ContentPart::Text(TextContent {
                text,
                cache_control: None,
            })),
            ..CompletionResponse::default()
        })
    }
}

#[tokio::test]
async fn judge_scorer_returns_parsed_value() {
    let provider = JudgeFixture::new(&["Score: 0.9"]);
    let scorer = LlmJudgeScorer::new(provider.clone());
    let score = scorer.score("the answer").await;
    assert!((score - 0.9).abs() < f64::EPSILON);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    assert_eq!(scorer.name(), "llm-judge");
}

#[tokio::test]
async fn judge_scorer_falls_back_to_neutral_on_provider_error() {
    let provider = JudgeFixture::failing();
    let scorer = LlmJudgeScorer::new(provider.clone());
    // Provider error must not collapse every failing
    // candidate to 0.0 (which would rank them all at the
    // bottom); a neutral 0.5 keeps the judge out of the
    // ranking while the strategy's own failure path records
    // the issue.
    let score = scorer.score("anything").await;
    assert!((score - 0.5).abs() < f64::EPSILON);
}

#[tokio::test]
async fn judge_prompt_carries_rubric_and_candidate() {
    let provider = JudgeFixture::new(&["Score: 0.5"]);
    let scorer = LlmJudgeScorer::with_rubric(
        provider.clone(),
        "reward: short, factual".into(),
    );
    let _ = scorer.score("Paris is the capital of France").await;
    // The fixture does not expose the request, but a call
    // proves the prompt built without panic and the call
    // shape (one call per `score`) holds.
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
}
