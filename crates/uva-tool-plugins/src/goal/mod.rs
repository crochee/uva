//! Agent-facing `goal` tool: the durable objective the agent keeps
//! working toward across turns.
//!
//! Both reference harnesses carry this surface: dsh's `tool-goal` and
//! omp's goal mode (`packages/coding-agent/src/modes/rpc/rpc-goal.ts`)
//! — a model-created objective with a lifecycle (create / pause /
//! resume / complete / drop), a token budget, and a host that can
//! pause it.
//!
//! The row is the truth: every change appends a `goal` row to the
//! session log, and the latest row is the goal's state — the same
//! "the log is the source" rule the approval rows follow. The host
//! reads that state on its own (the session controller's
//! continuation) and the tool never keeps a second copy.
//!
//! # Why the tool is not the continuation
//!
//! A goal that only the model remembers is a note, not a goal. The
//! continuation — "the turn ended, the goal is still active, start
//! the next turn" — belongs to whoever owns the turn lifecycle (the
//! host's session controller), not to a tool a model may forget to
//! call. The tool writes the state; the host acts on it.

use std::sync::Arc;

use async_trait::async_trait;
use schemars_derive::JsonSchema;
use serde::Deserialize;
use uva_tool::{
    Context,
    ExecutionMode,
    Tool,
    ToolAnnotations,
    ToolOutput,
    ToolOutputDefinition,
};

/// The `goal` tool brick as a native kernel plugin.
pub mod plugin;

pub use plugin::GoalToolPlugin;

#[cfg(test)]
mod tests;

/// The lifecycle states a goal row carries.
pub const GOAL_ACTIVE: &str = "active";
/// The objective is paused: the agent must not continue on its own.
pub const GOAL_PAUSED: &str = "paused";
/// The objective is met.
pub const GOAL_COMPLETED: &str = "completed";
/// The objective was abandoned.
pub const GOAL_DROPPED: &str = "dropped";

/// The session's goal, as the log's latest `goal` row says.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoalSnapshot {
    /// `"active"` | `"paused"` | `"completed"` | `"dropped"`.
    pub state: String,
    /// The objective, in the model's own words.
    pub objective: String,
    /// The token budget the goal was created with, when one was
    /// given.
    pub budget_tokens: Option<u64>,
}

impl GoalSnapshot {
    /// Whether the goal is still being worked on.
    pub fn is_active(&self) -> bool {
        self.state == GOAL_ACTIVE
    }

    /// Whether the goal is paused.
    pub fn is_paused(&self) -> bool {
        self.state == GOAL_PAUSED
    }
}

/// Where the session's goal lives: the log's `goal` rows.
///
/// The host implements this over the session's one writer, so the
/// tool never spells a row and the log stays the single truth.
#[async_trait]
pub trait GoalStore: Send + Sync {
    /// The session's goal, as the latest `goal` row says.
    async fn read(&self, session_id: &str) -> Option<GoalSnapshot>;

    /// Append one `goal` row and make it durable.
    ///
    /// # Errors
    ///
    /// The session's writer failed; the caller reports it.
    async fn write(
        &self,
        session_id: &str,
        goal: &GoalSnapshot,
    ) -> Result<(), String>;
}

/// What the tool is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum GoalAction {
    /// Start a goal (fails while one is active or paused).
    Create,
    /// The session's goal, if it has one.
    Get,
    /// Pause an active goal.
    Pause,
    /// Resume a paused goal.
    Resume,
    /// Mark the goal met.
    Complete,
    /// Abandon the goal.
    Drop,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[schemars(extend("additionalProperties" = false))]
struct GoalRequest {
    #[schemars(
        description = "What to do: `create`, `get`, `pause`, `resume`, `complete` or `drop`."
    )]
    action: GoalAction,
    #[schemars(
        description = "The objective, in your own words. Required for `action = \"create\"`."
    )]
    #[serde(default)]
    objective: Option<String>,
    #[schemars(
        description = "A token budget for the goal, when one is wanted. Optional, for `action = \"create\"`."
    )]
    #[serde(default)]
    budget_tokens: Option<u64>,
}

/// `goal` — the durable objective.
pub struct GoalTool {
    store: Arc<dyn GoalStore>,
}

impl std::fmt::Debug for GoalTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GoalTool").finish_non_exhaustive()
    }
}

impl GoalTool {
    /// The tool over `store`.
    #[must_use]
    pub fn new(store: Arc<dyn GoalStore>) -> Self {
        Self { store }
    }
}

#[async_trait]
impl Tool for GoalTool {
    fn name(&self) -> &str {
        "goal"
    }

    fn description(&self) -> &str {
        "Keep a durable objective across turns. `create` starts one (an \
         objective, optionally a token budget); `get` reads the current \
         goal; `pause` / `resume` stop and restart the host's automatic \
         continuation; `complete` marks the objective met; `drop` \
         abandons it. The host continues working toward an active goal \
         when a turn ends."
    }

    fn parameters(&self) -> serde_json::Value {
        serde_json::to_value(schemars::schema_for!(GoalRequest))
            .expect("GoalRequest schema is always serializable")
    }

    fn output_definition(&self) -> ToolOutputDefinition {
        ToolOutputDefinition::passthrough("goal")
            .with_kind(uva_tool::output::RenderKind::Text)
            .with_title("Goal")
    }

    /// The goal's state is one row per change: two writers at once
    /// would race the log's last row.
    fn mode(&self) -> ExecutionMode {
        ExecutionMode::Sequential
    }

    fn annotations(&self) -> Option<ToolAnnotations> {
        Some(ToolAnnotations {
            read_only_hint: Some(false),
            destructive_hint: Some(false),
            idempotent_hint: Some(false),
            open_world_hint: Some(false),
        })
    }

    async fn call(
        &self,
        input: serde_json::Value,
        context: &Context,
    ) -> ToolOutput {
        let request: GoalRequest = match serde_json::from_value(input) {
            Ok(r) => r,
            Err(e) => {
                return ToolOutput::error(format!("Invalid arguments: {e}"));
            }
        };
        let current = self.store.read(&context.session_id).await;
        match request.action {
            GoalAction::Get => match current {
                Some(goal) => ToolOutput::text(render(&goal)),
                None => ToolOutput::text(
                    "No goal. Start one with `action = \"create\"`.",
                ),
            },
            GoalAction::Create => {
                if let Some(goal) = current.as_ref()
                    && (goal.is_active() || goal.is_paused())
                {
                    return ToolOutput::error(format!(
                        "A goal is already {}: {}. Complete or drop it \
                         before creating another.",
                        goal.state, goal.objective
                    ));
                }
                let objective = match request.objective.as_deref() {
                    Some(objective) if !objective.trim().is_empty() => {
                        objective
                    }
                    _ => {
                        return ToolOutput::error(
                            "`objective` is required for `action = \"create\"`.",
                        );
                    }
                };
                let goal = GoalSnapshot {
                    state: GOAL_ACTIVE.to_string(),
                    objective: objective.to_string(),
                    budget_tokens: request.budget_tokens,
                };
                self.write(&context.session_id, &goal).await
            }
            GoalAction::Pause => {
                self.transition(&context.session_id, current, |goal| {
                    if !goal.is_active() {
                        return Err(format!(
                            "the goal is {}, not active; only an active \
                             goal can be paused",
                            goal.state
                        ));
                    }
                    Ok(GoalSnapshot {
                        state: GOAL_PAUSED.to_string(),
                        ..goal.clone()
                    })
                })
                .await
            }
            GoalAction::Resume => {
                self.transition(&context.session_id, current, |goal| {
                    if !goal.is_paused() {
                        return Err(format!(
                            "the goal is {}, not paused; only a paused \
                             goal can be resumed",
                            goal.state
                        ));
                    }
                    Ok(GoalSnapshot {
                        state: GOAL_ACTIVE.to_string(),
                        ..goal.clone()
                    })
                })
                .await
            }
            GoalAction::Complete => {
                self.transition(&context.session_id, current, |goal| {
                    if goal.is_active() || goal.is_paused() {
                        return Ok(GoalSnapshot {
                            state: GOAL_COMPLETED.to_string(),
                            ..goal.clone()
                        });
                    }
                    Err(format!("the goal is already {}", goal.state))
                })
                .await
            }
            GoalAction::Drop => {
                self.transition(&context.session_id, current, |goal| {
                    if goal.is_active() || goal.is_paused() {
                        return Ok(GoalSnapshot {
                            state: GOAL_DROPPED.to_string(),
                            objective: String::new(),
                            budget_tokens: None,
                        });
                    }
                    Err(format!("the goal is already {}", goal.state))
                })
                .await
            }
        }
    }
}

impl GoalTool {
    /// Apply one transition to the current goal, or say why not.
    async fn transition(
        &self,
        session_id: &str,
        current: Option<GoalSnapshot>,
        next: impl FnOnce(&GoalSnapshot) -> Result<GoalSnapshot, String>,
    ) -> ToolOutput {
        let Some(goal) = current else {
            return ToolOutput::error(
                "There is no goal. Start one with `action = \"create\"`.",
            );
        };
        match next(&goal) {
            Ok(next) => self.write(session_id, &next).await,
            Err(message) => ToolOutput::error(message),
        }
    }

    /// Write the row and answer with the new state.
    async fn write(&self, session_id: &str, goal: &GoalSnapshot) -> ToolOutput {
        match self.store.write(session_id, goal).await {
            Ok(()) => ToolOutput::text(render(goal)),
            Err(error) => ToolOutput::error(format!(
                "The goal could not be recorded: {error}"
            )),
        }
    }
}

/// The goal's answer: its state line, then the objective.
fn render(goal: &GoalSnapshot) -> String {
    let budget = match goal.budget_tokens {
        Some(budget) => format!(", budget {budget} tokens"),
        None => String::new(),
    };
    if goal.objective.is_empty() {
        return format!("Goal: {}{budget}.", goal.state);
    }
    format!("Goal: {}{budget}.\n{}", goal.state, goal.objective)
}
