//! The isolation seam: a delegated child runs **inside** the worktree
//! the `task` tool created for it, not in the parent's checkout.
//!
//! A scripted `git` runner stands in for the real commands (it creates
//! the directory a real `git worktree add` would), and a probe agent
//! records the workspace it was handed while asserting the directory
//! exists at that moment. The failure this pins is the one that let
//! "run in isolation" mean nothing: the worktree used to be created
//! *after* the child finished.

use std::{
    os::unix::process::ExitStatusExt,
    path::{Path, PathBuf},
    process::ExitStatus,
    sync::{Arc, Mutex},
};

use async_trait::async_trait;
use futures::stream;
use uva_agent::{Agent, AgentInput, AgentRegistry};
use uva_core::{
    CancelToken,
    registry::{Registry, RegistryItem},
};
use uva_events::{AgentEvent, SessionEndReason, SystemEvent};
use uva_provider::ContentPart;
use uva_tool::{Context, Tool};
use uva_tool_task::{
    CommandOutcome,
    CommandRunner,
    CommandStopReason,
    TaskTool,
};

const BASE_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const CHILD_ANSWER: &str = "the isolated child ran";
/// The diff the fake `git` reports for an isolated round.
const DIFF: &str = "diff --git a/src/x.rs b/src/x.rs\n+changed\n";

/// The directory the "repo" lives in; the fake `git` only ever needs
/// it as a `cwd`.
fn base_dir() -> PathBuf {
    std::env::temp_dir()
}

/// A `git` that behaves like the handful of commands the worktree
/// lifecycle issues, and creates the directory `git worktree add`
/// would create.
struct FakeGit {
    runs: Mutex<Vec<Vec<String>>>,
}

impl FakeGit {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            runs: Mutex::new(Vec::new()),
        })
    }
}

impl CommandRunner for FakeGit {
    fn run(
        &self,
        argv: &[String],
        _cwd: &Path,
        _cancel: Arc<dyn CancelToken>,
    ) -> CommandOutcome {
        self.runs.lock().expect("lock").push(argv.to_vec());
        let mut stdout = String::new();
        if argv.get(1).map(String::as_str) == Some("rev-parse") {
            stdout = format!("{BASE_SHA}\n");
        }
        if argv.get(1).map(String::as_str) == Some("diff") {
            // What an isolated round that changed something looks
            // like: the tool reads this into the `.patch` artefact.
            stdout = DIFF.to_string();
        }
        if argv.get(1).map(String::as_str) == Some("worktree")
            && argv.get(2).map(String::as_str) == Some("add")
            && let Some(path) = argv.get(4)
        {
            // What the real command does: materialise the checkout.
            std::fs::create_dir_all(path).expect("create the worktree");
        }
        CommandOutcome {
            status: ExitStatus::from_raw(0),
            stdout,
            stderr: String::new(),
            reason: CommandStopReason::Exited,
        }
    }
}

/// Records the workspace each child run was handed, and whether the
/// directory existed at that moment.
struct ProbeAgent {
    desc: uva_core::agent::AgentDescriptor,
    seen: Mutex<Vec<(PathBuf, bool)>>,
}

impl ProbeAgent {
    fn new() -> Self {
        let mut desc = uva_harness::default_descriptor(String::new());
        desc.name = "peer".to_string();
        desc.description = "a probe peer".to_string();
        Self {
            desc,
            seen: Mutex::new(Vec::new()),
        }
    }
}

impl RegistryItem for ProbeAgent {
    fn name(&self) -> &str {
        &self.desc.name
    }

    fn description(&self) -> &str {
        &self.desc.description
    }
}

#[async_trait]
impl Agent for ProbeAgent {
    fn descriptor(&self) -> &uva_core::agent::AgentDescriptor {
        &self.desc
    }

    async fn run(
        &self,
        input: AgentInput,
        _cancel: Arc<dyn CancelToken>,
    ) -> std::pin::Pin<
        Box<dyn futures::Stream<Item = AgentEvent> + Send + 'static>,
    > {
        let workspace = input
            .workspace
            .clone()
            .unwrap_or_else(|| PathBuf::from("."));
        self.seen
            .lock()
            .expect("lock")
            .push((workspace.clone(), workspace.is_dir()));
        let events = vec![
            AgentEvent::System(SystemEvent::SessionStarted {
                session_id: "child".to_string(),
            }),
            AgentEvent::Model(ContentPart::Text(uva_provider::TextContent {
                text: CHILD_ANSWER.to_string(),
                cache_control: None,
            })),
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: SessionEndReason::Completed,
            }),
        ];
        Box::pin(stream::iter(events))
    }
}

#[tokio::test]
async fn the_child_runs_inside_the_worktree_the_tool_created() {
    let probe = Arc::new(ProbeAgent::new());
    let registry = AgentRegistry::new();
    registry
        .put(uva_agent::AgentEntry::new(probe.clone()))
        .await
        .expect("register the peer");
    let git = FakeGit::new();
    let tool = TaskTool::new(Arc::new(registry)).with_command_runner(git);

    let context = Context::new("parent".to_string(), base_dir());
    let output: uva_tool::ToolOutput = tool
        .call(
            serde_json::json!({
                "agent": "peer",
                "prompt": "do the work",
                "isolation": true,
            }),
            &context,
        )
        .await;

    assert!(!output.is_error.unwrap_or(false), "{output:?}");
    let text = output
        .content
        .iter()
        .filter_map(|part| part.text())
        .collect::<String>();
    assert!(text.contains(CHILD_ANSWER), "{text}");

    let seen = probe.seen.lock().expect("lock");
    assert_eq!(seen.len(), 1, "exactly one child run");
    let (workspace, existed) = &seen[0];
    assert!(
        *existed,
        "the worktree must exist before the child's first event; \
         got {workspace:?}"
    );
    assert!(
        workspace
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|name| name.starts_with("uva-harness-")),
        "the child's workspace is the delegated worktree, not the \
         parent's checkout: {workspace:?}"
    );
}

/// Opens child sessions the way the deployment's source does.
struct SessionWriters {
    manager: Arc<uva_session::manager::SessionRegistry>,
    parent: String,
}

impl uva_agent::SubagentWriters for SessionWriters {
    fn open_child(
        &self,
        child_session_id: &str,
        _parent_session_id: &str,
        depth: usize,
        agent_name: &str,
        prompt: Option<&str>,
    ) -> Arc<dyn uva_session::SessionLog> {
        let writer = self.manager.writer("alice", child_session_id);
        writer.append_event_nowait(&uva_session::subagent_enter(
            child_session_id,
            &self.parent,
            u32::try_from(depth).unwrap_or(u32::MAX),
            agent_name,
        ));
        if let Some(prompt) = prompt.filter(|prompt| !prompt.is_empty())
            && let Ok(data) =
                serde_json::to_value(uva_provider::Message::user(prompt))
        {
            writer.append_event_nowait(&uva_session::user_message(data, None));
        }
        writer
    }
}

/// The deployment's session index, as far as the artefact set needs
/// it: where a session's directory is.
struct RootIndex {
    root: PathBuf,
}

#[async_trait]
impl uva_tool_task::SessionIndex for RootIndex {
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

    fn writer(&self, _id: &str) -> Option<Arc<dyn uva_session::SessionLog>> {
        None
    }

    fn session_dir(&self, id: &str) -> Option<PathBuf> {
        Some(self.root.join("alice").join(id))
    }
}

/// An isolated round that changed files leaves its diff as the third
/// artefact of the set.
#[tokio::test]
async fn an_isolated_round_leaves_its_diff_in_the_artefact_set() {
    let temp = tempfile::tempdir().expect("tempdir");
    let manager = Arc::new(uva_session::manager::SessionRegistry::new(
        temp.path().to_path_buf(),
    ));

    let probe = Arc::new(ProbeAgent::new());
    let registry = AgentRegistry::new();
    registry
        .put(uva_agent::AgentEntry::new(probe))
        .await
        .expect("register the peer");
    let git = FakeGit::new();
    let tool = TaskTool::new(Arc::new(registry))
        .with_command_runner(git)
        .with_writers(Arc::new(SessionWriters {
            manager: Arc::clone(&manager),
            parent: "parent".to_string(),
        }))
        .with_session_index(Arc::new(RootIndex {
            root: temp.path().to_path_buf(),
        }))
        .with_id_gen(uva_core::SharedIdGen::sequence());

    let context = Context::new("parent".to_string(), base_dir());
    let output = tool
        .call(
            serde_json::json!({
                "agent": "peer",
                "prompt": "do the work",
                "isolation": true,
            }),
            &context,
        )
        .await;
    assert!(!output.is_error.unwrap_or(false), "{output:?}");

    let artifacts = temp.path().join("alice").join("id-0").join("artifacts");
    let patch = artifacts.join("id-0.patch");
    assert!(patch.is_file(), "the diff artefact landed: {patch:?}");
    assert_eq!(
        std::fs::read_to_string(&patch).expect("the patch"),
        DIFF,
        "the patch is the round's diff against its base sha"
    );
    assert!(artifacts.join("id-0.md").is_file());
    assert!(artifacts.join("id-0.jsonl").is_file());
}
