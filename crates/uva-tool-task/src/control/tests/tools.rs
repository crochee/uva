//! The three control tools, driven the way the model drives them:
//! through the tool registry, with a real child session behind them.

use std::sync::Arc;

use serde_json::{Value, json};
use uva_agent::{AgentEntry, AgentRegistry, ChannelInbox, RunInbox};
use uva_core::{AtomicCancelToken, CancelToken, Clock, Registry, SharedClock};
use uva_session::{
    LogWriter,
    SessionLog,
    in_memory::InMemorySessionSink,
    subagent_enter,
    user_message,
};
use uva_tool::{Context, Tool, ToolOutput};

use super::{
    super::{
        ControlSurface,
        registry::ChildHandle,
        state::{ChildState, RoundOutcome},
    },
    support::EchoAgent,
};

/// A child session the tests can read.
fn child_log(id: &str) -> Arc<LogWriter> {
    Arc::new(LogWriter::new(
        Arc::new(InMemorySessionSink::new(id)),
        Arc::new(SharedClock::system()) as Arc<dyn Clock>,
    ))
}

/// The rows of a child's log.
async fn rows(log: &Arc<LogWriter>) -> Vec<Value> {
    log.read().await.expect("the child's rows")
}

/// The types of a child's rows.
async fn row_types(log: &Arc<LogWriter>) -> Vec<String> {
    rows(log)
        .await
        .iter()
        .filter_map(|row| row.get("type").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// A surface with one peer named `scout` and no session index.
async fn surface() -> (Arc<ControlSurface>, Arc<AgentRegistry>) {
    let peers = Arc::new(AgentRegistry::new());
    peers
        .put(AgentEntry::new(EchoAgent::shared("scout", "the answer")))
        .await
        .expect("register the peer");
    (Arc::new(ControlSurface::new(Arc::clone(&peers))), peers)
}

/// The context of a session.
fn context(session: &str) -> Context {
    Context::new(session.to_string(), std::path::PathBuf::from("/tmp"))
}

/// The text of a tool output.
fn text_of(output: &ToolOutput) -> String {
    output
        .content
        .iter()
        .filter_map(|part| match part {
            uva_provider::ContentPart::Text(text) => Some(text.text.clone()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("")
}

/// The roster of the surface, as the list tool reports it.
async fn roster(surface: &Arc<ControlSurface>, session: &str) -> Vec<Value> {
    let output = surface.list_tool().call(json!({}), &context(session)).await;
    let parsed: Value =
        serde_json::from_str(&text_of(&output)).expect("the roster is json");
    parsed["children"]
        .as_array()
        .cloned()
        .expect("the roster carries children")
}

#[tokio::test]
async fn the_roster_lists_the_children_of_the_session() {
    let (surface, _peers) = surface().await;
    let log = child_log("child-1");
    let (inbox, steer) = ChannelInbox::channel();
    let cancel = Arc::new(AtomicCancelToken::new());
    surface.registry().register(
        "parent",
        ChildHandle::new(
            ChildState::spawn("child-1", "scout"),
            Arc::clone(&cancel) as Arc<dyn CancelToken>,
        )
        .with_steer(steer)
        .with_writer(Arc::clone(&log) as Arc<dyn SessionLog>)
        .with_peer(EchoAgent::shared("scout", "the answer")),
    );
    let _inbox = inbox;

    let rows = roster(&surface, "parent").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "child-1");
    assert_eq!(rows[0]["name"], "scout");
    assert_eq!(rows[0]["status"], "running");
    assert_eq!(rows[0]["activity"], "starting");
}

#[tokio::test]
async fn the_roster_is_empty_for_a_session_without_children() {
    let (surface, _peers) = surface().await;
    assert!(roster(&surface, "parent").await.is_empty());
}

#[tokio::test]
async fn a_message_to_a_running_child_steers_it_and_leaves_a_row() {
    let (surface, _peers) = surface().await;
    let log = child_log("child-1");
    let (inbox, steer) = ChannelInbox::channel();
    let cancel = Arc::new(AtomicCancelToken::new());
    surface.registry().register(
        "parent",
        ChildHandle::new(
            ChildState::spawn("child-1", "scout"),
            Arc::clone(&cancel) as Arc<dyn CancelToken>,
        )
        .with_steer(steer)
        .with_writer(Arc::clone(&log) as Arc<dyn SessionLog>)
        .with_peer(EchoAgent::shared("scout", "the answer")),
    );

    let output = surface
        .message_tool()
        .call(
            json!({"id": "child-1", "text": "focus on the tests"}),
            &context("parent"),
        )
        .await;
    assert!(output.is_text(), "{:?}", output.is_error);

    // The steer row is the fact: it is in the child's own log.
    let types = row_types(&log).await;
    assert!(types.contains(&"user_message".to_string()), "{types:?}");
    let user_rows = rows(&log)
        .await
        .into_iter()
        .filter(|row| row["type"] == "user_message")
        .collect::<Vec<_>>();
    assert_eq!(user_rows.len(), 1);
    assert_eq!(
        user_rows[0]["source"], "steer",
        "the row says where the turn came from"
    );

    // The channel the running round drains carries the same message.
    let drained = inbox.drain().await;
    assert_eq!(drained.len(), 1, "the running round sees the steer");
    assert_eq!(
        drained[0].content.extract_text().as_deref(),
        Some("focus on the tests")
    );
}

#[tokio::test]
async fn a_message_to_an_idle_child_runs_another_round() {
    let (surface, _peers) = surface().await;
    let log = child_log("child-1");
    let (inbox, steer) = ChannelInbox::channel();
    let cancel = Arc::new(AtomicCancelToken::new());
    surface.registry().register(
        "parent",
        ChildHandle::new(
            ChildState::spawn("child-1", "scout"),
            Arc::clone(&cancel) as Arc<dyn CancelToken>,
        )
        .with_steer(steer)
        .with_writer(Arc::clone(&log) as Arc<dyn SessionLog>)
        .with_peer(EchoAgent::shared("scout", "the answer")),
    );
    let _inbox = inbox;
    surface
        .registry()
        .settle("parent", "child-1", RoundOutcome::Completed);

    let output = surface
        .message_tool()
        .call(
            json!({"id": "child-1", "text": "and now the tests"}),
            &context("parent"),
        )
        .await;
    assert!(output.is_text(), "{:?}", output.is_error);
    assert_eq!(text_of(&output), "the answer");

    // The round's rows went into the child's own session: the turn
    // that asked for it and the footer that closes it. (The answer
    // row is the child harness's own: a peer that does not write to
    // the log has none.)
    let types = row_types(&log).await;
    assert_eq!(
        types.first().map(String::as_str),
        Some("user_message"),
        "{types:?}"
    );
    assert_eq!(
        types.last().map(String::as_str),
        Some("subagent_exit"),
        "{types:?}"
    );

    // The child is idle again: the round ended.
    let roster = roster(&surface, "parent").await;
    assert_eq!(roster[0]["status"], "idle");
    assert_eq!(roster[0]["activity"], "finished");
}

#[tokio::test]
async fn a_message_to_an_aborted_child_is_refused() {
    let (surface, _peers) = surface().await;
    let log = child_log("child-1");
    let (_inbox, steer) = ChannelInbox::channel();
    let cancel = Arc::new(AtomicCancelToken::new());
    surface.registry().register(
        "parent",
        ChildHandle::new(
            ChildState::spawn("child-1", "scout"),
            Arc::clone(&cancel) as Arc<dyn CancelToken>,
        )
        .with_steer(steer)
        .with_writer(Arc::clone(&log) as Arc<dyn SessionLog>)
        .with_peer(EchoAgent::shared("scout", "the answer")),
    );
    surface.registry().kill("parent", "child-1").expect("kill");

    let output = surface
        .message_tool()
        .call(
            json!({"id": "child-1", "text": "one more thing"}),
            &context("parent"),
        )
        .await;
    assert_eq!(output.is_error, Some(true), "the refusal is an error");
    assert!(
        text_of(&output).contains("terminal"),
        "the refusal says why: {}",
        text_of(&output)
    );
}

#[tokio::test]
async fn an_interrupt_stops_the_round_and_leaves_the_child_usable() {
    let (surface, _peers) = surface().await;
    let log = child_log("child-1");
    let (_inbox, steer) = ChannelInbox::channel();
    let cancel = Arc::new(AtomicCancelToken::new());
    surface.registry().register(
        "parent",
        ChildHandle::new(
            ChildState::spawn("child-1", "scout"),
            Arc::clone(&cancel) as Arc<dyn CancelToken>,
        )
        .with_steer(steer)
        .with_writer(Arc::clone(&log) as Arc<dyn SessionLog>)
        .with_peer(EchoAgent::shared("scout", "the answer")),
    );

    let output = surface
        .interrupt_tool()
        .call(json!({"id": "child-1"}), &context("parent"))
        .await;
    assert!(output.is_text(), "{:?}", output.is_error);
    assert!(cancel.is_cancelled(), "the child's own token fired");

    // An interrupt is not a kill: the child can be asked again.
    let roster = roster(&surface, "parent").await;
    assert_eq!(roster[0]["status"], "idle");
    assert_eq!(roster[0]["activity"], "interrupted");
}

#[tokio::test]
async fn a_follow_up_after_an_interrupt_runs_again() {
    let (surface, _peers) = surface().await;
    let log = child_log("child-1");
    let (_inbox, steer) = ChannelInbox::channel();
    let cancel = Arc::new(AtomicCancelToken::new());
    surface.registry().register(
        "parent",
        ChildHandle::new(
            ChildState::spawn("child-1", "scout"),
            Arc::clone(&cancel) as Arc<dyn CancelToken>,
        )
        .with_steer(steer)
        .with_writer(Arc::clone(&log) as Arc<dyn SessionLog>)
        .with_peer(EchoAgent::shared("scout", "the answer")),
    );

    // The interrupt fires the round's token.
    surface
        .interrupt_tool()
        .call(json!({"id": "child-1"}), &context("parent"))
        .await;
    assert!(cancel.is_cancelled(), "the round's token fired");

    // The next round runs on a token of its own: the fired one is not
    // inherited.
    let output = surface
        .message_tool()
        .call(
            json!({"id": "child-1", "text": "and now the tests"}),
            &context("parent"),
        )
        .await;
    assert!(output.is_text(), "{:?}", output.is_error);
    assert_eq!(text_of(&output), "the answer");
}

#[tokio::test]
async fn an_interrupt_of_an_unknown_child_is_refused() {
    let (surface, _peers) = surface().await;
    let output = surface
        .interrupt_tool()
        .call(json!({"id": "nobody"}), &context("parent"))
        .await;
    assert_eq!(output.is_error, Some(true));
    assert!(text_of(&output).contains("nobody"), "{}", text_of(&output));
}

#[tokio::test]
async fn a_message_without_a_target_is_refused() {
    let (surface, _peers) = surface().await;
    let output = surface
        .message_tool()
        .call(json!({"text": "hello"}), &context("parent"))
        .await;
    assert_eq!(output.is_error, Some(true));
    assert!(text_of(&output).contains("`id`"), "{}", text_of(&output));

    let output = surface
        .message_tool()
        .call(json!({"id": "child-1"}), &context("parent"))
        .await;
    assert_eq!(output.is_error, Some(true));
    assert!(text_of(&output).contains("`text`"), "{}", text_of(&output));
}

/// A cold wake reads the child's own log: the header names the agent
/// it ran as, and the wake runs that agent again.
#[tokio::test]
async fn a_message_to_a_finished_child_wakes_it_again() {
    let (_surface, peers) = surface().await;
    let log = child_log("child-1");
    log.append_event_nowait(&subagent_enter("child-1", "parent", 1, "scout"));
    log.append_event_nowait(&user_message(
        serde_json::to_value(uva_provider::Message::user("the first ask"))
            .expect("the message serialises"),
        None,
    ));
    log.flush().await.expect("the child's header is durable");

    let index = Arc::new(super::scan::FakeIndex::new());
    index.with_session(
        "child-1",
        vec![
            serde_json::to_value(subagent_enter(
                "child-1", "parent", 1, "scout",
            ))
            .expect("the header serialises"),
        ],
    );
    index.with_writer("child-1", Arc::clone(&log) as Arc<dyn SessionLog>);
    let surface = Arc::new(
        ControlSurface::new(Arc::clone(&peers)).with_index(index.clone()),
    );

    let output = surface
        .message_tool()
        .call(
            json!({"id": "child-1", "text": "one more thing"}),
            &context("parent"),
        )
        .await;
    assert!(output.is_text(), "{:?}", output.is_error);
    assert_eq!(text_of(&output), "the answer");

    // The woken child is running again, and its session took the new
    // round's rows.
    let roster = roster(&surface, "parent").await;
    assert_eq!(roster.len(), 1);
    assert_eq!(roster[0]["id"], "child-1");
    assert_eq!(roster[0]["name"], "scout");
}

/// The tools' definitions are the registry's business: a model has to
/// be able to call them.
#[tokio::test]
async fn the_three_tools_are_ordinary_registry_entries() {
    let (surface, _peers) = surface().await;
    let registry = Arc::new(uva_tool::ToolRegistry::new());
    for tool in [
        surface.list_tool() as Arc<dyn Tool>,
        surface.message_tool() as Arc<dyn Tool>,
        surface.interrupt_tool() as Arc<dyn Tool>,
    ] {
        registry.register_core_entry(uva_tool::ToolEntry::new(tool));
    }
    let names: Vec<String> = registry
        .snapshot()
        .iter()
        .map(|meta| meta.name.clone())
        .collect();
    assert!(names.contains(&"agents_list".to_string()), "{names:?}");
    assert!(names.contains(&"agents_message".to_string()), "{names:?}");
    assert!(names.contains(&"agents_interrupt".to_string()), "{names:?}");

    let entry = registry.get("agents_message").expect("the tool is there");
    assert_eq!(
        entry.tool_instance().parameters()["required"],
        json!(["id", "text"])
    );
}

/// A message the model sends to a child that does not exist is
/// refused, not silently accepted.
#[tokio::test]
async fn a_message_to_a_child_that_never_existed_is_refused() {
    let (surface, _peers) = surface().await;
    let output = surface
        .message_tool()
        .call(json!({"id": "ghost", "text": "hello"}), &context("parent"))
        .await;
    assert_eq!(output.is_error, Some(true));
    assert!(text_of(&output).contains("ghost"), "{}", text_of(&output));
}

/// The roster is read from the handle table, and the table
/// reconciles itself with the durable sessions: a child a previous
/// process dispatched is adopted as `parked` — no handles, still
/// addressable — instead of vanishing from the list.
#[tokio::test]
async fn the_roster_adopts_children_a_previous_process_dispatched() {
    let (_surface, peers) = surface().await;
    let index = Arc::new(super::scan::FakeIndex::new());
    index.with_child("child-1", "parent", "scout", 1, true);
    let surface =
        Arc::new(ControlSurface::new(Arc::clone(&peers)).with_index(index));

    let rows = roster(&surface, "parent").await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["id"], "child-1");
    assert_eq!(rows[0]["name"], "scout");
    assert_eq!(
        rows[0]["status"], "parked",
        "a child with no in-memory handles is parked"
    );
    assert_eq!(
        surface.registry().has_handles("parent", "child-1"),
        Some(false)
    );

    // Reading again is stable: the row is adopted once, and a live
    // row would win over the log's copy.
    assert_eq!(roster(&surface, "parent").await, rows);
}

/// A message to a parked child takes the cold-wake branch: the log is
/// read, the peer resolved, and the woken round's handles take over
/// the row.
#[tokio::test]
async fn a_message_to_a_parked_child_wakes_it_from_its_log() {
    let (_surface, peers) = surface().await;
    let log = child_log("child-1");
    log.append_event_nowait(&subagent_enter("child-1", "parent", 1, "scout"));
    log.flush().await.expect("the child's header is durable");

    let index = Arc::new(super::scan::FakeIndex::new());
    index.with_session(
        "child-1",
        vec![
            serde_json::to_value(subagent_enter(
                "child-1", "parent", 1, "scout",
            ))
            .expect("the header serialises"),
        ],
    );
    index.with_writer("child-1", Arc::clone(&log) as Arc<dyn SessionLog>);
    let surface =
        Arc::new(ControlSurface::new(Arc::clone(&peers)).with_index(index));

    // The roster adopted the child as parked.
    assert_eq!(roster(&surface, "parent").await[0]["status"], "parked");

    let output = surface
        .message_tool()
        .call(
            json!({"id": "child-1", "text": "one more thing"}),
            &context("parent"),
        )
        .await;
    assert!(output.is_text(), "{:?}", output.is_error);
    assert_eq!(text_of(&output), "the answer");

    // The wake took the row over: the round ran (and is idle now that
    // it ended) with live handles attached.
    let rows = roster(&surface, "parent").await;
    assert_eq!(rows[0]["status"], "idle");
    assert_eq!(
        surface.registry().has_handles("parent", "child-1"),
        Some(true),
        "the woken round left the child in memory"
    );

    // The child's own session holds the new round.
    let types = row_types(&log).await;
    assert!(
        types.iter().filter(|kind| *kind == "user_message").count() >= 1,
        "the wake logged its turn in the child's session: {types:?}"
    );
}

/// A child dispatched just before a restart is adopted **and wakeable**
/// even when the deployment holds more sessions than one scan reads.
///
/// The roster's scan is a bounded window over the deployment's
/// sessions. A child dispatched minutes ago is the newest session, so
/// the window has to be the recent end: an ascending window would read
/// the oldest sessions every time, and this child would be missing from
/// the roster and from the cold wake's resolution alike.
#[tokio::test]
async fn a_child_past_the_scan_window_is_adopted_and_wakeable() {
    const CHILD: &str = "01J8NEWCHILD";

    let (_surface, peers) = surface().await;
    let log = child_log(CHILD);
    log.append_event_nowait(&subagent_enter(CHILD, "parent", 1, "scout"));
    log.flush().await.expect("the child's header is durable");

    let index = Arc::new(super::scan::FakeIndex::new());
    for ordinal in 0..(super::super::scan::SCAN_LIMIT + 5) {
        index.with_session(
            &format!("00J8OLD{ordinal:04}"),
            vec![json!({"type": "user_message", "data": {}})],
        );
    }
    index.with_session(
        CHILD,
        vec![
            serde_json::to_value(subagent_enter(CHILD, "parent", 1, "scout"))
                .expect("the header serialises"),
        ],
    );
    index.with_writer(CHILD, Arc::clone(&log) as Arc<dyn SessionLog>);
    let surface =
        Arc::new(ControlSurface::new(Arc::clone(&peers)).with_index(index));

    // The roster adopted the newest child, not the oldest sessions.
    let adopted = roster(&surface, "parent").await;
    assert_eq!(adopted.len(), 1, "{adopted:?}");
    assert_eq!(adopted[0]["id"], CHILD);
    assert_eq!(adopted[0]["status"], "parked");

    // And the cold wake finds it from the same scan.
    let output = surface
        .message_tool()
        .call(
            json!({"id": CHILD, "text": "keep going"}),
            &context("parent"),
        )
        .await;
    assert!(output.is_text(), "{:?}", output.is_error);
    assert_eq!(text_of(&output), "the answer");
    assert_eq!(
        surface.registry().has_handles("parent", CHILD),
        Some(true),
        "the woken round took the row over"
    );
}
