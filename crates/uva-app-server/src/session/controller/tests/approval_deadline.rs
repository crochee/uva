//! The approval deadline: a held call that nobody answers is denied,
//! and the turn continues.
//!
//! The deadline is the host's business (the controller waits; the
//! harness ends the turn when it holds a call), so these tests drive
//! the controller: a log that holds one open interrupt, a deadline,
//! and the answer the deadline produces.

use std::{pin::Pin, sync::Arc, time::Duration};

use parking_lot::Mutex;
use uva::harness::{AgentEvent, ResumeStatus, SessionEndReason, SystemEvent};

use super::support::{
    RecordedCall,
    make_manager_and_controller_with_deps,
    test_deps,
    wait_for_runs,
};
use crate::session::controller::{RunStreamFactory, SessionOp};

/// A factory that records the run and writes the decision rows the
/// harness writes when it applies a resume.
///
/// The rows matter: the paused state is a property of the log, so a
/// fake run that answered an interrupt without recording the answer
/// would leave the session paused — and the deadline would (correctly)
/// deny it a second time.
struct DecisionWritingFactory {
    calls: Arc<Mutex<Vec<RecordedCall>>>,
}

impl RunStreamFactory for DecisionWritingFactory {
    fn run_stream(
        &self,
        config: crate::agent_run_config::AgentRunConfig,
        overrides: crate::agent_run_config::RunOverrides,
        input: uva::harness::AgentInput,
        _cancel: Arc<uva::core::AtomicCancelToken>,
    ) -> Pin<Box<dyn futures::Stream<Item = AgentEvent> + Send + 'static>> {
        self.calls.lock().push(RecordedCall {
            config: config.clone(),
            overrides,
            input: input.clone(),
        });
        if let Some(writer) = &config.log_writer {
            for decision in &input.resume_decisions {
                writer.append_event_nowait(&uva::session::approval_decided(
                    &decision.id,
                    &decision.id,
                    decision.durable_status(),
                    "resume",
                    decision.payload.clone(),
                ));
            }
        }
        let stream = futures::stream::iter(vec![AgentEvent::System(
            SystemEvent::SessionEnded {
                reason: SessionEndReason::Completed,
            },
        )]);
        Box::pin(stream)
    }
}

/// The log a suspension leaves: the turn that asked, the envelope it
/// was sampled under, the open interrupt, and the turn closed as
/// suspended.
///
/// The request snapshot matters: the resume token is derived from it,
/// so a log without one cannot be resumed at all — by an operator or
/// by the deadline.
async fn write_suspended_turn(
    manager: &super::support::FixtureRegistry,
) -> uva::harness::ApprovalRequest {
    let writer = manager.writer("alice", "s1");
    writer
        .append_event(&uva::session::turn_start(0))
        .await
        .unwrap();
    writer
        .append_event(&uva::session::request_snapshot(
            "anthropic",
            "claude",
            "sha256:params",
            "sha256:tools",
            "sha256:system",
            Vec::new(),
            "sha256:policy",
        ))
        .await
        .unwrap();
    writer
        .append_event(&uva::session::approval_asked(
            "a1",
            "c1",
            "shell",
            "sha256:args",
            "high",
            "default",
        ))
        .await
        .unwrap();
    writer
        .append_event(&uva::session::turn_end_suspended())
        .await
        .unwrap();
    writer.flush().await.unwrap();
    let rows = manager.sink("alice", "s1").read().await.unwrap();
    let open = uva::harness::open_requests(&rows);
    open.into_iter().next().expect("one open interrupt")
}

/// Nobody answered: the deadline denies the held call and the turn
/// continues with the model's own refusal.
///
/// The denial is a normal resume — the same path an operator's
/// answer takes — so the log records exactly one decision per call
/// and the run the deadline starts carries it.
#[tokio::test]
async fn the_deadline_denies_the_held_call_and_the_turn_continues() {
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn RunStreamFactory> = Arc::new(DecisionWritingFactory {
        calls: Arc::clone(&calls),
    });
    let deps =
        test_deps().with_approval_timeout(Some(Duration::from_millis(50)));
    let (controller, manager, _temp) = make_manager_and_controller_with_deps(
        Duration::from_secs(60),
        factory,
        deps,
    )
    .await;
    write_suspended_turn(&manager).await;

    // A prompt cannot start: the log holds an open interrupt, so the
    // session is waiting for an operator. That wait is what the
    // deadline measures.
    controller
        .submit(SessionOp::Prompt {
            content: "hello".to_string(),
            priority: 0,
        })
        .await
        .unwrap();

    // The deadline fires and resumes the suspended turn.
    wait_for_runs(&calls, 1).await;
    let recorded = calls.lock();
    // The resumed turn is the one that carries decisions; the
    // queued prompt (parked while the session waited) runs after it.
    let resumed = recorded
        .iter()
        .find(|call| !call.input.resume_decisions.is_empty())
        .expect("the deadline resumed the suspended turn");
    let decisions = &resumed.input.resume_decisions;
    assert_eq!(decisions.len(), 1, "one decision per open interrupt");
    assert_eq!(
        decisions[0].status,
        ResumeStatus::TimedOut,
        "the deadline's answer is its own, not a rejection"
    );
    assert_eq!(decisions[0].id, "a1", "and it names the interrupt");
}

/// An operator who answers first keeps their answer: the deadline
/// finds no open interrupt and says nothing.
#[tokio::test]
async fn an_operators_answer_wins_over_the_deadline() {
    let calls = Arc::new(Mutex::new(Vec::<RecordedCall>::new()));
    let factory: Arc<dyn RunStreamFactory> = Arc::new(DecisionWritingFactory {
        calls: Arc::clone(&calls),
    });
    let deps =
        test_deps().with_approval_timeout(Some(Duration::from_millis(200)));
    let (controller, manager, _temp) = make_manager_and_controller_with_deps(
        Duration::from_secs(60),
        factory,
        deps,
    )
    .await;
    write_suspended_turn(&manager).await;

    controller
        .submit(SessionOp::Prompt {
            content: "hello".to_string(),
            priority: 0,
        })
        .await
        .unwrap();

    // The operator answers before the deadline.
    let rows = manager.sink("alice", "s1").read().await.unwrap();
    let snapshot = uva::harness::latest_request_snapshot_hash(&rows)
        .expect("the log holds a request snapshot");
    let tail = rows
        .iter()
        .filter_map(|row| row.get("seq").and_then(serde_json::Value::as_u64))
        .max()
        .unwrap_or(0);
    let token =
        uva::harness::ResumeToken::new(tail, vec!["c1".to_string()], snapshot);
    controller
        .submit(SessionOp::Resume {
            token,
            decisions: vec![uva::harness::ResumeDecision {
                id: "a1".to_string(),
                status: ResumeStatus::Resolved,
                payload: None,
            }],
            allow_drift: true,
        })
        .await
        .unwrap();

    wait_for_runs(&calls, 1).await;
    // The deadline would have fired by now; it must not have decided
    // anything.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let recorded = calls.lock();
    let resumed = recorded
        .iter()
        .find(|call| !call.input.resume_decisions.is_empty())
        .expect("the operator's resume ran the turn");
    let decisions = &resumed.input.resume_decisions;
    assert_eq!(
        decisions[0].status,
        ResumeStatus::Resolved,
        "the operator's answer stands"
    );
    assert!(
        recorded.iter().all(|call| !call
            .input
            .resume_decisions
            .iter()
            .any(|decision| { decision.status == ResumeStatus::TimedOut })),
        "the deadline decided nothing: the operator had already answered"
    );
}
