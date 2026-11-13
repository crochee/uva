//! `delivery: "steer"` at the RPC boundary.
//!
//! A steer is durable before its answer: the handler appends the
//! `user_message` row (with `source: "steer"`) through the session's
//! one writer and flushes it. When the run has already settled the
//! handler must NOT pre-append anything — the ordinary queued turn
//! logs its own row when it starts, and two rows for one message
//! would make the model see it twice.

use std::{
    pin::Pin,
    sync::{
        Arc,
        Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::{Stream, StreamExt};
use serde_json::{Value, json};
use uva::{
    core::{AtomicCancelToken, CancelToken},
    harness::{AgentEvent, AgentInput},
    session::manager::SessionRegistry,
};

use crate::{
    agent_run_config::{AgentRunConfig, RunOverrides},
    routes::chat::{
        Delivery,
        SendMessageRequest,
        WireAttachment,
        send_message,
    },
    session::controller::{
        RunDependencies,
        RunStreamFactory,
        SessionController,
        SessionState,
    },
    state::AppState,
};

/// One live run that never produces an event until its cancel token
/// fires — the "run is still working" shape a steer has to land in.
struct BlockingFactory {
    calls: Arc<AtomicUsize>,
    /// Every run's own cancel token, in start order.
    ///
    /// A test ends a run through its token rather than through
    /// [`SessionController::cancel`] when it must not also drain the
    /// queue: a `Cancel` op drops pending inputs, which is the right
    /// answer to a user stop and the wrong instrument for a test that
    /// wants the run to settle *under* a message.
    tokens: Arc<Mutex<Vec<Arc<AtomicCancelToken>>>>,
}

impl RunStreamFactory for BlockingFactory {
    fn run_stream(
        &self,
        _config: AgentRunConfig,
        _overrides: RunOverrides,
        _input: AgentInput,
        cancel: Arc<AtomicCancelToken>,
    ) -> Pin<Box<dyn Stream<Item = AgentEvent> + Send + 'static>> {
        // The token is published before the call is counted: a test
        // that saw run `n` start may index its token.
        self.tokens
            .lock()
            .expect("run tokens mutex poisoned")
            .push(Arc::clone(&cancel));
        self.calls.fetch_add(1, Ordering::SeqCst);
        Box::pin(
            async_stream::stream! {
                cancel.cancelled().await;
            }
            .filter_map(|()| futures::future::ready(None::<AgentEvent>)),
        )
    }
}

struct Fixture {
    state: Arc<AppState>,
    controller: Arc<SessionController>,
    calls: Arc<AtomicUsize>,
    tokens: Arc<Mutex<Vec<Arc<AtomicCancelToken>>>>,
    _temp: tempfile::TempDir,
}

const SESSION: &str = "steer-session";

async fn fixture() -> Fixture {
    let temp = tempfile::TempDir::new().expect("a temp workspace");
    let registry = SessionRegistry::new(temp.path().to_path_buf());
    let state =
        Arc::new(AppState::for_test(registry, temp.path().to_path_buf()).await);
    let user = state.default_user_id().to_string();
    state
        .session_manager
        .create_with_user(SESSION.to_string(), user.clone())
        .await
        .expect("the session is created");
    let deps = RunDependencies::new(
        state.default_provider.read().clone(),
        Arc::clone(&state.tool_registry),
        state.workspace_root.clone(),
        state.system_prompt.clone(),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let tokens = Arc::new(Mutex::new(Vec::new()));
    let controller = SessionController::spawn(
        user.clone(),
        SESSION,
        state.session_manager.sink(&user, SESSION),
        state.session_manager.writer(&user, SESSION),
        deps,
        Duration::from_secs(300),
        Arc::new(BlockingFactory {
            calls: Arc::clone(&calls),
            tokens: Arc::clone(&tokens),
        }),
    );
    state
        .active_sessions
        .insert((user, SESSION.to_string()), Arc::clone(&controller));
    Fixture {
        state,
        controller,
        calls,
        tokens,
        _temp: temp,
    }
}

impl Fixture {
    async fn post(
        &self,
        text: &str,
        delivery: Delivery,
    ) -> crate::routes::chat::SendMessageResponse {
        self.post_named(text, delivery, None).await
    }

    /// Post with an explicit agent name — the shape the web composer
    /// always sends, which routes a text turn through `PromptMulti`.
    async fn post_named(
        &self,
        text: &str,
        delivery: Delivery,
        agent_name: Option<&str>,
    ) -> crate::routes::chat::SendMessageResponse {
        send_message(
            Arc::clone(&self.state),
            SESSION.to_string(),
            SendMessageRequest {
                text: text.to_string(),
                delivery,
                agent_name: agent_name.map(str::to_string),
                ..Default::default()
            },
        )
        .await
        .expect("the message is accepted")
    }

    async fn wait_for_state(&self, wanted: SessionState) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while self.controller.state() != wanted {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| {
            panic!(
                "state never reached {wanted:?}; it is {:?}",
                self.controller.state()
            )
        });
    }

    async fn wait_for_runs(&self, n: usize) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while self.calls.load(Ordering::SeqCst) < n {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the run factory was not invoked in time");
    }

    /// The session's durable rows. Reading through the registry's
    /// writer goes through the sink, so only flushed rows appear —
    /// exactly what the acceptance promises the caller.
    async fn durable_rows(&self) -> Vec<Value> {
        let user = self.state.default_user_id().to_string();
        self.state
            .session_manager
            .writer(&user, SESSION)
            .read()
            .await
            .expect("the session log is readable")
    }

    /// End the `index`-th run by firing its own cancel token.
    ///
    /// Unlike [`SessionController::cancel`] this leaves the queued
    /// turns alone, so a message that degraded to a queued turn
    /// still starts the next run.
    fn end_run(&self, index: usize) {
        let token = self
            .tokens
            .lock()
            .expect("run tokens mutex poisoned")
            .get(index)
            .cloned()
            .unwrap_or_else(|| panic!("run {index} never started"));
        token.cancel();
    }

    /// Wait until `text` is in the durable log, whichever row shape
    /// carries it.
    async fn wait_for_row_text(&self, text: &str) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let rows = self.durable_rows().await;
                if rows.iter().any(|row| row.to_string().contains(text)) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("the message never reached the durable log");
    }

    /// Settle the live run, and wait until it has.
    ///
    /// The run ends through its own cancel token, so the queued
    /// turns survive: a message that degraded to a queued turn must
    /// still start the next run. The barrier is the run task's settle
    /// boundary — the state flips to `Idle` the moment the run's
    /// stream ends — because that is the moment "live" ends. The
    /// run's prompt row is written when the run STARTS, so waiting
    /// for it would establish nothing.
    async fn settle_run(&self) {
        self.end_run(0);
        self.wait_for_state(SessionState::Idle).await;
    }
}

/// The acceptance is the durability promise: by the time the handler
/// returns, the steer row is in the log, even though the run has not
/// advanced to its next sample.
///
/// The handler answers `Ok` for both deliveries, so the promise is
/// the flush — not the result.
#[tokio::test]
async fn steer_is_durable_before_the_response() {
    let fixture = fixture().await;
    let started = fixture.post("start", Delivery::Queue).await;
    assert!(started.queued, "the first turn is an ordinary queue push");
    fixture.wait_for_runs(1).await;
    fixture.wait_for_state(SessionState::Running).await;

    let response = fixture.post("focus on the tests", Delivery::Steer).await;
    assert_eq!(response.delivery, Delivery::Steer);
    assert!(!response.queued, "a steer is not a queued turn");

    // The run is still blocked in its factory: nothing flushed this
    // row except the steer path itself.
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    let rows = fixture.durable_rows().await;
    let steers: Vec<&Value> = rows
        .iter()
        .filter(|row| {
            row["type"] == json!("user_message")
                && row["source"] == json!("steer")
        })
        .collect();
    assert_eq!(steers.len(), 1, "the steer row is durable: {rows:?}");
    assert!(
        steers[0].to_string().contains("focus on the tests"),
        "the row carries the message: {:?}",
        steers[0]
    );
    let decoded: uva::provider::Message =
        serde_json::from_value(steers[0]["data"].clone())
            .expect("the payload is a provider message");
    assert_eq!(decoded.role, uva::provider::Role::User);

    fixture
        .controller
        .close()
        .await
        .expect("the session closes");
}

/// Two ordinary queued turns while one run is live must both be kept,
/// and each must run as its own turn.
///
/// This is the regression guard for the prompt-dispatch shape: a
/// queued turn used to be folded into the next run's prompt (all but
/// the last became history). Every queued text now parks durably, so
/// settling the live run starts one run per parked turn, in order,
/// and both land as durable `UserInput` rows.
#[tokio::test]
async fn two_queued_text_turns_both_start_and_are_logged() {
    let fixture = fixture().await;
    let _ = fixture.post("start", Delivery::Queue).await;
    fixture.wait_for_runs(1).await;
    fixture.wait_for_state(SessionState::Running).await;

    let second = fixture.post("second turn", Delivery::Queue).await;
    let third = fixture.post("third turn", Delivery::Queue).await;
    assert!(second.queued, "a queued text turn reports `queued`");
    assert!(third.queued);
    assert_eq!(
        fixture.calls.load(Ordering::SeqCst),
        1,
        "no second run starts while the first is live"
    );

    // Settling the live run releases the queued turns: one run per
    // parked turn, in order.
    fixture.end_run(0);
    fixture.wait_for_runs(2).await;
    fixture.wait_for_state(SessionState::Running).await;
    fixture.end_run(1);
    fixture.wait_for_runs(3).await;
    fixture.wait_for_state(SessionState::Running).await;

    // The last run's rows are flushed when it settles.
    fixture.end_run(2);
    fixture.wait_for_state(SessionState::Idle).await;

    let rows = fixture.durable_rows().await;
    assert!(
        rows.iter()
            .any(|row| row.to_string().contains("second turn")),
        "the first queued turn is in the log: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.to_string().contains("third turn")),
        "the second queued turn is in the log: {rows:?}"
    );

    fixture
        .controller
        .close()
        .await
        .expect("the session closes");
}

/// The same guarantee when the caller names its agent.
///
/// The web composer resolves the default agent into the URL and sends
/// that name on every turn, so its text turns take the `PromptMulti`
/// path. Those prompts park in a queue now; two queued turns must each
/// start their own run and be logged, not overwrite one another.
#[tokio::test]
async fn two_queued_turns_with_an_explicit_agent_are_both_logged() {
    let fixture = fixture().await;
    let _ = fixture
        .post_named("start", Delivery::Queue, Some("agent"))
        .await;
    fixture.wait_for_runs(1).await;
    fixture.wait_for_state(SessionState::Running).await;

    let second = fixture
        .post_named("named second", Delivery::Queue, Some("agent"))
        .await;
    let third = fixture
        .post_named("named third", Delivery::Queue, Some("agent"))
        .await;
    assert!(second.queued);
    assert!(third.queued);

    fixture.end_run(0);
    fixture.wait_for_runs(2).await;
    fixture.wait_for_state(SessionState::Running).await;
    fixture.end_run(1);
    fixture.wait_for_runs(3).await;
    fixture.wait_for_state(SessionState::Running).await;
    fixture.end_run(2);
    fixture.wait_for_state(SessionState::Idle).await;

    let rows = fixture.durable_rows().await;
    assert!(
        rows.iter()
            .any(|row| row.to_string().contains("named second")),
        "the first named turn is in the log: {rows:?}"
    );
    assert!(
        rows.iter()
            .any(|row| row.to_string().contains("named third")),
        "the second named turn is in the log: {rows:?}"
    );

    fixture
        .controller
        .close()
        .await
        .expect("the session closes");
}

/// The settle race, settled side: by the time the steer arrives the
/// run is gone, so there is no live boundary to inject into. The turn
/// must fall back to the ordinary queue, and the route must not leave
/// a pre-appended row behind — the queued run logs the prompt itself.
#[tokio::test]
async fn a_settled_run_queues_the_steer_without_a_pre_appended_row() {
    let fixture = fixture().await;
    let _ = fixture.post("start", Delivery::Queue).await;
    fixture.wait_for_runs(1).await;
    fixture.wait_for_state(SessionState::Running).await;

    fixture.settle_run().await;

    let response = fixture.post("focus on the tests", Delivery::Steer).await;
    assert_eq!(
        response.delivery,
        Delivery::Queue,
        "no run is live, so the steer degrades to a queued turn"
    );
    assert!(response.queued);

    // The fallback starts the next run, which logs its prompt on
    // start — through the ordinary `UserInput` path, not as a steer.
    fixture.wait_for_runs(2).await;
    fixture.wait_for_state(SessionState::Running).await;
    fixture
        .controller
        .cancel()
        .await
        .expect("cancel is delivered");
    fixture
        .controller
        .close()
        .await
        .expect("the session closes");

    let rows = fixture.durable_rows().await;
    assert!(
        rows.iter().all(|row| row["source"] != json!("steer")),
        "a settled steer must not be pre-appended: {rows:?}"
    );
    let turns: Vec<&Value> = rows
        .iter()
        .filter(|row| {
            row.to_string().contains("focus on the tests")
                && row["type"] == json!("UserInput")
        })
        .collect();
    assert_eq!(
        turns.len(),
        1,
        "the queued turn logs the message exactly once: {rows:?}"
    );
}

/// The settle race, racing side: the steer and the run's end arrive
/// together.
///
/// Which side wins is a genuine race, and the gate makes both
/// outcomes correct: either the row is appended while the run is
/// still `Running` (and the caller is told `steer`), or the run has
/// already flipped to `Idle` and the message becomes an ordinary
/// queued turn (and the caller is told `queue`). What must never
/// happen is both — one message, two rows.
#[tokio::test]
async fn a_steer_racing_the_settle_is_delivered_exactly_once() {
    const RACE_TEXT: &str = "race: keep going";

    let fixture = fixture().await;
    let _ = fixture.post("start", Delivery::Queue).await;
    fixture.wait_for_runs(1).await;
    fixture.wait_for_state(SessionState::Running).await;

    let state = Arc::clone(&fixture.state);
    let steer = tokio::spawn(async move {
        send_message(
            state,
            SESSION.to_string(),
            SendMessageRequest {
                text: RACE_TEXT.to_string(),
                delivery: Delivery::Steer,
                ..Default::default()
            },
        )
        .await
    });
    // Give the steer its chance to reach the controller first, then
    // end the run underneath it.
    tokio::task::yield_now().await;
    fixture.end_run(0);

    let response = steer
        .await
        .expect("the steer task joins")
        .expect("the steer is accepted");
    match response.delivery {
        Delivery::Steer => {
            assert!(!response.queued, "a steer is not a queued turn");
            // Nothing was queued, so the run it landed in is the only
            // run that ever starts.
            assert_eq!(
                fixture.calls.load(Ordering::SeqCst),
                1,
                "a steered run must not also start a queued turn"
            );
        }
        Delivery::Queue => {
            assert!(response.queued, "the degrade reports a queue");
            // The degraded turn starts the next run, which logs the
            // message itself; end it so its rows flush.
            fixture.wait_for_runs(2).await;
            fixture.end_run(1);
        }
    }
    fixture.wait_for_row_text(RACE_TEXT).await;
    fixture
        .controller
        .close()
        .await
        .expect("the session closes");

    let rows = fixture.durable_rows().await;
    let hits: Vec<&Value> = rows
        .iter()
        .filter(|row| row.to_string().contains(RACE_TEXT))
        .collect();
    assert_eq!(hits.len(), 1, "one message, one row: {rows:?}");
    match response.delivery {
        Delivery::Steer => {
            assert_eq!(hits[0]["type"], json!("user_message"));
            assert_eq!(hits[0]["source"], json!("steer"));
        }
        Delivery::Queue => assert_eq!(hits[0]["type"], json!("UserInput")),
    }
}

/// The cancel race: a steer and a cancel arrive together.
///
/// The gate makes both orders correct. A steer that wins the gate
/// appends its row while the run is still `Running`; the cancel then
/// stops the run, and the row stays in the log — the repair path
/// closes the tool call it interrupted, and the message becomes the
/// next turn's first user message. A cancel that wins flips the
/// state first, so the steer behind it reads the cancelled state and
/// takes the queued path. What must never happen is both: one
/// message, two rows.
#[tokio::test]
async fn a_steer_racing_the_cancel_is_delivered_exactly_once() {
    const RACE_TEXT: &str = "cancel race: keep going";

    let fixture = fixture().await;
    let _ = fixture.post("start", Delivery::Queue).await;
    fixture.wait_for_runs(1).await;
    fixture.wait_for_state(SessionState::Running).await;

    let state = Arc::clone(&fixture.state);
    let steer = tokio::spawn(async move {
        send_message(
            state,
            SESSION.to_string(),
            SendMessageRequest {
                text: RACE_TEXT.to_string(),
                delivery: Delivery::Steer,
                ..Default::default()
            },
        )
        .await
    });
    let controller = Arc::clone(&fixture.controller);
    let cancel = tokio::spawn(async move { controller.cancel().await });
    tokio::task::yield_now().await;

    let response = steer
        .await
        .expect("the steer task joins")
        .expect("the steer is accepted");
    cancel
        .await
        .expect("the cancel task joins")
        .expect("cancel is delivered");

    match response.delivery {
        Delivery::Steer => {
            assert!(!response.queued, "a steer is not a queued turn");
        }
        Delivery::Queue => {
            assert!(response.queued, "the degrade reports a queue");
            // The degraded turn starts the next run, which logs the
            // message itself; end it so its rows flush.
            fixture.wait_for_runs(2).await;
            fixture.end_run(1);
        }
    }
    fixture.wait_for_row_text(RACE_TEXT).await;
    fixture
        .controller
        .close()
        .await
        .expect("the session closes");

    let rows = fixture.durable_rows().await;
    let hits: Vec<&Value> = rows
        .iter()
        .filter(|row| row.to_string().contains(RACE_TEXT))
        .collect();
    assert_eq!(hits.len(), 1, "one message, one row: {rows:?}");
    match response.delivery {
        Delivery::Steer => {
            assert_eq!(hits[0]["type"], json!("user_message"));
            assert_eq!(hits[0]["source"], json!("steer"));
        }
        Delivery::Queue => assert_eq!(hits[0]["type"], json!("UserInput")),
    }
}

/// A steer carries text only: the log is its only carrier, so a
/// request that also carries attachment bytes is refused rather than
/// losing the image on the way.
///
/// The refusal happens BEFORE the attachment bytes are decoded, so a
/// request that cannot carry them never pays for them — and no row
/// lands in the log.
#[tokio::test]
async fn a_steer_with_an_attachment_is_refused() {
    let fixture = fixture().await;
    let _ = fixture.post("start", Delivery::Queue).await;
    fixture.wait_for_runs(1).await;
    fixture.wait_for_state(SessionState::Running).await;

    let error = send_message(
        Arc::clone(&fixture.state),
        SESSION.to_string(),
        SendMessageRequest {
            text: "look at this".to_string(),
            delivery: Delivery::Steer,
            attachments: vec![WireAttachment {
                kind: "image".to_string(),
                data_base64: Some("BASE64".to_string()),
                mime_type: Some("image/png".to_string()),
                ..Default::default()
            }],
            ..Default::default()
        },
    )
    .await
    .expect_err("a steer cannot carry an attachment");
    assert_eq!(error.code, "invalid_steer");

    fixture
        .controller
        .close()
        .await
        .expect("the session closes");
    let rows = fixture.durable_rows().await;
    assert!(
        rows.iter()
            .all(|row| !row.to_string().contains("look at this")),
        "a refused steer writes no row: {rows:?}"
    );
}
