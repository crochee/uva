//! The shape a round reports, from both paths.
//!
//! Two callers drive a child session's round: the `task` tool's
//! delegation and the control surface's wake. The round they share
//! owns what the parent is told, so both paths hand the model the
//! same thing — the answer, the child session's handle line, and the
//! same wording when the round ends without an answer.
//!
//! What the wake keeps to itself is its own: it does not open a
//! session, does not isolate, does not run a gate and does not file
//! the child again. Those are the delegation's decisions, and the
//! tests below hold the wake to them.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use serde_json::json;
use uva_agent::{
    Agent,
    AgentEntry,
    AgentInput,
    AgentRegistry,
    SubagentUsage,
    SubagentWriters,
};
use uva_core::{
    AtomicCancelToken,
    CancelToken,
    Clock,
    Registry,
    SharedClock,
    SharedIdGen,
    agent::AgentDescriptor,
};
use uva_events::{AgentEvent, SessionEndReason, SystemEvent};
use uva_provider::{ContentPart, TextContent};
use uva_session::{
    LogWriter,
    SessionLog,
    SessionSink,
    in_memory::InMemorySessionSink,
};
use uva_tool::{Context, Tool, ToolOutput};

use super::super::{
    ControlSurface,
    registry::ChildHandle,
    state::{ChildState, RoundOutcome},
};
use crate::tool::TaskTool;

/// The child session id the delegation path mints under the
/// sequence id generator.
const CHILD_ID: &str = "id-0";

/// A peer that answers with `text` and ends its round with `reason`.
struct EndingPeer {
    descriptor: AgentDescriptor,
    text: Option<String>,
    reason: SessionEndReason,
}

impl EndingPeer {
    /// A peer named `name` that answers `text` and ends with `reason`.
    fn new(name: &str, text: Option<&str>, reason: SessionEndReason) -> Self {
        let mut descriptor = uva_harness::default_descriptor(String::new());
        descriptor.name = name.to_string();
        Self {
            descriptor,
            text: text.map(str::to_string),
            reason,
        }
    }

    /// The peer as the registry stores it.
    fn shared(
        name: &str,
        text: Option<&str>,
        reason: SessionEndReason,
    ) -> Arc<dyn Agent> {
        Arc::new(Self::new(name, text, reason))
    }
}

#[async_trait::async_trait]
impl Agent for EndingPeer {
    fn descriptor(&self) -> &AgentDescriptor {
        &self.descriptor
    }

    async fn run(
        &self,
        _input: AgentInput,
        _cancel: Arc<dyn CancelToken>,
    ) -> std::pin::Pin<
        Box<dyn futures::Stream<Item = AgentEvent> + Send + 'static>,
    > {
        let mut events = Vec::new();
        if let Some(text) = self.text.clone() {
            events.push(AgentEvent::Model(ContentPart::Text(TextContent {
                text,
                cache_control: None,
            })));
        }
        events.push(AgentEvent::System(SystemEvent::Usage {
            input_tokens: 11,
            output_tokens: 5,
            cache_read_tokens: None,
            cache_creation_tokens: None,
        }));
        events.push(AgentEvent::System(SystemEvent::SessionEnded {
            reason: self.reason.clone(),
        }));
        Box::pin(futures::stream::iter(events))
    }
}

impl uva_core::registry::RegistryItem for EndingPeer {
    fn name(&self) -> &str {
        &self.descriptor.name
    }

    fn description(&self) -> &str {
        &self.descriptor.description
    }
}

/// A usage account that counts what the rounds reported.
#[derive(Default)]
struct CountingUsage {
    tokens: AtomicUsize,
    rounds: AtomicUsize,
}

impl SubagentUsage for CountingUsage {
    fn tokens(&self, _child_session_id: &str, _input: usize, _output: usize) {
        self.tokens.fetch_add(1, Ordering::SeqCst);
    }

    fn round_ended(&self, _child_session_id: &str) {
        self.rounds.fetch_add(1, Ordering::SeqCst);
    }
}

/// A writer source that opens every child session in one sink, so a
/// test can read the whole transcript.
struct TestWriters {
    sink: Arc<InMemorySessionSink>,
}

impl SubagentWriters for TestWriters {
    fn open_child(
        &self,
        _child_session_id: &str,
        _parent_session_id: &str,
        _depth: usize,
        _agent_name: &str,
        _prompt: Option<&str>,
    ) -> Arc<dyn SessionLog> {
        Arc::new(LogWriter::new(
            Arc::clone(&self.sink) as Arc<dyn SessionSink>,
            Arc::new(SharedClock::system()) as Arc<dyn Clock>,
        ))
    }
}

/// The peer catalog one test's peer lives in.
async fn peers_with(peer: Arc<dyn Agent>) -> Arc<AgentRegistry> {
    let peers = Arc::new(AgentRegistry::new());
    peers
        .put(AgentEntry::new(peer))
        .await
        .expect("register the peer");
    peers
}

/// The context of the parent session.
fn context() -> Context {
    Context::new("parent".to_string(), PathBuf::from("/tmp"))
}

/// The text of a tool output.
fn text_of(output: &ToolOutput) -> String {
    output
        .content
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// A tool output's shape: whether it is an error, and what it says.
fn shape(output: &ToolOutput) -> (bool, String) {
    (output.is_error.unwrap_or(false), text_of(output))
}

/// The types of a child's rows.
fn row_types(rows: &[serde_json::Value]) -> Vec<String> {
    rows.iter()
        .filter_map(|row| row.get("type").and_then(|t| t.as_str()))
        .map(str::to_string)
        .collect()
}

/// A surface with one peer and one idle child of it: the state a
/// message finds when it asks a child for another round.
async fn surface_with_idle_child(
    peer: Arc<dyn Agent>,
    writer: Option<Arc<dyn SessionLog>>,
    depth: usize,
) -> (Arc<ControlSurface>, Arc<AgentRegistry>) {
    let peers = peers_with(Arc::clone(&peer)).await;
    let surface = Arc::new(ControlSurface::new(Arc::clone(&peers)));
    let mut handle = ChildHandle::new(
        ChildState::spawn("child-1", "scout"),
        Arc::new(AtomicCancelToken::new()) as Arc<dyn CancelToken>,
    )
    .with_peer(peer)
    .with_depth(depth);
    if let Some(writer) = writer {
        handle = handle.with_writer(writer);
    }
    surface.registry().register("parent", handle);
    surface
        .registry()
        .settle("parent", "child-1", RoundOutcome::Completed);
    (surface, peers)
}

/// The two paths' reports of one child: the delegation path's result
/// and the wake path's result.
///
/// The wake asks the very child the delegation minted, so both
/// reports name the same session — the path is the only thing left
/// that could tell them apart.
async fn both_paths(
    peer: Arc<dyn Agent>,
    writers: Option<Arc<TestWriters>>,
    usage: Option<Arc<CountingUsage>>,
) -> (ToolOutput, ToolOutput) {
    let peers = peers_with(peer).await;
    let mut tool =
        TaskTool::new(Arc::clone(&peers)).with_id_gen(SharedIdGen::sequence());
    if let Some(writers) = writers {
        tool = tool.with_writers(writers);
    }
    if let Some(usage) = usage {
        tool = tool.with_usage(usage);
    }
    let ctx = context();
    let delegated = tool
        .call(json!({"agent": "scout", "prompt": "the prompt"}), &ctx)
        .await;
    let woken = tool
        .control()
        .message_tool()
        .call(json!({"id": CHILD_ID, "text": "the prompt"}), &ctx)
        .await;
    (delegated, woken)
}

/// The wake path's result names the child session the round ran in,
/// exactly as the delegation path's result does.
#[tokio::test]
async fn the_wake_path_reports_the_child_session_it_ran_in() {
    let peer = EndingPeer::shared(
        "scout",
        Some("the answer"),
        SessionEndReason::Completed,
    );
    let (surface, _peers) = surface_with_idle_child(peer, None, 1).await;
    let output = surface
        .message_tool()
        .call(
            json!({"id": "child-1", "text": "and now the tests"}),
            &context(),
        )
        .await;
    assert!(output.is_text(), "{:?}", output.is_error);
    assert_eq!(
        text_of(&output),
        "the answer\n\n[subagent session: child-1]",
        "the wake names the session its round ran in"
    );
}

/// A round that answers is reported the same by both paths.
#[tokio::test]
async fn a_finished_round_is_reported_the_same_by_both_paths() {
    let peer = EndingPeer::shared(
        "scout",
        Some("the answer"),
        SessionEndReason::Completed,
    );
    let (delegated, woken) = both_paths(peer, None, None).await;
    assert_eq!(
        shape(&delegated),
        shape(&woken),
        "both paths report a finished round the same way"
    );
    assert_eq!(
        shape(&woken).1,
        format!("the answer\n\n[subagent session: {CHILD_ID}]")
    );
}

/// A round that is cancelled is reported the same by both paths.
#[tokio::test]
async fn a_cancelled_round_is_reported_the_same_by_both_paths() {
    let peer = EndingPeer::shared("scout", None, SessionEndReason::Cancelled);
    let (delegated, woken) = both_paths(peer, None, None).await;
    assert_eq!(
        shape(&delegated),
        shape(&woken),
        "both paths report a cancelled round the same way"
    );
    assert_eq!(
        shape(&woken).1,
        format!(
            "sub-agent `scout` ended before completing: cancelled\n\n\
             [subagent session: {CHILD_ID}]"
        )
    );
}

/// A round that fails is reported the same by both paths.
#[tokio::test]
async fn a_failed_round_is_reported_the_same_by_both_paths() {
    let peer = EndingPeer::shared(
        "scout",
        None,
        SessionEndReason::Error("the peer gave up".to_string()),
    );
    let (delegated, woken) = both_paths(peer, None, None).await;
    assert_eq!(
        shape(&delegated),
        shape(&woken),
        "both paths report a failed round the same way"
    );
    assert_eq!(
        shape(&woken).1,
        format!(
            "sub-agent `scout` ended before completing: error \
             (the peer gave up)\n\n\
             [subagent session: {CHILD_ID}]"
        )
    );
}

/// Each path counts its own round once: the usage account and the
/// child's session hold one round each, never two.
///
/// The counts are read **between** the two paths: a total of two
/// would also come from one path counting twice and the other not at
/// all.
#[tokio::test]
async fn each_path_counts_its_own_round_once() {
    let sink = Arc::new(InMemorySessionSink::new("child"));
    let usage = Arc::new(CountingUsage::default());
    let peer = EndingPeer::shared(
        "scout",
        Some("the answer"),
        SessionEndReason::Completed,
    );
    let peers = peers_with(peer).await;
    let tool = TaskTool::new(Arc::clone(&peers))
        .with_id_gen(SharedIdGen::sequence())
        .with_writers(Arc::new(TestWriters {
            sink: Arc::clone(&sink),
        }))
        .with_usage(Arc::clone(&usage) as Arc<CountingUsage>);
    let ctx = context();

    // The delegation counts its own round, once.
    let delegated = tool
        .call(json!({"agent": "scout", "prompt": "the prompt"}), &ctx)
        .await;
    assert!(!shape(&delegated).0, "the delegation answered");
    assert_eq!(
        usage.rounds.load(Ordering::SeqCst),
        1,
        "the delegation counted its round once"
    );
    assert_eq!(
        usage.tokens.load(Ordering::SeqCst),
        1,
        "the delegation reported its tokens once"
    );
    let types = row_types(&sink.read().await.expect("the child's rows"));
    assert_eq!(
        types.iter().filter(|kind| *kind == "subagent_exit").count(),
        1,
        "the delegation landed one footer: {types:?}"
    );

    // The wake counts its own round, once — on top of the
    // delegation's, not instead of it.
    let woken = tool
        .control()
        .message_tool()
        .call(json!({"id": CHILD_ID, "text": "the prompt"}), &ctx)
        .await;
    assert!(!shape(&woken).0, "the wake answered");
    assert_eq!(
        usage.rounds.load(Ordering::SeqCst),
        2,
        "the wake counted its own round once, and only once"
    );
    assert_eq!(
        usage.tokens.load(Ordering::SeqCst),
        2,
        "the wake reported its tokens once"
    );
    let types = row_types(&sink.read().await.expect("the child's rows"));
    assert_eq!(
        types.iter().filter(|kind| *kind == "subagent_exit").count(),
        2,
        "one exit footer per round: {types:?}"
    );
}

/// The wake keeps the semantics that are its own: it does not mint a
/// child, does not open a session, does not isolate, does not run a
/// gate and does not file the child again.
#[tokio::test]
async fn the_wake_path_keeps_the_semantics_that_are_its_own() {
    let sink = Arc::new(InMemorySessionSink::new("child"));
    let writer: Arc<dyn SessionLog> = Arc::new(LogWriter::new(
        Arc::clone(&sink) as Arc<dyn SessionSink>,
        Arc::new(SharedClock::system()) as Arc<dyn Clock>,
    ));
    let peer = EndingPeer::shared(
        "scout",
        Some("the answer"),
        SessionEndReason::Completed,
    );
    let (surface, _peers) =
        surface_with_idle_child(peer, Some(Arc::clone(&writer)), 2).await;

    let output = surface
        .message_tool()
        .call(
            json!({"id": "child-1", "text": "and now the tests"}),
            &context(),
        )
        .await;
    assert!(output.is_text(), "{:?}", output.is_error);

    // The child is the one that was already there: no second row, no
    // new id, the same name.
    let rows = surface.registry().list("parent");
    assert_eq!(rows.len(), 1, "the wake files no second child: {rows:?}");
    assert_eq!(rows[0].id, "child-1");
    assert_eq!(rows[0].name, "scout");

    // The session was opened long ago: the wake appends to it and
    // never opens it again.
    let child_rows = sink.read().await.expect("the child's rows");
    let types = row_types(&child_rows);
    assert_eq!(
        types.first().map(String::as_str),
        Some("user_message"),
        "the wake writes the turn itself: {types:?}"
    );
    assert!(
        !types.iter().any(|kind| kind == "subagent_enter"),
        "the wake opens no session: {types:?}"
    );
    assert_eq!(
        types.last().map(String::as_str),
        Some("subagent_exit"),
        "{types:?}"
    );

    // The round ran at the depth the handle remembered: a child
    // filed again would start over at the default depth.
    let footer = child_rows
        .iter()
        .find(|row| row["type"] == "subagent_exit")
        .expect("the round closes the child session");
    assert_eq!(footer["data"]["depth"], 2);

    // The wake is not isolated and has no gate: a worktree line and
    // a gate are the delegation's decisions.
    assert!(
        !text_of(&output).contains("[worktree:"),
        "the wake runs no worktree"
    );
    let wake_schema = surface.message_tool().parameters();
    assert!(
        wake_schema["properties"].get("gate").is_none(),
        "the wake has no gate to run: {wake_schema}"
    );
    let delegation_peers = peers_with(EndingPeer::shared(
        "scout",
        None,
        SessionEndReason::Completed,
    ))
    .await;
    let delegation_schema = TaskTool::new(delegation_peers).parameters();
    assert!(
        delegation_schema["properties"].get("gate").is_some(),
        "the gate belongs to the delegation: {delegation_schema}"
    );
}
