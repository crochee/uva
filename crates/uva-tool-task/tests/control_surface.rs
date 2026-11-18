//! The control surface through a real run: a parent model delegates,
//! lists its sub-agents, and asks one of them for another round.
//!
//! The two session logs tell the story. The parent's log holds the
//! ordinary tool rows; the child's session holds the whole transcript
//! — the dispatch prompt, the first answer, the follow-up turn and
//! the second answer.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use uva_agent::{
    Agent,
    AgentEntry,
    AgentInput,
    AgentRegistry,
    SubagentWriters,
};
use uva_core::{AtomicCancelToken, Registry};
use uva_harness::ReActAgent;
use uva_provider::{ContentPart, StreamChunk, TextContent};
use uva_session::{
    SessionLog,
    manager::SessionRegistry,
    subagent_enter,
    user_message,
};
use uva_test_support::ReplayProvider;
use uva_tool::{ToolRegistry, ToolRegistryPlugin};
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

/// A tool call the parent's model makes.
fn call(
    id: &str,
    name: &str,
    arguments: serde_json::Value,
) -> Vec<StreamChunk> {
    vec![
        StreamChunk::ToolCallStart {
            id: id.to_string(),
            name: name.to_string(),
            arguments,
        },
        StreamChunk::ToolCallEnd { id: id.to_string() },
        StreamChunk::IsDone {
            result: Box::default(),
        },
    ]
}

/// Opens child sessions the way the server's router-backed source
/// does: the header, the dispatch prompt, and the child's own writer.
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
            depth.try_into().unwrap_or(u32::MAX),
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

/// The parent's model: delegate, look at the roster, then ask the
/// child for another round.
fn parent_script(child_id: &str) -> Vec<Vec<StreamChunk>> {
    vec![
        call(
            "t1",
            "task",
            serde_json::json!({"agent": "scout", "prompt": "map the repo"}),
        ),
        call("t2", "agents_list", serde_json::json!({})),
        call(
            "t3",
            "agents_message",
            serde_json::json!({"id": child_id, "text": "and now the tests"}),
        ),
        text("parent done"),
    ]
}

/// The whole surface, driven by one parent run.
#[tokio::test]
async fn a_parent_lists_its_children_and_asks_one_for_another_round() {
    let child_id = "id-0";
    let parent_provider =
        Arc::new(ReplayProvider::new(parent_script(child_id)));

    let temp = tempfile::tempdir().expect("tempdir");
    let manager = Arc::new(SessionRegistry::new(temp.path().to_path_buf()));
    manager
        .create_with_user("parent".to_string(), "alice".to_string())
        .await
        .expect("parent session");

    let peers = Arc::new(AgentRegistry::new());
    // The peer is a real agent: its own harness writes the answers
    // into the child's session, exactly like a top-level run does.
    let child_provider = Arc::new(ReplayProvider::new(vec![
        text("the first answer"),
        text("the follow-up answer"),
    ]));
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
    let tool = Arc::new(
        TaskTool::new(Arc::clone(&peers))
            .with_writers(writers)
            .with_id_gen(uva_core::SharedIdGen::sequence()),
    );

    // The brick files the `task` tool and the control surface's three
    // tools in the registry.
    let registry = Arc::new(ToolRegistry::new());
    let ctx = uva_kernel::Context::new();
    let _mounted = uva_kernel::mount_all(
        &ctx,
        &[
            Arc::new(ToolRegistryPlugin::new(Arc::clone(&registry)))
                as Arc<dyn uva_kernel::Plugin>,
            Arc::new(TaskToolPlugin::new(Arc::clone(&tool))),
        ],
    )
    .expect("the brick mounts");
    for name in ["task", "agents_list", "agents_message", "agents_interrupt"] {
        assert!(
            registry.contains(name),
            "the brick files `{name}` in the registry"
        );
    }

    let parent_writer = manager.writer("alice", "parent");
    let parent = ReActAgent::new(parent_provider, Arc::clone(&registry))
        .with_name("parent")
        .with_log_writer(Arc::clone(&parent_writer) as Arc<dyn SessionLog>);

    let input = AgentInput::text("delegate please").thread_id("parent");
    let mut stream =
        parent.run(input, Arc::new(AtomicCancelToken::new())).await;
    while stream.next().await.is_some() {}

    // The child ran under the minted id.
    let children = opened.lock().expect("lock").clone();
    assert_eq!(children, vec![child_id.to_string()]);

    // The parent's log holds the ordinary tool rows — the control
    // surface's calls are ordinary tool calls.
    let parent_rows = parent_writer.read().await.expect("parent rows");
    let parent_types = row_types(&parent_rows);
    assert!(
        parent_types.iter().filter(|t| *t == "tool_result").count() >= 3,
        "the parent records every tool result: {parent_types:?}"
    );
    assert!(
        !parent_types.iter().any(|t| t.starts_with("subagent_")),
        "no child markers in the parent log: {parent_types:?}"
    );

    // The child's session holds the whole transcript: the dispatch
    // prompt, the answer, the follow-up turn and the second answer.
    let child_log = manager.writer("alice", child_id);
    let child_types = row_types(&child_log.read().await.expect("child rows"));
    assert_eq!(
        child_types.first().map(String::as_str),
        Some("subagent_enter")
    );
    assert_eq!(child_types.get(1).map(String::as_str), Some("user_message"));
    assert!(
        child_types.iter().filter(|t| *t == "user_message").count() >= 2,
        "the follow-up turn is in the child's session: {child_types:?}"
    );
    assert!(
        child_types
            .iter()
            .filter(|t| *t == "assistant_message")
            .count()
            >= 2,
        "both answers are in the child's session: {child_types:?}"
    );

    // The roster the model saw named the child and its state.
    let roster_row = parent_rows
        .iter()
        .find(|row| {
            row["type"] == "tool_result" && row.to_string().contains("children")
        })
        .expect("the roster result");
    let roster = roster_row.to_string();
    assert!(
        roster.contains(child_id),
        "the roster names the child: {roster}"
    );
    assert!(
        roster.contains("idle"),
        "the roster says the state: {roster}"
    );
}

/// The host's session index, as the deployment implements it: the
/// only thing this test needs is where a session's directory is.
struct TestIndex {
    root: std::path::PathBuf,
}

#[async_trait]
impl uva_tool_task::SessionIndex for TestIndex {
    async fn session_ids(&self) -> Vec<String> {
        Vec::new()
    }

    async fn session_ids_of_user(&self, _user_id: &str) -> Vec<String> {
        Vec::new()
    }

    async fn read_head(
        &self,
        _id: &str,
        _limit: usize,
    ) -> Result<Vec<serde_json::Value>, uva_session::SessionError> {
        Ok(Vec::new())
    }

    async fn read_tail(
        &self,
        _id: &str,
        _limit: usize,
    ) -> Result<Vec<serde_json::Value>, uva_session::SessionError> {
        Ok(Vec::new())
    }

    fn writer(&self, _id: &str) -> Option<Arc<dyn SessionLog>> {
        None
    }

    fn session_dir(&self, id: &str) -> Option<std::path::PathBuf> {
        Some(self.root.join("alice").join(id))
    }
}

/// A dispatched child leaves its artefact set where the convention
/// says: `<session dir>/artifacts/<id>.{md,jsonl}`. The dispatch round
/// writes it; the follow-up round rewrites it with the second turn in
/// it.
#[tokio::test]
async fn a_child_leaves_its_artefact_set_in_its_session_directory() {
    let child_id = "id-0";
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = Arc::new(SessionRegistry::new(temp.path().to_path_buf()));
    manager
        .create_with_user("parent".to_string(), "alice".to_string())
        .await
        .expect("parent session");

    let peers = Arc::new(AgentRegistry::new());
    let child_provider = Arc::new(ReplayProvider::new(vec![
        text("the first answer"),
        text("the follow-up answer"),
    ]));
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
    let tool = Arc::new(
        TaskTool::new(Arc::clone(&peers))
            .with_writers(writers)
            .with_session_index(Arc::new(TestIndex {
                root: temp.path().to_path_buf(),
            }))
            .with_id_gen(uva_core::SharedIdGen::sequence()),
    );

    let registry = Arc::new(ToolRegistry::new());
    let ctx = uva_kernel::Context::new();
    let _mounted = uva_kernel::mount_all(
        &ctx,
        &[
            Arc::new(ToolRegistryPlugin::new(Arc::clone(&registry)))
                as Arc<dyn uva_kernel::Plugin>,
            Arc::new(TaskToolPlugin::new(Arc::clone(&tool))),
        ],
    )
    .expect("the brick mounts");

    let parent_writer = manager.writer("alice", "parent");
    let parent_provider =
        Arc::new(ReplayProvider::new(parent_script(child_id)));
    let parent = ReActAgent::new(parent_provider, Arc::clone(&registry))
        .with_name("parent")
        .with_log_writer(Arc::clone(&parent_writer) as Arc<dyn SessionLog>);
    let input = AgentInput::text("delegate please").thread_id("parent");
    let mut stream =
        parent.run(input, Arc::new(AtomicCancelToken::new())).await;
    while stream.next().await.is_some() {}

    let artifacts = temp.path().join("alice").join(child_id).join("artifacts");
    let markdown = artifacts.join(format!("{child_id}.md"));
    let transcript = artifacts.join(format!("{child_id}.jsonl"));
    assert!(markdown.is_file(), "the rendering landed: {markdown:?}");
    assert!(transcript.is_file(), "the transcript copy landed");
    assert!(
        !artifacts.join(format!("{child_id}.patch")).exists(),
        "a round that ran in the parent's checkout has no diff"
    );

    let rendered = std::fs::read_to_string(&markdown).expect("the rendering");
    assert!(rendered.starts_with(&format!("# Sub-agent `{child_id}`")));
    assert!(rendered.contains("| agent | `scout` |"), "{rendered}");
    assert!(rendered.contains("map the repo"), "{rendered}");
    assert!(
        rendered.contains("the follow-up answer"),
        "the set is rewritten by the follow-up round: {rendered}"
    );

    let copied = std::fs::read_to_string(&transcript).expect("the copy");
    assert!(copied.contains("assistant_message"), "{copied}");
    assert!(copied.contains("the follow-up answer"), "{copied}");
}
