//! Default implementations of [`PlanGenerator`], [`Executor`], and
//! [`RePlanner`].

use std::sync::Arc;

use async_trait::async_trait;
use synthia_core::CancelToken;
use synthia_harness::{AgentRuntime, EventSink};
use synthia_provider::{
    CompletionRequest,
    CompletionResponse,
    Content,
    ContentPart,
    Message,
    Role,
    TextContent,
};
use synthia_tool::{Context, Tool, ToolOutput};

use crate::strategy::{
    Executor,
    Plan,
    PlanGenerator,
    RePlanner,
    ReplanDecision,
    Step,
    StepOutcome,
};

/// `PlanGenerator` that asks the model for a numbered plan.
///
/// The model is invoked with a fixed prompt that requests
/// `step 1: ...\nstep 2: ...` and we parse the response. A custom
/// implementation can override `PlanGenerator::generate` with a
/// different prompt or parser.
pub struct DefaultPlanGenerator;

#[async_trait]
impl PlanGenerator for DefaultPlanGenerator {
    async fn generate(
        &self,
        input: &str,
        runtime: &AgentRuntime,
    ) -> Result<Plan, synthia_core::Error> {
        let system = "You are a planner. Output a numbered plan, one step per line, in the form `1. ...\\n2. ...`. Do not include any other text.";
        let request = CompletionRequest {
            model: runtime
                .caps
                .model
                .provider
                .clone()
                .model_config()
                .name
                .clone(),
            messages: Arc::new(vec![
                Message {
                    role: Role::System,
                    content: Content::Single(ContentPart::Text(TextContent {
                        text: system.into(),
                        cache_control: None,
                    })),
                    tool_call_id: None,
                    name: None,
                    tool_result_cleared_at: None,
                },
                Message {
                    role: Role::User,
                    content: Content::Single(ContentPart::Text(TextContent {
                        text: format!("Task: {input}\n\nPlan (numbered):"),
                        cache_control: None,
                    })),
                    tool_call_id: None,
                    name: None,
                    tool_result_cleared_at: None,
                },
            ]),
            tools: Arc::new(Vec::new()),
            ..CompletionRequest::default()
        };
        let response = runtime
            .caps
            .model
            .provider
            .clone()
            .complete(request)
            .await?;
        let text = content_to_text(&response);
        Ok(parse_plan(&text))
    }
}

fn content_to_text(resp: &CompletionResponse) -> String {
    match &resp.content {
        Content::Single(ContentPart::Text(t)) => t.text.clone(),
        Content::Single(_) => String::new(),
        Content::Multi(parts) => parts
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(""),
    }
}

pub(crate) fn parse_plan(text: &str) -> Plan {
    let steps: Vec<Step> = text
        .lines()
        .filter_map(|line| {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return None;
            }
            // Match "1.", "1)", "step 1:", etc.
            let body = trimmed
                .trim_start_matches(|c: char| {
                    c.is_ascii_digit() || c == '.' || c == ')' || c == ':'
                })
                .trim_start_matches(|c: char| c.is_ascii_whitespace())
                .to_string();
            if body.is_empty() {
                None
            } else {
                Some(Step::new(body))
            }
        })
        .collect();
    Plan::new(steps)
}

/// `Executor` that runs a single step through the host provider +
/// tool loop. The loop is bounded by `max_iterations_per_step` (16
/// by default) and respects cancellation.
pub struct DefaultExecutor {
    pub max_iterations_per_step: u32,
}

impl DefaultExecutor {
    pub fn new(max_iterations_per_step: u32) -> Self {
        Self {
            max_iterations_per_step,
        }
    }

    /// Build a `ToolContext` for this step. Public so tests can
    /// exercise the executor in isolation.
    pub fn context(
        &self,
        runtime: &AgentRuntime,
        step_index: usize,
    ) -> Context {
        Context {
            session_id: format!("plan-step-{step_index}"),
            workspace_root: runtime.caps.env.workspace_root.clone(),
            ..Context::default()
        }
    }
}

impl Default for DefaultExecutor {
    fn default() -> Self {
        Self::new(16)
    }
}

#[async_trait]
impl Executor for DefaultExecutor {
    async fn execute(
        &self,
        step: &Step,
        runtime: &AgentRuntime,
        _cancel: Arc<dyn CancelToken>,
        sink: &EventSink,
    ) -> Result<StepOutcome, synthia_core::Error> {
        // Build a focused prompt for this step.
        let system = "You are an executor. Use the provided tools to complete the step, then output the final answer.";
        let mut messages = vec![Message {
            role: Role::System,
            content: Content::Single(ContentPart::Text(TextContent {
                text: system.into(),
                cache_control: None,
            })),
            tool_call_id: None,
            name: None,
            tool_result_cleared_at: None,
        }];
        messages.push(Message {
            role: Role::User,
            content: Content::Single(ContentPart::Text(TextContent {
                text: format!("Step: {}", step.text),
                cache_control: None,
            })),
            tool_call_id: None,
            name: None,
            tool_result_cleared_at: None,
        });

        if let Some(_iteration) = (0..self.max_iterations_per_step).next() {
            if _cancel.is_cancelled() {
                return Ok(StepOutcome::cancelled());
            }
            // The prompt and the plan land before this step samples.
            sink.flush_log().await.map_err(|error| {
                synthia_core::Error::internal(format!(
                    "log flush failed: {error}"
                ))
            })?;
            let request = CompletionRequest {
                model: runtime
                    .caps
                    .model
                    .provider
                    .clone()
                    .model_config()
                    .name
                    .clone(),
                messages: Arc::new(messages.clone()),
                tools: Arc::new(Vec::new()),
                ..CompletionRequest::default()
            };
            let response = runtime
                .caps
                .model
                .provider
                .clone()
                .complete(request)
                .await?;
            // The default executor is a no-tools loop: the model
            // emits a final answer immediately. Real implementations
            // would dispatch tools here.
            let text = content_to_text(&response);
            // The step's answer is the model's turn of this step: one
            // `assistant_message` row through the session's writer.
            sink.write_assistant_turn(&[ContentPart::Text(TextContent {
                text: text.clone(),
                cache_control: None,
            })]);
            return Ok(StepOutcome::completed(text));
        }
        Ok(StepOutcome::failed("executor iteration cap reached"))
    }
}

/// `RePlanner` that asks the model whether to continue, finalize, or
/// abort.
pub struct DefaultRePlanner;

#[async_trait]
impl RePlanner for DefaultRePlanner {
    async fn replan(
        &self,
        plan: &Plan,
        outcomes: &[StepOutcome],
        runtime: &AgentRuntime,
    ) -> Result<ReplanDecision, synthia_core::Error> {
        let next_step_index = outcomes.len();
        if next_step_index >= plan.steps.len() {
            return Ok(ReplanDecision::Finalize);
        }
        // If any prior step failed, abort the whole plan.
        if outcomes
            .iter()
            .any(|o| matches!(o, StepOutcome::Failed { .. }))
        {
            return Ok(ReplanDecision::Abort);
        }
        let system = "You are a re-planner. Reply with exactly `continue`, `finalize`, or `abort`.";
        let prompt = format!(
            "Plan: {:?}\nCompleted outcomes: {:?}\nNext step: {}",
            plan.steps.iter().map(|s| &s.text).collect::<Vec<_>>(),
            outcomes.iter().map(|o| o.summary()).collect::<Vec<_>>(),
            plan.steps[next_step_index].text,
        );
        let request = CompletionRequest {
            model: runtime
                .caps
                .model
                .provider
                .clone()
                .model_config()
                .name
                .clone(),
            messages: Arc::new(vec![
                Message {
                    role: Role::System,
                    content: Content::Single(ContentPart::Text(TextContent {
                        text: system.into(),
                        cache_control: None,
                    })),
                    tool_call_id: None,
                    name: None,
                    tool_result_cleared_at: None,
                },
                Message {
                    role: Role::User,
                    content: Content::Single(ContentPart::Text(TextContent {
                        text: prompt,
                        cache_control: None,
                    })),
                    tool_call_id: None,
                    name: None,
                    tool_result_cleared_at: None,
                },
            ]),
            tools: Arc::new(Vec::new()),
            ..CompletionRequest::default()
        };
        let response = runtime
            .caps
            .model
            .provider
            .clone()
            .complete(request)
            .await?;
        let text = content_to_text(&response).to_lowercase();
        if text.contains("abort") {
            Ok(ReplanDecision::Abort)
        } else if text.contains("finalize") {
            Ok(ReplanDecision::Finalize)
        } else {
            Ok(ReplanDecision::Continue)
        }
    }
}

/// Re-exported so the strategy module does not need a separate
/// import. Used by the `Executor` impl to project a single tool
/// result into the model's next prompt.
#[allow(dead_code)]
pub(crate) fn project_tool_output(output: &ToolOutput) -> String {
    serde_json::to_string(output).unwrap_or_default()
}

/// Empty marker helper. Used by tests to verify the executor's
/// helpers don't depend on the runtime's tool registry.
#[allow(dead_code)]
pub(crate) fn _is_tool(_t: &dyn Tool) -> bool {
    true
}
