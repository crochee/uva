//! Who answers a session's questions.
//!
//! The `ask` tool holds a [`QuestionOwner`](uva::tool_ask::QuestionOwner)
//! — the same seam the file tools hold for files and the shell tool
//! holds for terminals. This module is the host's side of it: the
//! registry every session's questions travel through, and the two
//! owners it knows.
//!
//! # The core face asks the client
//!
//! The core's own JSON-RPC face can ask its peer a question — the
//! same channel the client-owned tools travel on (the `tools/execute`
//! request of the third round). The question names the session it is
//! about, so a gateway routes it to the connection that owns that
//! session; the answer comes back on the same id.
//!
//! ```
//! → {"jsonrpc":"2.0","id":"core:internal:3","method":"questions/ask",
//!    "params":{"session_id":"s-1","questions":[{"id":"db", …}]}}
//! ← {"jsonrpc":"2.0","id":"core:internal:3",
//!    "result":{"answers":[{"id":"db","selected":["Postgres"]}]}}
//! ```
//!
//! # The ACP face has no question primitive
//!
//! ACP v1 lets the agent ask the client for a **permission** — a
//! yes/no on one call — and nothing else: there is no question, no
//! options, no free text. An editor driving the deployment therefore
//! cannot answer a question, and a session it owns says so at once
//! rather than waiting out the budget for an answer that can never
//! come. The model reads the refusal and decides on its own.
//!
//! # The wait is bounded
//!
//! The budget is the tool's (dsh's timed variant waits 120 seconds);
//! a client that walks away ends the call, not the turn. The failure
//! is the structured `TOOL_TIMEOUT` the model can see.

use std::{collections::HashMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use parking_lot::RwLock;
use serde_json::json;
use uva::tool_ask::{AskAnswers, AskError, AskQuestion, QuestionOwner};

use crate::rpc::{ClientCallError, RpcRuntime};

/// The method the **client** answers when the core asks a question.
///
/// The name is not in the core's method table — that table lists the
/// methods the process *answers*; this one travels the other
/// direction, and a client that wants to answer implements it.
pub const QUESTIONS_ASK: &str = "questions/ask";

/// The owner that asks over the core's own JSON-RPC face.
///
/// Every question travels to the client that owns the session and
/// comes back as an answer batch; a client that answers with
/// anything else is refused rather than believed.
pub struct CoreQuestionOwner {
    rpc: Arc<RpcRuntime>,
}

impl std::fmt::Debug for CoreQuestionOwner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CoreQuestionOwner").finish_non_exhaustive()
    }
}

impl CoreQuestionOwner {
    /// The owner over the core's own face.
    #[must_use]
    pub fn new(rpc: Arc<RpcRuntime>) -> Self {
        Self { rpc }
    }
}

#[async_trait]
impl QuestionOwner for CoreQuestionOwner {
    async fn ask(
        &self,
        session_id: &str,
        questions: &[AskQuestion],
        timeout: Duration,
    ) -> Result<AskAnswers, AskError> {
        let params = json!({
            "session_id": session_id,
            "questions": questions,
        });
        match self.rpc.call_client(QUESTIONS_ASK, params, timeout).await {
            Ok(value) => serde_json::from_value(value).map_err(|error| {
                AskError::Refused(format!(
                    "the client's answer was not a question batch: {error}"
                ))
            }),
            Err(ClientCallError::Timeout) => Err(AskError::Timeout),
            Err(ClientCallError::Refused { message }) => {
                Err(AskError::Refused(message))
            }
            Err(ClientCallError::Closed) => Err(AskError::Refused(
                "the client is gone; the question was never answered"
                    .to_string(),
            )),
        }
    }
}

/// An owner that answers nothing, and says why at once.
///
/// The ACP face's sessions use it: an editor cannot answer a
/// question, and a refusal the model can read is better than a
/// budget the model waits out.
#[derive(Debug, Clone)]
pub struct RefusingQuestions {
    reason: String,
}

impl RefusingQuestions {
    /// The owner that refuses every question with `reason`.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }
}

#[async_trait]
impl QuestionOwner for RefusingQuestions {
    async fn ask(
        &self,
        _session_id: &str,
        _questions: &[AskQuestion],
        _timeout: Duration,
    ) -> Result<AskAnswers, AskError> {
        Err(AskError::Refused(self.reason.clone()))
    }
}

/// The registry of every session's question owner.
///
/// A session with no registered owner asks over the core's own face;
/// a client that cannot answer registers its own owner — the ACP
/// face registers the refusal above — and the questions go there
/// instead.
pub struct SessionQuestions {
    /// The owner a session asks when nothing else is registered.
    default: Arc<dyn QuestionOwner>,
    /// The owners that claimed a session, keyed by session id.
    owners: RwLock<HashMap<String, Arc<dyn QuestionOwner>>>,
}

impl std::fmt::Debug for SessionQuestions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionQuestions")
            .field("claimed_sessions", &self.owners.read().len())
            .finish_non_exhaustive()
    }
}

impl SessionQuestions {
    /// The registry whose unclaimed sessions ask `default`.
    #[must_use]
    pub fn new(default: Arc<dyn QuestionOwner>) -> Self {
        Self {
            default,
            owners: RwLock::new(HashMap::new()),
        }
    }

    /// The owner that answers `session_id`'s questions.
    ///
    /// Registering a session that already has an owner replaces it —
    /// a client that reconnects brings its own answerer, and the old
    /// one would answer for a session it no longer holds.
    pub fn register(&self, session_id: &str, owner: Arc<dyn QuestionOwner>) {
        self.owners.write().insert(session_id.to_string(), owner);
    }

    /// Drop the owner that answered `session_id`'s questions: the
    /// session asks over the core's own face again.
    pub fn release(&self, session_id: &str) {
        self.owners.write().remove(session_id);
    }

    /// The owner that answers `session_id`'s questions.
    #[must_use]
    pub fn owner(&self, session_id: &str) -> Arc<dyn QuestionOwner> {
        self.owners
            .read()
            .get(session_id)
            .cloned()
            .unwrap_or_else(|| Arc::clone(&self.default))
    }
}

#[async_trait]
impl QuestionOwner for SessionQuestions {
    async fn ask(
        &self,
        session_id: &str,
        questions: &[AskQuestion],
        timeout: Duration,
    ) -> Result<AskAnswers, AskError> {
        self.owner(session_id)
            .ask(session_id, questions, timeout)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An owner the tests script: it answers with a fixed batch.
    struct FixedQuestions;

    #[async_trait]
    impl QuestionOwner for FixedQuestions {
        async fn ask(
            &self,
            _session_id: &str,
            _questions: &[AskQuestion],
            _timeout: Duration,
        ) -> Result<AskAnswers, AskError> {
            Ok(AskAnswers {
                answers: vec![uva::tool_ask::AskAnswer {
                    id: "q1".to_string(),
                    selected: vec!["A".to_string()],
                    custom: None,
                }],
            })
        }
    }

    fn question() -> AskQuestion {
        AskQuestion {
            id: "q1".to_string(),
            question: "Which one?".to_string(),
            header: None,
            options: None,
            multi_select: false,
        }
    }

    #[tokio::test]
    async fn a_claimed_session_asks_its_owner() {
        let registry = SessionQuestions::new(Arc::new(RefusingQuestions::new(
            "the default owner refuses",
        )));
        registry.register("s1", Arc::new(FixedQuestions));
        let answers = registry
            .ask("s1", &[question()], Duration::from_secs(5))
            .await
            .expect("the owner answers");
        assert_eq!(answers.answers[0].selected, vec!["A".to_string()]);
    }

    #[tokio::test]
    async fn an_unclaimed_session_asks_the_default_owner() {
        let registry = SessionQuestions::new(Arc::new(RefusingQuestions::new(
            "the default owner refuses",
        )));
        let error = registry
            .ask("s1", &[question()], Duration::from_secs(5))
            .await
            .expect_err("the default owner refuses");
        assert_eq!(
            error,
            AskError::Refused("the default owner refuses".to_string())
        );
    }

    #[tokio::test]
    async fn a_released_session_asks_the_default_owner_again() {
        let registry = SessionQuestions::new(Arc::new(RefusingQuestions::new(
            "the default owner refuses",
        )));
        registry.register("s1", Arc::new(FixedQuestions));
        registry.release("s1");
        let error = registry
            .ask("s1", &[question()], Duration::from_secs(5))
            .await
            .expect_err("the default owner refuses");
        assert!(matches!(error, AskError::Refused(_)));
    }

    #[tokio::test]
    async fn a_client_that_never_answers_times_out() {
        let owner = CoreQuestionOwner::new(Arc::new(RpcRuntime::detached()));
        let error = owner
            .ask("s1", &[question()], Duration::from_millis(50))
            .await
            .expect_err("nobody answers");
        assert_eq!(error, AskError::Timeout);
    }
}
