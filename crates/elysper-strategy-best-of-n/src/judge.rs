//! LLM-judge scorer for `BestOfNStrategy`. Gated behind the
//! `llm-judge` feature so the default crate build does not pull
//! in the `ModelProvider` plumbing for a feature some users will
//! never turn on.

use std::sync::Arc;

use async_trait::async_trait;
use elysper_provider::{
    CompletionRequest,
    Content,
    ContentPart,
    ModelProvider,
    TextContent,
};

use super::CandidateScorer;

/// Default rubric appended to the judge prompt when the caller
/// does not supply one. `Score: 0.0..=1.0` on its own line — the
/// shape `elysper_eval::metrics::parse_score` recognises.
pub const DEFAULT_JUDGE_PROMPT: &str = "\
You are grading a candidate answer to a task. Reply with one line in the form:\n\
Score: <number between 0.0 and 1.0>\n\
Higher = better. Be strict: 1.0 means correct and complete, 0.0 means wrong or empty.";

/// An LLM-backed [`CandidateScorer`]: the model receives a
/// rubric + the candidate, returns a `0.0..=1.0` rating, the
/// scorer surfaces that as the candidate's score.
pub struct LlmJudgeScorer {
    pub(super) provider: Arc<dyn ModelProvider>,
    pub(super) rubric: String,
    pub(super) max_tokens: Option<u32>,
}

impl std::fmt::Debug for LlmJudgeScorer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LlmJudgeScorer")
            .field("rubric", &self.rubric)
            .field("max_tokens", &self.max_tokens)
            .finish_non_exhaustive()
    }
}

impl LlmJudgeScorer {
    /// Build a judge that grades each candidate against
    /// [`DEFAULT_JUDGE_PROMPT`].
    #[must_use]
    pub fn new(provider: Arc<dyn ModelProvider>) -> Self {
        Self::with_rubric(provider, DEFAULT_JUDGE_PROMPT.to_string())
    }

    /// Build a judge that grades each candidate against
    /// `rubric` (one or more sentences describing the
    /// criterion).
    #[must_use]
    pub fn with_rubric(
        provider: Arc<dyn ModelProvider>,
        rubric: String,
    ) -> Self {
        Self {
            provider,
            rubric,
            max_tokens: None,
        }
    }

    /// Cap the judge's reply length. Most judges need only a
    /// few tokens; a too-low cap truncates before the score
    /// line surfaces and the shared judge-score parser returns
    /// `0.0` (see [`elysper_core::judge_score`]).
    #[must_use]
    pub fn with_max_tokens(mut self, max_tokens: u32) -> Self {
        self.max_tokens = Some(max_tokens);
        self
    }

    /// The rubric the scorer grades against.
    #[must_use]
    pub fn rubric(&self) -> &str {
        &self.rubric
    }
}

#[async_trait]
impl CandidateScorer for LlmJudgeScorer {
    async fn score(&self, candidate: &str) -> f64 {
        let prompt = format!(
            "{rubric}\n\n--- candidate answer to grade ---\n\
             {candidate}\n--- end ---\n",
            rubric = self.rubric,
        );
        let request = CompletionRequest {
            model: self.provider.model_config().name,
            messages: Arc::new(vec![elysper_provider::Message::new(
                elysper_provider::Role::User,
                Content::Single(ContentPart::Text(TextContent {
                    text: prompt,
                    cache_control: None,
                })),
            )]),
            max_tokens: Some(self.max_tokens.map(|v| v as usize).unwrap_or(32)),
            ..CompletionRequest::default()
        };
        match self.provider.complete(request).await {
            Ok(response) => {
                let text =
                    elysper_provider::completion_to_sampling(&response).text;
                elysper_core::parse_judge_score(&text)
            }
            Err(error) => {
                // A judge error must not skew the ranking toward
                // "no answer" — a `0.0` would still be a real rank
                // (every judge error would tie at the bottom), and
                // dropping the candidate would discard a usable
                // sample over a grader hiccup. Return the neutral
                // 0.5 instead; the candidate stays in the ranking
                // and the operator sees the warn.
                tracing::warn!(
                    %error,
                    "LlmJudgeScorer: judge provider call failed; \
                     returning neutral 0.5 so the candidate is not \
                     advantaged or buried by the error",
                );
                0.5
            }
        }
    }

    fn name(&self) -> &str {
        "llm-judge"
    }
}
