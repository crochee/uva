//! A background job's completion reaches the session that owns it.
//!
//! The model starts a command in the background and keeps working; when
//! the command finishes, the session hears about it — the same way a
//! steer reaches a live run: a durable row plus the run's inbox. The
//! model learns that its command is done without polling for it.

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::Stream;
use serde_json::json;
use uva::{
    core::CancelToken,
    harness::{AgentEvent, AgentInput},
    tool::Tool,
};

use super::support::{make_manager_and_controller_with_deps, test_deps};
use crate::session::controller::{
    RunDependencies,
    RunStreamFactory,
    SessionOp,
};

/// A factory that records what the live run received through its
/// inbox — the loop's step boundary, stood in for.
struct InboxRecordingFactory {
    inboxed: Arc<Mutex<Vec<String>>>,
    /// How many runs the factory was asked for.
    calls: Arc<Mutex<Vec<()>>>,
}

impl RunStreamFactory for InboxRecordingFactory {
    fn run_stream(
        &self,
        _config: crate::agent_run_config::AgentRunConfig,
        _overrides: crate::agent_run_config::RunOverrides,
        input: AgentInput,
        cancel: Arc<uva::core::AtomicCancelToken>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'static>> {
        self.calls.lock().expect("calls mutex poisoned").push(());
        if let Some(inbox) = input.inbox.clone() {
            let sink = Arc::clone(&self.inboxed);
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    let drained = inbox.drain().await;
                    if drained.is_empty() {
                        continue;
                    }
                    for message in drained {
                        let text =
                            message.content.extract_text().unwrap_or_default();
                        sink.lock().expect("inbox mutex poisoned").push(text);
                    }
                    return;
                }
            });
        }
        Box::pin(async_stream::stream! {
            cancel.cancelled().await;
            // The run ends without a model answer: the test only
            // cares about what the run received.
            yield AgentEvent::System(uva::events::SystemEvent::Warning {
                kind: uva::events::WarningKind::Hook,
                message: "the run ended".to_string(),
                iteration: None,
            });
        })
    }
}

#[tokio::test]
async fn a_finished_job_reaches_the_session_that_started_it() {
    let jobs = Arc::new(uva::tool_job::JobRegistry::new());
    let deps: RunDependencies = test_deps().with_jobs(Arc::clone(&jobs));
    let inboxed = Arc::new(Mutex::new(Vec::new()));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let (controller, _manager, _temp) = make_manager_and_controller_with_deps(
        Duration::from_secs(300),
        Arc::new(InboxRecordingFactory {
            inboxed: Arc::clone(&inboxed),
            calls: Arc::clone(&calls),
        }),
        deps,
    )
    .await;

    // A live run: the session is running, so a completion has someone
    // to reach.
    controller
        .submit(SessionOp::Prompt {
            content: "start the build".to_string(),
            priority: 0,
        })
        .await
        .expect("the turn starts");
    // The run is live.
    tokio::time::timeout(Duration::from_secs(2), async {
        while calls.lock().expect("calls mutex poisoned").is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the run starts");

    // A background job finishes.
    let tool = uva::tool_job::JobTool::with_registry(Arc::clone(&jobs));
    let context = uva::tool::Context::new(
        "s1".to_string(),
        std::path::PathBuf::from("/tmp"),
    );
    let started = tool
        .call(
            json!({ "action": "start", "command": "echo the build is done" }),
            &context,
        )
        .await;
    assert!(
        first_text(&started).contains("Started job"),
        "the job starts: {}",
        first_text(&started)
    );

    // The session hears about it.
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let received = inboxed.lock().expect("inbox mutex poisoned").clone();
        if received
            .iter()
            .any(|text| text.contains("the build is done"))
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the completion never reached the session: {received:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn first_text(out: &uva::tool::ToolOutput) -> String {
    out.content
        .iter()
        .find_map(|c| c.text().map(str::to_string))
        .unwrap_or_default()
}
