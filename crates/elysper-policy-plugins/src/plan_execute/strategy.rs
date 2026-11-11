//! [`PlanExecuteStrategy`] — the `ReasoningStrategy` implementation.

use std::sync::Arc;

use async_trait::async_trait;
use elysper_core::CancelToken;
use elysper_harness::{AgentInput, AgentRuntime, EventSink, ReasoningStrategy};
use elysper_provider::{Content, ContentPart, Message, Role, TextContent};

use crate::plan_execute::defaults::{
    DefaultExecutor,
    DefaultPlanGenerator,
    DefaultRePlanner,
};

/// One step in the plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Step {
    pub text: String,
}

impl Step {
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into() }
    }
}

/// A plan: ordered list of steps + the original task description.
#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub steps: Vec<Step>,
}

impl Plan {
    pub fn new(steps: Vec<Step>) -> Self {
        Self { steps }
    }
}

/// Outcome of a single step.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepOutcome {
    Completed { output: String },
    Failed { reason: String },
    Cancelled,
}

impl StepOutcome {
    pub fn completed(output: impl Into<String>) -> Self {
        StepOutcome::Completed {
            output: output.into(),
        }
    }

    pub fn failed(reason: impl Into<String>) -> Self {
        StepOutcome::Failed {
            reason: reason.into(),
        }
    }

    pub fn cancelled() -> Self {
        StepOutcome::Cancelled
    }

    pub fn summary(&self) -> String {
        match self {
            StepOutcome::Completed { output } => {
                format!("completed: {}", truncate(output, 80))
            }
            StepOutcome::Failed { reason } => format!("failed: {reason}"),
            StepOutcome::Cancelled => "cancelled".to_string(),
        }
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

/// The plan as the model wrote it: one step per line.
fn plan_text(plan: &Plan) -> String {
    plan.steps
        .iter()
        .map(|step| step.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Decision emitted by the re-planner between steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReplanDecision {
    /// Continue with the next step.
    Continue,
    /// Wrap up; the plan is satisfied.
    Finalize,
    /// Abort the plan; subsequent steps will not run.
    Abort,
}

/// `PlanGenerator` trait. Pluggable.
#[async_trait]
pub trait PlanGenerator: Send + Sync {
    async fn generate(
        &self,
        input: &str,
        runtime: &AgentRuntime,
    ) -> Result<Plan, elysper_core::Error>;
}

/// `Executor` trait. Pluggable per-step tool loop.
#[async_trait]
pub trait Executor: Send + Sync {
    async fn execute(
        &self,
        step: &Step,
        runtime: &AgentRuntime,
        cancel: Arc<dyn CancelToken>,
        sink: &EventSink,
    ) -> Result<StepOutcome, elysper_core::Error>;
}

/// `RePlanner` trait. Pluggable.
#[async_trait]
pub trait RePlanner: Send + Sync {
    async fn replan(
        &self,
        plan: &Plan,
        outcomes: &[StepOutcome],
        runtime: &AgentRuntime,
    ) -> Result<ReplanDecision, elysper_core::Error>;
}

/// Configuration knobs.
#[derive(Clone)]
pub struct PlanExecuteConfig {
    pub max_steps: u32,
    pub reflect_every: u32,
    /// Names of tools the executor is allowed to advertise. `None`
    /// = no restriction.
    pub tool_subset: Option<Vec<String>>,
    /// Custom planner. `None` ⇒ [`DefaultPlanGenerator`].
    pub planner: Option<Arc<dyn PlanGenerator>>,
    /// Custom executor. `None` ⇒ [`DefaultExecutor`].
    pub executor: Option<Arc<dyn Executor>>,
    /// Custom re-planner. `None` ⇒ [`DefaultRePlanner`].
    pub re_planner: Option<Arc<dyn RePlanner>>,
}

impl std::fmt::Debug for PlanExecuteConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PlanExecuteConfig")
            .field("max_steps", &self.max_steps)
            .field("reflect_every", &self.reflect_every)
            .field("tool_subset", &self.tool_subset)
            .field("planner", &"<dyn PlanGenerator>")
            .field("executor", &"<dyn Executor>")
            .field("re_planner", &"<dyn RePlanner>")
            .finish()
    }
}

impl Default for PlanExecuteConfig {
    fn default() -> Self {
        Self {
            max_steps: 20,
            reflect_every: 5,
            tool_subset: None,
            planner: None,
            executor: None,
            re_planner: None,
        }
    }
}

impl PlanExecuteConfig {
    #[must_use]
    pub fn max_steps(mut self, n: u32) -> Self {
        self.max_steps = n;
        self
    }

    #[must_use]
    pub fn reflect_every(mut self, n: u32) -> Self {
        self.reflect_every = n;
        self
    }

    /// Builder-style tool-subset setter. Accepts any iterable of
    /// stringy names.
    #[must_use]
    pub fn tool_subset<I, S>(mut self, names: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.tool_subset = Some(names.into_iter().map(Into::into).collect());
        self
    }

    /// Builder-style planner setter.
    #[must_use]
    pub fn planner(mut self, p: Arc<dyn PlanGenerator>) -> Self {
        self.planner = Some(p);
        self
    }

    /// Builder-style executor setter.
    #[must_use]
    pub fn executor(mut self, e: Arc<dyn Executor>) -> Self {
        self.executor = Some(e);
        self
    }

    /// Builder-style re-planner setter.
    #[must_use]
    pub fn re_planner(mut self, r: Arc<dyn RePlanner>) -> Self {
        self.re_planner = Some(r);
        self
    }
}

/// `PlanExecuteStrategy` itself.
pub struct PlanExecuteStrategy {
    config: PlanExecuteConfig,
}

impl PlanExecuteStrategy {
    pub fn new(config: PlanExecuteConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &PlanExecuteConfig {
        &self.config
    }
}

#[async_trait]
impl ReasoningStrategy for PlanExecuteStrategy {
    fn name(&self) -> &str {
        "plan-execute"
    }

    async fn run(
        &self,
        runtime: AgentRuntime,
        input: AgentInput,
        sink: &EventSink,
    ) {
        let cancel = runtime.cancel.clone();

        if !sink.begin(&cancel) {
            sink.finish(&cancel);
            return;
        }
        // The run's epoch marker: one header naming the request
        // envelope this run's facts are produced under.
        sink.stamp_request_header(&runtime.caps, &[]);

        // Render the user prompt from the input's content.
        let user_prompt = input
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text(t) => Some(t.text.clone()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");

        let planner = self
            .config
            .planner
            .clone()
            .unwrap_or_else(|| Arc::new(DefaultPlanGenerator));
        let executor = self
            .config
            .executor
            .clone()
            .unwrap_or_else(|| Arc::new(DefaultExecutor::default()));
        let re_planner = self
            .config
            .re_planner
            .clone()
            .unwrap_or_else(|| Arc::new(DefaultRePlanner));

        // The prompt lands before the planner sees it.
        if let Err(error) = sink.flush_log().await {
            sink.fail(format!("log flush failed: {error}"));
            sink.finish(&cancel);
            return;
        }
        let plan = match planner.generate(&user_prompt, &runtime).await {
            Ok(p) => p,
            Err(e) => {
                sink.fail(format!("planner failed: {e}"));
                sink.finish(&cancel);
                return;
            }
        };

        // The plan is the model's first turn of the run: one
        // `assistant_message` row through the session's writer, so a
        // resumed session sees what the model planned.
        sink.write_assistant_turn(&[ContentPart::Text(TextContent {
            text: plan_text(&plan),
            cache_control: None,
        })]);

        let mut outcomes: Vec<StepOutcome> = Vec::new();
        for (idx, step) in plan.steps.iter().enumerate() {
            if idx as u32 >= self.config.max_steps {
                // PlanTruncated.
                sink.fail("plan truncated: max_steps reached".to_string());
                sink.finish(&cancel);
                return;
            }
            if cancel.is_cancelled() {
                sink.finish(&cancel);
                return;
            }
            // Emit StepStarted via SystemEvent::Progress.
            let _ = step; // step is used below.
            let _ = idx;
            let outcome = match executor
                .execute(step, &runtime, Arc::clone(&cancel), sink)
                .await
            {
                Ok(o) => o,
                Err(e) => StepOutcome::failed(format!("executor error: {e}")),
            };
            outcomes.push(outcome);

            // The step's rows land before the re-planner samples.
            if let Err(error) = sink.flush_log().await {
                sink.fail(format!("log flush failed: {error}"));
                sink.finish(&cancel);
                return;
            }
            let decision =
                match re_planner.replan(&plan, &outcomes, &runtime).await {
                    Ok(d) => d,
                    Err(e) => {
                        sink.fail(format!("re-planner failed: {e}"));
                        sink.finish(&cancel);
                        return;
                    }
                };
            match decision {
                ReplanDecision::Continue => {}
                ReplanDecision::Finalize => break,
                ReplanDecision::Abort => {
                    sink.fail("plan aborted by re-planner".to_string());
                    sink.finish(&cancel);
                    return;
                }
            }
        }

        sink.finish(&cancel);
    }
}

// `Message` re-export so the lib module compiles without the
// provider crate leaking into the public API. Tests use the full
// path.
#[allow(dead_code)]
const _MESSAGE_TYPE_CHECK: fn() = || {
    let m: Message = Message {
        role: Role::System,
        content: Content::Single(ContentPart::Text(TextContent {
            text: String::new(),
            cache_control: None,
        })),
        tool_call_id: None,
        name: None,
        tool_result_cleared_at: None,
    };
    let _ = m;
};
