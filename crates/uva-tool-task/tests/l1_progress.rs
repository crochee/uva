//! The L1 progress contract through the tool stream and a real run:
//! the parent's tool block grows one merged line per child, a burst of
//! child activity is coalesced far below the live-event cap, and the
//! durable `tool_output` row carries the same summary for a reload.

use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures::StreamExt;
use uva_agent::{Agent, AgentEntry, AgentInput, AgentRegistry};
use uva_core::{
    AtomicCancelToken,
    CancelToken,
    Registry,
    SharedIdGen,
    Sleeper,
    agent::AgentDescriptor,
    registry::RegistryItem,
};
use uva_events::{AgentEvent, SessionEndReason, SystemEvent};
use uva_harness::ReActAgent;
use uva_provider::{ContentPart, StreamChunk, TextContent};
use uva_session::{SessionLog, manager::SessionRegistry};
use uva_test_support::ReplayProvider;
use uva_tool::{Context, StreamOutput, Tool, ToolRegistry};
use uva_tool_task::TaskTool;

fn text_of(output: &uva_tool::ToolOutput) -> String {
    output
        .content
        .iter()
        .filter_map(|part| part.text())
        .collect::<String>()
}

fn descriptor(name: &str) -> AgentDescriptor {
    let mut descriptor = uva_harness::default_descriptor(String::new());
    descriptor.name = name.to_string();
    descriptor
}

fn progress(message: &str, step: usize, total: usize) -> AgentEvent {
    AgentEvent::System(SystemEvent::Progress {
        message: message.to_string(),
        step,
        total,
    })
}

fn chat(text: &str) -> AgentEvent {
    AgentEvent::Model(ContentPart::Text(TextContent {
        text: text.to_string(),
        cache_control: None,
    }))
}

fn ended(reason: SessionEndReason) -> AgentEvent {
    AgentEvent::System(SystemEvent::SessionEnded { reason })
}

/// A peer that announces a step, keeps running across a merge window,
/// and then answers.
struct PacedPeer {
    descriptor: AgentDescriptor,
}

#[async_trait::async_trait]
impl Agent for PacedPeer {
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
        let first =
            futures::stream::once(async { progress("LLM pass 1", 1, 5) });
        let rest = futures::stream::once(async {
            uva_core::ThreadSleeper
                .sleep(Duration::from_millis(250))
                .await;
            chat("child reply")
        })
        .chain(futures::stream::once(async {
            ended(SessionEndReason::Completed)
        }));
        Box::pin(first.chain(rest))
    }
}

impl RegistryItem for PacedPeer {
    fn name(&self) -> &str {
        &self.descriptor.name
    }

    fn description(&self) -> &str {
        &self.descriptor.description
    }
}

/// A peer that dumps a burst of step notices and ends at once.
struct BurstPeer {
    descriptor: AgentDescriptor,
    emitted: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl Agent for BurstPeer {
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
        let emitted = Arc::clone(&self.emitted);
        emitted.fetch_add(5_000, Ordering::SeqCst);
        let events = (0..5_000)
            .map(|step| progress(&format!("step {step}"), 0, 0))
            .chain(std::iter::once(ended(SessionEndReason::Completed)));
        Box::pin(futures::stream::iter(events))
    }
}

impl RegistryItem for BurstPeer {
    fn name(&self) -> &str {
        &self.descriptor.name
    }

    fn description(&self) -> &str {
        &self.descriptor.description
    }
}

fn task_context(session: &str) -> Context {
    Context::new(session.to_string(), PathBuf::from("/tmp"))
}

async fn drain(
    stream: &mut std::pin::Pin<
        Box<dyn futures::Stream<Item = StreamOutput> + Send + '_>,
    >,
) -> (Vec<String>, uva_tool::ToolOutput) {
    let mut progress = Vec::new();
    while let Some(item) = stream.next().await {
        match item {
            StreamOutput::Progress(output) => {
                progress.push(text_of(&output));
            }
            StreamOutput::Result(output) => return (progress, output),
        }
    }
    panic!("the tool stream closed without a result");
}

#[tokio::test]
async fn the_parent_tool_block_grows_while_the_child_runs() {
    let peers = Arc::new(AgentRegistry::new());
    peers
        .put(AgentEntry::new(Arc::new(PacedPeer {
            descriptor: descriptor("scout"),
        })))
        .await
        .expect("register");
    let tool =
        TaskTool::new(Arc::clone(&peers)).with_id_gen(SharedIdGen::sequence());
    let context = task_context("parent");

    let mut stream = tool.stream(
        serde_json::json!({"agent": "scout", "prompt": "map the repo"}),
        &context,
    );
    let (progress, result) = drain(&mut stream).await;

    assert!(
        progress.iter().any(|line| line.contains("[id-0] running")
            && line.contains("LLM pass 1")),
        "the running child's line reaches the tool block: {progress:?}"
    );
    assert!(
        progress
            .iter()
            .any(|line| line.contains("[id-0] idle: finished")),
        "the block ends on the child's outcome: {progress:?}"
    );
    assert!(
        text_of(&result).contains("[subagent session: id-0]"),
        "the result names the child session for L2 drill-down: {result:?}"
    );
}

#[tokio::test]
async fn a_burst_of_child_activity_is_coalesced() {
    let peers = Arc::new(AgentRegistry::new());
    let emitted = Arc::new(AtomicUsize::new(0));
    peers
        .put(AgentEntry::new(Arc::new(BurstPeer {
            descriptor: descriptor("burst"),
            emitted: Arc::clone(&emitted),
        })))
        .await
        .expect("register");
    let tool =
        TaskTool::new(Arc::clone(&peers)).with_id_gen(SharedIdGen::sequence());
    let context = task_context("parent");

    let mut stream = tool.stream(
        serde_json::json!({"agent": "burst", "prompt": "go"}),
        &context,
    );
    let (progress, _) = drain(&mut stream).await;

    assert_eq!(emitted.load(Ordering::SeqCst), 5_000);
    assert!(
        progress.len() <= 2,
        "5000 child notices must not become 5000 chunks: {}",
        progress.len()
    );
    for chunk in &progress {
        assert!(
            chunk.len() < 200,
            "a merged chunk stays one line per child: {chunk:?}"
        );
    }
}

#[tokio::test]
async fn the_summary_lands_in_the_parent_log_as_tool_output_rows() {
    let parent_provider = Arc::new(ReplayProvider::new(vec![
        vec![
            StreamChunk::ToolCallStart {
                id: "t1".to_string(),
                name: "task".to_string(),
                arguments: serde_json::json!({
                    "agent": "scout",
                    "prompt": "map the repo",
                }),
            },
            StreamChunk::ToolCallEnd {
                id: "t1".to_string(),
            },
            StreamChunk::IsDone {
                result: Box::default(),
            },
        ],
        vec![
            StreamChunk::Content(ContentPart::Text(TextContent {
                text: "parent done".to_string(),
                cache_control: None,
            })),
            StreamChunk::IsDone {
                result: Box::default(),
            },
        ],
    ]));

    let temp = tempfile::tempdir().expect("tempdir");
    let manager = Arc::new(SessionRegistry::new(temp.path().to_path_buf()));
    manager
        .create_with_user("parent".to_string(), "alice".to_string())
        .await
        .expect("parent session");

    let peers = Arc::new(AgentRegistry::new());
    peers
        .put(AgentEntry::new(Arc::new(PacedPeer {
            descriptor: descriptor("scout"),
        })))
        .await
        .expect("register");
    let tool = Arc::new(
        TaskTool::new(Arc::clone(&peers)).with_id_gen(SharedIdGen::sequence()),
    );
    let registry = Arc::new(ToolRegistry::new());
    registry.register_core_entry(uva_tool::ToolEntry::new(
        Arc::clone(&tool) as Arc<dyn Tool>
    ));

    let parent_writer = manager.writer("alice", "parent");
    let parent = ReActAgent::new(parent_provider, Arc::clone(&registry))
        .with_name("parent")
        .with_log_writer(Arc::clone(&parent_writer) as Arc<dyn SessionLog>);

    let input = AgentInput::text("delegate please").thread_id("parent");
    let mut stream =
        parent.run(input, Arc::new(AtomicCancelToken::new())).await;
    let mut live = Vec::new();
    while let Some(event) = stream.next().await {
        live.push(event);
    }

    assert!(
        live.iter().any(|event| matches!(
            event,
            AgentEvent::System(SystemEvent::ToolProgress {
                tool_name,
                call_id,
                output,
            }) if tool_name == "task"
                && call_id == "t1"
                && text_of(output).contains("[id-0]")
        )),
        "the live channel carries the child id"
    );

    let rows = parent_writer.read().await.expect("parent rows");
    let streamed = rows
        .iter()
        .filter(|row| row["type"] == "tool_output")
        .collect::<Vec<_>>();
    assert!(
        !streamed.is_empty(),
        "the durable half is written: {:?}",
        rows.iter()
            .filter_map(|row| row["type"].as_str())
            .collect::<Vec<_>>()
    );
    assert!(
        streamed.iter().any(|row| {
            row["data"]["output"]
                .as_str()
                .is_some_and(|chunk| chunk.contains("[id-0]"))
        }),
        "a reload replays the summary from these rows: {streamed:?}"
    );
    let result = rows
        .iter()
        .find(|row| row["type"] == "tool_result")
        .expect("tool result row");
    assert!(
        result.to_string().contains("id-0"),
        "the result names the child session: {result}"
    );
}
