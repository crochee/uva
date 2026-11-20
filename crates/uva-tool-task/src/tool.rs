//! The `task` tool itself: a plain [`Tool`] that runs a registered peer
//! agent as a child run.
//!
//! # What the tool owns
//!
//! - **Peer resolution** — the `agent` argument names an entry in the
//!   [`AgentRegistry`] the host handed the tool.
//! - **The child session** — before the child starts, the tool mints the
//!   child's session id and asks the host's [`SubagentWriters`] source
//!   to open it. Opening lands the `subagent_enter` header and the
//!   dispatch prompt; the child's own harness then writes its model
//!   facts through the writer the source returned. When the child ends,
//!   the tool appends the `subagent_exit` footer and flushes. The
//!   parent's log gets only the ordinary `tool_call` / `tool_result`
//!   rows — the child session is the one full-fidelity record.
//! - **Depth** — the round module decides whether this session may
//!   start a round and at what depth the child runs (see
//!   [`round::depth`]); the tool only asks it and records the child
//!   it dispatched.
//! - **One child per call** — the tool is
//!   [`ExecutionMode::Sequential`], so the loop serialises its calls
//!   within a batch: one `task` call starts one child run, and several
//!   `task` calls in one batch run one after another. Delegating a
//!   whole run is what makes a batch's parallel bucket the wrong
//!   home — parallel calls would change the loop's cancellation and
//!   batch semantics, not just its timing.
//! - **Cancellation** — the run's token, read from
//!   [`uva_tool::Context::cancel`], is handed to the child run. A
//!   cancelled parent therefore ends its children; a child that ignores
//!   the token is the child's bug, not a second cancellation channel.
//!
//! # What the tool does not own
//!
//! The writer source. A host that wants durable child sessions installs
//! one with [`TaskTool::with_writers`]; without it the child streams
//! live but persists nothing, exactly like a run without a writer.

use std::{path::PathBuf, sync::Arc, time::Instant};

use futures::future::{BoxFuture, Either};
use uva_agent::{AgentRegistry, ChannelInbox, SubagentWriters};
use uva_core::{
    CancelToken,
    IdGen,
    SharedIdGen,
    Sleeper,
    ThreadSleeper,
    spawn::SharedSpawner,
};
use uva_provider::ToolDefinition;
use uva_session::SessionLog;
use uva_tool::{
    Context,
    ExecutionMode,
    StreamOutput,
    Tool,
    ToolFeatures,
    ToolOutput,
};

use crate::{
    args::tool_definition,
    child::ChildCancel,
    control::{ChildHandle, ChildState, ControlSurface, RoundOutcome},
    gate::{CommandRunner, StdCommandRunner, apply_gate_to_output, run_gate},
    progress::{L1Feed, PROGRESS_WINDOW},
    round::{ChildRound, Isolation, RoundInput, depth},
    spec::TaskSpec,
    worktree::{self, WorktreeCleanupResult, WorktreeIsolation},
};

/// The model-facing `task` tool.
///
/// Build it with [`TaskTool::new`] (the peer catalog), chain the host's
/// child-session writer source with [`TaskTool::with_writers`], and file
/// it in a [`ToolRegistry`](uva_tool::ToolRegistry) — typically through
/// [`TaskToolPlugin`](crate::TaskToolPlugin), which withdraws the
/// registration when it unloads.
pub struct TaskTool {
    peers: Arc<AgentRegistry>,
    writers: Option<Arc<dyn SubagentWriters>>,
    runner: Arc<dyn CommandRunner>,
    definition: ToolDefinition,
    id_gen: SharedIdGen,
    control: Arc<ControlSurface>,
    round: ChildRound,
    sleep: Arc<dyn Sleeper>,
}

impl std::fmt::Debug for TaskTool {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TaskTool")
            .field("peers", &self.peers.names())
            .field("writers", &self.writers.is_some())
            .finish_non_exhaustive()
    }
}

impl TaskTool {
    /// The tool resolving peers from `registry`.
    #[must_use]
    pub fn new(peers: Arc<AgentRegistry>) -> Self {
        let control = Arc::new(ControlSurface::new(Arc::clone(&peers)));
        Self {
            round: ChildRound::new(Arc::clone(&control)),
            control,
            peers,
            writers: None,
            runner: Arc::new(StdCommandRunner::new()),
            definition: tool_definition(&ToolFeatures::isolation()),
            id_gen: SharedIdGen::ulid(),
            sleep: Arc::new(ThreadSleeper),
        }
    }

    /// Install the host's child-session writer source.
    #[must_use]
    pub fn with_writers(mut self, writers: Arc<dyn SubagentWriters>) -> Self {
        self.writers = Some(writers);
        self
    }

    /// Install the command seam the gate and the worktree lifecycle use.
    #[must_use]
    pub fn with_command_runner(
        mut self,
        runner: Arc<dyn CommandRunner>,
    ) -> Self {
        self.runner = runner;
        self
    }

    /// Advertise a different feature set in the tool schema.
    #[must_use]
    pub fn with_features(mut self, features: &ToolFeatures) -> Self {
        self.definition = tool_definition(features);
        self
    }

    /// Install the id generator that mints child session ids. Tests use
    /// [`SharedIdGen::sequence`].
    #[must_use]
    pub fn with_id_gen(mut self, id_gen: SharedIdGen) -> Self {
        self.id_gen = id_gen;
        self
    }

    /// Install the delay seam the L1 progress window waits on.
    ///
    /// A `task` call streams one merged child-activity line per 150 ms
    /// window and arms that window on this sleeper. The default is the
    /// runtime-free [`ThreadSleeper`](uva_core::ThreadSleeper); a host
    /// on a runtime installs that runtime's sleeper instead (its wait
    /// must not block the polling thread, as [`Sleeper`] requires).
    #[must_use]
    pub fn with_sleeper(mut self, sleep: Arc<dyn Sleeper>) -> Self {
        self.sleep = sleep;
        self
    }

    /// Install the spawner a round's tail runs on when the round's
    /// dispatch future is dropped.
    ///
    /// The tail (exit footer + artefact set) of an abandoned round
    /// cannot be awaited — `Drop` is synchronous — so it is detached
    /// here. The default is the runtime-free
    /// [`InlineSpawner`](uva_core::spawn::InlineSpawner); a host on a
    /// runtime installs its own, like every other spawn seam.
    #[must_use]
    pub fn with_spawner(self, spawner: SharedSpawner) -> Self {
        self.control.set_spawner(spawner);
        self
    }

    /// Install the deployment's sub-agent usage account.
    ///
    /// A child round runs on its own harness, so its token usage and
    /// its terminal event never reach the parent session's controller.
    /// The tool reports them here while it drains the child, which is
    /// what keeps a delegated round in the deployment's usage counters.
    #[must_use]
    pub fn with_usage(self, usage: Arc<dyn uva_agent::SubagentUsage>) -> Self {
        self.control.set_usage(usage);
        self
    }

    /// The peer catalog this tool resolves `agent` names from.
    #[must_use]
    pub fn peers(&self) -> &Arc<AgentRegistry> {
        &self.peers
    }

    /// Install the host's session index.
    ///
    /// The index is how the control surface reads the sessions a
    /// delegation touched: the roster falls back to it, and a cold
    /// wake resolves the woken child's peer and writer through it.
    /// A host that keeps its sessions elsewhere implements the trait
    /// over its own store.
    #[must_use]
    pub fn with_session_index(
        self,
        index: Arc<dyn crate::control::SessionIndex>,
    ) -> Self {
        self.control.set_index(index);
        self
    }

    /// The control surface this tool's children are driven through.
    ///
    /// The surface owns the handle table every child is filed in, and
    /// the three tools (`agents_list` / `agents_message` /
    /// `agents_interrupt`) that read it.
    #[must_use]
    pub(crate) fn control(&self) -> &Arc<ControlSurface> {
        &self.control
    }
}

#[async_trait::async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        &self.definition.name
    }

    fn description(&self) -> &str {
        &self.definition.description
    }

    fn parameters(&self) -> serde_json::Value {
        self.definition.input_schema.clone()
    }

    fn mode(&self) -> ExecutionMode {
        // One `task` call starts one child run, and the batch
        // scheduler serialises `task` calls: a batch's parallel bucket
        // would run whole runs concurrently, which changes the
        // cancellation and batch semantics rather than just the
        // timing. Whether `task` may share that bucket is a decision a
        // plugin makes, not a default.
        ExecutionMode::Sequential
    }

    async fn call(
        &self,
        input: serde_json::Value,
        context: &Context,
    ) -> ToolOutput {
        self.dispatch(input, context, None).await
    }

    /// Yield the child's L1 summary while the dispatch runs, then its
    /// result.
    ///
    /// The summary lines are merged by the call's feed and handed out
    /// at most once per 150 ms window, so a chatty child costs the
    /// parent's live-event buffer a bounded number of chunks instead of
    /// one per child event. A child that finishes inside one window
    /// still leaves a final summary before its result.
    fn stream<'a>(
        &'a self,
        input: serde_json::Value,
        context: &'a Context,
    ) -> std::pin::Pin<Box<dyn futures::Stream<Item = StreamOutput> + Send + 'a>>
    {
        let feed = Arc::new(L1Feed::default());
        let state = ProgressStream {
            tool: self,
            context,
            feed: Arc::clone(&feed),
            sleep: Arc::clone(&self.sleep),
            dispatch: Some(Box::pin(self.dispatch(input, context, Some(feed)))),
            terminal: None,
        };
        Box::pin(futures::stream::unfold(state, |mut state| async move {
            loop {
                if let Some(output) = state.terminal.take() {
                    return Some((StreamOutput::Result(output), state));
                }
                let finished = {
                    let dispatch = state.dispatch.as_mut()?;
                    let window = state.sleep.sleep(PROGRESS_WINDOW);
                    futures::pin_mut!(window);
                    match futures::future::select(dispatch, window).await {
                        Either::Left((output, _)) => Some(output),
                        Either::Right(((), _)) => None,
                    }
                };
                if let Some(output) = finished {
                    state.dispatch = None;
                    state.terminal = Some(output);
                    if let Some(mut summary) = state.feed.flush(Instant::now())
                    {
                        state.tool.truncate(&mut summary, state.context).await;
                        return Some((StreamOutput::Progress(summary), state));
                    }
                } else if let Some(mut summary) =
                    state.feed.poll(Instant::now())
                {
                    state.tool.truncate(&mut summary, state.context).await;
                    return Some((StreamOutput::Progress(summary), state));
                }
            }
        }))
    }
}

/// The streaming path's state: one dispatch future, the merge window's
/// timer seam, and the summary still owed before the result.
struct ProgressStream<'a> {
    tool: &'a TaskTool,
    context: &'a Context,
    feed: Arc<L1Feed>,
    sleep: Arc<dyn Sleeper>,
    dispatch: Option<BoxFuture<'a, ToolOutput>>,
    terminal: Option<ToolOutput>,
}

impl TaskTool {
    /// One dispatch, with the L1 feed the streaming path reads.
    ///
    /// `call` passes `None` (no progress sink); `stream` passes the
    /// call's feed.
    pub(crate) async fn dispatch(
        &self,
        input: serde_json::Value,
        context: &Context,
        feed: Option<Arc<L1Feed>>,
    ) -> ToolOutput {
        // The lifecycle advances on every dispatch: idle children past
        // the TTL park here too, and the parking loop starts when the
        // host installed its seams.
        self.control.tick();
        let spec = match TaskSpec::from_value(&input) {
            Ok(spec) => spec,
            Err(message) => return ToolOutput::error(message),
        };
        let parent_id = context.session_id.as_str();
        // The depth rule: the module decides whether this session may
        // start a round, and at what depth the child runs. The tool
        // does not count layers itself — and it holds no handle on the
        // depth table of its own: the surface owns the one table.
        let depths = self.control.depth_ledger();
        let child_depth = match depth::admit_child(&depths, parent_id) {
            Ok(child_depth) => child_depth,
            Err(refusal) => {
                tracing::warn!(
                    session_id = parent_id,
                    depth = refusal.depth(),
                    "task: sub-agent depth limit reached"
                );
                return ToolOutput::error(refusal.delegation());
            }
        };
        let peer = match self.peers.resolve_sync(&spec.agent) {
            Some(peer) => peer,
            None => {
                let names = self.peers.names().join(", ");
                return ToolOutput::error(format!(
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

        let child_id = self.id_gen.next_id();
        // The accounting: the child session runs at the depth the
        // module handed out, so a nested `task` call reads it back.
        depth::record_child(&depths, &child_id, child_depth);
        self.run_child(peer, spec, context, parent_id, &child_id, feed)
            .await
    }
}

impl TaskTool {
    async fn run_child(
        &self,
        peer: Arc<dyn uva_agent::Agent>,
        spec: TaskSpec,
        context: &Context,
        parent_id: &str,
        child_id: &str,
        feed: Option<Arc<L1Feed>>,
    ) -> ToolOutput {
        // The depth was recorded at mint time; the child run reads it
        // back from the module instead of threading a sixth parameter
        // through.
        let child_depth =
            depth::depth_of(&self.control.depth_ledger(), child_id);
        let base_cwd = if context.workspace_root.as_os_str().is_empty() {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        } else {
            context.workspace_root.clone()
        };
        // The worktree's lifecycle is the caller's: whether this
        // delegation is isolated is this decision, not the round's.
        // The round only asks its isolation where to run and what it
        // changed.
        let mut isolation: Option<Arc<WorktreeIsolation>> = None;
        if spec.isolation.is_some() {
            match worktree::create_worktree(&base_cwd, self.runner.as_ref()) {
                Ok(info) => {
                    isolation = Some(Arc::new(WorktreeIsolation::new(
                        info,
                        Arc::clone(&self.runner),
                    )));
                }
                Err(err) => {
                    return ToolOutput::error(format!(
                        "failed to set up worktree for sub-agent `{}`: \
                         {err}",
                        spec.agent
                    ));
                }
            }
        }

        // The child's session opens before its first row can be
        // stamped: the enter header and dispatch prompt land in order.
        let child_log: Option<Arc<dyn SessionLog>> =
            self.writers.as_ref().map(|writers| {
                writers.open_child(
                    child_id,
                    parent_id,
                    child_depth,
                    &spec.agent,
                    Some(&spec.prompt),
                )
            });

        // The child's own handles: the steer channel the control
        // surface writes into, and the cancellation token its own
        // interrupt fires. The token stays linked to the parent's, so
        // cancelling the parent still ends the child.
        let (inbox, steer) = ChannelInbox::channel();
        let cancel = ChildCancel::new(Arc::clone(&context.cancel));
        let mut handle = ChildHandle::new(
            ChildState::spawn(child_id, &spec.agent),
            Arc::clone(&cancel) as Arc<dyn CancelToken>,
        )
        .with_steer(steer)
        .with_peer(Arc::clone(&peer))
        .with_depth(child_depth);
        if let Some(log) = child_log.as_ref() {
            handle = handle.with_writer(Arc::clone(log));
        }
        self.control.registry().register(parent_id, handle);

        // The round itself: the module assembles the child's input,
        // drains the peer, maps the end reason into the roster's
        // vocabulary and lands the tail.
        let result = self
            .round
            .run(RoundInput {
                peer,
                prompt: spec.prompt.clone(),
                parent_id: parent_id.to_string(),
                child_id: child_id.to_string(),
                agent: spec.agent.clone(),
                depth: child_depth,
                writer: child_log,
                inbox,
                cancel,
                isolation: isolation
                    .clone()
                    .map(|isolation| isolation.as_isolation()),
                feed,
            })
            .await;

        // The gate is the parent's verdict on the child, not part of
        // the child's run: the child's session has already closed.
        let gate_cwd = isolation
            .as_ref()
            .map(|isolation| isolation.path().to_path_buf())
            .unwrap_or_else(|| base_cwd.clone());

        if let Some(gate_spec) = &spec.gate {
            let verdict = match run_gate(
                gate_spec,
                &gate_cwd,
                self.runner.as_ref(),
                Arc::clone(&context.cancel),
            ) {
                Ok(verdict) => verdict,
                Err(err) => {
                    cleanup(
                        &base_cwd,
                        &mut isolation,
                        &spec.agent,
                        &self.runner,
                    );
                    return ToolOutput::error(format!(
                        "failed gate `{}`: {err}",
                        gate_spec.label()
                    ));
                }
            };
            if verdict.is_failure() {
                let output = apply_gate_to_output(
                    result.text.as_deref(),
                    &verdict,
                    gate_spec.label(),
                );
                cleanup(&base_cwd, &mut isolation, &spec.agent, &self.runner);
                return output;
            }
        }

        let cleanup_report: Option<WorktreeCleanupResult> =
            isolation.take().map(|isolation| {
                worktree::cleanup_worktree(
                    &base_cwd,
                    isolation.info(),
                    &spec.agent,
                    self.runner.as_ref(),
                )
            });

        if result.outcome != RoundOutcome::Completed {
            return ToolOutput::error(result.failure());
        }
        // The worktree's report is the delegation's own line: it
        // lands between the answer and the round's handle line.
        let worktree_note = cleanup_report
            .as_ref()
            .filter(|report| report.has_changes)
            .and_then(|report| {
                match (report.branch.as_ref(), report.path.as_ref()) {
                    (Some(branch), Some(path)) => Some(format!(
                        "\n\n[worktree: branch `{branch}` at {path}, \
                         base {}]",
                        report.base_sha
                    )),
                    _ => None,
                }
            });
        match result.answer(worktree_note.as_deref()) {
            Some(text) => ToolOutput::text(text),
            None => ToolOutput::error(result.failure()),
        }
    }
}

fn cleanup(
    base_cwd: &std::path::Path,
    isolation: &mut Option<Arc<WorktreeIsolation>>,
    agent: &str,
    runner: &Arc<dyn CommandRunner>,
) {
    if let Some(isolation) = isolation.take() {
        let _ = worktree::cleanup_worktree(
            base_cwd,
            isolation.info(),
            agent,
            runner.as_ref(),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use uva_agent::{Agent, AgentEntry, AgentInput};
    use uva_core::{Registry, agent::AgentDescriptor};
    use uva_events::{AgentEvent, SessionEndReason};
    use uva_provider::ContentPart;

    use super::*;

    fn descriptor(name: &str) -> AgentDescriptor {
        let mut descriptor = uva_harness::default_descriptor(String::new());
        descriptor.name = name.to_string();
        descriptor
    }

    /// A peer that never finishes on its own; its `run` records the
    /// cancellation token it received and waits for it.
    struct BlockingAgent {
        descriptor: AgentDescriptor,
        started: Arc<AtomicUsize>,
        cancelled: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl Agent for BlockingAgent {
        fn descriptor(&self) -> &AgentDescriptor {
            &self.descriptor
        }

        async fn run(
            &self,
            _input: AgentInput,
            cancel: Arc<dyn CancelToken>,
        ) -> std::pin::Pin<
            Box<dyn futures::Stream<Item = AgentEvent> + Send + 'static>,
        > {
            self.started.fetch_add(1, Ordering::SeqCst);
            let cancelled = Arc::clone(&self.cancelled);
            Box::pin(futures::stream::once(async move {
                cancel.cancelled().await;
                cancelled.fetch_add(1, Ordering::SeqCst);
                AgentEvent::System(uva_events::SystemEvent::SessionEnded {
                    reason: SessionEndReason::Cancelled,
                })
            }))
        }
    }

    impl uva_core::registry::RegistryItem for BlockingAgent {
        fn name(&self) -> &str {
            &self.descriptor.name
        }

        fn description(&self) -> &str {
            &self.descriptor.description
        }
    }

    #[test]
    fn depth_map_seeds_from_the_parent_session() {
        let ledger = crate::round::depth::DepthLedger::new();
        assert_eq!(depth::depth_of(&ledger, "parent"), 0);
        depth::record_child(&ledger, "child", 1);
        assert_eq!(depth::depth_of(&ledger, "child"), 1);
        depth::forget(&ledger, "child");
        assert_eq!(
            depth::depth_of(&ledger, "child"),
            0,
            "a parked child is pruned"
        );
    }

    /// Dropping the dispatch future — what the harness does when the
    /// parent run is cancelled mid-call — must still close the round: a
    /// roster row stuck at `running` is what makes `agents_list` lie,
    /// and a child session without its `subagent_exit` footer breaks
    /// the enter/exit pairing the log format promises.
    #[tokio::test]
    async fn a_dropped_dispatch_future_still_closes_the_round() {
        use uva_agent::SubagentWriters;
        use uva_core::SystemClock;
        use uva_session::{
            LogWriter,
            SessionLog,
            SessionSink,
            in_memory::InMemorySessionSink,
        };

        struct Writers {
            sink: Arc<InMemorySessionSink>,
        }

        impl SubagentWriters for Writers {
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
                    Arc::new(SystemClock),
                ))
            }
        }

        let peers = Arc::new(AgentRegistry::new());
        let started = Arc::new(AtomicUsize::new(0));
        let agent: Arc<dyn Agent> = Arc::new(BlockingAgent {
            descriptor: descriptor("stuck"),
            started: Arc::clone(&started),
            cancelled: Arc::new(AtomicUsize::new(0)),
        });
        peers.put(AgentEntry::new(agent)).await.expect("register");

        let sink = Arc::new(InMemorySessionSink::new("child"));
        let tool = Arc::new(TaskTool::new(Arc::clone(&peers)).with_writers(
            Arc::new(Writers {
                sink: Arc::clone(&sink),
            }),
        ));
        let ctx = Context::new("dropper".into(), PathBuf::from("/tmp"));
        let dispatch = tokio::spawn({
            let tool = Arc::clone(&tool);
            let ctx = ctx.clone();
            async move {
                tool.call(
                    serde_json::json!({"agent": "stuck", "prompt": "wait"}),
                    &ctx,
                )
                .await
            }
        });

        // Wait until the child is genuinely in flight, then drop the
        // future the way the harness does on cancel.
        for _ in 0..5_000 {
            if started.load(Ordering::SeqCst) == 1
                && !tool.control().registry().list("dropper").is_empty()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        dispatch.abort();
        let _ = dispatch.await;

        let rows = tool.control().registry().list("dropper");
        assert_eq!(rows.len(), 1, "the row outlives the dispatch");
        assert_eq!(
            rows[0].status,
            crate::control::ChildStatus::Idle,
            "a dropped dispatch settles the row instead of leaving it \
             running: {rows:?}"
        );
        assert_eq!(rows[0].activity, "cancelled");

        // The writer half is detached (a `Drop` cannot await), so the
        // footer lands a moment later.
        let mut footer = None;
        for _ in 0..5_000 {
            let rows = sink.read().await.expect("read the child's rows");
            if let Some(row) =
                rows.iter().find(|row| row["type"] == "subagent_exit")
            {
                footer = Some(row.clone());
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        }
        let footer = footer.expect("the detached tail writes the footer");
        assert_eq!(footer["data"]["status"], "cancelled");
        assert_eq!(footer["data"]["depth"], 1);
    }

    #[tokio::test]
    async fn the_depth_limit_refuses_dispatch_with_a_readable_error() {
        let peers = Arc::new(AgentRegistry::new());
        let started = Arc::new(AtomicUsize::new(0));
        let agent: Arc<dyn Agent> = Arc::new(BlockingAgent {
            descriptor: descriptor("scout"),
            started: Arc::clone(&started),
            cancelled: Arc::new(AtomicUsize::new(0)),
        });
        peers.put(AgentEntry::new(agent)).await.expect("register");

        let tool = TaskTool::new(Arc::clone(&peers));
        depth::record_child(
            &tool.control().depth_ledger(),
            "deep-parent",
            crate::spec::MAX_SUBAGENT_DEPTH,
        );
        let ctx = Context::new("deep-parent".into(), PathBuf::from("/tmp"));
        let out = tool
            .call(serde_json::json!({"agent": "scout", "prompt": "go"}), &ctx)
            .await;
        assert!(out.is_error.unwrap_or(false), "{out:?}");
        let text = out
            .content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(
            text,
            "cannot delegate: maximum sub-agent depth (3) reached; the \
             `task` tool is disabled at this nesting level",
            "the refusal the model reads has not changed"
        );
        assert_eq!(
            started.load(Ordering::SeqCst),
            0,
            "a refused dispatch never starts the child"
        );
    }
}
