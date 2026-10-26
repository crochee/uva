//! The `task` tool as a harness plugin.
//!
//! Wraps an [`AgentRegistry`] of peer agents and implements
//! [`DelegatingTool`]: installed on a
//! [`ReActAgent`](synthia_harness::ReActAgent) via
//! `with_delegator`, the model sees a `task(agent, prompt)`
//! tool; calls resolve the named peer, run it as a
//! depth-capped child, forward its events, and commit its
//! final text as the tool result. Not installed, the agent
//! has no `task` tool at all.

use std::sync::Arc;

use synthia_core::CancelToken;
use synthia_harness::{
    AgentEvent,
    AgentRegistry,
    DelegatingTool,
    DelegationCall,
};
use synthia_provider::{ToolDefinition, ToolUse};
use tracing::{info, warn};

use super::{
    runner::{HostContext, run_subagent_with_runner},
    schema::task_tool_definition,
    spec::{MAX_SUBAGENT_DEPTH, TaskSpec},
};
use crate::gate::{CommandRunner, StdCommandRunner};

#[derive(Clone)]
pub struct TaskDelegator {
    peers: Arc<AgentRegistry>,
}

impl TaskDelegator {
    /// Delegator resolving peers from `registry`.
    #[must_use]
    pub fn new(peers: Arc<AgentRegistry>) -> Self {
        Self { peers }
    }

    /// The peer catalog this delegator resolves names from.
    #[must_use]
    pub fn peers(&self) -> &Arc<AgentRegistry> {
        &self.peers
    }

    #[allow(dead_code)]
    async fn execute_call(
        &self,
        call: &synthia_provider::ToolUse,
        cancel: Arc<dyn CancelToken>,
        depth: usize,
        emit: futures::channel::mpsc::UnboundedSender<AgentEvent>,
    ) -> synthia_tool::ToolOutput {
        let spec = match TaskSpec::from_value(&call.input) {
            Ok(spec) => spec,
            Err(message) => return synthia_tool::ToolOutput::error(message),
        };

        if depth >= MAX_SUBAGENT_DEPTH {
            warn!(
                tool_use_id = %call.id,
                depth,
                "task_delegator: sub-agent depth limit reached"
            );
            #[cfg(feature = "otel")]
            synthia_telemetry::record_subagent(
                "depth_exceeded",
                if spec.isolation.is_some() {
                    "worktree"
                } else {
                    "none"
                },
            );
            return synthia_tool::ToolOutput::error(format!(
                "cannot delegate: maximum sub-agent depth ({}) reached; \
                 the `task` tool is disabled at this nesting level",
                MAX_SUBAGENT_DEPTH
            ));
        }

        let peer = match self.peers.resolve_sync(&spec.agent) {
            Some(peer) => peer,
            None => {
                let names = self.peers.names().join(", ");
                #[cfg(feature = "otel")]
                synthia_telemetry::record_subagent("failed", "none");
                return synthia_tool::ToolOutput::error(format!(
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

        // R-OPT-1 Step 5.5: typed `SpanKind::Handoff` span that
        // brackets the whole sub-agent run, with the OTel metric
        // (`record_subagent`) emitted before the spawn. The
        // metric counter is bound to the `from_agent`/`to_agent`
        // keys via the harness-level sub-agent surface; here we
        // only carry the generic outcome labels.
        let peer_name = peer.descriptor().name.clone();
        let span = tracing::info_span!(
            "task_delegate",
            synthia.span.name = "task_delegate",
            synthia.span.kind = synthia_telemetry::SpanKind::Handoff.as_str(),
            otel.kind = synthia_telemetry::SpanKind::Handoff.otel_kind(),
            from_agent = "parent",
            to_agent = %peer_name,
            depth = depth + 1,
            tool_use_id = %call.id,
        );
        let _enter = span.enter();

        info!(
            tool_name = %call.name,
            tool_use_id = %call.id,
            peer = %spec.agent,
            depth = depth + 1,
            "task_delegator: spawning sub-agent"
        );

        #[cfg_attr(not(feature = "otel"), allow(unused_variables))]
        let isolation = if spec.isolation.is_some() {
            "worktree"
        } else {
            "none"
        };
        #[cfg(feature = "otel")]
        synthia_telemetry::record_subagent("spawned", isolation);

        // R-OPT-1 Step 6.2: capture the current OTel span
        // context into the wire shim so the child run chains
        // its spans into this delegation's trace tree. Done
        // *inside* the Handoff span, after `_enter`, so the
        // captured `traceparent` reflects this hop.
        //
        // Without the `otel` feature on this crate, the
        // ambient context is the global no-op and we leave
        // `trace_context` unset (the child starts a root).
        // This is the same acceptable-degradation plan note.
        #[cfg(feature = "otel")]
        let mut spec = spec;
        #[cfg(feature = "otel")]
        {
            spec.trace_context = current_trace_context_wire();
        }

        // `parent_session_id` is empty at this layer (the server
        // assigns session ids); delegation to nested children
        // preserves depth bookkeeping regardless.
        let parent_session_id = String::new();
        let runner: Arc<dyn CommandRunner> = Arc::new(StdCommandRunner::new());
        let outcome = run_subagent_with_runner(
            peer,
            spec,
            HostContext {
                parent_session_id: &parent_session_id,
                writers: None,
            },
            depth,
            cancel,
            emit,
            &runner,
        )
        .await;
        #[cfg(feature = "otel")]
        match outcome.is_error.unwrap_or(false) {
            true => synthia_telemetry::record_subagent("failed", isolation),
            false => synthia_telemetry::record_subagent("completed", isolation),
        }
        outcome
    }
}

/// Snapshot the current OTel span context into the wire shim
/// form. Returns `None` when no exporter is configured or
/// when the ambient context is the no-op default — the child
/// then starts a root span, which OTel collapses into an
/// independent trace.
#[cfg(feature = "otel")]
#[allow(dead_code)]
fn current_trace_context_wire() -> Option<crate::task::spec::TaskTraceContext> {
    let cx = opentelemetry::Context::current();
    use opentelemetry::trace::TraceContextExt;
    let span_ref = cx.span();
    let span_cx = span_ref.span_context();
    if !span_cx.is_valid() {
        return None;
    }
    let trace_id_hex = format!("{:032x}", span_cx.trace_id());
    let parent_span_id_hex = format!("{:016x}", span_cx.span_id());
    Some(crate::task::spec::TaskTraceContext {
        trace_id_hex,
        parent_span_id_hex,
        trace_flags_byte: span_cx.trace_flags().to_u8(),
        trace_state: span_cx.trace_state().header(),
    })
}

impl DelegatingTool for TaskDelegator {
    fn definitions(&self) -> Vec<ToolDefinition> {
        vec![task_tool_definition()]
    }

    fn execute(
        &self,
        call: DelegationCall<'_>,
    ) -> futures::future::BoxFuture<'static, synthia_tool::ToolOutput> {
        let cancel = Arc::clone(&call.cancel);
        let depth = call.depth;
        let emit = call.emit.clone();
        let writers = call.subagent_writers.clone();
        let call_data = call.call.clone();
        // The child runs from its own agent's workspace root, not the
        // parent's tool context: the peer carries its own env plane.
        let _ = call.context;
        let peers = Arc::clone(&self.peers);
        let runner: Arc<dyn CommandRunner> = Arc::new(StdCommandRunner::new());
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
    writers: Option<Arc<dyn synthia_harness::SubagentWriters>>,
) -> synthia_tool::ToolOutput {
    let spec = match TaskSpec::from_value(&call.input) {
        Ok(spec) => spec,
        Err(message) => return synthia_tool::ToolOutput::error(message),
    };
    if depth >= MAX_SUBAGENT_DEPTH {
        warn!(
            tool_use_id = %call.id,
            depth,
            "task_delegator: sub-agent depth limit reached"
        );
        return synthia_tool::ToolOutput::error(format!(
            "cannot delegate: maximum sub-agent depth ({}) reached; \
             the `task` tool is disabled at this nesting level",
            MAX_SUBAGENT_DEPTH
        ));
    }
    let peer = match peers.resolve_sync(&spec.agent) {
        Some(peer) => peer,
        None => {
            let names = peers.names().join(", ");
            return synthia_tool::ToolOutput::error(format!(
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
