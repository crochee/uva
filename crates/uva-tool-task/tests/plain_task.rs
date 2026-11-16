//! The plain `task` tool through a real run: a parent model calls
//! `task`, the tool drives a registered peer as a child run, and the
//! two session logs tell the story — the child holds the full
//! transcript (header, dispatch prompt, its own rows, footer), the
//! parent holds only the ordinary tool rows.

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::StreamExt;
use uva_agent::{
    Agent,
    AgentEntry,
    AgentInput,
    AgentRegistry,
    SubagentWriters,
};
use uva_core::{AtomicCancelToken, CancelToken, Registry};
use uva_events::{AgentEvent, SessionEndReason, SystemEvent};
use uva_harness::ReActAgent;
use uva_provider::{ContentPart, StreamChunk, TextContent};
use uva_session::{
    SessionLog,
    manager::SessionRegistry,
    subagent_enter,
    user_message,
};
use uva_test_support::ReplayProvider;
use uva_tool::{Tool, ToolRegistry};
use uva_tool_task::{TaskTool, TaskToolPlugin};

fn text(text: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::Content(ContentPart::Text(TextContent {
            text: text.to_string(),
            cache_control: None,
        })),
        StreamChunk::IsDone {
            result: Box::default(),
        },
    ]
}

fn task_call(id: &str, agent: &str, prompt: &str) -> Vec<StreamChunk> {
    vec![
        StreamChunk::ToolCallStart {
            id: id.to_string(),
            name: "task".to_string(),
            arguments: serde_json::json!({"agent": agent, "prompt": prompt}),
        },
        StreamChunk::ToolCallEnd { id: id.to_string() },
        StreamChunk::IsDone {
            result: Box::default(),
        },
    ]
}

/// Opens child sessions the way the server's router-backed source does:
/// header + dispatch prompt, then the child's own writer.
struct TestWriters {
    manager: Arc<SessionRegistry>,
    parent: String,
    opened: Arc<Mutex<Vec<String>>>,
}

impl SubagentWriters for TestWriters {
    fn open_child(
        &self,
        child_session_id: &str,
        _parent_session_id: &str,
        depth: usize,
        agent_name: &str,
        prompt: Option<&str>,
    ) -> Arc<dyn SessionLog> {
        let child = child_session_id.to_string();
        let writer = self.manager.writer("alice", &child);
        writer.append_event_nowait(&subagent_enter(
            &child,
            &self.parent,
            depth as u32,
            agent_name,
        ));
        if let Some(prompt) = prompt.filter(|p| !p.is_empty())
            && let Ok(data) =
                serde_json::to_value(uva_provider::Message::user(prompt))
        {
            writer.append_event_nowait(&user_message(data, None));
        }
        self.opened.lock().expect("lock").push(child.clone());
        writer
    }
}

fn row_types(rows: &[serde_json::Value]) -> Vec<String> {
    rows.iter()
        .filter_map(|row| row.get("type").and_then(|t| t.as_str()))
        .map(str::to_string)
        .collect()
}

#[tokio::test]
async fn a_task_call_runs_a_child_session_and_leaves_only_tool_rows_parent() {
    let parent_provider = Arc::new(ReplayProvider::new(vec![
        task_call("t1", "scout", "map the repo"),
        text("parent done"),
    ]));
    let child_provider =
        Arc::new(ReplayProvider::new(vec![text("child reply")]));

    let temp = tempfile::tempdir().expect("tempdir");
    let manager = Arc::new(SessionRegistry::new(temp.path().to_path_buf()));
    manager
        .create_with_user("parent".to_string(), "alice".to_string())
        .await
        .expect("parent session");

    let peers = Arc::new(AgentRegistry::new());
    let child = ReActAgent::new(child_provider, Arc::new(ToolRegistry::new()))
        .with_name("scout");
    peers
        .put(AgentEntry::new(Arc::new(child)))
        .await
        .expect("register the peer");

    let opened = Arc::new(Mutex::new(Vec::new()));
    let writers = Arc::new(TestWriters {
        manager: Arc::clone(&manager),
        parent: "parent".to_string(),
        opened: Arc::clone(&opened),
    });
    let tool =
        Arc::new(TaskTool::new(Arc::clone(&peers)).with_writers(writers));

    let registry = Arc::new(ToolRegistry::new());
    registry.register_core_entry(uva_tool::ToolEntry::new(
        Arc::clone(&tool) as Arc<dyn uva_tool::Tool>
    ));

    let parent_writer = manager.writer("alice", "parent");
    let parent = ReActAgent::new(parent_provider, Arc::clone(&registry))
        .with_name("parent")
        .with_log_writer(Arc::clone(&parent_writer) as Arc<dyn SessionLog>);

    let input = AgentInput::text("delegate please").thread_id("parent");
    let mut stream =
        parent.run(input, Arc::new(AtomicCancelToken::new())).await;
    while stream.next().await.is_some() {}

    let parent_rows = parent_writer.read().await.expect("parent rows");
    let parent_types = row_types(&parent_rows);
    assert!(
        parent_types
            .iter()
            .any(|t| t == "tool_dispatch" || t == "tool_call"),
        "the parent records the task call: {parent_types:?}"
    );
    assert!(
        parent_types.iter().any(|t| t == "tool_result"),
        "the parent records the result: {parent_types:?}"
    );
    assert!(
        !parent_types.iter().any(|t| t.starts_with("subagent_")),
        "no child markers in the parent log: {parent_types:?}"
    );

    let children = opened.lock().expect("lock").clone();
    assert_eq!(children.len(), 1, "exactly one child session");
    let child_id = &children[0];
    let child_log = manager.writer("alice", child_id);
    let child_types = row_types(&child_log.read().await.expect("child rows"));
    assert_eq!(child_types[0], "subagent_enter", "{child_types:?}");
    assert_eq!(child_types[1], "user_message", "{child_types:?}");
    assert!(
        child_types.iter().any(|t| t == "assistant_message"),
        "the child's own harness wrote its reply: {child_types:?}"
    );
    assert_eq!(
        child_types.last().map(String::as_str),
        Some("subagent_exit"),
        "the tool closes the child session: {child_types:?}"
    );

    let result = parent_rows
        .iter()
        .find(|row| row["type"] == "tool_result")
        .expect("tool result row");
    assert!(
        result.to_string().contains(child_id),
        "the result carries the child id: {result}"
    );
}

#[tokio::test]
async fn the_task_tool_definition_is_registered_like_any_other_tool() {
    let plugin = TaskToolPlugin::new(Arc::new(TaskTool::new(Arc::new(
        AgentRegistry::new(),
    ))));
    let ctx = uva_kernel::Context::new();
    let registry = Arc::new(ToolRegistry::new());
    let mounted = uva_kernel::mount_all(
        &ctx,
        &[
            Arc::new(uva_tool::ToolRegistryPlugin::new(Arc::clone(&registry)))
                as Arc<dyn uva_kernel::Plugin>,
            Arc::new(plugin),
        ],
    )
    .expect("the brick mounts");
    let entry = registry.get("task").expect("the brick files the tool");
    let parameters = entry.tool_instance().parameters();
    assert_eq!(
        parameters["required"],
        serde_json::json!(["agent", "prompt"])
    );
    assert_eq!(
        entry.tool_instance().mode(),
        uva_tool::ExecutionMode::Sequential,
        "a dispatch starts a whole child run"
    );
    // Exposure and restriction are the registry's own knobs — the same
    // path every other tool brick goes through.
    assert_eq!(
        registry.execution_mode("task"),
        uva_tool::ExecutionMode::Sequential
    );
    assert_eq!(
        registry.exposure("task"),
        Some(uva_tool::ToolExposure::Direct)
    );
    assert!(registry.set_exposure("task", uva_tool::ToolExposure::Deferred));
    assert_eq!(
        registry.exposure("task"),
        Some(uva_tool::ToolExposure::Deferred)
    );
    assert!(registry.set_hidden("task", true));
    assert!(
        registry.snapshot().iter().all(|meta| meta.name != "task"),
        "a hidden tool leaves the model-facing listing"
    );
    assert!(registry.set_hidden("task", false));
    assert!(
        registry.snapshot().iter().any(|meta| meta.name == "task"),
        "unhiding restores the listing"
    );
    drop(mounted);
    assert!(
        !registry.contains("task"),
        "unload withdraws the registration"
    );
}

/// A peer that announces its start and ends only when the token it was
/// handed fires: the shape a parent cancellation has to reach.
struct CancelAwaitedPeer {
    descriptor: uva_core::agent::AgentDescriptor,
    started: Arc<tokio::sync::Semaphore>,
}

#[async_trait::async_trait]
impl Agent for CancelAwaitedPeer {
    fn descriptor(&self) -> &uva_core::agent::AgentDescriptor {
        &self.descriptor
    }

    async fn run(
        &self,
        _input: AgentInput,
        cancel: Arc<dyn CancelToken>,
    ) -> std::pin::Pin<
        Box<dyn futures::Stream<Item = AgentEvent> + Send + 'static>,
    > {
        // Announced before the stream is polled so the test can cancel
        // only once the child is genuinely in flight.
        self.started.add_permits(1);
        Box::pin(futures::stream::once(async move {
            cancel.cancelled().await;
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Cancelled,
            })
        }))
    }
}

impl uva_core::registry::RegistryItem for CancelAwaitedPeer {
    fn name(&self) -> &str {
        &self.descriptor.name
    }

    fn description(&self) -> &str {
        &self.descriptor.description
    }
}

/// A peer that notices the cancel but takes longer to shut down than
/// the parent's 150 ms progress window: the shape where the parent's
/// cancel wins the race and the harness drops the dispatch future
/// instead of polling it to the end.
struct SlowCancelPeer {
    descriptor: uva_core::agent::AgentDescriptor,
    started: Arc<tokio::sync::Semaphore>,
}

#[async_trait::async_trait]
impl Agent for SlowCancelPeer {
    fn descriptor(&self) -> &uva_core::agent::AgentDescriptor {
        &self.descriptor
    }

    async fn run(
        &self,
        _input: AgentInput,
        cancel: Arc<dyn CancelToken>,
    ) -> std::pin::Pin<
        Box<dyn futures::Stream<Item = AgentEvent> + Send + 'static>,
    > {
        self.started.add_permits(1);
        Box::pin(futures::stream::once(async move {
            cancel.cancelled().await;
            tokio::time::sleep(Duration::from_millis(400)).await;
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Cancelled,
            })
        }))
    }
}

impl uva_core::registry::RegistryItem for SlowCancelPeer {
    fn name(&self) -> &str {
        &self.descriptor.name
    }

    fn description(&self) -> &str {
        &self.descriptor.description
    }
}

/// The parent's cancel wins the race against the child's own shutdown.
///
/// The harness stops draining the tool stream and **drops** the
/// dispatch future, so nothing after its current `await` runs — and
/// the child's footer used to be one of those things. The round's tail
/// is not part of the dropped future anymore, so the child session
/// still closes.
#[tokio::test]
async fn a_cancel_that_wins_the_race_still_closes_the_child_session() {
    let parent_provider = Arc::new(ReplayProvider::new(vec![
        task_call("t1", "slow", "wait for the parent"),
        text("unreachable"),
    ]));

    let temp = tempfile::tempdir().expect("tempdir");
    let manager = Arc::new(SessionRegistry::new(temp.path().to_path_buf()));
    manager
        .create_with_user("parent".to_string(), "alice".to_string())
        .await
        .expect("parent session");

    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let mut descriptor = uva_harness::default_descriptor(String::new());
    descriptor.name = "slow".to_string();
    let peers = Arc::new(AgentRegistry::new());
    peers
        .put(AgentEntry::new(Arc::new(SlowCancelPeer {
            descriptor,
            started: Arc::clone(&started),
        })))
        .await
        .expect("register the peer");

    let opened = Arc::new(Mutex::new(Vec::new()));
    let writers = Arc::new(TestWriters {
        manager: Arc::clone(&manager),
        parent: "parent".to_string(),
        opened: Arc::clone(&opened),
    });
    let tool =
        Arc::new(TaskTool::new(Arc::clone(&peers)).with_writers(writers));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_core_entry(uva_tool::ToolEntry::new(
        Arc::clone(&tool) as Arc<dyn uva_tool::Tool>
    ));

    let parent_writer = manager.writer("alice", "parent");
    let parent = ReActAgent::new(parent_provider, Arc::clone(&registry))
        .with_name("parent")
        .with_log_writer(Arc::clone(&parent_writer) as Arc<dyn SessionLog>);

    let cancel: Arc<dyn CancelToken> = Arc::new(AtomicCancelToken::new());
    let input = AgentInput::text("delegate then stop").thread_id("parent");
    let mut stream = parent.run(input, Arc::clone(&cancel)).await;
    let drive = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event);
        }
        events
    });

    let _started =
        tokio::time::timeout(Duration::from_secs(10), started.acquire())
            .await
            .expect("the child run starts")
            .expect("the start signal stays open");
    cancel.cancel();

    tokio::time::timeout(Duration::from_secs(10), drive)
        .await
        .expect("the parent run ends after the cancel")
        .expect("the driver task joins");

    let children = opened.lock().expect("lock").clone();
    assert_eq!(children.len(), 1, "exactly one child session");
    let child_id = &children[0];

    // The tail outlives the dropped dispatch future, so the footer
    // arrives a moment after the parent's run ended.
    let mut footer = None;
    for _ in 0..5_000 {
        let rows = manager
            .writer("alice", child_id)
            .read()
            .await
            .expect("child rows");
        if let Some(row) =
            rows.iter().find(|row| row["type"] == "subagent_exit")
        {
            footer = Some(row.clone());
            break;
        }
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
    let footer = footer.expect(
        "the dropped dispatch future still leaves the tail behind: no \
         subagent_exit row",
    );
    assert_eq!(footer["data"]["status"], "cancelled");
    assert_eq!(footer["data"]["depth"], 1);
}

/// Cancelling the parent run reaches the tool's context token, the
/// child run is handed that same token, and the child session still
/// closes with its `subagent_exit` footer (`status: cancelled`) — no
/// runaway child, no unclosed session.
#[tokio::test]
async fn a_cancelled_parent_ends_its_child_and_closes_the_child_session() {
    let parent_provider = Arc::new(ReplayProvider::new(vec![
        task_call("t1", "sleeper", "wait for the parent"),
        text("unreachable"),
    ]));

    let temp = tempfile::tempdir().expect("tempdir");
    let manager = Arc::new(SessionRegistry::new(temp.path().to_path_buf()));
    manager
        .create_with_user("parent".to_string(), "alice".to_string())
        .await
        .expect("parent session");

    let started = Arc::new(tokio::sync::Semaphore::new(0));
    let mut descriptor = uva_harness::default_descriptor(String::new());
    descriptor.name = "sleeper".to_string();
    let peers = Arc::new(AgentRegistry::new());
    peers
        .put(AgentEntry::new(Arc::new(CancelAwaitedPeer {
            descriptor,
            started: Arc::clone(&started),
        })))
        .await
        .expect("register the peer");

    let opened = Arc::new(Mutex::new(Vec::new()));
    let writers = Arc::new(TestWriters {
        manager: Arc::clone(&manager),
        parent: "parent".to_string(),
        opened: Arc::clone(&opened),
    });
    let tool =
        Arc::new(TaskTool::new(Arc::clone(&peers)).with_writers(writers));
    let registry = Arc::new(ToolRegistry::new());
    registry.register_core_entry(uva_tool::ToolEntry::new(
        Arc::clone(&tool) as Arc<dyn uva_tool::Tool>
    ));

    let parent_writer = manager.writer("alice", "parent");
    let parent = ReActAgent::new(parent_provider, Arc::clone(&registry))
        .with_name("parent")
        .with_log_writer(Arc::clone(&parent_writer) as Arc<dyn SessionLog>);

    let cancel: Arc<dyn CancelToken> = Arc::new(AtomicCancelToken::new());
    let input = AgentInput::text("delegate then stop").thread_id("parent");
    let mut stream = parent.run(input, Arc::clone(&cancel)).await;
    let drive = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = stream.next().await {
            events.push(event);
        }
        events
    });

    let _started =
        tokio::time::timeout(Duration::from_secs(10), started.acquire())
            .await
            .expect("the child run starts")
            .expect("the start signal stays open");
    cancel.cancel();

    let events = tokio::time::timeout(Duration::from_secs(10), drive)
        .await
        .expect("the parent run ends after the cancel")
        .expect("the driver task joins");
    assert!(
        events.iter().any(|event| matches!(
            event,
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Cancelled,
            })
        )),
        "the parent run ends cancelled: {events:?}"
    );

    let children = opened.lock().expect("lock").clone();
    assert_eq!(children.len(), 1, "exactly one child session");
    let child_id = &children[0];
    let child_rows = manager
        .writer("alice", child_id)
        .read()
        .await
        .expect("child rows");
    let child_types = row_types(&child_rows);
    assert_eq!(child_types[0], "subagent_enter", "{child_types:?}");
    let enter = &child_rows[0];
    assert_eq!(enter["data"]["parent_session_id"], "parent");
    assert_eq!(enter["data"]["depth"], 1);
    assert_eq!(enter["data"]["agent_name"], "sleeper");
    let exit = child_rows.last().expect("the child log has rows");
    assert_eq!(
        exit["type"], "subagent_exit",
        "a cancelled child still closes its session: {child_types:?}"
    );
    assert_eq!(exit["data"]["status"], "cancelled");

    let parent_types =
        row_types(&parent_writer.read().await.expect("parent rows"));
    assert!(
        !parent_types.iter().any(|t| t.starts_with("subagent_")),
        "the parent log still holds no child markers: {parent_types:?}"
    );
}

/// A peer that reports one step's usage and ends.
struct UsagePeer {
    descriptor: uva_core::agent::AgentDescriptor,
}

#[async_trait::async_trait]
impl Agent for UsagePeer {
    fn descriptor(&self) -> &uva_core::agent::AgentDescriptor {
        &self.descriptor
    }

    async fn run(
        &self,
        _input: AgentInput,
        _cancel: Arc<dyn CancelToken>,
    ) -> std::pin::Pin<
        Box<dyn futures::Stream<Item = AgentEvent> + Send + 'static>,
    > {
        Box::pin(futures::stream::iter([
            AgentEvent::usage(11, 5, None, None),
            AgentEvent::text_delta("child answer"),
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Completed,
            }),
        ]))
    }
}

impl uva_core::registry::RegistryItem for UsagePeer {
    fn name(&self) -> &str {
        &self.descriptor.name
    }

    fn description(&self) -> &str {
        &self.descriptor.description
    }
}

/// The deployment's account, as the tool reports into it.
#[derive(Default)]
struct CountingUsage {
    tokens: Mutex<Vec<(String, usize, usize)>>,
    turns: Mutex<Vec<String>>,
}

impl uva_agent::SubagentUsage for CountingUsage {
    fn tokens(&self, child_session_id: &str, input: usize, output: usize) {
        self.tokens.lock().expect("lock").push((
            child_session_id.to_string(),
            input,
            output,
        ));
    }

    fn round_ended(&self, child_session_id: &str) {
        self.turns
            .lock()
            .expect("lock")
            .push(child_session_id.to_string());
    }
}

/// A delegated round's consumption is real consumption: the child runs
/// on its own harness, so its `Usage` events never reach the parent
/// session's controller and `/api/v1/chat/usage` would miss them. The
/// tool reports the child's tokens and its turn to the account the host
/// installed.
#[tokio::test]
async fn a_child_round_reports_its_tokens_and_its_turn() {
    let peers = Arc::new(AgentRegistry::new());
    let mut descriptor = uva_harness::default_descriptor(String::new());
    descriptor.name = "metered".to_string();
    peers
        .put(AgentEntry::new(Arc::new(UsagePeer { descriptor })))
        .await
        .expect("register the peer");

    let usage = Arc::new(CountingUsage::default());
    let tool = TaskTool::new(Arc::clone(&peers)).with_usage(usage.clone());
    let ctx = uva_tool::Context::new(
        "parent".to_string(),
        std::path::PathBuf::from("/tmp"),
    );
    let output = tool
        .call(
            serde_json::json!({"agent": "metered", "prompt": "go"}),
            &ctx,
        )
        .await;
    assert!(!output.is_error.unwrap_or(false), "{output:?}");

    let tokens = usage.tokens.lock().expect("lock").clone();
    assert_eq!(tokens.len(), 1, "one reported step: {tokens:?}");
    assert!(
        !tokens[0].0.is_empty(),
        "the report names the child session"
    );
    assert_eq!((tokens[0].1, tokens[0].2), (11, 5));
    let turns = usage.turns.lock().expect("lock").clone();
    assert_eq!(turns, vec![tokens[0].0.clone()], "one round, one turn");
}
