//! The `ask` tool: the model puts a question to the operator.
//!
//! Both reference harnesses carry this surface: dsh's `tool-ask-user`
//! (`ask_user_question`, `packages/interaction/tool-ask-user/src/index.ts`)
//! and omp's builtin `ask` (`packages/coding-agent/src/tools/ask.ts`).
//! The behaviour both share — and this tool reproduces — is a tool
//! call that **pauses until a human answers**, then feeds the answer
//! back into the agent loop as an ordinary tool result:
//!
//! ```text
//! model: ask({questions: [{id: "db", question: "Which database?",
//!                          options: [{label: "Postgres"}, {label: "SQLite"}]}]})
//!   → the question travels to the client that owns the session
//!   ← the operator's answer comes back
//! model: {answers: [{id: "db", selected: ["Postgres"]}]}
//! ```
//!
//! omp's variant takes an HTML `<form>` string; ours takes dsh's
//! structured questions, because the core is JSON-only — the wire
//! carries questions, not a rendering.
//!
//! # The seam
//!
//! Who answers is the deployment's business, not the tool's. The tool
//! holds one [`QuestionOwner`] — the same seam `read`/`write` use for
//! files and `shell` uses for terminals — and the host installs the
//! implementation: the core's own JSON-RPC face for a session whose
//! client answers there (see `crates/uva-app-server/src/session/questions.rs`),
//! or a capable client's native question primitive for a session it
//! claimed. A deployment that installs nothing gets [`NoOperator`]:
//! the tool fails closed rather than hanging a turn on a question
//! nobody can answer.
//!
//! # The wait is bounded
//!
//! dsh's blocking (legacy) mode has no deadline; omp's `ask` has a
//! configurable timeout. A bound is the headless-safe default — a
//! client that walks away must not hang the turn — so the tool waits
//! within [`ASK_TIMEOUT`] and answers a budget that ran out with the
//! structured `TOOL_TIMEOUT` failure (the same vocabulary the
//! tool-timeout policy uses), which the model can see and retry.

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use schemars_derive::JsonSchema;
use serde::{Deserialize, Serialize};
use uva_tool::{Context, ExecutionMode, Tool, ToolOutput};

/// The `ask` tool brick as a native kernel plugin.
pub mod plugin;

pub use plugin::AskToolPlugin;

#[cfg(test)]
mod tests;

/// How long the core waits for the operator before the call fails as
/// a structured timeout.
///
/// dsh's timed variant defaults to 120 seconds; omp's `ask.timeout`
/// is configurable. A bound is the repo's rule for every wait a
/// client must answer (`TOOL_TIMEOUT`), and a human answering a
/// question is the slowest of them, so the budget is generous — but
/// it is a bound: an operator that never answers ends the call, not
/// the turn.
pub const ASK_TIMEOUT: Duration = Duration::from_secs(120);

/// How many questions one call may carry.
///
/// The schema says one to sixteen; the runtime enforces the same
/// bound, so a model that ignores the schema cannot make the
/// operator answer a flood.
pub const MAX_QUESTIONS: usize = 16;

/// One selectable answer offered to the operator.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
pub struct AskOption {
    /// User-facing option label.
    #[schemars(description = "Short user-facing option label.")]
    pub label: String,
    /// One sentence explaining the tradeoff or impact.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "One sentence explaining the tradeoff or impact."
    )]
    pub description: Option<String>,
}

/// One question the model asks the operator.
///
/// The shape is dsh's (`ask_user_question`'s parameters): a stable
/// `id` the answer echoes, the `question` itself, optional `options`,
/// and whether more than one may be selected. A question without
/// options is a free-text one — the answer's `custom` carries the
/// text.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
pub struct AskQuestion {
    /// Stable id for this question; echoed in the answer.
    #[schemars(
        description = "Stable id for this question; echoed in the answer."
    )]
    pub id: String,
    /// The specific question to ask the user.
    #[schemars(description = "The specific question to ask the user.")]
    pub question: String,
    /// Optional short heading for the question.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Optional short heading for the question, such as \"Confirm\"."
    )]
    pub header: Option<String>,
    /// Optional choices to show the user.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(
        description = "Optional choices to show the user. If you recommend one, put it first."
    )]
    pub options: Option<Vec<AskOption>>,
    /// Whether more than one option may be selected. Defaults to
    /// false.
    #[serde(default, skip_serializing_if = "is_false")]
    #[schemars(
        description = "Whether the user may select more than one option. Defaults to false."
    )]
    pub multi_select: bool,
}

/// The operator's answer to one question.
///
/// `selected` holds the chosen **labels** — the same strings the
/// question offered — and `custom` the free-text answer of a question
/// without options. An answer with neither is an explicit skip.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
pub struct AskAnswer {
    /// The answered question id.
    #[schemars(description = "The answered question id.")]
    pub id: String,
    /// Selected option labels.
    #[serde(default)]
    #[schemars(description = "Selected option labels.")]
    pub selected: Vec<String>,
    /// Optional free-form answer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[schemars(description = "Optional free-form answer.")]
    pub custom: Option<String>,
}

/// The operator's answers to one `ask` call.
///
/// One item per question the call asked; the batch that leaves the
/// tool keeps the same invariant dsh enforces at its own boundary —
/// every question answered exactly once.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct AskAnswers {
    /// One item per question of the call.
    pub answers: Vec<AskAnswer>,
}

/// Why a question did not come back answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskError {
    /// Nobody is attached who could answer.
    NoOperator,
    /// The operator's side refused or failed; the string says why.
    Refused(String),
    /// The wait ran out.
    Timeout,
}

/// Who answers a session's questions.
///
/// The host installs the implementation; the tool never speaks a
/// transport. The owner decides where the question travels — the
/// core's own JSON-RPC face, a capable client's native question
/// primitive — and hands back the answers it collected, enforcing the
/// `timeout` the caller budgeted.
#[async_trait]
pub trait QuestionOwner: Send + Sync {
    /// Put the questions to the operator and wait for the answers.
    ///
    /// # Errors
    ///
    /// [`AskError::NoOperator`] when nobody can answer this session,
    /// [`AskError::Refused`] when the operator's side refused or
    /// failed, [`AskError::Timeout`] when the budget ran out.
    async fn ask(
        &self,
        session_id: &str,
        questions: &[AskQuestion],
        timeout: Duration,
    ) -> Result<AskAnswers, AskError>;
}

/// An owner that answers nothing: the default a deployment has when
/// no client can be asked.
///
/// Asking would hang a turn on a question nobody can answer, so the
/// tool fails closed instead.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoOperator;

#[async_trait]
impl QuestionOwner for NoOperator {
    async fn ask(
        &self,
        _session_id: &str,
        _questions: &[AskQuestion],
        _timeout: Duration,
    ) -> Result<AskAnswers, AskError> {
        Err(AskError::NoOperator)
    }
}

/// What the tool is asked: one call's questions.
#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
struct AskRequest {
    #[schemars(
        length(min = 1, max = 16),
        description = "Questions to ask the user before continuing."
    )]
    questions: Vec<AskQuestion>,
}

/// `ask` — put questions to the operator and return the answers.
pub struct AskTool {
    owner: Arc<dyn QuestionOwner>,
}

impl std::fmt::Debug for AskTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AskTool").finish_non_exhaustive()
    }
}

impl AskTool {
    /// The tool that asks `owner`.
    #[must_use]
    pub fn new(owner: Arc<dyn QuestionOwner>) -> Self {
        Self { owner }
    }
}

#[async_trait]
impl Tool for AskTool {
    fn name(&self) -> &str {
        "ask"
    }

    fn description(&self) -> &str {
        "Ask the user a concise question when you need confirmation, a \
         choice, or missing information before proceeding. Each question \
         has a stable id, optional options (put the recommended one \
         first), and may allow multiple selections. An answer with \
         neither a selection nor custom text means the user skipped \
         that question: proceed without it and do not ask again."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(AskRequest))
            .expect("AskRequest schema is always serializable")
    }

    /// The operator answers one batch at a time: two questions sets
    /// racing for one human is a dialog nobody can follow, and the
    /// call blocks its wave until the answer arrives.
    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    fn output_definition(&self) -> uva_tool::ToolOutputDefinition {
        uva_tool::ToolOutputDefinition::passthrough("ask")
            .with_kind(uva_tool::output::RenderKind::Text)
            .with_title("Ask")
    }

    async fn call(
        &self,
        input: serde_json::Value,
        context: &Context,
    ) -> ToolOutput {
        let request: AskRequest = match serde_json::from_value(input) {
            Ok(request) => request,
            Err(error) => {
                return ToolOutput::error(format!(
                    "Invalid arguments: {error}"
                ));
            }
        };
        // The schema says one to sixteen questions; the runtime says
        // it too — an empty ask is a question nobody asked, and a
        // flood of them is a dialog nobody can follow. Neither
        // reaches the operator.
        if request.questions.is_empty() {
            return ToolOutput::error(
                "Invalid arguments: ask at least one question",
            );
        }
        if request.questions.len() > MAX_QUESTIONS {
            return ToolOutput::error(format!(
                "Invalid arguments: ask at most {MAX_QUESTIONS} questions \
                 at a time ({} asked)",
                request.questions.len()
            ));
        }
        match self
            .owner
            .ask(&context.session_id, &request.questions, ASK_TIMEOUT)
            .await
        {
            Ok(answers) => match completed(&request.questions, answers) {
                Ok(batch) => render(&batch),
                Err(message) => ToolOutput::error(message),
            },
            Err(AskError::NoOperator) => ToolOutput::error(
                "no operator is attached to this session; the question \
                 cannot be answered here",
            ),
            Err(AskError::Refused(message)) => ToolOutput::error(format!(
                "the question was refused: {message}"
            )),
            Err(AskError::Timeout) => ToolOutput::timeout(ASK_TIMEOUT),
        }
    }
}

/// Check the batch answers every question exactly once.
///
/// The client's answer is untrusted: a batch that misses a question,
/// answers one twice, or answers a question the call never asked is
/// not an answer, and feeding it to the model as one would let a
/// broken client invent the operator's words.
fn completed(
    questions: &[AskQuestion],
    answers: AskAnswers,
) -> Result<Vec<AskAnswer>, String> {
    let mut expected: Vec<&str> = questions
        .iter()
        .map(|question| question.id.as_str())
        .collect();
    expected.sort_unstable();
    let mut answered: Vec<&str> =
        answers.answers.iter().map(|a| a.id.as_str()).collect();
    answered.sort_unstable();
    if expected != answered {
        return Err(format!(
            "the answer batch must name each of the {} questions exactly \
             once ({})",
            questions.len(),
            expected.join(", "),
        ));
    }
    Ok(answers.answers)
}

/// The answer the model reads: dsh's output shape, rendered the same
/// way (`JSON.stringify(value)`).
fn render(answers: &[AskAnswer]) -> ToolOutput {
    ToolOutput::text(
        serde_json::to_string(&serde_json::json!({ "answers": answers }))
            .expect("answers are always serializable"),
    )
}

/// `serde`'s `is_false` for `skip_serializing_if`.
fn is_false(value: &bool) -> bool {
    !*value
}
