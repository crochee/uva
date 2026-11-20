//! The depth rule's tests: what the module says at each depth, and
//! that both paths ask it.
//!
//! The seam is the rule's interface — a session id in, a verdict out
//! — and the two callers that ask it: the `task` tool's delegation
//! and the message tool's wake.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use uva_agent::{Agent, AgentEntry, AgentInput, AgentRegistry, ChannelInbox};
use uva_core::{
    AtomicCancelToken,
    CancelToken,
    Registry,
    agent::AgentDescriptor,
};
use uva_events::AgentEvent;
use uva_provider::{ContentPart, TextContent};
use uva_tool::{Context, Tool, ToolOutput};

use super::DepthLedger;
use crate::{
    control::{ChildHandle, ChildState, RoundOutcome},
    spec::MAX_SUBAGENT_DEPTH,
    tool::TaskTool,
};

/// A peer that answers with a fixed text and counts the rounds it ran.
struct EchoPeer {
    descriptor: AgentDescriptor,
    runs: Arc<AtomicUsize>,
}

impl EchoPeer {
    /// A peer named `name`.
    fn new(name: &str, runs: Arc<AtomicUsize>) -> Self {
        let mut descriptor = uva_harness::default_descriptor(String::new());
        descriptor.name = name.to_string();
        Self { descriptor, runs }
    }
}

#[async_trait::async_trait]
impl Agent for EchoPeer {
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
        self.runs.fetch_add(1, Ordering::SeqCst);
        Box::pin(futures::stream::once(async move {
            AgentEvent::Model(ContentPart::Text(TextContent {
                text: "the answer".to_string(),
                cache_control: None,
            }))
        }))
    }
}

impl uva_core::registry::RegistryItem for EchoPeer {
    fn name(&self) -> &str {
        &self.descriptor.name
    }

    fn description(&self) -> &str {
        &self.descriptor.description
    }
}

/// The tool the depth rule is asked through, and the peer it runs.
async fn tool() -> (TaskTool, Arc<dyn Agent>, Arc<AtomicUsize>) {
    let peers = Arc::new(AgentRegistry::new());
    let runs = Arc::new(AtomicUsize::new(0));
    let peer: Arc<dyn Agent> =
        Arc::new(EchoPeer::new("scout", Arc::clone(&runs)));
    peers
        .put(AgentEntry::new(Arc::clone(&peer)))
        .await
        .expect("register the peer");
    (TaskTool::new(Arc::clone(&peers)), peer, runs)
}

/// The context of a session.
fn context(session: &str) -> Context {
    Context::new(session.to_string(), std::path::PathBuf::from("/tmp"))
}

/// The text a tool output carries.
fn text_of(output: &ToolOutput) -> String {
    output
        .content
        .iter()
        .filter_map(|part| part.text())
        .collect::<String>()
}

/// Whether the output is the cap's refusal.
fn refuses_the_cap(output: &ToolOutput) -> bool {
    text_of(output).contains("maximum sub-agent depth")
}

/// The child session the result names.
fn child_session(output: &ToolOutput) -> String {
    text_of(output)
        .rsplit_once("[subagent session: ")
        .and_then(|(_, tail)| tail.split(']').next())
        .expect("the result names the child session")
        .to_string()
}

/// A session that runs at `depth`: the root, or a child the tool
/// dispatched `depth` times over.
async fn session_at_depth(tool: &TaskTool, depth: usize) -> String {
    let mut session = "root".to_string();
    for _ in 0..depth {
        let output = tool
            .call(
                serde_json::json!({"agent": "scout", "prompt": "go deeper"}),
                &context(&session),
            )
            .await;
        assert!(
            !output.is_error.unwrap_or(false),
            "the dispatch below the cap runs: {output:?}"
        );
        session = child_session(&output);
    }
    session
}

/// A child of `parent`, idle and ready for another round.
fn idle_child(
    tool: &TaskTool,
    parent: &str,
    peer: &Arc<dyn Agent>,
    depth: usize,
) {
    let cancel = Arc::new(AtomicCancelToken::new());
    tool.control().registry().register(
        parent,
        ChildHandle::new(
            ChildState::spawn("child-1", "scout"),
            Arc::clone(&cancel) as Arc<dyn CancelToken>,
        )
        .with_peer(Arc::clone(peer))
        .with_depth(depth),
    );
    tool.control().registry().settle(
        parent,
        "child-1",
        RoundOutcome::Completed,
    );
}

/// A child of `parent`, running and ready to be steered.
///
/// The inbox is handed back: a steer only reaches the running round
/// while the round's side of the channel is alive.
fn running_child(
    tool: &TaskTool,
    parent: &str,
    peer: &Arc<dyn Agent>,
    depth: usize,
) -> Arc<ChannelInbox> {
    let cancel = Arc::new(AtomicCancelToken::new());
    let (inbox, steer) = ChannelInbox::channel();
    tool.control().registry().register(
        parent,
        ChildHandle::new(
            ChildState::spawn("child-1", "scout"),
            Arc::clone(&cancel) as Arc<dyn CancelToken>,
        )
        .with_steer(steer)
        .with_peer(Arc::clone(peer))
        .with_depth(depth),
    );
    inbox
}

/// A session at the root may start a round; one at the cap may not.
#[test]
fn the_cap_refuses_the_session_that_sits_at_it() {
    let ledger = DepthLedger::new();
    assert!(
        super::admit_round(&ledger, "root").is_ok(),
        "the root sits below the cap"
    );

    super::record_child(&ledger, "at-cap", MAX_SUBAGENT_DEPTH);
    let refusal = super::admit_round(&ledger, "at-cap")
        .expect_err("the cap refuses the session that sits at it");
    assert_eq!(refusal.depth(), MAX_SUBAGENT_DEPTH);
    assert!(
        refusal.delegation().contains("maximum sub-agent depth"),
        "the refusal names the reason: {}",
        refusal.delegation()
    );
}

/// A child runs one level below its parent — the counting happens in
/// the module, so no caller counts layers itself.
#[test]
fn a_child_runs_one_level_below_its_parent() {
    let ledger = DepthLedger::new();
    assert_eq!(
        super::admit_child(&ledger, "root").expect("the root may delegate"),
        1
    );

    super::record_child(&ledger, "child", 1);
    assert_eq!(
        super::admit_child(&ledger, "child")
            .expect("a first-level child may delegate"),
        2
    );
}

/// A steer starts no round, so the depth rule does not refuse it: the
/// rule is about the rounds a session may start, and a steer runs
/// none.
#[tokio::test]
async fn a_steer_is_not_refused_by_the_depth_rule() {
    let (tool, peer, _runs) = tool().await;
    // The parent sits at the cap...
    super::record_child(
        &tool.control().depth_ledger(),
        "at-cap",
        MAX_SUBAGENT_DEPTH,
    );
    // ...and holds a running child to steer.
    let _inbox = running_child(&tool, "at-cap", &peer, MAX_SUBAGENT_DEPTH + 1);

    let steered = tool
        .control()
        .message_tool()
        .call(
            serde_json::json!({"id": "child-1", "text": "focus on the tests"}),
            &context("at-cap"),
        )
        .await;
    assert!(
        !steered.is_error.unwrap_or(false),
        "a steer starts no round, so the depth rule does not refuse it: \
         {steered:?}"
    );
}

/// A killed child's depth entry goes with its handles: a terminal
/// child will never run another round, so nothing reads its depth
/// again.
#[tokio::test]
async fn a_killed_childs_depth_entry_is_forgotten() {
    let (tool, peer, _runs) = tool().await;
    let parent = "parent".to_string();
    let _inbox = running_child(&tool, &parent, &peer, 2);
    let ledger = tool.control().depth_ledger();
    super::record_child(&ledger, "child-1", 2);
    assert_eq!(
        super::depth_of(&ledger, "child-1"),
        2,
        "the child's depth is recorded"
    );

    tool.control()
        .kill(&parent, "child-1")
        .expect("the child is killed");

    assert_eq!(
        super::depth_of(&ledger, "child-1"),
        0,
        "a terminal child's depth entry is gone"
    );
}

/// The cap is one rule: the session that starts a round may not sit
/// at the cap. The `task` tool and the message tool ask this module
/// about the same session, so at the same depth they refuse the same
/// rounds.
#[tokio::test]
async fn both_paths_judge_the_same_depth_the_same_way() {
    let (tool, peer, runs) = tool().await;

    for depth in 0..MAX_SUBAGENT_DEPTH {
        let session = session_at_depth(&tool, depth).await;
        idle_child(&tool, &session, &peer, depth + 1);

        // Below the cap both paths run the round.
        let delegated = tool
            .call(
                serde_json::json!({"agent": "scout", "prompt": "go deeper"}),
                &context(&session),
            )
            .await;
        assert!(
            !delegated.is_error.unwrap_or(false),
            "a session at depth {depth} may delegate: {delegated:?}"
        );
        let woken = tool
            .control()
            .message_tool()
            .call(
                serde_json::json!({"id": "child-1", "text": "and now this"}),
                &context(&session),
            )
            .await;
        assert!(
            !woken.is_error.unwrap_or(false),
            "a session at depth {depth} may start another round: {woken:?}"
        );
    }

    // At the cap both paths refuse — and neither starts a round.
    let at_cap = session_at_depth(&tool, MAX_SUBAGENT_DEPTH).await;
    idle_child(&tool, &at_cap, &peer, MAX_SUBAGENT_DEPTH);
    let runs_before = runs.load(Ordering::SeqCst);

    let delegated = tool
        .call(
            serde_json::json!({"agent": "scout", "prompt": "too deep"}),
            &context(&at_cap),
        )
        .await;
    assert!(
        refuses_the_cap(&delegated),
        "the delegation path refuses at the cap: {delegated:?}"
    );

    let woken = tool
        .control()
        .message_tool()
        .call(
            serde_json::json!({"id": "child-1", "text": "and now this"}),
            &context(&at_cap),
        )
        .await;
    assert!(
        refuses_the_cap(&woken),
        "the wake path refuses at the cap: {woken:?}"
    );

    assert_eq!(
        runs.load(Ordering::SeqCst),
        runs_before,
        "a refused round never runs the peer"
    );

    // The refusal leaves the roster alone: the child is still idle.
    let roster = tool.control().registry().list(&at_cap);
    assert_eq!(
        roster[0].status,
        crate::control::ChildStatus::Idle,
        "a refused message leaves the child where it was: {roster:?}"
    );
}

/// The wake path's accounting reaches the delegation path: the child
/// the wake ran at depth 2 hands a nested `task` call a child at
/// depth 3, and the call after that is refused at the cap.
#[tokio::test]
async fn the_wake_path_records_the_depth_the_child_runs_at() {
    let (tool, peer, _runs) = tool().await;
    // A child of the root, at depth 1.
    let parent = session_at_depth(&tool, 1).await;
    // Its own child, at depth 2, idle and ready for another round.
    idle_child(&tool, &parent, &peer, 2);

    let woken = tool
        .control()
        .message_tool()
        .call(
            serde_json::json!({"id": "child-1", "text": "and now this"}),
            &context(&parent),
        )
        .await;
    assert!(
        !woken.is_error.unwrap_or(false),
        "the wake runs the child's round: {woken:?}"
    );

    // The nested delegation from that child runs one level below it.
    let nested = tool
        .call(
            serde_json::json!({"agent": "scout", "prompt": "go deeper"}),
            &context("child-1"),
        )
        .await;
    assert!(
        !nested.is_error.unwrap_or(false),
        "the nested delegation runs below the cap: {nested:?}"
    );

    // The call after that would sit at the cap: it is refused.
    let deeper = tool
        .call(
            serde_json::json!({"agent": "scout", "prompt": "too deep"}),
            &context(&child_session(&nested)),
        )
        .await;
    assert!(
        refuses_the_cap(&deeper),
        "the nested delegation ran at the recorded depth: {deeper:?}"
    );
}
