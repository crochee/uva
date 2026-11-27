//! The `ask` tool's own tests: the answer contract it enforces, and
//! the owner seam it calls through.

use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;

/// An owner the tests script: it answers what it was told to answer
/// and remembers what it was asked.
#[derive(Default)]
struct ScriptedOwner {
    asked: Mutex<Vec<(String, Vec<AskQuestion>)>>,
    answer: Mutex<Option<AskAnswers>>,
    error: Mutex<Option<AskError>>,
}

#[async_trait]
impl QuestionOwner for ScriptedOwner {
    async fn ask(
        &self,
        session_id: &str,
        questions: &[AskQuestion],
        _timeout: Duration,
    ) -> Result<AskAnswers, AskError> {
        self.asked
            .lock()
            .expect("asked mutex")
            .push((session_id.to_string(), questions.to_vec()));
        if let Some(error) = self.error.lock().expect("error mutex").clone() {
            return Err(error);
        }
        Ok(self
            .answer
            .lock()
            .expect("answer mutex")
            .clone()
            .unwrap_or_default())
    }
}

fn question(id: &str) -> AskQuestion {
    AskQuestion {
        id: id.to_string(),
        question: "Which one?".to_string(),
        header: None,
        options: Some(vec![AskOption {
            label: "A".to_string(),
            description: None,
        }]),
        multi_select: false,
    }
}

fn make_context() -> Context {
    Context::new("s1".to_string(), std::path::PathBuf::from("/tmp"))
}

fn first_text(out: &ToolOutput) -> String {
    out.content
        .iter()
        .find_map(|c| c.text().map(str::to_string))
        .unwrap_or_default()
}

fn input(questions: &[AskQuestion]) -> serde_json::Value {
    serde_json::to_value(serde_json::json!({ "questions": questions }))
        .expect("the request serialises")
}

/// The tool over a scripted owner.
///
/// The owner crosses into the tool as a trait object; the test keeps
/// the concrete handle to read what it was asked.
fn tool_for(owner: &Arc<ScriptedOwner>) -> AskTool {
    let owner: Arc<dyn QuestionOwner> = owner.clone();
    AskTool::new(owner)
}

// ---- Tool metadata ---------------------------------------------

#[test]
fn tool_uses_agent_facing_name() {
    let tool = AskTool::new(Arc::new(ScriptedOwner::default()));
    assert_eq!(tool.name(), "ask");
}

#[test]
fn the_operator_answers_one_batch_at_a_time() {
    let tool = AskTool::new(Arc::new(ScriptedOwner::default()));
    assert_eq!(tool.mode(), ExecutionMode::Sequential);
}

#[test]
fn the_parameters_are_the_questions() {
    let tool = AskTool::new(Arc::new(ScriptedOwner::default()));
    let schema = tool.parameters();
    let question = &schema["$defs"]["AskQuestion"]["properties"];
    assert!(
        question["multi_select"].is_object(),
        "the schema spells multi_select: {schema}"
    );
    assert_eq!(
        question.as_object().expect("the properties").len(),
        5,
        "the schema has exactly dsh's five members: {schema}"
    );
    assert_eq!(
        schema["properties"]["questions"]["maxItems"], 16,
        "the batch bound is in the schema: {schema}"
    );
}

// ---- The happy path --------------------------------------------

#[tokio::test]
async fn the_answer_becomes_the_tool_result() {
    let owner = Arc::new(ScriptedOwner::default());
    *owner.answer.lock().expect("answer mutex") = Some(AskAnswers {
        answers: vec![AskAnswer {
            id: "db".to_string(),
            selected: vec!["Postgres".to_string()],
            custom: None,
        }],
    });
    let tool = tool_for(&owner);
    let out = tool
        .call(
            input(&[AskQuestion {
                id: "db".to_string(),
                question: "Which database?".to_string(),
                header: None,
                options: None,
                multi_select: false,
            }]),
            &make_context(),
        )
        .await;
    assert!(out.is_error.is_none(), "{:?}", out.content);
    assert_eq!(
        first_text(&out),
        r#"{"answers":[{"id":"db","selected":["Postgres"]}]}"#,
        "dsh's output shape, rendered as JSON"
    );
    // The question went to the owner with the session the call
    // belongs to.
    let asked = owner.asked.lock().expect("asked mutex");
    assert_eq!(asked[0].0, "s1");
    assert_eq!(asked[0].1.len(), 1);
}

#[tokio::test]
async fn a_skipped_question_is_an_answer_not_a_failure() {
    let owner = Arc::new(ScriptedOwner::default());
    *owner.answer.lock().expect("answer mutex") = Some(AskAnswers {
        answers: vec![AskAnswer {
            id: "q1".to_string(),
            selected: vec![],
            custom: None,
        }],
    });
    let tool = tool_for(&owner);
    let out = tool.call(input(&[question("q1")]), &make_context()).await;
    assert!(out.is_error.is_none(), "a skip is an answer");
}

// ---- The batch contract ----------------------------------------

#[tokio::test]
async fn an_answer_that_misses_a_question_is_refused() {
    let owner = Arc::new(ScriptedOwner::default());
    *owner.answer.lock().expect("answer mutex") = Some(AskAnswers {
        answers: vec![AskAnswer {
            id: "q2".to_string(),
            selected: vec![],
            custom: None,
        }],
    });
    let tool = tool_for(&owner);
    let out = tool.call(input(&[question("q1")]), &make_context()).await;
    let text = first_text(&out);
    assert_eq!(out.is_error, Some(true));
    assert!(
        text.contains("exactly once") && text.contains("q1"),
        "the refusal names the missed question: {text}"
    );
}

#[tokio::test]
async fn an_answer_that_duplicates_a_question_is_refused() {
    let owner = Arc::new(ScriptedOwner::default());
    *owner.answer.lock().expect("answer mutex") = Some(AskAnswers {
        answers: vec![
            AskAnswer {
                id: "q1".to_string(),
                selected: vec![],
                custom: None,
            },
            AskAnswer {
                id: "q1".to_string(),
                selected: vec![],
                custom: None,
            },
        ],
    });
    let tool = tool_for(&owner);
    let out = tool.call(input(&[question("q1")]), &make_context()).await;
    assert_eq!(out.is_error, Some(true));
}

#[tokio::test]
async fn an_answer_to_a_question_nobody_asked_is_refused() {
    let owner = Arc::new(ScriptedOwner::default());
    *owner.answer.lock().expect("answer mutex") = Some(AskAnswers {
        answers: vec![AskAnswer {
            id: "invented".to_string(),
            selected: vec![],
            custom: None,
        }],
    });
    let tool = tool_for(&owner);
    let out = tool.call(input(&[question("q1")]), &make_context()).await;
    let text = first_text(&out);
    assert_eq!(out.is_error, Some(true));
    assert!(
        text.contains("q1"),
        "the refusal names the real set: {text}"
    );
}

// ---- The failure paths -----------------------------------------

#[tokio::test]
async fn no_operator_fails_closed() {
    let tool = AskTool::new(Arc::new(NoOperator));
    let out = tool.call(input(&[question("q1")]), &make_context()).await;
    let text = first_text(&out);
    assert_eq!(out.is_error, Some(true));
    assert!(
        text.contains("no operator"),
        "the refusal names the reason: {text}"
    );
}

#[tokio::test]
async fn a_refusal_carries_the_reason() {
    let owner = Arc::new(ScriptedOwner::default());
    *owner.error.lock().expect("error mutex") =
        Some(AskError::Refused("the client is gone".to_string()));
    let tool = tool_for(&owner);
    let out = tool.call(input(&[question("q1")]), &make_context()).await;
    let text = first_text(&out);
    assert_eq!(out.is_error, Some(true));
    assert!(text.contains("the client is gone"), "{text}");
}

#[tokio::test]
async fn a_timeout_is_the_structured_tool_timeout() {
    let owner = Arc::new(ScriptedOwner::default());
    *owner.error.lock().expect("error mutex") = Some(AskError::Timeout);
    let tool = tool_for(&owner);
    let out = tool.call(input(&[question("q1")]), &make_context()).await;
    assert_eq!(
        out.error_code(),
        Some(uva_tool::ToolOutput::TIMEOUT_CODE),
        "the model sees the TOOL_TIMEOUT vocabulary"
    );
}

#[tokio::test]
async fn empty_questions_do_not_travel() {
    let owner = Arc::new(ScriptedOwner::default());
    let tool = tool_for(&owner);
    let out = tool.call(json!({ "questions": [] }), &make_context()).await;
    assert_eq!(out.is_error, Some(true));
    assert!(
        owner.asked.lock().expect("asked mutex").is_empty(),
        "an empty ask never reaches the operator"
    );
}

#[tokio::test]
async fn malformed_arguments_answer_with_the_reason() {
    let tool = AskTool::new(Arc::new(ScriptedOwner::default()));
    let out = tool
        .call(json!({ "questions": "nope" }), &make_context())
        .await;
    assert_eq!(out.is_error, Some(true));
    assert!(first_text(&out).contains("Invalid arguments"));
}
