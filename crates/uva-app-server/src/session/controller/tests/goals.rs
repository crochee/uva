//! The goal's continuation: an active goal starts the next turn when
//! one ends, a completed one does not, and the cap pauses a goal the
//! model never ends.

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::Stream;
use uva::{
    harness::{AgentEvent, AgentInput},
    session::manager::SessionRegistry,
    tool_goal::{GOAL_ACTIVE, GOAL_COMPLETED, GoalSnapshot, GoalStore},
};

use super::support::test_deps;
use crate::session::{
    controller::{RunDependencies, RunStreamFactory, SessionOp},
    goals::SessionGoals,
};

/// A factory whose every run ends immediately, recording the prompts
/// it was handed.
struct EndingFactory {
    prompts: Arc<Mutex<Vec<String>>>,
}

impl RunStreamFactory for EndingFactory {
    fn run_stream(
        &self,
        _config: crate::agent_run_config::AgentRunConfig,
        _overrides: crate::agent_run_config::RunOverrides,
        input: AgentInput,
        _cancel: Arc<uva::core::AtomicCancelToken>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'static>> {
        let text = input
            .content
            .iter()
            .find_map(|part| part.text().map(str::to_string))
            .unwrap_or_default();
        self.prompts.lock().expect("prompts mutex").push(text);
        Box::pin(futures::stream::empty())
    }
}

/// Build the fixture: a session whose controller reads its goal from
/// the log, and a factory whose runs end at once.
async fn fixture(
    goal: Option<GoalSnapshot>,
) -> (
    SessionGoals,
    Arc<Mutex<Vec<String>>>,
    Arc<crate::session::controller::SessionController>,
    tempfile::TempDir,
) {
    let temp = tempfile::TempDir::new().expect("a temp workspace");
    let registry = Arc::new(SessionRegistry::new(temp.path().to_path_buf()));
    registry
        .create_with_user("s1".to_string(), "alice".to_string())
        .await
        .expect("the session is created");
    // The controller's store and this handle read the same log: one
    // registry, one session.
    let goals = SessionGoals::resolved(Arc::clone(&registry));
    let deps: RunDependencies = test_deps()
        .with_goals(Arc::new(SessionGoals::resolved(Arc::clone(&registry))));
    let prompts = Arc::new(Mutex::new(Vec::new()));
    let controller = crate::session::controller::SessionController::spawn(
        "alice",
        "s1",
        registry.writer("alice", "s1"),
        deps,
        Duration::from_secs(120),
        Arc::new(EndingFactory {
            prompts: Arc::clone(&prompts),
        }),
    );
    // Write the goal after the session exists.
    if let Some(goal) = goal {
        goals
            .write("s1", &goal)
            .await
            .expect("the goal is recorded");
    }
    (goals, prompts, controller, temp)
}

fn active_goal() -> GoalSnapshot {
    GoalSnapshot {
        state: GOAL_ACTIVE.to_string(),
        objective: "ship the tool family".to_string(),
        budget_tokens: None,
    }
}

/// Wait until the factory has been handed at least `n` prompts.
async fn wait_for_prompts(prompts: &Arc<Mutex<Vec<String>>>, n: usize) {
    tokio::time::timeout(Duration::from_secs(10), async {
        while prompts.lock().expect("prompts mutex").len() < n {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the runs did not start in time");
}

#[tokio::test]
async fn an_active_goal_starts_the_next_turn() {
    let (_goals, prompts, controller, _temp) =
        fixture(Some(active_goal())).await;

    controller
        .submit(SessionOp::Prompt {
            content: "start".to_string(),
            priority: 0,
        })
        .await
        .expect("the first turn starts");

    // The first turn ends at once, and the host starts the next one
    // for the goal.
    wait_for_prompts(&prompts, 2).await;
    let prompts = prompts.lock().expect("prompts mutex").clone();
    assert_eq!(prompts[0], "start", "the user's turn ran first");
    assert!(
        prompts[1].contains("Continue working toward the goal"),
        "the continuation names the goal: {:?}",
        prompts[1]
    );
    assert!(
        prompts[1].contains("ship the tool family"),
        "the continuation carries the objective: {:?}",
        prompts[1]
    );

    controller.close().await.expect("the session closes");
}

#[tokio::test]
async fn a_completed_goal_does_not_continue() {
    let completed = GoalSnapshot {
        state: GOAL_COMPLETED.to_string(),
        ..active_goal()
    };
    let (_goals, prompts, controller, _temp) = fixture(Some(completed)).await;

    controller
        .submit(SessionOp::Prompt {
            content: "start".to_string(),
            priority: 0,
        })
        .await
        .expect("the first turn starts");
    wait_for_prompts(&prompts, 1).await;
    // Nothing continues a finished goal.
    tokio::time::sleep(Duration::from_millis(150)).await;
    let prompts = prompts.lock().expect("prompts mutex").clone();
    assert_eq!(
        prompts.len(),
        1,
        "a completed goal starts no further turn: {prompts:?}"
    );

    controller.close().await.expect("the session closes");
}

/// A goal the model never completes pauses at the cap instead of
/// spinning forever.
#[tokio::test]
async fn the_continuation_cap_pauses_the_goal() {
    let (goals, prompts, controller, _temp) =
        fixture(Some(active_goal())).await;

    controller
        .submit(SessionOp::Prompt {
            content: "start".to_string(),
            priority: 0,
        })
        .await
        .expect("the first turn starts");

    // The cap is the turn count the host starts for one goal.
    wait_for_prompts(
        &prompts,
        crate::session::controller::dispatch::MAX_GOAL_CONTINUATIONS + 1,
    )
    .await;
    // The next completion pauses the goal instead of continuing.
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let goal = goals.read("s1").await.expect("the goal");
            if goal.state == uva::tool_goal::GOAL_PAUSED {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the goal is paused at the cap");

    controller.close().await.expect("the session closes");
}
