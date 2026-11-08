//! The isolation seam: a delegated child runs **inside** the worktree
//! the delegator created for it, not in the parent's checkout.
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
use elysper_core::{
    CancelToken,
    registry::{Registry, RegistryItem},
};
use elysper_harness::{
    Agent,
    AgentDescriptor,
    AgentEntry,
    AgentEvent,
    AgentInput,
    AgentRegistry,
    DelegatingTool,
    DelegationCall,
    SystemEvent,
};
use elysper_provider::ContentPart;
use elysper_tool::{Context, ToolOutput};
use elysper_tool_task::{
    CommandOutcome,
    CommandRunner,
    CommandStopReason,
    TaskDelegator,
};
use futures::{StreamExt, stream};

const BASE_SHA: &str = "0123456789abcdef0123456789abcdef01234567";
const CHILD_ANSWER: &str = "the isolated child ran";

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
    desc: AgentDescriptor,
    seen: Mutex<Vec<(PathBuf, bool)>>,
}

impl ProbeAgent {
    fn new() -> Self {
        Self {
            desc: AgentDescriptor {
                name: "peer".to_string(),
                description: "a probe peer".to_string(),
                kind: "react".to_string(),
                version: "1.0.0".to_string(),
                instructions: String::new(),
                capabilities: Vec::new(),
                tools: Vec::new(),
                model_hint: None,
                handoffs: Vec::new(),
                handoff_hint: None,
                output_schema: None,
                owner: None,
                domain: None,
                persona: None,
                display_name: None,
                max_iterations: None,
            },
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
    fn descriptor(&self) -> &AgentDescriptor {
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
            AgentEvent::Model(ContentPart::Text(
                elysper_provider::TextContent {
                    text: CHILD_ANSWER.to_string(),
                    cache_control: None,
                },
            )),
            AgentEvent::System(SystemEvent::SessionEnded {
                reason: elysper_harness::SessionEndReason::Completed,
            }),
        ];
        Box::pin(stream::iter(events))
    }
}

#[test]
fn the_child_runs_inside_the_worktree_the_delegator_created() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        let probe = Arc::new(ProbeAgent::new());
        let registry = AgentRegistry::new();
        registry
            .put(AgentEntry::new(probe.clone()))
            .await
            .expect("register the peer");
        let git = FakeGit::new();
        let delegator =
            TaskDelegator::new(Arc::new(registry)).with_command_runner(git);
        let (tx, mut rx) = futures::channel::mpsc::unbounded();

        let call = elysper_provider::ToolUse {
            id: "call-1".to_string(),
            name: "task".to_string(),
            input: serde_json::json!({
                "agent": "peer",
                "prompt": "do the work",
                "isolation": true,
            }),
        };
        let context = Context::new("parent".to_string(), base_dir());
        let output: ToolOutput = delegator
            .execute(DelegationCall {
                call: &call,
                cancel: Arc::new(elysper_core::AtomicCancelToken::shared()),
                depth: 0,
                context: &context,
                emit: tx,
                subagent_writers: None,
            })
            .await;
        while rx.next().await.is_some() {}

        assert!(!output.is_error.unwrap_or(false), "{output:?}");
        let text = output
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text(t) => Some(t.text.clone()),
                _ => None,
            })
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
                .is_some_and(|name| name.starts_with("elysper-harness-")),
            "the child's workspace is the delegated worktree, not the \
             parent's checkout: {workspace:?}"
        );
    });
}
