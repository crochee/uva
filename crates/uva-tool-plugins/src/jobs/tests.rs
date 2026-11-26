//! The `job` tool's own tests: a command that keeps running after the
//! call ends, and the model that comes back to it.

use std::path::PathBuf;

use serde_json::json;

use super::*;

fn make_context() -> Context {
    Context::new("s1".to_string(), PathBuf::from("/tmp"))
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
    assert_eq!(JobTool::new().name(), "job");
}

/// A job starts a process: two starts in one pass would race for the
/// registry, so the tool runs sequentially.
#[test]
fn job_is_sequential() {
    assert_eq!(JobTool::new().mode(), ExecutionMode::Sequential);
}

#[test]
fn parameters_carry_the_action() {
    let params = JobTool::new().parameters();
    let required = params["required"]
        .as_array()
        .expect("required should be a list");
    let labels: Vec<&str> =
        required.iter().filter_map(|v| v.as_str()).collect();
    assert!(labels.contains(&"action"));
}

// ---- The contract ----------------------------------------------

/// A command that finishes quickly is readable through the tool.
#[cfg(unix)]
#[tokio::test]
async fn a_started_job_answers_with_output_and_status() {
    let tool = JobTool::new();
    let context = make_context();

    let started = tool
        .call(
            json!({ "action": "start", "command": "echo hello from the job" }),
            &context,
        )
        .await;
    let text = first_text(&started);
    assert!(
        text.contains("Started job"),
        "the answer names the job it started: {text}"
    );
    let job_id = text
        .split("Started job `")
        .nth(1)
        .and_then(|rest| rest.split('`').next())
        .expect("the answer carries the job id")
        .to_string();

    let output = tool
        .call(
            json!({ "action": "output", "job_id": job_id, "wait_secs": 5 }),
            &context,
        )
        .await;
    let text = first_text(&output);
    assert!(
        text.contains("hello from the job"),
        "the job's output reaches the model: {text}"
    );
    assert!(
        text.contains("[exit code: 0]"),
        "the job's status reaches the model: {text}"
    );
}

/// A long command keeps running after the call that started it.
#[cfg(unix)]
#[tokio::test]
async fn a_long_job_is_still_running_when_the_model_looks() {
    let tool = JobTool::new();
    let context = make_context();

    let started = tool
        .call(
            json!({ "action": "start", "command": "sleep 30" }),
            &context,
        )
        .await;
    let text = first_text(&started);
    let job_id = text
        .split("Started job `")
        .nth(1)
        .and_then(|rest| rest.split('`').next())
        .expect("the answer carries the job id")
        .to_string();

    let output = tool
        .call(json!({ "action": "output", "job_id": job_id }), &context)
        .await;
    let text = first_text(&output);
    assert!(
        text.contains("[still running]"),
        "the model is told the job has not finished: {text}"
    );

    // The listing shows it too.
    let listed = tool.call(json!({ "action": "list" }), &context).await;
    let text = first_text(&listed);
    assert!(
        text.contains("[running]"),
        "the listing shows the running job: {text}"
    );

    // And it can be stopped.
    let killed = tool
        .call(json!({ "action": "kill", "job_id": job_id }), &context)
        .await;
    assert!(
        first_text(&killed).contains("Killed job"),
        "the kill is answered: {}",
        first_text(&killed)
    );
}

/// A finished job keeps its output; the registry keeps only the
/// recent ones, so a long deployment's memory stays bounded.
#[cfg(unix)]
#[tokio::test]
async fn old_finished_jobs_are_dropped() {
    let registry = Arc::new(JobRegistry::new());
    let tool = JobTool::with_registry(Arc::clone(&registry));
    let context = make_context();

    for _ in 0..(MAX_FINISHED_JOBS + 4) {
        let started = tool
            .call(json!({ "action": "start", "command": "true" }), &context)
            .await;
        assert!(
            first_text(&started).contains("Started job"),
            "the job starts"
        );
    }
    // The jobs finish on their own; wait for the registry to settle.
    let deadline =
        std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let finished = registry
            .list()
            .iter()
            .filter(|job| job.is_finished())
            .count();
        if finished >= MAX_FINISHED_JOBS {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the jobs never finished"
        );
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
    }
    // The cap holds: the finished jobs past the cap are gone.
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    let total = registry.list().len();
    assert!(
        total <= MAX_FINISHED_JOBS + 1,
        "the registry keeps at most the cap, got {total}"
    );
}

#[tokio::test]
async fn a_start_without_a_command_is_refused() {
    let tool = JobTool::new();
    let out = tool
        .call(json!({ "action": "start" }), &make_context())
        .await;
    assert!(
        first_text(&out).contains("`command` is required"),
        "the refusal names the missing argument: {}",
        first_text(&out)
    );
}

#[tokio::test]
async fn an_unknown_job_is_refused_by_name() {
    let tool = JobTool::new();
    let out = tool
        .call(
            json!({ "action": "output", "job_id": "nope" }),
            &make_context(),
        )
        .await;
    assert!(
        first_text(&out).contains("No job `nope`"),
        "the refusal names the job: {}",
        first_text(&out)
    );
}

#[tokio::test]
async fn an_empty_registry_lists_nothing() {
    let tool = JobTool::new();
    let out = tool
        .call(json!({ "action": "list" }), &make_context())
        .await;
    assert!(
        first_text(&out).contains("No jobs"),
        "the listing says so: {}",
        first_text(&out)
    );
}

/// A confined policy without a backend refuses the job: a background
/// job must not be the way around the sandbox.
#[cfg(unix)]
#[tokio::test]
async fn a_confined_policy_without_a_backend_refuses_the_job() {
    let tool = JobTool::new()
        .with_policy(crate::shell::sandbox::ExecutionPolicy::ReadOnly);
    let out = tool
        .call(
            json!({ "action": "start", "command": "echo hi" }),
            &make_context(),
        )
        .await;
    assert!(
        first_text(&out).contains("SANDBOX_UNAVAILABLE"),
        "the refusal is the sandbox's own: {}",
        first_text(&out)
    );
}
