use std::sync::Arc;

use futures::StreamExt;
use synthia_core::CancelToken;
use synthia_harness::{
    Agent,
    AgentEvent,
    AgentInput,
    AgentMeta,
    SessionEndReason,
    SystemEvent,
};
use synthia_provider::{ContentPart, TextContent};

use super::spec::TaskSpec;
use crate::{
    gate::{CommandRunner, StdCommandRunner, apply_gate_to_output, run_gate},
    worktree::{self, WorktreeCleanupResult, WorktreeInfo},
};

/// Run one sub-agent and translate its event stream into a tool
/// result.
///
/// Every child event is forwarded through `on_event` wrapped in
/// [`AgentEvent::Agent`] with an [`AgentMeta`] tying it to the
/// parent (`parent_session_id`, `child_depth` =
/// `input_depth + 1`). The child's accumulated `Model` text
/// becomes the tool result; a cancelled or failed child becomes
/// an error result so the parent model sees a usable signal.
///
/// # Gate + isolation lifecycle
///
/// When `spec.gate` is `Some`, the gate runs **after** the child
/// completes (and **after** the worktree is set up, if isolation
/// is set). The gate's cwd is the worktree path when isolation
/// is on, otherwise the parent base cwd. A passing gate lets
/// the child's text through; a failing / timed-out / killed
/// gate replaces the tool result with a structured error.
///
/// When `spec.isolation` is `Some`, the child runs against a
/// fresh detached worktree and the worktree is cleaned up
/// **before** the gate runs, so the gate observes whatever the
/// child committed (or, for dirty trees, whatever the child left
/// on disk at cleanup time). Cleanup reports branch + path +
/// `base_sha` when changes are present so the caller can
/// surface them.
#[allow(dead_code)]
pub(crate) async fn run_subagent(
    agent: Arc<dyn Agent>,
    spec: TaskSpec,
    parent_session_id: &str,
    input_depth: usize,
    cancel: Arc<dyn CancelToken>,
    on_event: futures::channel::mpsc::UnboundedSender<AgentEvent>,
) -> synthia_tool::ToolOutput {
    let runner: Arc<dyn CommandRunner> = Arc::new(StdCommandRunner::new());
    run_subagent_with_runner(
        agent,
        spec,
        HostContext {
            parent_session_id,
            writers: None,
        },
        input_depth,
        cancel,
        on_event,
        &runner,
    )
    .await
}

/// What the delegator knows about the host context a child runs
/// in: the parent session id (empty at this layer — the server
/// assigns session ids) and the writer source that opens the
/// child's own session. `writers: None` runs the child with no
/// log of its own.
pub(crate) struct HostContext<'a> {
    pub(crate) parent_session_id: &'a str,
    pub(crate) writers: Option<Arc<dyn synthia_harness::SubagentWriters>>,
}

/// Like [`run_subagent`] but with an injectable
/// [`CommandRunner`]. Production callers use [`run_subagent`]
/// (which delegates here with the real runner); tests pass a
/// recording fake so the gate + worktree lifecycle can be
/// observed without spawning real `git` / shell processes.
pub(crate) async fn run_subagent_with_runner(
    agent: Arc<dyn Agent>,
    spec: TaskSpec,
    host: HostContext<'_>,
    input_depth: usize,
    cancel: Arc<dyn CancelToken>,
    on_event: futures::channel::mpsc::UnboundedSender<AgentEvent>,
    runner: &Arc<dyn CommandRunner>,
) -> synthia_tool::ToolOutput {
    let parent_session_id = host.parent_session_id;
    let writers = host.writers;
    let child_depth = input_depth + 1;
    // A stable per-child trace id so downstream consumers (the
    // web frontend, session replay) can group a child's events
    // into one sub-agent bubble even when the parent delegated
    // to several peers in parallel. The agent runtime has no
    // session registry of its own (the server assigns session
    // ids), so we mint an opaque ULID here. The same id names
    // the child's own session log when a writer source is
    // wired.
    let child_trace_id = ulid::Ulid::generate().to_string();
    let meta =
        AgentMeta::new(parent_session_id, child_trace_id.clone(), child_depth)
            .with_agent_name(Some(spec.agent.clone()));
    // The first forwarded event carries the dispatch prompt; the
    // session opener wants it up front so the child's log opens
    // with the prompt before the child's first row.
    let first_meta = meta.clone().with_prompt(Some(spec.prompt.clone()));
    let mut input = AgentInput::text(spec.prompt.clone());
    input.subagent_depth = child_depth;
    // The child's harness writes the child's model facts into
    // the child's own session: open that session (its header and
    // prompt land ahead of the child's first row) and hand the
    // run the writer, plus the source for any grandchild this
    // child delegates to.
    if let Some(source) = &writers {
        input.log_writer = Some(source.open_child(&first_meta));
        input.subagent_writers = Some(Arc::clone(source));
    }

    // R-OPT-1 Step 6.2: when the parent populated
    // `spec.trace_context`, resolve it into an OTel
    // `Context` here. We deliberately do NOT call
    // `Context::attach()` over the `agent.run().await`
    // future — `ContextGuard` is `!Send` and breaks the
    // `+ Send` bound on the agent future. The wire shim
    // is therefore available for callers that surface the
    // parent explicitly (a future iteration can set the
    // ambient OTel context via `tokio::task::LocalSet`),
    // and the child starts a root span in the meantime.
    // This is the documented limitation in the plan
    // ("sub-agent starts a root span — acceptable
    // degradation").
    #[cfg(feature = "otel")]
    let _parent_cx = spec
        .trace_context
        .as_ref()
        .and_then(parent_context_from_wire);
    #[cfg(not(feature = "otel"))]
    let _parent_cx = Option::<()>::None;
    let mut stream = agent.run(input, cancel.clone()).await;
    let mut first_event = true;
    let mut text = String::new();
    let mut end_reason = SessionEndReason::Completed;
    while let Some(event) = stream.next().await {
        match &event {
            AgentEvent::Model(ContentPart::Text(TextContent {
                text: t,
                ..
            })) => {
                if !t.is_empty() {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(t);
                }
            }
            AgentEvent::System(SystemEvent::SessionEnded { reason }) => {
                end_reason = reason.clone();
            }
            _ => {}
        }
        let event_meta = if first_event {
            first_event = false;
            first_meta.clone()
        } else {
            meta.clone()
        };
        on_event
            .unbounded_send(AgentEvent::Agent(event_meta, Box::new(event)))
            .ok();
    }

    let child_text = if matches!(end_reason, SessionEndReason::Completed) {
        if text.trim().is_empty() {
            None
        } else {
            Some(text)
        }
    } else {
        None
    };

    let child_failed = !matches!(end_reason, SessionEndReason::Completed);

    // 1. Set up the worktree (if isolation is on). The worktree
    //    path becomes the gate's cwd and the only path the
    //    caller ever needs to know about.
    let mut worktree: Option<WorktreeInfo> = None;
    if spec.isolation.is_some() {
        let base_cwd = std::env::current_dir()
            .unwrap_or_else(|_| std::path::PathBuf::from("."));
        match worktree::create_worktree(&base_cwd, runner.as_ref()) {
            Ok(info) => worktree = Some(info),
            Err(err) => {
                return synthia_tool::ToolOutput::error(format!(
                    "failed to set up worktree for sub-agent \
                     `{}`: {err}",
                    spec.agent
                ));
            }
        }
    }

    let gate_cwd: std::path::PathBuf = worktree
        .as_ref()
        .map(|w| w.path.clone())
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    // 2. Run the gate (if attached). Pass ⇒ keep the child's
    //    text; anything else ⇒ wrap in a structured error.
    if let Some(gate_spec) = &spec.gate {
        let verdict = match run_gate(
            gate_spec,
            &gate_cwd,
            runner.as_ref(),
            cancel.clone(),
        ) {
            Ok(verdict) => verdict,
            Err(err) => {
                return synthia_tool::ToolOutput::error(format!(
                    "failed gate `{}`: {err}",
                    gate_spec.label()
                ));
            }
        };
        if verdict.is_failure() {
            let output = apply_gate_to_output(
                child_text.as_deref(),
                &verdict,
                gate_spec.label(),
            );
            // Best-effort cleanup of any worktree we created.
            if let Some(info) = worktree.take() {
                let _ = worktree::cleanup_worktree(
                    &std::env::current_dir().unwrap_or_default(),
                    &info,
                    &spec.agent,
                    runner.as_ref(),
                );
            }
            return output;
        }
    }

    // 3. Cleanup the worktree (if any). Dirty ⇒ branch-committed
    //    result is surfaced in the tool result only when no gate
    //    ran; a gate's verdict already produced the result, and
    //    appending a worktree report would muddy the message.
    let cleanup_report: Option<WorktreeCleanupResult> =
        if let Some(info) = worktree.take() {
            let base_cwd = std::env::current_dir().unwrap_or_default();
            let result = worktree::cleanup_worktree(
                &base_cwd,
                &info,
                &spec.agent,
                runner.as_ref(),
            );
            Some(result)
        } else {
            None
        };

    // 4. Compose the tool result. Order of preference:
    //    a. child failed ⇒ error message naming the reason.
    //    b. child produced text ⇒ return it (with an optional
    //       worktree branch summary appended when relevant).
    //    c. child produced no text + no gate ⇒ "produced no
    //       final output" error.
    if child_failed {
        return synthia_tool::ToolOutput::error(format!(
            "sub-agent `{}` ended before completing: {:?}\n\
             [subagent session: {child_trace_id}]",
            spec.agent, end_reason
        ));
    }

    match child_text {
        Some(text) => {
            let mut combined = text;
            if let Some(report) =
                cleanup_report.as_ref().filter(|r| r.has_changes)
                && let (Some(branch), Some(path)) =
                    (report.branch.as_ref(), report.path.as_ref())
            {
                combined.push_str(&format!(
                    "\n\n[worktree: branch `{branch}` at {path}, \
                     base {}]",
                    report.base_sha
                ));
            }
            combined.push_str(&session_line(&child_trace_id));
            synthia_tool::ToolOutput::text(combined)
        }
        None => synthia_tool::ToolOutput::error(format!(
            "sub-agent `{}` produced no final output{}",
            spec.agent,
            session_line(&child_trace_id)
        )),
    }
}

/// The machine-readable tail line every `task` result carries,
/// naming the child session the trace lives in. Consumers that
/// route child events into their own sessions (the server's
/// subagent router) keep the model's result and the UI's link
/// pointing at the same id.
fn session_line(child_session_id: &str) -> String {
    format!("\n\n[subagent session: {child_session_id}]")
}

/// Reconstruct an [`opentelemetry::Context`] carrying the
/// parent's remote span from a wire shim. The returned context
/// is the one to attach as the ambient OTel context for the
/// child run, so the child opens its first span as a child of
/// the parent's `traceparent`-encoded span.
///
/// `None` when the hex fields are malformed (extra defensive
/// check; `TaskSpec::from_value` already validated lengths).
#[cfg(feature = "otel")]
fn parent_context_from_wire(
    wire: &crate::task::spec::TaskTraceContext,
) -> Option<opentelemetry::Context> {
    use std::str::FromStr;

    use opentelemetry::trace::{
        SpanContext,
        SpanId,
        TraceContextExt,
        TraceFlags,
        TraceId,
        TraceState,
    };

    let trace_id = TraceId::from_hex(&wire.trace_id_hex).ok()?;
    let parent_span_id = SpanId::from_hex(&wire.parent_span_id_hex).ok()?;
    let trace_flags = if wire.trace_flags_byte & 0x01 != 0 {
        TraceFlags::SAMPLED
    } else {
        TraceFlags::default()
    };
    let trace_state = if wire.trace_state.is_empty() {
        TraceState::default()
    } else {
        TraceState::from_str(&wire.trace_state)
            .unwrap_or_else(|_| TraceState::default())
    };
    let span_cx = SpanContext::new(
        trace_id,
        parent_span_id,
        trace_flags,
        true,
        trace_state,
    );
    if !span_cx.is_valid() {
        return None;
    }
    Some(opentelemetry::Context::new().with_remote_span_context(span_cx))
}
