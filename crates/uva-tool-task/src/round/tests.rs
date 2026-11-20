//! The round module's own tests: what one round of a child session
//! reports, and what it leaves behind.
//!
//! The seam is the module's interface — a round's input in, one
//! result value out — so a round's end reason can be read without
//! driving a whole parent run.

use std::{
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Instant,
};

use uva_agent::{Agent, AgentInput, AgentRegistry, ChannelInbox};
use uva_core::{AtomicCancelToken, CancelToken, agent::AgentDescriptor};
use uva_events::{AgentEvent, SessionEndReason, SystemEvent};
use uva_provider::{ContentPart, TextContent};
use uva_session::{
    LogWriter,
    SessionLog,
    SessionSink,
    in_memory::InMemorySessionSink,
};

use super::{ChildRound, RoundInput};
use crate::{
    control::{ChildHandle, ChildState, ControlSurface, RoundOutcome},
    progress::L1Feed,
};

/// How a test's peer ends its round.
enum Ending {
    /// The peer answers and completes.
    Completed(String),
    /// The peer waits for the cancellation and then ends.
    Cancelled,
    /// The peer ends with an error.
    Failed(String),
    /// The peer stops because it ran out of iterations.
    MaxIterations,
}

/// A peer that ends its round the way the test asks for.
struct TestPeer {
    descriptor: AgentDescriptor,
    ending: Ending,
}

impl TestPeer {
    /// A peer named `name` that ends the way `ending` says.
    fn new(name: &str, ending: Ending) -> Self {
        let mut descriptor = uva_harness::default_descriptor(String::new());
        descriptor.name = name.to_string();
        Self { descriptor, ending }
    }
}

#[async_trait::async_trait]
impl Agent for TestPeer {
    fn descriptor(&self) -> &AgentDescriptor {
        &self.descriptor
    }

    async fn run(
        &self,
        _input: AgentInput,
        cancel: Arc<dyn CancelToken>,
    ) -> std::pin::Pin<
        Box<dyn futures::Stream<Item = AgentEvent> + Send + 'static>,
    > {
        let events = match &self.ending {
            Ending::Completed(text) => vec![
                AgentEvent::Model(ContentPart::Text(TextContent {
                    text: text.clone(),
                    cache_control: None,
                })),
                AgentEvent::System(SystemEvent::Usage {
                    input_tokens: 11,
                    output_tokens: 5,
                    cache_read_tokens: None,
                    cache_creation_tokens: None,
                }),
                AgentEvent::System(SystemEvent::SessionEnded {
                    reason: SessionEndReason::Completed,
                }),
            ],
            Ending::Cancelled => {
                cancel.cancelled().await;
                vec![AgentEvent::System(SystemEvent::SessionEnded {
                    reason: SessionEndReason::Cancelled,
                })]
            }
            Ending::Failed(reason) => {
                vec![AgentEvent::System(SystemEvent::SessionEnded {
                    reason: SessionEndReason::Error(reason.clone()),
                })]
            }
            Ending::MaxIterations => {
                vec![AgentEvent::System(SystemEvent::SessionEnded {
                    reason: SessionEndReason::MaxIterations,
                })]
            }
        };
        Box::pin(futures::stream::iter(events))
    }
}

impl uva_core::registry::RegistryItem for TestPeer {
    fn name(&self) -> &str {
        &self.descriptor.name
    }

    fn description(&self) -> &str {
        &self.descriptor.description
    }
}

/// A usage account that counts what the round reported.
#[derive(Default)]
struct CountingUsage {
    tokens: AtomicUsize,
    rounds: AtomicUsize,
}

impl uva_agent::SubagentUsage for CountingUsage {
    fn tokens(&self, _child_session_id: &str, _input: usize, _output: usize) {
        self.tokens.fetch_add(1, Ordering::SeqCst);
    }

    fn round_ended(&self, _child_session_id: &str) {
        self.rounds.fetch_add(1, Ordering::SeqCst);
    }
}

/// The surface the round settles its roster row in.
fn surface() -> Arc<ControlSurface> {
    Arc::new(ControlSurface::new(Arc::new(AgentRegistry::new())))
}

/// A round's input, with the pieces a test cares about left out.
fn input(peer: Arc<dyn Agent>) -> RoundInput {
    let (inbox, _steer) = ChannelInbox::channel();
    RoundInput {
        peer,
        prompt: "the prompt".to_string(),
        parent_id: "parent".to_string(),
        child_id: "child-1".to_string(),
        agent: "scout".to_string(),
        depth: 1,
        writer: None,
        inbox,
        cancel: Arc::new(AtomicCancelToken::new()),
        isolation: None,
        feed: None,
    }
}

/// The three places a round's ending is written down: the roster
/// row's activity, the progress line's activity, and the exit
/// footer's status.
struct Projections {
    /// The roster row's activity line.
    roster: String,
    /// The L1 progress line's activity.
    progress: String,
    /// The exit footer's status.
    footer: String,
}

/// Run one round and read the three words its ending was written in.
async fn projections(ending: Ending) -> Projections {
    let surface = surface();
    let sink = Arc::new(InMemorySessionSink::new("child"));
    let writer: Arc<dyn SessionLog> = Arc::new(LogWriter::new(
        Arc::clone(&sink) as Arc<dyn SessionSink>,
        Arc::new(uva_core::SystemClock),
    ));
    let feed = Arc::new(L1Feed::default());
    let cancelled = matches!(ending, Ending::Cancelled);
    let mut round_input = input(Arc::new(TestPeer::new("scout", ending)));
    round_input.writer = Some(Arc::clone(&writer));
    round_input.feed = Some(Arc::clone(&feed));
    // The cancelled peer waits for the token; the other two never look
    // at it.
    let cancel = Arc::new(AtomicCancelToken::new());
    if cancelled {
        cancel.cancel();
    }
    round_input.cancel = Arc::clone(&cancel) as Arc<dyn CancelToken>;
    surface.registry().register(
        "parent",
        ChildHandle::new(
            ChildState::spawn("child-1", "scout"),
            Arc::new(AtomicCancelToken::new()) as Arc<dyn CancelToken>,
        ),
    );

    ChildRound::new(Arc::clone(&surface)).run(round_input).await;

    let rows = sink.read().await.expect("the child's rows");
    let footer = rows
        .iter()
        .find(|row| row["type"] == "subagent_exit")
        .expect("the round closes the child session");
    let roster = surface.registry().list("parent");
    let progress = feed.flush(Instant::now()).expect("the round's last line");
    Projections {
        roster: roster[0].activity.clone(),
        progress: progress.metadata["children"][0]["activity"]
            .as_str()
            .expect("the progress line carries its activity")
            .to_string(),
        footer: footer["data"]["status"]
            .as_str()
            .expect("the exit footer carries its status")
            .to_string(),
    }
}

/// The ways a round can end, with the name the test reports them
/// under.
fn endings() -> [(Ending, &'static str); 3] {
    [
        (Ending::Completed("the answer".to_string()), "Completed"),
        (Ending::Cancelled, "Cancelled"),
        (Ending::Failed("the peer gave up".to_string()), "Failed"),
    ]
}

#[tokio::test]
async fn a_round_that_answers_reports_its_text() {
    let surface = surface();
    let peer = Arc::new(TestPeer::new(
        "scout",
        Ending::Completed("the answer".to_string()),
    ));
    let result = ChildRound::new(Arc::clone(&surface)).run(input(peer)).await;
    assert_eq!(result.text.as_deref(), Some("the answer"));
    assert_eq!(result.outcome, RoundOutcome::Completed);
    assert_eq!(result.end_reason, SessionEndReason::Completed);
    assert_eq!(result.child_id, "child-1");
}

#[tokio::test]
async fn a_cancelled_round_reports_its_cancellation() {
    let surface = surface();
    let peer = Arc::new(TestPeer::new("scout", Ending::Cancelled));
    let mut round_input = input(peer);
    let cancel = Arc::new(AtomicCancelToken::new());
    round_input.cancel = Arc::clone(&cancel) as Arc<dyn CancelToken>;
    let round = ChildRound::new(Arc::clone(&surface));
    let running = tokio::spawn(async move { round.run(round_input).await });
    cancel.cancel();
    let result = running.await.expect("the round ends");
    assert_eq!(result.text, None, "a cancelled round answers nothing");
    assert_eq!(result.outcome, RoundOutcome::Cancelled);
    assert_eq!(result.end_reason, SessionEndReason::Cancelled);
}

#[tokio::test]
async fn a_failed_round_reports_its_failure() {
    let surface = surface();
    let peer = Arc::new(TestPeer::new(
        "scout",
        Ending::Failed("the peer gave up".to_string()),
    ));
    let result = ChildRound::new(Arc::clone(&surface)).run(input(peer)).await;
    assert_eq!(result.text, None, "a failed round answers nothing");
    assert_eq!(result.outcome, RoundOutcome::Failed);
    assert_eq!(
        result.end_reason,
        SessionEndReason::Error("the peer gave up".to_string())
    );
}

#[tokio::test]
async fn the_round_closes_the_child_session_and_settles_its_row() {
    let surface = surface();
    let sink = Arc::new(InMemorySessionSink::new("child"));
    let writer: Arc<dyn SessionLog> = Arc::new(LogWriter::new(
        Arc::clone(&sink) as Arc<dyn SessionSink>,
        Arc::new(uva_core::SystemClock),
    ));
    let peer = Arc::new(TestPeer::new(
        "scout",
        Ending::Completed("the answer".to_string()),
    ));
    let mut round_input = input(peer);
    round_input.writer = Some(Arc::clone(&writer));
    surface.registry().register(
        "parent",
        ChildHandle::new(
            ChildState::spawn("child-1", "scout"),
            Arc::new(AtomicCancelToken::new()) as Arc<dyn CancelToken>,
        )
        .with_peer(Arc::new(TestPeer::new(
            "scout",
            Ending::Completed("the answer".to_string()),
        ))),
    );

    ChildRound::new(Arc::clone(&surface)).run(round_input).await;

    let rows = sink.read().await.expect("the child's rows");
    let footer = rows
        .iter()
        .find(|row| row["type"] == "subagent_exit")
        .expect("the round closes the child session");
    assert_eq!(footer["data"]["status"], "completed");
    assert_eq!(footer["data"]["depth"], 1);

    let roster = surface.registry().list("parent");
    assert_eq!(roster.len(), 1);
    assert_eq!(
        roster[0].activity, "completed",
        "the round settles its roster row: {roster:?}"
    );
}

#[tokio::test]
async fn the_round_reports_its_own_consumption() {
    let surface = surface();
    let usage = Arc::new(CountingUsage::default());
    surface.set_usage(Arc::clone(&usage) as Arc<dyn uva_agent::SubagentUsage>);
    let peer = Arc::new(TestPeer::new(
        "scout",
        Ending::Completed("the answer".to_string()),
    ));
    ChildRound::new(Arc::clone(&surface)).run(input(peer)).await;
    assert_eq!(usage.tokens.load(Ordering::SeqCst), 1);
    assert_eq!(usage.rounds.load(Ordering::SeqCst), 1);
}

/// The roster's row and the child's exit footer are two words for
/// the same round: what the model reads about a child and what the
/// child's own session says about its round are one thing.
#[tokio::test]
async fn the_roster_row_and_the_exit_footer_say_the_same_thing() {
    for (ending, outcome) in endings() {
        let words = projections(ending).await;
        assert_eq!(
            words.roster, words.footer,
            "the roster row and the exit footer say the same thing about \
             a {outcome} round"
        );
    }
}

/// The progress line the parent's tool block carries is the third
/// place a round's ending is written down: it says what the roster
/// says.
#[tokio::test]
async fn the_progress_line_says_what_the_roster_row_says() {
    for (ending, outcome) in endings() {
        let words = projections(ending).await;
        assert_eq!(
            words.progress, words.roster,
            "the progress line and the roster row say the same thing \
             about a {outcome} round"
        );
    }
}

/// The model-visible failure text carries the **detail** the end
/// reason came with: "the peer gave up" is the only clue the model
/// has about why the round failed.
#[tokio::test]
async fn the_failure_text_carries_the_failure_detail() {
    let surface = surface();
    let peer = Arc::new(TestPeer::new(
        "scout",
        Ending::Failed("the peer gave up".to_string()),
    ));
    let result = ChildRound::new(surface).run(input(peer)).await;
    let text = result.failure();
    assert!(
        text.contains("the peer gave up"),
        "the model reads why the round failed: {text}"
    );
}

/// The model-visible failure text names the **reason** the round
/// ended, not the outcome the roster reports: the reason is the finer
/// axis, and it keeps its precision.
#[tokio::test]
async fn the_failure_text_names_the_reason_not_the_outcome() {
    let surface = surface();
    let peer = Arc::new(TestPeer::new("scout", Ending::MaxIterations));
    let result = ChildRound::new(surface).run(input(peer)).await;
    assert_eq!(
        result.outcome,
        RoundOutcome::Failed,
        "the round ended badly, whatever the reason was"
    );
    let text = result.failure();
    assert!(
        text.contains("max_iterations"),
        "the model reads the reason the round ended: {text}"
    );
    assert!(
        !text.contains("error"),
        "the outcome's word does not replace the reason: {text}"
    );
}
