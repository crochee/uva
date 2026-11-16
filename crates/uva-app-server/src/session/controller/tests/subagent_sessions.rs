//! A delegated child's own session transcript.
//!
//! The child's model facts — one `assistant_message` row per turn,
//! one `tool_result` row per result — are written by the child's
//! harness through the child session's writer (the `SubagentWriters`
//! source the boot hands the `task` tool), not copied from the
//! parent's event stream. The router contributes only the
//! `subagent_enter` / dispatch-prompt / `subagent_exit` frame.

use std::sync::Arc;

use serde_json::json;
use uva::{
    core::registry::Registry,
    harness::{Agent, AgentEntry, AgentRegistry, ReActAgent},
    provider::{ContentPart, StreamChunk, TextContent},
};

use super::{
    super::{
        AgentRunStreamFactory,
        SessionController,
        SessionOp,
        SessionState,
    },
    support::{
        ScriptedStreamProvider,
        find_all_events_jsonl,
        wait_for_log_row,
    },
};

fn text_chunks(text: &str) -> Vec<StreamChunk> {
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
            arguments: json!({"agent": agent, "prompt": prompt}),
        },
        StreamChunk::ToolCallEnd { id: id.to_string() },
        StreamChunk::IsDone {
            result: Box::default(),
        },
    ]
}

/// A `task` delegation leaves a complete, resumable transcript in
/// the child's own session: header, dispatch prompt, the child's
/// assistant row(s) written by the child's harness, footer. The
/// events alone persist nothing — without the writer handed down
/// by the `task` tool the child's reply would survive only as the
/// parent's `tool_result` text, not as a resumable transcript.
#[tokio::test]
async fn a_delegated_child_writes_its_own_session_transcript() {
    let parent = Arc::new(ScriptedStreamProvider::new(vec![
        task_call("t1", "scout", "map the repo"),
        text_chunks("parent done"),
    ]));
    let child = Arc::new(ScriptedStreamProvider::new(vec![text_chunks(
        "child reply",
    )]));

    let peers = Arc::new(AgentRegistry::new());
    let scout: Arc<dyn Agent> = Arc::new(
        ReActAgent::new(child, Arc::new(uva::tool::ToolRegistry::new()))
            .with_name("scout")
            // The JSONL sink needs a reactor: registered agents run
            // on the server spawner, and so must this fixture.
            .with_spawner(crate::spawner::server_spawner()),
    );
    peers.put(AgentEntry::new(scout)).await.expect("register");

    // Manual spawn (the shared helper hides the registry this
    // test needs): the router resolves child sessions through the
    // same registry, so both are built here.
    let temp = tempfile::tempdir().expect("temp workspace");
    let manager = Arc::new(uva::session::manager::SessionRegistry::new(
        temp.path().to_path_buf(),
    ));
    manager
        .create_with_user("s1".to_string(), "alice".to_string())
        .await
        .unwrap();
    let mut deps = super::support::test_deps();
    deps.provider = parent;
    deps.agent_registry = Some(Arc::clone(&peers));
    let router =
        Arc::new(crate::session::subagent_router::SubagentRouter::new(
            Arc::clone(&manager),
        ));
    // Delegation is a registry tool now: the fixture files the same
    // `task` tool the boot's `tool-task` row files, over the test's
    // router-backed writer source.
    deps.tool_registry
        .register_entry(uva::tool::ToolEntry::new(Arc::new(
            uva::tool_task::TaskTool::new(Arc::clone(&peers))
                .with_writers(Arc::new(
                crate::session::subagent_router::SessionSubagentWriters::new(
                    router,
                    Arc::clone(&manager),
                ),
            )),
        )));
    let controller = SessionController::spawn(
        "alice",
        "s1",
        manager.writer("alice", "s1"),
        deps,
        std::time::Duration::from_secs(60),
        Arc::new(AgentRunStreamFactory),
    );

    controller
        .submit(SessionOp::Prompt {
            content: "delegate please".to_string(),
            priority: 1,
        })
        .await
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(3), async {
        while controller.state() != SessionState::Idle {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("run should finish");
    // The tool closes the child session (footer + flush) before it
    // returns, so the parent's ordinary `tool_result` row proves the
    // child log is complete. The parent's own log carries no subagent
    // markers at all: the child session is the one record.
    let parent_rows = wait_for_log_row(&manager, "tool_result").await;
    assert!(
        !parent_rows.iter().any(|row| row["type"]
            .as_str()
            .is_some_and(|kind| kind.starts_with("subagent_"))),
        "the parent log holds no subagent markers: {parent_rows:?}"
    );
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while controller.state() != SessionState::Idle {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("run should finish");

    // The child's session is a separate events.jsonl under the same
    // sessions root, in a directory named after the child's session
    // id. The parent's log carries a `subagent_enter` marker too, so
    // the marker alone cannot tell the two apart: a log belongs to the
    // child when the id its own enter header names is the id its
    // directory is named after. Candidates are sorted, because the
    // walk follows the filesystem's readdir order.
    let child_log = find_all_events_jsonl(temp.path())
        .into_iter()
        .filter(|path| {
            let Ok(body) = std::fs::read_to_string(path) else {
                return false;
            };
            let own_id = path
                .parent()
                .and_then(|dir| dir.file_name())
                .and_then(|name| name.to_str());
            body.lines()
                .filter_map(|line| {
                    serde_json::from_str::<serde_json::Value>(line).ok()
                })
                .any(|row| {
                    row["type"] == "subagent_enter"
                        && row
                            .pointer("/data/child_session_id")
                            .and_then(|id| id.as_str())
                            == own_id
                })
        })
        .map(|path| std::fs::read_to_string(path).expect("child log"))
        .next()
        .expect("the child session's log exists")
        .lines()
        .map(serde_json::from_str::<serde_json::Value>)
        .map(|row| row.expect("row parses"))
        .collect::<Vec<_>>();
    let types: Vec<&str> = child_log
        .iter()
        // The first line is the file's `_meta` header, not a row.
        .filter(|row| row.get("type").is_some())
        .map(|row| row["type"].as_str().unwrap_or("?"))
        .collect();
    assert_eq!(
        types,
        vec![
            "subagent_enter",
            "user_message",
            // The child's own run follows, exactly like a
            // top-level session's: the turn boundary that lets a crash
            // be repaired, the request envelope snapshot it is checked
            // against on a resume, its request header, its step
            // boundaries, and — the point of this test — its assistant
            // turn, written by the child's harness through the child's
            // own writer.
            "turn",
            "request_snapshot",
            "request_header",
            "step",
            "assistant_message",
            "usage",
            "step",
            "turn",
            "subagent_exit",
        ],
        "child log: header, prompt, the child's own rows, footer; \
         got {child_log:?}"
    );
    let assistant = child_log
        .iter()
        .find(|row| row["type"] == "assistant_message")
        .expect("the child's assistant row");
    assert!(
        assistant.to_string().contains("child reply"),
        "the child's harness wrote the child's reply: {assistant}"
    );

    controller.close().await.unwrap();
}
