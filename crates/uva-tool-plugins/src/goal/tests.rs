//! The `goal` tool's own tests: the lifecycle it enforces, and the
//! store it writes through.

use std::sync::{Arc, Mutex};

use serde_json::json;

use super::*;

/// A store that keeps one session's goal in memory.
#[derive(Default)]
struct MemoryGoal {
    goal: Mutex<Option<GoalSnapshot>>,
    writes: Mutex<usize>,
}

#[async_trait]
impl GoalStore for MemoryGoal {
    async fn read(&self, _session_id: &str) -> Option<GoalSnapshot> {
        self.goal.lock().expect("goal mutex").clone()
    }

    async fn write(
        &self,
        _session_id: &str,
        goal: &GoalSnapshot,
    ) -> Result<(), String> {
        *self.writes.lock().expect("writes mutex") += 1;
        *self.goal.lock().expect("goal mutex") = Some(goal.clone());
        Ok(())
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

// ---- Tool metadata --------------------------------------------

#[test]
fn tool_uses_agent_facing_name() {
    let tool = GoalTool::new(Arc::new(MemoryGoal::default()));
    assert_eq!(tool.name(), "goal");
}

#[test]
fn the_goal_is_written_sequentially() {
    let tool = GoalTool::new(Arc::new(MemoryGoal::default()));
    assert_eq!(tool.mode(), ExecutionMode::Sequential);
}

// ---- The lifecycle ---------------------------------------------

#[tokio::test]
async fn create_then_get_reads_the_same_goal() {
    let store = Arc::new(MemoryGoal::default());
    let tool = GoalTool::new(Arc::clone(&store) as Arc<dyn GoalStore>);
    let context = make_context();

    let created = tool
        .call(
            json!({
                "action": "create",
                "objective": "ship the tool family",
                "budget_tokens": 50_000,
            }),
            &context,
        )
        .await;
    let text = first_text(&created);
    assert!(
        text.contains("Goal: active, budget 50000 tokens."),
        "the answer names the new state: {text}"
    );
    assert!(
        text.contains("ship the tool family"),
        "the answer carries the objective: {text}"
    );

    let read = tool.call(json!({ "action": "get" }), &context).await;
    assert_eq!(first_text(&read), first_text(&created));
    assert_eq!(*store.writes.lock().expect("writes mutex"), 1);
}

#[tokio::test]
async fn a_second_create_is_refused_while_one_is_live() {
    let store = Arc::new(MemoryGoal::default());
    let tool = GoalTool::new(Arc::clone(&store) as Arc<dyn GoalStore>);
    let context = make_context();

    tool.call(
        json!({ "action": "create", "objective": "first" }),
        &context,
    )
    .await;
    let again = tool
        .call(
            json!({ "action": "create", "objective": "second" }),
            &context,
        )
        .await;
    assert!(
        first_text(&again).contains("A goal is already active"),
        "the refusal names the state: {}",
        first_text(&again)
    );
    assert_eq!(*store.writes.lock().expect("writes mutex"), 1);
}

#[tokio::test]
async fn pause_and_resume_walk_the_state() {
    let store = Arc::new(MemoryGoal::default());
    let tool = GoalTool::new(Arc::clone(&store) as Arc<dyn GoalStore>);
    let context = make_context();
    tool.call(json!({ "action": "create", "objective": "work" }), &context)
        .await;

    let paused = tool.call(json!({ "action": "pause" }), &context).await;
    assert!(
        first_text(&paused).contains("Goal: paused."),
        "{}",
        first_text(&paused)
    );

    let resumed = tool.call(json!({ "action": "resume" }), &context).await;
    assert!(
        first_text(&resumed).contains("Goal: active."),
        "{}",
        first_text(&resumed)
    );
}

#[tokio::test]
async fn pausing_a_paused_goal_is_refused() {
    let store = Arc::new(MemoryGoal::default());
    let tool = GoalTool::new(Arc::clone(&store) as Arc<dyn GoalStore>);
    let context = make_context();
    tool.call(json!({ "action": "create", "objective": "work" }), &context)
        .await;
    tool.call(json!({ "action": "pause" }), &context).await;

    let again = tool.call(json!({ "action": "pause" }), &context).await;
    assert!(
        first_text(&again).contains("only an active goal can be paused"),
        "the refusal says what is allowed: {}",
        first_text(&again)
    );
}

#[tokio::test]
async fn complete_ends_the_objective() {
    let store = Arc::new(MemoryGoal::default());
    let tool = GoalTool::new(Arc::clone(&store) as Arc<dyn GoalStore>);
    let context = make_context();
    tool.call(json!({ "action": "create", "objective": "work" }), &context)
        .await;

    let done = tool.call(json!({ "action": "complete" }), &context).await;
    let text = first_text(&done);
    assert!(
        text.contains("Goal: completed."),
        "the answer names the new state: {text}"
    );

    // A completed goal can be replaced.
    let next = tool
        .call(json!({ "action": "create", "objective": "next" }), &context)
        .await;
    assert!(
        first_text(&next).contains("Goal: active."),
        "a finished goal does not block a new one: {}",
        first_text(&next)
    );
}

#[tokio::test]
async fn drop_abandons_the_goal_and_clears_it() {
    let store = Arc::new(MemoryGoal::default());
    let tool = GoalTool::new(Arc::clone(&store) as Arc<dyn GoalStore>);
    let context = make_context();
    tool.call(json!({ "action": "create", "objective": "work" }), &context)
        .await;

    let dropped = tool.call(json!({ "action": "drop" }), &context).await;
    let text = first_text(&dropped);
    assert!(text.contains("Goal: dropped."), "{}", text);
    let goal = store.goal.lock().expect("goal mutex").clone();
    assert_eq!(
        goal.map(|goal| goal.objective),
        Some(String::new()),
        "a dropped goal carries no objective"
    );
}

#[tokio::test]
async fn acting_without_a_goal_is_refused() {
    let tool =
        GoalTool::new(Arc::new(MemoryGoal::default()) as Arc<dyn GoalStore>);
    let context = make_context();

    let out = tool.call(json!({ "action": "complete" }), &context).await;
    assert!(
        first_text(&out).contains("There is no goal"),
        "the refusal says so: {}",
        first_text(&out)
    );
    let read = tool.call(json!({ "action": "get" }), &context).await;
    assert!(
        first_text(&read).contains("No goal."),
        "reading none says so: {}",
        first_text(&read)
    );
}

#[tokio::test]
async fn create_without_an_objective_is_refused() {
    let tool =
        GoalTool::new(Arc::new(MemoryGoal::default()) as Arc<dyn GoalStore>);
    let out = tool
        .call(
            json!({ "action": "create", "objective": "  " }),
            &make_context(),
        )
        .await;
    assert!(
        first_text(&out).contains("`objective` is required"),
        "the refusal names the argument: {}",
        first_text(&out)
    );
}
