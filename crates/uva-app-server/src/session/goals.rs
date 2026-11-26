//! The session's goal, over its one log.
//!
//! The `goal` tool writes through this store; the session controller
//! reads it to decide whether a turn that ended should start
//! another. Both sides go through the log — one row per change, the
//! latest row the goal's state — so the tool and the continuation can
//! never disagree about the objective.
//!
//! The store needs the session registry and nothing else: the rows
//! come from the session's sink, the writes go through the session's
//! one writer.

use std::sync::{Arc, OnceLock};

use async_trait::async_trait;
use uva::{
    session::{SessionEvent, manager::SessionRegistry},
    tool_goal::{GOAL_ACTIVE, GoalSnapshot, GoalStore},
};

/// The session's goal, read and written through its log.
pub struct SessionGoals {
    /// The deployment's sessions: the sink to read, the writer to
    /// append.
    ///
    /// Behind a slot because the sessions service is published by a
    /// row during the mount — the tool that holds this store exists
    /// before the service does.
    sessions: Arc<OnceLock<Arc<SessionRegistry>>>,
}

impl std::fmt::Debug for SessionGoals {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionGoals")
            .field("resolved", &self.sessions.get().is_some())
            .finish_non_exhaustive()
    }
}

impl SessionGoals {
    /// The store over the slot the boot fills after the mount.
    #[must_use]
    pub fn new(sessions: Arc<OnceLock<Arc<SessionRegistry>>>) -> Self {
        Self { sessions }
    }

    /// The store over a registry that already exists (the controller
    /// builds one from the deployment's sessions).
    #[must_use]
    pub fn resolved(sessions: Arc<SessionRegistry>) -> Self {
        let slot = Arc::new(OnceLock::new());
        let _ = slot.set(sessions);
        Self { sessions: slot }
    }

    /// The sessions service, once the boot has filled the slot.
    fn sessions(&self) -> Option<&Arc<SessionRegistry>> {
        self.sessions.get()
    }

    /// The session's rows, when the registry knows the session.
    async fn rows(&self, session_id: &str) -> Option<Vec<serde_json::Value>> {
        let sessions = self.sessions()?;
        let session = sessions.get(session_id).await?;
        sessions
            .sink(&session.user_id, session_id)
            .read()
            .await
            .ok()
    }
}

#[async_trait]
impl GoalStore for SessionGoals {
    async fn read(&self, session_id: &str) -> Option<GoalSnapshot> {
        let rows = self.rows(session_id).await?;
        // The latest goal row is the goal's state.
        rows.iter().rev().find_map(goal_from_row)
    }

    async fn write(
        &self,
        session_id: &str,
        goal: &GoalSnapshot,
    ) -> Result<(), String> {
        let sessions = self.sessions().ok_or_else(|| {
            "the sessions service is not available yet".to_string()
        })?;
        let session = sessions.get(session_id).await.ok_or_else(|| {
            format!("the session '{session_id}' is not known")
        })?;
        let writer = sessions.writer(&session.user_id, session_id);
        writer
            .append_event(&uva::session::goal(
                &goal.state,
                &goal.objective,
                goal.budget_tokens,
            ))
            .await
            .map_err(|error| error.to_string())?;
        // The answer is a durability promise: the goal is on disk
        // before the tool returns.
        writer.flush().await.map_err(|error| error.to_string())
    }
}

/// The goal one log row carries, when the row is a `goal` row.
fn goal_from_row(row: &serde_json::Value) -> Option<GoalSnapshot> {
    match SessionEvent::from_value(row)? {
        SessionEvent::Goal {
            state,
            objective,
            budget_tokens,
            ..
        } => Some(GoalSnapshot {
            state,
            objective,
            budget_tokens,
        }),
        _ => None,
    }
}

/// The tokens a session has spent, from its `usage` rows.
///
/// The goal's budget is checked against this, so a goal cannot run a
/// deployment's account dry.
pub async fn spent_tokens(goal_store: &SessionGoals, session_id: &str) -> u64 {
    let Some(rows) = goal_store.rows(session_id).await else {
        return 0;
    };
    let mut total = 0u64;
    for row in &rows {
        let Some(SessionEvent::Usage { usage, .. }) =
            SessionEvent::from_value(row)
        else {
            continue;
        };
        // A row without the typed report contributes nothing: the sum
        // covers the rows this build wrote.
        let Some(buckets) = usage else {
            continue;
        };
        total = total
            .saturating_add(buckets.input_tokens)
            .saturating_add(buckets.output_tokens);
    }
    total
}

/// Whether the goal's budget still allows another turn.
pub fn budget_allows(goal: &GoalSnapshot, spent: u64) -> bool {
    match goal.budget_tokens {
        Some(budget) => spent < budget,
        None => true,
    }
}

/// The goal the continuation should act on, if any.
pub fn continuation_goal(goal: Option<GoalSnapshot>) -> Option<GoalSnapshot> {
    goal.filter(|goal| goal.state == GOAL_ACTIVE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_latest_goal_row_wins() {
        let rows = [
            serde_json::json!({
                "seq": 1, "ts": "t", "type": "goal",
                "state": "active", "objective": "first",
            }),
            serde_json::json!({
                "seq": 2, "ts": "t", "type": "goal",
                "state": "completed", "objective": "first",
            }),
        ];
        let goal = rows.iter().rev().find_map(goal_from_row).expect("a goal");
        assert_eq!(goal.state, "completed");
        assert_eq!(goal.objective, "first");
    }

    #[test]
    fn a_row_that_is_not_a_goal_is_ignored() {
        let row = serde_json::json!({
            "seq": 1, "ts": "t", "type": "step", "n": 1,
        });
        assert!(goal_from_row(&row).is_none());
    }

    #[test]
    fn the_budget_bounds_the_continuation() {
        let goal = GoalSnapshot {
            state: GOAL_ACTIVE.to_string(),
            objective: "work".to_string(),
            budget_tokens: Some(100),
        };
        assert!(budget_allows(&goal, 99));
        assert!(!budget_allows(&goal, 100));
        let unbounded = GoalSnapshot {
            budget_tokens: None,
            ..goal
        };
        assert!(budget_allows(&unbounded, u64::MAX));
    }

    #[test]
    fn only_an_active_goal_continues() {
        let active = GoalSnapshot {
            state: GOAL_ACTIVE.to_string(),
            objective: "work".to_string(),
            budget_tokens: None,
        };
        assert!(continuation_goal(Some(active.clone())).is_some());
        let paused = GoalSnapshot {
            state: "paused".to_string(),
            ..active
        };
        assert!(continuation_goal(Some(paused)).is_none());
        assert!(continuation_goal(None).is_none());
    }
}
