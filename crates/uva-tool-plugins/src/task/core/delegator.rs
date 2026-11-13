//! The `task` tool as a harness plugin.
//!
//! Wraps an [`AgentRegistry`] of peer agents and implements
//! [`DelegatingTool`]: installed on a
//! [`ReActAgent`](uva_harness::ReActAgent) via
//! `with_delegator`, the model sees a `task(agent, prompt)`
//! tool; calls resolve the named peer, run it as a
//! depth-capped child, forward its events, and commit its
//! final text as the tool result. Not installed, the agent
//! has no `task` tool at all.

use std::sync::Arc;

use tracing::warn;
use uva_core::CancelToken;
use uva_harness::{AgentEvent, AgentRegistry, DelegatingTool, DelegationCall};
use uva_provider::{ToolDefinition, ToolUse};
use uva_tool::ToolFeatures;

use super::{
    runner::{HostContext, run_subagent_with_runner},
    schema::task_tool_definition_with_features,
    spec::{MAX_SUBAGENT_DEPTH, TaskSpec},
};
use crate::task::gate::{CommandRunner, StdCommandRunner};

#[derive(Clone)]
pub struct TaskDelegator {
    peers: Arc<AgentRegistry>,
    /// The `git` / shell seam the gate and the worktree lifecycle
    /// run through. Production uses [`StdCommandRunner`]; a test
    /// installs a recording fake with
    /// [`TaskDelegator::with_command_runner`].
    runner: Arc<dyn CommandRunner>,
}

impl TaskDelegator {
    /// Delegator resolving peers from `registry`.
    #[must_use]
    pub fn new(peers: Arc<AgentRegistry>) -> Self {
        Self {
            peers,
            runner: Arc::new(StdCommandRunner::new()),
        }
    }

    /// Install the command seam the gate and the worktree lifecycle
    /// use.
    #[must_use]
    pub fn with_command_runner(
        mut self,
        runner: Arc<dyn CommandRunner>,
    ) -> Self {
        self.runner = runner;
        self
    }

    /// The peer catalog this delegator resolves names from.
    #[must_use]
    pub fn peers(&self) -> &Arc<AgentRegistry> {
        &self.peers
    }
}

impl DelegatingTool for TaskDelegator {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![task_tool_definition_with_features(
            &ToolFeatures::isolation(),
        )]
    }

    fn execute(
        &self,
        call: DelegationCall<'_>,
    ) -> futures::future::BoxFuture<'static, uva_tool::ToolOutput> {
        let cancel = Arc::clone(&call.cancel);
        let depth = call.depth;
        let emit = call.emit.clone();
        let writers = call.subagent_writers.clone();
        let call_data = call.call.clone();
        // The child runs from its own agent's workspace root, not the
        // parent's tool context: the peer carries its own env plane.
        let _ = call.context;
        let peers = Arc::clone(&self.peers);
        let runner = Arc::clone(&self.runner);
        Box::pin(async move {
            execute_with_runner(
                peers, runner, &call_data, cancel, depth, emit, writers,
            )
            .await
        })
    }
}

async fn execute_with_runner(
    peers: Arc<AgentRegistry>,
    runner: Arc<dyn CommandRunner>,
    call: &ToolUse,
    cancel: Arc<dyn CancelToken>,
    depth: usize,
    emit: futures::channel::mpsc::UnboundedSender<AgentEvent>,
    writers: Option<Arc<dyn uva_harness::SubagentWriters>>,
) -> uva_tool::ToolOutput {
    let spec = match TaskSpec::from_value(&call.input) {
        Ok(spec) => spec,
        Err(message) => return uva_tool::ToolOutput::error(message),
    };
    if depth >= MAX_SUBAGENT_DEPTH {
        warn!(
            tool_use_id = %call.id,
            depth,
            "task_delegator: sub-agent depth limit reached"
        );
        return uva_tool::ToolOutput::error(format!(
            "cannot delegate: maximum sub-agent depth ({}) reached; \
             the `task` tool is disabled at this nesting level",
            MAX_SUBAGENT_DEPTH
        ));
    }
    let peer = match peers.resolve_sync(&spec.agent) {
        Some(peer) => peer,
        None => {
            let names = peers.names().join(", ");
            return uva_tool::ToolOutput::error(format!(
                "unknown peer agent `{}`; registered agents: {}",
                spec.agent,
                if names.is_empty() {
                    "(none)".to_string()
                } else {
                    names
                }
            ));
        }
    };
    run_subagent_with_runner(
        peer,
        spec,
        HostContext {
            parent_session_id: &call.id,
            writers,
        },
        depth,
        cancel,
        emit,
        &runner,
    )
    .await
}
